//! The git `pre-commit` and `pre-push` hooks: refuse a commit or push that changes anything the
//! agent has not claimed, whichever tool made the change (a shell edit included). `install`
//! writes a small shell shim into the repository's hooks directory; the shim runs `tessel hook git`
//! and, when the binary is gone, falls through by itself (see `shim_text`).
//!
//! The hooks live in the directory every worktree of the repository shares, so each run gates
//! itself: a worktree with no `.tessel/state.json` passes. An existing hook of another tool is
//! kept as `<name>.pre-tessel` and run first, with the same arguments and stdin. When a tool
//! overwrites the shim later, the next install chains its hook and keeps the earlier chained
//! one under a dated name.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use tessel_coordinator::protocol::{uncovered, Mode, ScopeClaim};

use crate::hook::{claim_arg, shell_quote};
use crate::plan;
use crate::render::{escape, mode_text, scope_text};
use crate::rpc::{self, Reply, Request};
use crate::state::{Connection, State};
use crate::submit;
use crate::worktree::Worktree;
use crate::GitHook;

const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const CHAINED_SUFFIX: &str = ".pre-tessel";
const EXIT_REFUSED: u8 = 1;
const MAX_BACKUP_NUMBER: u32 = 10;
const SHIM_MARKER: &str = "# tessel git hook";
const MERGE_NOTE: &str = "tessel: concluding a merge, so the pre-commit coverage check is \
skipped; pre-push and the coordinator still check the merge commit\n";
const ALL_HOOKS: [GitHook; 2] = [GitHook::PreCommit, GitHook::PrePush];

impl GitHook {
    fn name(self) -> &'static str {
        match self {
            Self::PreCommit => "pre-commit",
            Self::PrePush => "pre-push",
        }
    }

    fn action(self) -> &'static str {
        match self {
            Self::PreCommit => "commit",
            Self::PrePush => "push",
        }
    }
}

// ---------- the hook ----------

/// Runs one git hook. Exit 0 lets git go on; anything else stops it.
pub async fn run(cwd: &Path, hook: GitHook, args: &[String]) -> anyhow::Result<ExitCode> {
    let stdin = match hook {
        GitHook::PrePush => read_stdin()?,
        GitHook::PreCommit => Vec::new(),
    };
    if let Some(failed) = run_chained(cwd, hook, args, &stdin)? {
        return Ok(failed);
    }
    let worktree = Worktree::discover(cwd)?;
    if !worktree.state_path().exists() {
        return Ok(ExitCode::SUCCESS);
    }
    if hook == GitHook::PreCommit && merge_in_progress(&worktree.root)? {
        let _ = std::io::stderr().write_all(MERGE_NOTE.as_bytes());
        return Ok(ExitCode::SUCCESS);
    }
    let Some(state) = daemon_state(&worktree, hook).await? else {
        return Ok(ExitCode::SUCCESS);
    };
    let missing = uncovered_changes(&worktree.root, hook, &state, &stdin)?;
    if missing.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    let _ = std::io::stderr().write_all(refusal_text(hook, &missing).as_bytes());
    Ok(ExitCode::from(EXIT_REFUSED))
}

/// The scopes the commit or push changes that no unsubmitted claim covers.
fn uncovered_changes(
    root: &Path,
    hook: GitHook,
    state: &State,
    stdin: &[u8],
) -> anyhow::Result<Vec<ScopeClaim>> {
    let held = unsubmitted_scopes(state);
    let mut missing = Vec::new();
    for touched in touched_ranges(root, hook, state, stdin)? {
        let touched = plan::collapse(touched, &held, plan::MAX_SCOPES_PER_MESSAGE);
        for scope in uncovered(&held, &touched) {
            if !missing.contains(&scope) {
                missing.push(scope);
            }
        }
    }
    Ok(missing)
}

/// Whether `git commit` is concluding a merge. The diff base is the history of `HEAD`, which
/// does not hold the merged-in side, so the index would look like the merged work was never
/// claimed.
fn merge_in_progress(root: &Path) -> anyhow::Result<bool> {
    Ok(git_path(root, "MERGE_HEAD")?.exists())
}

fn read_stdin() -> anyhow::Result<Vec<u8>> {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .context("cannot read the list of refs git is pushing")?;
    Ok(input)
}

/// The agent's claims that are not submitted. A submitted claim is the steward's now.
fn unsubmitted_scopes(state: &State) -> Vec<ScopeClaim> {
    state
        .claims
        .iter()
        .filter(|held| !held.submitted)
        .flat_map(|held| held.scopes.iter().cloned())
        .collect()
}

/// The daemon's state, or `None` when `tessel stop` ended it: a stopped worktree holds nothing
/// and is not guarded.
async fn daemon_state(worktree: &Worktree, hook: GitHook) -> anyhow::Result<Option<State>> {
    match rpc::call(&worktree.sock(), &Request::Status, STATUS_TIMEOUT).await {
        Ok(Reply::Status { state }) => Ok(Some(*state)),
        Ok(_) => bail!("the daemon answered the status request with something unexpected"),
        Err(_) if stopped(worktree) => Ok(None),
        Err(e) => bail!(
            "this worktree has Tessel state (.tessel/state.json) but its daemon does not answer \
             ({e}), so the {} cannot be checked against your claims. Run `tessel start \
             \"<intent>\"`, or use `--no-verify` to skip this check; the coordinator still \
             rejects a submission your claims do not cover",
            hook.action()
        ),
    }
}

fn stopped(worktree: &Worktree) -> bool {
    matches!(
        State::read(worktree),
        Ok(Some(State {
            connection: Connection::Stopped,
            ..
        }))
    )
}

/// The scopes to check: the index for a commit, one range per pushed commit for a push.
fn touched_ranges(
    root: &Path,
    hook: GitHook,
    state: &State,
    stdin: &[u8],
) -> anyhow::Result<Vec<Vec<ScopeClaim>>> {
    match hook {
        GitHook::PreCommit => {
            let base = submit::diff_base(root, state, "HEAD")?;
            Ok(vec![submit::touched_index(root, &base)?])
        }
        GitHook::PrePush => {
            let mut ranges = Vec::new();
            for sha in pushed_commits(stdin)? {
                let base = submit::diff_base(root, state, &sha)?;
                ranges.push(submit::touched(root, &base, &sha)?);
            }
            Ok(ranges)
        }
    }
}

/// The local object of each ref git is about to push, from lines of `<local ref> <local sha>
/// <remote ref> <remote sha>`. A deletion (all-zero local sha) sends no commit and is skipped.
fn pushed_commits(stdin: &[u8]) -> anyhow::Result<Vec<String>> {
    let text = String::from_utf8_lossy(stdin);
    let mut shas = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<&str> = line.split(' ').collect();
        let [_local_ref, sha, _remote_ref, _remote_sha] = fields.as_slice() else {
            bail!(
                "git sent a pre-push line that is not `<ref> <sha> <ref> <sha>`: {}",
                escape(line)
            );
        };
        if !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) || sha.is_empty() {
            bail!(
                "git sent a pre-push line with a bad object name: {}",
                escape(line)
            );
        }
        if sha.bytes().all(|byte| byte == b'0') || shas.iter().any(|seen| seen == sha) {
            continue;
        }
        shas.push((*sha).to_string());
    }
    Ok(shas)
}

fn refusal_text(hook: GitHook, missing: &[ScopeClaim]) -> String {
    let mut out = format!(
        "tessel: refusing the {}: {} scope(s) it changes are not covered by your claims:\n",
        hook.action(),
        missing.len()
    );
    for claim in missing {
        let _ = writeln!(
            out,
            "  - {} ({})",
            scope_text(&claim.scope),
            mode_text(claim.mode)
        );
    }
    out.push_str("Claim them, then try again:\n");
    for line in claim_hints(missing) {
        let _ = writeln!(out, "  {line}");
    }
    out.push_str(
        "`--no-verify` skips this check; the coordinator still rejects a submission your claims \
         do not cover.\n",
    );
    out
}

/// One `tessel claim` command per mode, scopes shell-quoted. Paths are file names, so the
/// result is escaped for the terminal as well.
fn claim_hints(missing: &[ScopeClaim]) -> Vec<String> {
    let mut modes: Vec<Mode> = Vec::new();
    for claim in missing {
        if !modes.contains(&claim.mode) {
            modes.push(claim.mode);
        }
    }
    let mut hints = Vec::new();
    for mode in modes {
        let scopes: Vec<String> = missing
            .iter()
            .filter(|claim| claim.mode == mode)
            .map(|claim| shell_quote(&claim_arg(&claim.scope)))
            .collect();
        let flag = match mode {
            Mode::EditBody => String::new(),
            Mode::Depend | Mode::EditSignature | Mode::Create => {
                format!(" --mode {}", mode_text(mode))
            }
        };
        hints.push(escape(&format!("tessel claim {}{flag}", scopes.join(" "))));
    }
    hints
}

// ---------- the hook of another tool ----------

/// Runs `<hooks dir>/<name>.pre-tessel` when it exists and is executable, with git's arguments
/// and stdin. Returns the exit code to stop with when it failed.
fn run_chained(
    cwd: &Path,
    hook: GitHook,
    args: &[String],
    stdin: &[u8],
) -> anyhow::Result<Option<ExitCode>> {
    let dir = hooks_dir(cwd)?;
    let path = dir.join(format!("{}{CHAINED_SUFFIX}", hook.name()));
    let executable = std::fs::metadata(&path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0);
    if !executable {
        return Ok(None);
    }
    let mut command = Command::new(&path);
    command.args(args);
    if hook == GitHook::PrePush {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot run the existing hook {}", path.display()))?;
    let feeder = child.stdin.take().map(|mut pipe| {
        let input = stdin.to_vec();
        std::thread::spawn(move || {
            // The hook may exit without reading its stdin.
            let _ = pipe.write_all(&input);
        })
    });
    let status = child
        .wait()
        .with_context(|| format!("cannot wait for the existing hook {}", path.display()))?;
    if let Some(feeder) = feeder {
        let _ = feeder.join();
    }
    if status.success() {
        return Ok(None);
    }
    let code = status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(EXIT_REFUSED);
    Ok(Some(ExitCode::from(code)))
}

// ---------- install ----------

/// Installs both hooks into the repository's hooks directory and reports what it did.
pub fn install(cwd: &Path) -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("cannot locate the tessel binary")?;
    let Some(exe) = exe.to_str() else {
        bail!("the tessel binary's path is not valid UTF-8, so a hook cannot run it");
    };
    refuse_relative_hooks_path(cwd, exe)?;
    let dir = hooks_dir(cwd)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create the hooks directory {}", dir.display()))?;
    let mut report = String::new();
    if in_build_directory(Path::new(exe)) {
        let _ = writeln!(
            report,
            "warning: {} is in a build directory that goes away with its worktree. The hooks fall \
             back safely without it, but install a stable binary (`cargo install --path \
             tessel-cli`) and run this again from it.",
            escape(exe)
        );
    }
    for hook in ALL_HOOKS {
        report.push_str(&install_one(&dir, hook, exe)?);
    }
    report.push_str(
        "The hooks apply to every worktree of this repository and pass in any worktree without \
         .tessel/ (no `tessel start` there). `prek install` and similar tools overwrite them: run \
         this again afterwards, and the hook they left runs first.\n",
    );
    Ok(report)
}

/// A relative `core.hooksPath` means a different directory in each worktree, so there is no one
/// place to install into.
fn refuse_relative_hooks_path(cwd: &Path, exe: &str) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--type=path", "--get", "core.hooksPath"])
        .output()
        .context("cannot run git; is git installed?")?;
    let configured = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if configured.is_empty() || Path::new(&configured).is_absolute() {
        return Ok(());
    }
    bail!(
        "core.hooksPath is the relative path {}, which names a different directory in each \
         worktree, so no hook was installed. Either set an absolute path \
         (`git config core.hooksPath /absolute/dir`) and run this again, or add \
         `exec {} hook git pre-commit \"$@\"` to the pre-commit hook in that directory and \
         `exec {} hook git pre-push \"$@\"` to its pre-push hook by hand",
        escape(&configured),
        shell_quote(exe),
        shell_quote(exe)
    );
}

fn install_one(dir: &Path, hook: GitHook, exe: &str) -> anyhow::Result<String> {
    let path = dir.join(hook.name());
    let shim = shim_text(hook, exe);
    let mut note = format!("{}: installed\n", hook.name());
    match std::fs::read_to_string(&path) {
        Ok(existing) if existing == shim => {
            return Ok(format!("{}: already installed\n", hook.name()));
        }
        Ok(existing) if is_tessel_shim(&existing, hook) => {
            note = format!("{}: updated\n", hook.name());
        }
        Ok(_) => note = chain_existing(dir, hook, SystemTime::now())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => bail!("cannot read the existing hook {}: {e}", path.display()),
    }
    write_executable(&path, &shim)?;
    Ok(note)
}

/// Makes the other tool's hook the one that runs first, as `<name>.pre-tessel`.
///
/// When `<name>.pre-tessel` already exists, an earlier install chained a hook and another tool
/// then overwrote the shim. The same text means the tool wrote the chained hook back, so nothing
/// moves. A different text replaces it, and the old one is kept under a dated name.
fn chain_existing(dir: &Path, hook: GitHook, now: SystemTime) -> anyhow::Result<String> {
    let from = dir.join(hook.name());
    let to = dir.join(format!("{}{CHAINED_SUFFIX}", hook.name()));
    if !to.exists() {
        move_file(&from, &to)?;
        return Ok(format!(
            "{}: installed; your existing hook is now {} and runs first\n",
            hook.name(),
            to.display()
        ));
    }
    let new = std::fs::read(&from)
        .with_context(|| format!("cannot read the existing hook {}", from.display()))?;
    let old = std::fs::read(&to)
        .with_context(|| format!("cannot read the chained hook {}", to.display()))?;
    if new == old {
        return Ok(format!(
            "{}: installed; your existing hook is still {} and runs first\n",
            hook.name(),
            to.display()
        ));
    }
    let backup = backup_path(&to, &utc_stamp(now))?;
    move_file(&to, &backup)?;
    move_file(&from, &to)?;
    Ok(format!(
        "{}: installed; your existing hook is now {} and runs first; the earlier chained hook \
         was kept as {} (merge it by hand if you still need it)\n",
        hook.name(),
        to.display(),
        backup.display()
    ))
}

fn move_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::rename(from, to)
        .with_context(|| format!("cannot move {} to {}", from.display(), to.display()))
}

/// `<chained>.<stamp>`, or `<chained>.<stamp>-1`, `-2`, ... when that name is taken. Never an
/// existing file, so an earlier backup survives.
fn backup_path(chained: &Path, stamp: &str) -> anyhow::Result<PathBuf> {
    let first = PathBuf::from(format!("{}.{stamp}", chained.display()));
    if !first.exists() {
        return Ok(first);
    }
    for n in 1..MAX_BACKUP_NUMBER {
        let numbered = PathBuf::from(format!("{}-{n}", first.display()));
        if !numbered.exists() {
            return Ok(numbered);
        }
    }
    bail!(
        "{} is a hook of another tool and {} already exists with different text, but its backup \
         names {}, -1 ... -{} are all taken. This hook was not changed; remove {} (merge it by \
         hand first if you need it) and run this again",
        chained.with_extension("").display(),
        chained.display(),
        first.display(),
        MAX_BACKUP_NUMBER - 1,
        first.display()
    );
}

/// `20261011T123456Z` for `time`, in UTC.
fn utc_stamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The hook file. It runs the binary that installed it, else one found on `PATH`. When neither
/// exists, a worktree with Tessel state fails closed and any other worktree of the repository
/// passes after running the other tool's hook, so deleting the binary (a lane's `target/`, say)
/// never breaks commits elsewhere. `exec` hands git's stdin on.
fn shim_text(hook: GitHook, exe: &str) -> String {
    let name = hook.name();
    format!(
        "#!/bin/sh\n\
         {SHIM_MARKER}\n\
         t={exe}; [ -x \"$t\" ] || t=$(command -v tessel) || t=\n\
         [ -x \"$t\" ] && exec \"$t\" hook git {name} \"$@\"\n\
         [ -e \"$(git rev-parse --show-toplevel)/.tessel/state.json\" ] && {{ echo \"tessel: \
         binary missing; reinstall or use --no-verify\" >&2; exit 1; }}\n\
         h=\"$(dirname \"$0\")/{name}{CHAINED_SUFFIX}\"; [ -x \"$h\" ] && exec \"$h\" \"$@\"; exit 0\n",
        exe = shell_quote(exe),
    )
}

/// Whether `text` is a shim this command wrote (for any path of the binary).
fn is_tessel_shim(text: &str, hook: GitHook) -> bool {
    let mut lines = text.lines();
    lines.next() == Some("#!/bin/sh")
        && lines.next() == Some(SHIM_MARKER)
        && text.contains(&format!(" hook git {} \"$@\"", hook.name()))
}

/// Whether `exe` sits in a Cargo `target/` directory, which goes away with its worktree.
fn in_build_directory(exe: &Path) -> bool {
    exe.components().any(|part| part.as_os_str() == "target")
}

fn write_executable(path: &Path, text: &str) -> anyhow::Result<()> {
    let temp = path.with_extension("tessel-new");
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o755)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&temp, path)
    };
    write().with_context(|| format!("cannot write the hook {}", path.display()))
}

/// The hooks directory git uses for `cwd`'s repository: `core.hooksPath` when set, else the
/// common `.git/hooks` of every worktree.
fn hooks_dir(cwd: &Path) -> anyhow::Result<PathBuf> {
    git_path(cwd, "hooks")
}

/// Where git keeps `name` for the worktree at `cwd` (`git rev-parse --git-path`).
fn git_path(cwd: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--path-format=absolute", "--git-path", name])
        .output()
        .context("cannot run git; is git installed?")?;
    if !output.status.success() {
        bail!(
            "git rev-parse --git-path {name} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim_end_matches('\n'),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_coordinator::protocol::{Scope, SymbolId};

    const ZERO: &str = "0000000000000000000000000000000000000000";
    const SHA: &str = "67890abcdef67890abcdef67890abcdef67890ab";

    fn push_line(local: &str) -> String {
        format!("refs/heads/topic {local} refs/heads/topic {ZERO}\n")
    }

    #[test]
    fn pushed_commits_skips_deletions_and_repeats() {
        let input = format!(
            "(delete) {ZERO} refs/heads/gone {SHA}\n{}{}",
            push_line(SHA),
            push_line(SHA)
        );
        assert_eq!(pushed_commits(input.as_bytes()).unwrap(), vec![SHA]);
        assert!(pushed_commits(b"").unwrap().is_empty());
    }

    #[test]
    fn pushed_commits_refuses_a_line_it_cannot_read() {
        assert!(pushed_commits(b"refs/heads/a nothex refs/heads/a 0\n").is_err());
        assert!(pushed_commits(b"too few fields\n").is_err());
    }

    #[test]
    fn a_shim_is_recognised_for_any_binary_path_but_only_for_its_own_hook() {
        let shim = shim_text(GitHook::PreCommit, "/opt/my tools/tessel");
        assert!(is_tessel_shim(&shim, GitHook::PreCommit));
        assert!(!is_tessel_shim(&shim, GitHook::PrePush));
        assert!(!is_tessel_shim("#!/bin/sh\nprek run\n", GitHook::PreCommit));
    }

    #[test]
    fn a_cargo_target_directory_is_a_build_directory() {
        assert!(in_build_directory(Path::new(
            "/work/lane/target/debug/tessel"
        )));
        assert!(!in_build_directory(Path::new("/usr/local/bin/tessel")));
    }

    #[test]
    fn claim_hints_quote_scopes_and_group_by_mode() {
        let file = |path: &str, mode| ScopeClaim {
            scope: Scope::File { path: path.into() },
            mode,
        };
        let symbol = ScopeClaim {
            scope: Scope::Symbol(SymbolId {
                path: "src/a.rs".into(),
                qualified_name: "a::a".into(),
            }),
            mode: Mode::EditBody,
        };
        let hints = claim_hints(&[
            file("src/it's here.rs", Mode::Create),
            symbol,
            file("src/new.rs", Mode::Create),
        ]);
        assert_eq!(
            hints,
            vec![
                "tessel claim 'src/it'\\''s here.rs' src/new.rs --mode create",
                "tessel claim src/a.rs::a::a",
            ]
        );
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn the_backup_stamp_prints_as_a_compact_utc_time() {
        assert_eq!(utc_stamp(at(0)), "19700101T000000Z");
        assert_eq!(utc_stamp(at(1_791_601_312)), "20261010T030152Z");
        assert_eq!(utc_stamp(at(951_782_400 + 3661)), "20000229T010101Z");
        assert_eq!(utc_stamp(at(951_868_799)), "20000229T235959Z");
    }

    #[test]
    fn a_backup_never_takes_a_name_that_exists_and_stops_at_a_bound() {
        let dir = tempfile::tempdir().unwrap();
        let chained = dir.path().join("pre-commit.pre-tessel");
        let stamp = "20261011T010203Z";
        let first = dir.path().join(format!("pre-commit.pre-tessel.{stamp}"));
        assert_eq!(backup_path(&chained, stamp).unwrap(), first);

        std::fs::write(&first, "").unwrap();
        let second = dir.path().join(format!("pre-commit.pre-tessel.{stamp}-1"));
        assert_eq!(backup_path(&chained, stamp).unwrap(), second);

        for n in 1..MAX_BACKUP_NUMBER {
            std::fs::write(format!("{}-{n}", first.display()), "").unwrap();
        }
        let refused = backup_path(&chained, stamp).unwrap_err().to_string();
        assert!(
            refused.contains(&format!("remove {}", first.display())),
            "{refused}"
        );
        assert!(refused.contains("This hook was not changed"), "{refused}");
    }

    #[test]
    fn a_reinstall_with_every_backup_name_taken_changes_neither_hook() {
        let dir = tempfile::tempdir().unwrap();
        let (hook, chained) = (
            dir.path().join("pre-commit"),
            dir.path().join("pre-commit.pre-tessel"),
        );
        std::fs::write(&hook, "#!/bin/sh\necho new\n").unwrap();
        std::fs::write(&chained, "#!/bin/sh\necho old\n").unwrap();
        let first = dir.path().join("pre-commit.pre-tessel.20261011T010203Z");
        std::fs::write(&first, "taken").unwrap();
        for n in 1..MAX_BACKUP_NUMBER {
            std::fs::write(format!("{}-{n}", first.display()), "taken").unwrap();
        }

        let refused = chain_existing(dir.path(), GitHook::PreCommit, at(1_791_680_523))
            .unwrap_err()
            .to_string();

        assert!(
            refused.contains(&format!("remove {}", first.display())),
            "{refused}"
        );
        assert_eq!(std::fs::read(&hook).unwrap(), b"#!/bin/sh\necho new\n");
        assert_eq!(std::fs::read(&chained).unwrap(), b"#!/bin/sh\necho old\n");
    }
}

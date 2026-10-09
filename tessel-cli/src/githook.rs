//! The git `pre-commit` and `pre-push` hooks: refuse a commit or push that changes anything the
//! agent has not claimed, whichever tool made the change (a shell edit included). `install`
//! writes a small shell shim into the repository's hooks directory; the shim runs `tessel hook git`
//! and, when the binary is gone, falls through by itself (see `shim_text`).
//!
//! The hooks live in the directory every worktree of the repository shares, so each run gates
//! itself: a worktree with no `.tessel/state.json` passes. An existing hook of another tool is
//! kept as `<name>.pre-tessel` and run first, with the same arguments and stdin.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

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
         this again afterwards.\n",
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
        Ok(_) => note = chain_existing(dir, hook)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => bail!("cannot read the existing hook {}: {e}", path.display()),
    }
    write_executable(&path, &shim)?;
    Ok(note)
}

/// Moves the other tool's hook to `<name>.pre-tessel`, where the shim's `tessel hook git` runs it.
/// Refuses to overwrite an earlier one.
fn chain_existing(dir: &Path, hook: GitHook) -> anyhow::Result<String> {
    let from = dir.join(hook.name());
    let to = dir.join(format!("{}{CHAINED_SUFFIX}", hook.name()));
    if to.exists() {
        bail!(
            "{} is a hook of another tool and {} already exists, so it cannot be kept; merge \
             them by hand and run this again",
            from.display(),
            to.display()
        );
    }
    std::fs::rename(&from, &to)
        .with_context(|| format!("cannot move {} to {}", from.display(), to.display()))?;
    Ok(format!(
        "{}: installed; your existing hook is now {} and runs first\n",
        hook.name(),
        to.display()
    ))
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
}

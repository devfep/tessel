//! The git `pre-commit` and `pre-push` hooks end to end: real `git commit` and `git push` in temp
//! repositories, with the hooks `tessel hook install --git` writes and the real daemon against
//! the fake coordinator.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]
#![expect(
    dead_code,
    reason = "each test crate compiles all of support, which the other test crates also use"
)]

mod support;

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use std::time::Duration;

use anyhow::{Context, Result};
use support::{eventually, git, Agent, Done, Fake};
use tessel_coordinator::protocol::EventKind;

const TOK1: &str = "tok-a1-S3CRETvalue";

const PAIR: &str = "pub fn one() {}\npub fn two() {}\n";

async fn world() -> Result<(Fake, Agent)> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    Ok((fake, a1))
}

/// An agent with `src/pair.rs` (two functions) committed, the hooks installed, and a daemon.
async fn started() -> Result<(Fake, Agent)> {
    let (fake, a1) = world().await?;
    std::fs::write(a1.root().join("src/pair.rs"), PAIR)?;
    git(&a1.root(), &["add", "src/pair.rs"])?;
    git(&a1.root(), &["commit", "-q", "-m", "pair"])?;
    install(&a1)?;
    a1.start_settled("edit things").await?;
    Ok((fake, a1))
}

fn install(agent: &Agent) -> Result<()> {
    let done = agent.tessel(&["hook", "install", "--git"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    Ok(())
}

fn git_done(dir: &Path, args: &[&str]) -> Result<Done> {
    Ok(Done::from(
        Command::new("git").arg("-C").arg(dir).args(args).output()?,
    ))
}

fn write(agent: &Agent, path: &str, text: &str) -> Result<()> {
    Ok(std::fs::write(agent.root().join(path), text)?)
}

/// Writes `text` to `path` and runs `git commit`, returning git's verdict.
fn commit_file(agent: &Agent, path: &str, text: &str) -> Result<Done> {
    write(agent, path, text)?;
    git(&agent.root(), &["add", path])?;
    git_done(&agent.root(), &["commit", "-q", "-m", "change"])
}

fn hooks_dir(agent: &Agent) -> PathBuf {
    agent.root().join(".git/hooks")
}

fn bare_remote(agent: &Agent) -> Result<tempfile::TempDir> {
    let remote = tempfile::tempdir()?;
    git(remote.path(), &["init", "-q", "--bare"])?;
    git(
        &agent.root(),
        &[
            "remote",
            "add",
            "origin",
            &remote.path().display().to_string(),
        ],
    )?;
    Ok(remote)
}

fn remote_has(remote: &Path, branch: &str) -> bool {
    git(remote, &["rev-parse", "--verify", "-q", branch]).is_ok()
}

fn executable_script(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_of_an_unclaimed_file_is_refused_with_the_claim_to_make() -> Result<()> {
    let (_fake, a1) = started().await?;
    let before = git(&a1.root(), &["rev-parse", "HEAD"])?;
    let done = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(done.stderr.contains("src/c.rs (create)"), "{}", done.stderr);
    assert!(
        done.stderr.contains("tessel claim src/c.rs --mode create"),
        "{}",
        done.stderr
    );
    assert_eq!(git(&a1.root(), &["rev-parse", "HEAD"])?, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_that_changes_only_a_claimed_symbol_passes() -> Result<()> {
    let (_fake, a1) = started().await?;
    assert_eq!(a1.tessel(&["claim", "src/pair.rs::pair::one"])?.code, 0);
    let done = commit_file(&a1, "src/pair.rs", "pub fn one() { 1; }\npub fn two() {}\n")?;
    assert_eq!(done.code, 0, "{}", done.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_symbol_of_the_same_file_is_not_covered_by_a_claimed_one() -> Result<()> {
    let (_fake, a1) = started().await?;
    assert_eq!(a1.tessel(&["claim", "src/pair.rs::pair::one"])?.code, 0);
    let done = commit_file(
        &a1,
        "src/pair.rs",
        "pub fn one() { 1; }\npub fn two() { 2; }\n",
    )?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(
        done.stderr.contains("src/pair.rs::pair::two"),
        "{}",
        done.stderr
    );
    assert!(
        !done.stderr.contains("pair::one (edit-body)"),
        "the claimed symbol must not be listed:\n{}",
        done.stderr
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_index_is_checked_and_a_subdirectory_makes_no_difference() -> Result<()> {
    let (_fake, a1) = started().await?;
    assert_eq!(a1.tessel(&["claim", "src/pair.rs::pair::one"])?.code, 0);
    write(&a1, "src/b.rs", "pub fn b() { 9; }\n")?;
    write(&a1, "src/pair.rs", "pub fn one() { 1; }\npub fn two() {}\n")?;
    git(&a1.root(), &["add", "src/pair.rs"])?;
    let done = git_done(&a1.root().join("src"), &["commit", "-q", "-m", "one only"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worktree_without_tessel_state_passes() -> Result<()> {
    let (_fake, a1) = world().await?;
    install(&a1)?;
    let done = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_eq!(done.code, 0, "{}", done.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_worktree_of_the_same_repository_is_untouched() -> Result<()> {
    let (_fake, a1) = world().await?;
    let other = tempfile::tempdir()?;
    let other_dir = other.path().join("other");
    git(
        &a1.root(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            &other_dir.display().to_string(),
        ],
    )?;
    let installed = Command::new(env!("CARGO_BIN_EXE_tessel"))
        .args(["hook", "install", "--git"])
        .current_dir(&other_dir)
        .output()?;
    assert!(installed.status.success(), "{installed:?}");
    assert!(
        hooks_dir(&a1).join("pre-commit").exists(),
        "a linked worktree installs into the common hooks directory"
    );
    a1.start("edit things")?;

    std::fs::write(other_dir.join("src/c.rs"), "pub fn c() {}\n")?;
    git(&other_dir, &["add", "src/c.rs"])?;
    let there = git_done(&other_dir, &["commit", "-q", "-m", "other"])?;
    assert_eq!(there.code, 0, "{}", there.all());

    let here = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_ne!(here.code, 0, "{}", here.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_that_died_blocks_with_the_way_out() -> Result<()> {
    let (_fake, a1) = started().await?;
    let pid = a1.status()?["state"]["pid"].as_u64().context("no pid")?;
    assert!(Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()?
        .success());
    eventually(Duration::from_secs(8), || {
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()?
            .status
            .success();
        Ok((!alive).then_some(()))
    })
    .await?;
    let done = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(done.stderr.contains("tessel start"), "{}", done.stderr);
    assert!(done.stderr.contains("--no-verify"), "{}", done.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_verify_skips_the_hook() -> Result<()> {
    let (_fake, a1) = started().await?;
    write(&a1, "src/c.rs", "pub fn c() {}\n")?;
    git(&a1.root(), &["add", "src/c.rs"])?;
    let done = git_done(&a1.root(), &["commit", "-q", "--no-verify", "-m", "skip"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_of_unclaimed_work_is_refused_and_a_deletion_is_not_checked() -> Result<()> {
    let (_fake, a1) = started().await?;
    let remote = bare_remote(&a1)?;
    let pushed = git_done(&a1.root(), &["push", "-q", "origin", "HEAD:refs/heads/old"])?;
    assert_eq!(pushed.code, 0, "{}", pushed.all());

    write(&a1, "src/c.rs", "pub fn c() {}\n")?;
    git(&a1.root(), &["add", "src/c.rs"])?;
    git(
        &a1.root(),
        &["commit", "-q", "--no-verify", "-m", "unclaimed"],
    )?;

    let refused = git_done(
        &a1.root(),
        &["push", "-q", "origin", "HEAD:refs/heads/work"],
    )?;
    assert_ne!(refused.code, 0, "{}", refused.all());
    assert!(
        refused.stderr.contains("src/c.rs (create)"),
        "{}",
        refused.stderr
    );
    assert!(
        refused.stderr.contains("refusing the push"),
        "{}",
        refused.stderr
    );
    assert!(!remote_has(remote.path(), "work"));

    let deleted = git_done(&a1.root(), &["push", "-q", "origin", ":refs/heads/old"])?;
    assert_eq!(deleted.code, 0, "{}", deleted.all());
    assert!(!remote_has(remote.path(), "old"));

    assert_eq!(
        a1.tessel(&["claim", "src/c.rs", "--mode", "create"])?.code,
        0
    );
    let covered = git_done(
        &a1.root(),
        &["push", "-q", "origin", "HEAD:refs/heads/work"],
    )?;
    assert_eq!(covered.code, 0, "{}", covered.all());
    assert!(remote_has(remote.path(), "work"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_existing_hook_is_kept_runs_first_and_its_failure_aborts() -> Result<()> {
    let (_fake, a1) = world().await?;
    let original = "#!/bin/sh\necho prek-ran >&2\nexit 7\n";
    executable_script(&hooks_dir(&a1).join("pre-commit"), original)?;
    install(&a1)?;
    let kept = hooks_dir(&a1).join("pre-commit.pre-tessel");
    assert_eq!(std::fs::read_to_string(&kept)?, original);
    assert_ne!(std::fs::metadata(&kept)?.permissions().mode() & 0o111, 0);

    let done = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(done.stderr.contains("prek-ran"), "{}", done.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chained_pre_push_hook_gets_the_arguments_and_stdin_and_can_abort() -> Result<()> {
    let (_fake, a1) = world().await?;
    let record = a1.root().join(".git/record");
    let script = format!(
        "#!/bin/sh\necho \"$@\" > {0}.args\ncat > {0}.stdin\n[ -f {0}.fail ] && exit 1\nexit 0\n",
        record.display()
    );
    executable_script(&hooks_dir(&a1).join("pre-push"), &script)?;
    install(&a1)?;
    let remote = bare_remote(&a1)?;

    let pushed = git_done(&a1.root(), &["push", "-q", "origin", "HEAD:refs/heads/one"])?;
    assert_eq!(pushed.code, 0, "{}", pushed.all());
    let stdin = std::fs::read_to_string(record.with_extension("stdin"))?;
    assert!(stdin.starts_with("HEAD "), "{stdin}");
    assert!(stdin.contains(" refs/heads/one "), "{stdin}");
    let args = std::fs::read_to_string(record.with_extension("args"))?;
    assert!(args.starts_with("origin "), "{args}");

    std::fs::write(record.with_extension("fail"), "")?;
    let aborted = git_done(&a1.root(), &["push", "-q", "origin", "HEAD:refs/heads/two"])?;
    assert_ne!(aborted.code, 0, "{}", aborted.all());
    assert!(!remote_has(remote.path(), "two"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installing_again_changes_nothing() -> Result<()> {
    let (_fake, a1) = world().await?;
    executable_script(&hooks_dir(&a1).join("pre-push"), "#!/bin/sh\nexit 0\n")?;
    install(&a1)?;
    let shim = std::fs::read_to_string(hooks_dir(&a1).join("pre-commit"))?;
    assert_eq!(shim.lines().nth(1), Some("# tessel git hook"), "{shim}");
    assert!(shim.contains(" hook git pre-commit \"$@\""), "{shim}");

    let again = a1.tessel(&["hook", "install", "--git"])?;
    assert_eq!(again.code, 0, "{}", again.all());
    assert!(
        again.stdout.contains("pre-commit: already installed"),
        "{}",
        again.stdout
    );
    assert_eq!(
        std::fs::read_to_string(hooks_dir(&a1).join("pre-commit"))?,
        shim
    );

    let mut names: Vec<String> = std::fs::read_dir(hooks_dir(&a1))?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    names.retain(|name| !name.ends_with(".sample"));
    names.sort();
    assert_eq!(names, ["pre-commit", "pre-push", "pre-push.pre-tessel"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relative_hooks_path_is_refused_with_manual_steps() -> Result<()> {
    let (_fake, a1) = world().await?;
    git(&a1.root(), &["config", "core.hooksPath", ".githooks"])?;
    let done = a1.tessel(&["hook", "install", "--git"])?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(
        done.stderr.contains("relative path .githooks"),
        "{}",
        done.stderr
    );
    assert!(
        done.stderr.contains("hook git pre-commit"),
        "{}",
        done.stderr
    );
    assert!(!a1.root().join(".githooks").exists());
    assert!(!hooks_dir(&a1).join("pre-commit").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_absolute_hooks_path_is_where_the_hooks_go() -> Result<()> {
    let (_fake, a1) = world().await?;
    let elsewhere = tempfile::tempdir()?;
    git(
        &a1.root(),
        &[
            "config",
            "core.hooksPath",
            &elsewhere.path().display().to_string(),
        ],
    )?;
    install(&a1)?;
    assert!(elsewhere.path().join("pre-commit").exists());
    assert!(!hooks_dir(&a1).join("pre-commit").exists());
    a1.start("edit things")?;
    let done = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_ne!(done.code, 0, "{}", done.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_submitted_claim_no_longer_covers_the_work_it_was_submitted_for() -> Result<()> {
    let (_fake, a1) = started().await?;
    assert_eq!(
        a1.tessel(&["claim", "src/c.rs", "--mode", "create"])?.code,
        0
    );
    let done = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_eq!(
        done.code,
        0,
        "{}\ndaemon files:\n{}",
        done.all(),
        a1.tessel_files().unwrap_or_default()
    );
    let submitted = a1.tessel(&["submit", "--evidence", "tests passed"])?;
    assert_eq!(submitted.code, 0, "{}", submitted.all());

    assert_eq!(
        a1.tessel(&["claim", "src/d.rs", "--mode", "create"])?.code,
        0
    );
    let done = commit_file(&a1, "src/d.rs", "pub fn d() {}\n")?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(done.stderr.contains("src/c.rs (create)"), "{}", done.stderr);
    assert!(!done.stderr.contains("src/d.rs"), "{}", done.stderr);
    Ok(())
}

/// The daemon's first event-log read can end between the coordinator granting a claim and the
/// daemon receiving the grant: the claim is in the log before the daemon holds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_racing_the_first_log_read_leaves_one_entry_per_claim() -> Result<()> {
    let (fake, a1) = world().await?;
    install(&a1)?;
    fake.hold_log_reads();
    a1.start("edit things")?;
    fake.hold_next_claim_reply();
    let claim = std::thread::scope(|scope| -> Result<Done> {
        let claim = scope.spawn(|| a1.tessel(&["claim", "src/c.rs", "--mode", "create"]));
        let granted = |fake: &Fake| {
            fake.events()
                .iter()
                .any(|event| matches!(event.kind, EventKind::ClaimGranted { .. }))
        };
        let log = a1.root().join(".tessel/daemon.log");
        let read = async {
            eventually(Duration::from_secs(8), || Ok(granted(&fake).then_some(()))).await?;
            fake.release_log_reads();
            eventually(Duration::from_secs(8), || {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                Ok(text.contains("1 live claims for this agent").then_some(()))
            })
            .await
        };
        let read = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(read));
        fake.release_held();
        let claim = claim
            .join()
            .map_err(|_| anyhow::anyhow!("claim thread panicked"))?;
        read?;
        claim
    })?;
    assert_eq!(claim.code, 0, "{}", claim.all());
    assert_eq!(
        a1.held_claims()?,
        1,
        "{}",
        a1.tessel_files().unwrap_or_default()
    );
    let done = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_eq!(
        done.code,
        0,
        "{}\ndaemon files:\n{}",
        done.all(),
        a1.tessel_files().unwrap_or_default()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_worktree_is_not_guarded() -> Result<()> {
    let (_fake, a1) = started().await?;
    assert_eq!(a1.tessel(&["stop"])?.code, 0);
    assert!(a1.root().join(".tessel/state.json").exists());
    let done = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_eq!(done.code, 0, "{}", done.all());
    Ok(())
}

/// `PATH` without a directory that holds a `tessel`, so a shim's own lookup finds none.
fn path_without_tessel() -> Result<std::ffi::OsString> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|dir| !dir.join("tessel").exists())
        .collect();
    Ok(std::env::join_paths(dirs)?)
}

/// Copies the binary under test to `dest`. The copy is made by a child process, so that no thread
/// of this test binary holds a write descriptor on a file it is about to run: a descriptor that a
/// parallel thread's fork leaves open in its child makes the exec fail with ETXTBSY on Linux.
fn copy_the_binary(dest: &Path) -> Result<()> {
    let done = Command::new("cp")
        .arg(env!("CARGO_BIN_EXE_tessel"))
        .arg(dest)
        .output()?;
    assert!(done.status.success(), "{done:?}");
    Ok(())
}

/// Installs the hooks from a copy of the binary, then deletes the copy, as removing a lane's
/// `target/` would.
fn install_then_delete_the_binary(agent: &Agent) -> Result<()> {
    let bin = tempfile::tempdir()?;
    let copy = bin.path().join("tessel");
    copy_the_binary(&copy)?;
    let done = Command::new(&copy)
        .args(["hook", "install", "--git"])
        .current_dir(agent.root())
        .output()?;
    assert!(done.status.success(), "{done:?}");
    std::fs::remove_file(&copy)?;
    Ok(())
}

fn git_without_tessel(dir: &Path, args: &[&str]) -> Result<Done> {
    Ok(Done::from(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("PATH", path_without_tessel()?)
            .output()?,
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_binary_leaves_other_worktrees_committing_and_runs_the_chained_hook() -> Result<()>
{
    let (_fake, a1) = world().await?;
    executable_script(
        &hooks_dir(&a1).join("pre-commit"),
        "#!/bin/sh\necho chained-ran >&2\nexit 0\n",
    )?;
    install_then_delete_the_binary(&a1)?;
    write(&a1, "src/c.rs", "pub fn c() {}\n")?;
    git(&a1.root(), &["add", "src/c.rs"])?;
    let done = git_without_tessel(&a1.root(), &["commit", "-q", "-m", "c"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.stderr.contains("chained-ran"), "{}", done.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_binary_blocks_a_worktree_that_has_tessel_state() -> Result<()> {
    let (_fake, a1) = world().await?;
    install_then_delete_the_binary(&a1)?;
    a1.start("edit things")?;
    write(&a1, "src/c.rs", "pub fn c() {}\n")?;
    git(&a1.root(), &["add", "src/c.rs"])?;
    let done = git_without_tessel(&a1.root(), &["commit", "-q", "-m", "c"])?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(done.stderr.contains("binary missing"), "{}", done.stderr);
    assert!(done.stderr.contains("--no-verify"), "{}", done.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_binary_still_hands_pre_push_stdin_to_the_chained_hook() -> Result<()> {
    let (_fake, a1) = world().await?;
    let record = a1.root().join(".git/record.stdin");
    let script = format!("#!/bin/sh\ncat > {}\nexit 0\n", record.display());
    executable_script(&hooks_dir(&a1).join("pre-push"), &script)?;
    install_then_delete_the_binary(&a1)?;
    let remote = bare_remote(&a1)?;
    let done = git_without_tessel(&a1.root(), &["push", "-q", "origin", "HEAD:refs/heads/one"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(remote_has(remote.path(), "one"));
    let stdin = std::fs::read_to_string(&record)?;
    assert!(stdin.contains(" refs/heads/one "), "{stdin}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_chained_hook_exit_code_is_passed_on() -> Result<()> {
    let (_fake, a1) = world().await?;
    executable_script(&hooks_dir(&a1).join("pre-commit"), "#!/bin/sh\nexit 7\n")?;
    install(&a1)?;
    let done = Command::new(hooks_dir(&a1).join("pre-commit"))
        .current_dir(a1.root())
        .output()?;
    assert_eq!(done.status.code(), Some(7), "{done:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn more_changed_symbols_than_a_submission_holds_are_judged_as_their_file() -> Result<()> {
    let (_fake, a1) = world().await?;
    let functions = |marker: u32| -> String {
        let mut text = String::new();
        for i in 0..260 {
            let _ = writeln!(text, "pub fn f{i}() {{ {marker}; }}");
        }
        text
    };
    write(&a1, "src/many.rs", &functions(0))?;
    git(&a1.root(), &["add", "src/many.rs"])?;
    git(&a1.root(), &["commit", "-q", "-m", "many"])?;
    install(&a1)?;
    a1.start("edit things")?;
    for batch in [0..130, 130..260] {
        let scopes: Vec<String> = batch.map(|i| format!("src/many.rs::many::f{i}")).collect();
        let mut args = vec!["claim"];
        args.extend(scopes.iter().map(String::as_str));
        assert_eq!(a1.tessel(&args)?.code, 0);
    }
    let done = commit_file(&a1, "src/many.rs", &functions(1))?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(
        done.stderr.contains("src/many.rs (edit-body)"),
        "{}",
        done.stderr
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concluding_a_merge_is_not_checked_by_pre_commit() -> Result<()> {
    let (_fake, a1) = started().await?;
    let root = a1.root();
    git(&root, &["checkout", "-q", "-b", "side"])?;
    write(&a1, "src/a.rs", "pub fn a() { 1; }\n")?;
    git(&root, &["commit", "-q", "--no-verify", "-am", "side"])?;
    git(&root, &["checkout", "-q", "-"])?;
    write(&a1, "src/a.rs", "pub fn a() { 2; }\n")?;
    git(&root, &["commit", "-q", "--no-verify", "-am", "main"])?;
    let merged = git_done(&root, &["merge", "-q", "side"])?;
    assert_ne!(merged.code, 0, "the merge must conflict: {}", merged.all());

    write(&a1, "src/a.rs", "pub fn a() { 3; }\n")?;
    git(&root, &["add", "src/a.rs"])?;
    let done = git_done(&root, &["commit", "-q", "--no-edit"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(
        done.stderr.contains("concluding a merge"),
        "{}",
        done.stderr
    );
    Ok(())
}

/// A directory holding a `tessel` that always exits 2, like a stale install.
fn stub_tessel_dir() -> Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    executable_script(&dir.path().join("tessel"), "#!/bin/sh\nexit 2\n")?;
    Ok(dir)
}

fn git_with_stub_first(dir: &Path, stub: &Path, args: &[&str]) -> Result<Done> {
    let mut paths = vec![stub.to_path_buf()];
    paths.extend(std::env::split_paths(&path_without_tessel()?));
    Ok(Done::from(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("PATH", std::env::join_paths(paths)?)
            .output()?,
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_recorded_binary_is_preferred_to_one_on_path() -> Result<()> {
    let (_fake, a1) = world().await?;
    install(&a1)?;
    let stub = stub_tessel_dir()?;
    write(&a1, "src/c.rs", "pub fn c() {}\n")?;
    git(&a1.root(), &["add", "src/c.rs"])?;
    let done = git_with_stub_first(&a1.root(), stub.path(), &["commit", "-q", "-m", "c"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_recorded_binary_the_one_on_path_is_used_whatever_it_says() -> Result<()> {
    let (_fake, a1) = world().await?;
    install_then_delete_the_binary(&a1)?;
    let stub = stub_tessel_dir()?;
    write(&a1, "src/c.rs", "pub fn c() {}\n")?;
    git(&a1.root(), &["add", "src/c.rs"])?;
    let done = git_with_stub_first(&a1.root(), stub.path(), &["commit", "-q", "-m", "c"])?;
    assert_ne!(
        done.code, 0,
        "the stub on PATH is run and its exit 2 stops the commit"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installing_from_a_build_directory_warns() -> Result<()> {
    let (_fake, a1) = world().await?;
    let root = tempfile::tempdir()?;
    let mut outputs = Vec::new();
    for dir in ["target", "bin"] {
        let copy = root.path().join(dir).join("tessel");
        std::fs::create_dir_all(copy.parent().context("no parent")?)?;
        copy_the_binary(&copy)?;
        let done = Command::new(&copy)
            .args(["hook", "install", "--git"])
            .current_dir(a1.root())
            .output()?;
        assert!(done.status.success(), "{done:?}");
        outputs.push(String::from_utf8_lossy(&done.stdout).into_owned());
    }
    assert!(outputs[0].contains("warning:"), "{}", outputs[0]);
    assert!(!outputs[1].contains("warning:"), "{}", outputs[1]);
    Ok(())
}

const FIRST_HOOK: &str = "#!/bin/sh\necho first-ran >&2\nexit 0\n";
const SECOND_HOOK: &str = "#!/bin/sh\necho second-ran >&2\nexit 7\n";

/// Files of the hooks directory named `<prefix>...`, sorted.
fn hook_files(agent: &Agent, prefix: &str) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(hooks_dir(agent))? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(prefix) {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// Installs over `FIRST_HOOK`, then lets another tool overwrite the shim with `replacement`.
async fn overwritten_after_install(replacement: &str) -> Result<(Fake, Agent)> {
    let (fake, a1) = world().await?;
    let pre_commit = hooks_dir(&a1).join("pre-commit");
    executable_script(&pre_commit, FIRST_HOOK)?;
    install(&a1)?;
    executable_script(&pre_commit, replacement)?;
    Ok((fake, a1))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reinstall_after_another_tool_overwrote_the_shim_chains_the_new_hook() -> Result<()> {
    let (_fake, a1) = overwritten_after_install(SECOND_HOOK).await?;
    let done = a1.tessel(&["hook", "install", "--git"])?;
    assert_eq!(done.code, 0, "{}", done.all());

    let dir = hooks_dir(&a1);
    let chained = dir.join("pre-commit.pre-tessel");
    assert_eq!(std::fs::read_to_string(&chained)?, SECOND_HOOK);
    assert_ne!(std::fs::metadata(&chained)?.permissions().mode() & 0o111, 0);
    let shim = std::fs::read_to_string(dir.join("pre-commit"))?;
    assert_eq!(shim.lines().nth(1), Some("# tessel git hook"), "{shim}");

    let backups = hook_files(&a1, "pre-commit.pre-tessel.")?;
    let [backup] = backups.as_slice() else {
        anyhow::bail!("expected one dated backup, found {backups:?}");
    };
    assert_eq!(std::fs::read_to_string(backup)?, FIRST_HOOK);
    let stamp = backup
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("pre-commit.pre-tessel."))
        .context("backup name")?;
    assert_eq!(stamp.len(), "20261011T123456Z".len(), "{stamp}");
    assert!(
        stamp.ends_with('Z') && stamp.as_bytes()[8] == b'T',
        "{stamp}"
    );

    let line = done
        .stdout
        .lines()
        .find(|line| line.starts_with("pre-commit: installed"))
        .context("no pre-commit line")?;
    assert!(
        line.contains(&format!("your existing hook is now {}", chained.display())),
        "{line}"
    );
    assert!(
        line.contains(&format!(
            "the earlier chained hook was kept as {} (merge it by hand if you still need it)",
            backup.display()
        )),
        "{line}"
    );
    assert!(!done.all().contains("cannot be kept"), "{}", done.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reinstalled_shim_runs_the_new_chained_hook_first_and_not_the_old_one() -> Result<()> {
    let (_fake, a1) = overwritten_after_install(SECOND_HOOK).await?;
    install(&a1)?;

    let done = commit_file(&a1, "src/c.rs", "pub fn c() {}\n")?;
    assert_ne!(done.code, 0, "{}", done.all());
    assert!(done.stderr.contains("second-ran"), "{}", done.stderr);
    assert!(!done.stderr.contains("first-ran"), "{}", done.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shim_overwritten_by_the_same_hook_keeps_it_without_a_backup() -> Result<()> {
    let (_fake, a1) = overwritten_after_install(FIRST_HOOK).await?;
    let done = a1.tessel(&["hook", "install", "--git"])?;
    assert_eq!(done.code, 0, "{}", done.all());

    let chained = hooks_dir(&a1).join("pre-commit.pre-tessel");
    assert!(
        done.stdout.contains(&format!(
            "pre-commit: installed; your existing hook is still {} and runs first",
            chained.display()
        )),
        "{}",
        done.stdout
    );
    assert_eq!(std::fs::read_to_string(&chained)?, FIRST_HOOK);
    assert_eq!(
        hook_files(&a1, "pre-commit.pre-tessel.")?,
        Vec::<PathBuf>::new()
    );
    let shim = std::fs::read_to_string(hooks_dir(&a1).join("pre-commit"))?;
    assert_eq!(shim.lines().nth(1), Some("# tessel git hook"), "{shim}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_third_install_with_the_shim_in_place_changes_nothing() -> Result<()> {
    let (_fake, a1) = overwritten_after_install(SECOND_HOOK).await?;
    install(&a1)?;
    let before = (
        hook_files(&a1, "pre-commit")?,
        std::fs::read_to_string(hooks_dir(&a1).join("pre-commit"))?,
    );

    let again = a1.tessel(&["hook", "install", "--git"])?;
    assert_eq!(again.code, 0, "{}", again.all());
    assert!(
        again.stdout.contains("pre-commit: already installed"),
        "{}",
        again.stdout
    );
    let after = (
        hook_files(&a1, "pre-commit")?,
        std::fs::read_to_string(hooks_dir(&a1).join("pre-commit"))?,
    );
    assert_eq!(before, after);
    Ok(())
}

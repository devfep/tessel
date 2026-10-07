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

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use support::{git, Agent, Done, Fake};

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
    a1.start("edit things")?;
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
async fn a_daemon_that_is_down_blocks_with_the_way_out() -> Result<()> {
    let (_fake, a1) = started().await?;
    assert_eq!(a1.tessel(&["stop"])?.code, 0);
    assert!(a1.root().join(".tessel/state.json").exists());
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
    assert_eq!(shim.lines().count(), 2, "{shim}");
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
    assert_eq!(done.code, 0, "{}", done.all());
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

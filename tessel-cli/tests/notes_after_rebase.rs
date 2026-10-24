//! The note of a submitted commit follows it to the commit the lander made from it: the real
//! binary and daemon against the fake coordinator, which runs the crate's own `Coordinator`
//! core. The tests build the landed commits in git themselves, as a rebase onto a moved main.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]
#![expect(
    dead_code,
    reason = "each test crate compiles all of support, which the other test crate also uses"
)]

mod support;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use support::{eventually, git, Agent, Fake};
use tessel_coordinator::protocol::{ClaimId, CommitId, ServerMsg};

const TOK1: &str = "tok-a1-S3CRETvalue";
const SHORT: Duration = Duration::from_secs(8);

async fn world() -> Result<(Fake, Agent)> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    Ok((fake, a1))
}

fn head_of(dir: &Path) -> Result<String> {
    Ok(git(dir, &["rev-parse", "HEAD"])?.trim().to_string())
}

/// Claims `src/a.rs`, commits an edit and submits it, so the commit carries its note. Returns
/// the submitted commit.
async fn submitted(a1: &Agent) -> Result<String> {
    a1.start_settled("fix a").await?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    let root = a1.root();
    std::fs::write(root.join("src/a.rs"), "pub fn a() { 1; }\n")?;
    git(&root, &["add", "src/a.rs"])?;
    git(&root, &["commit", "-q", "-m", "change a"])?;
    let sha = head_of(&root)?;
    let done = a1.tessel(&["submit", "--evidence", "e1"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    Ok(sha)
}

/// What the lander makes of `commit`: main moves (a commit on `src/b.rs`), and the commit is
/// replayed on top, with its author line and message and a new committer. Built in a clone of
/// `a1`'s repository, so `a1` does not have the result until it fetches. Returns the clone,
/// main's new commit and the replayed commit.
fn landed_in_a_clone(a1: &Agent, commit: &str) -> Result<(tempfile::TempDir, String, String)> {
    let clone = tempfile::tempdir()?;
    let url = a1.root().display().to_string();
    let path = clone.path();
    git(path, &["clone", "-q", &url, "."])?;
    for args in [
        ["config", "user.email", "lander@example.test"],
        ["config", "user.name", "Lander"],
    ] {
        git(path, &args)?;
    }
    git(path, &["fetch", "-q", "origin", commit])?;
    git(
        path,
        &["checkout", "-q", "--detach", &format!("{commit}~1")],
    )?;
    std::fs::write(path.join("src/b.rs"), "pub fn b() { 2; }\n")?;
    git(path, &["commit", "-qam", "someone else"])?;
    let base = head_of(path)?;
    git(path, &["cherry-pick", commit])?;
    let head = head_of(path)?;
    Ok((clone, base, head))
}

fn fetch_into(a1: &Agent, clone: &Path, head: &str) -> Result<()> {
    git(
        &a1.root(),
        &["fetch", "-q", &clone.display().to_string(), head],
    )?;
    Ok(())
}

fn note_of(dir: &Path, commit: &str) -> Option<String> {
    git(dir, &["notes", "show", commit]).ok()
}

fn pending_notes(a1: &Agent) -> Result<usize> {
    Ok(a1.status()?["state"]["pending_notes"]
        .as_array()
        .map_or(0, Vec::len))
}

fn daemon_log(a1: &Agent) -> String {
    std::fs::read_to_string(a1.root().join(".tessel/daemon.log")).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_landing_copies_the_note_to_the_rebased_commit() -> Result<()> {
    let (fake, a1) = world().await?;
    let sha = submitted(&a1).await?;
    let note = note_of(&a1.root(), &sha).context("no note on the submitted commit")?;
    let (clone, base, head) = landed_in_a_clone(&a1, &sha)?;
    fetch_into(&a1, clone.path(), &head)?;

    fake.merge_next_at(&base, &head);

    let copied = eventually(SHORT, || {
        Ok(note_of(&a1.root(), &head).filter(|copy| *copy == note))
    })
    .await
    .context("the landed commit never got the note")?;
    assert!(copied.contains("intent: fix a"), "{copied}");
    assert_eq!(pending_notes(&a1)?, 0);
    let log = daemon_log(&a1);
    assert!(log.contains("copied 1"), "{log}");
    assert!(
        !log.contains("intent: fix a"),
        "the log holds note text: {log}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_merged_without_a_base_copies_nothing_and_says_so() -> Result<()> {
    let (fake, a1) = world().await?;
    let sha = submitted(&a1).await?;
    let (clone, _base, head) = landed_in_a_clone(&a1, &sha)?;
    fetch_into(&a1, clone.path(), &head)?;
    let claim = a1.status()?["state"]["claims"][0]["claim"]
        .as_u64()
        .context("no claim")?;

    fake.push(
        "a1",
        ServerMsg::Merged {
            claim: ClaimId(claim),
            head: CommitId(head.clone()),
            base: None,
        },
    );

    eventually(SHORT, || {
        Ok(daemon_log(&a1).contains("without a base").then_some(()))
    })
    .await
    .context("the daemon never said the landing had no base")?;
    assert_eq!(note_of(&a1.root(), &head), None);
    assert_eq!(pending_notes(&a1)?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_that_was_offline_for_the_landing_learns_it_from_the_log() -> Result<()> {
    let (fake, a1) = world().await?;
    let sha = submitted(&a1).await?;
    let note = note_of(&a1.root(), &sha).context("no note on the submitted commit")?;
    let (clone, base, head) = landed_in_a_clone(&a1, &sha)?;
    fetch_into(&a1, clone.path(), &head)?;

    fake.set_accepting(false);
    fake.drop_connections();
    eventually(SHORT, || {
        Ok((a1.status()?["state"]["connection"] != "online").then_some(()))
    })
    .await?;
    fake.merge_next_at(&base, &head);
    assert_eq!(note_of(&a1.root(), &head), None);
    fake.set_accepting(true);

    eventually(SHORT, || {
        Ok(note_of(&a1.root(), &head).filter(|copy| *copy == note))
    })
    .await
    .context("the landed commit never got the note after the reconnect")?;
    assert_eq!(pending_notes(&a1)?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_head_fetched_later_gets_its_note_on_the_next_command() -> Result<()> {
    let (fake, a1) = world().await?;
    let sha = submitted(&a1).await?;
    let note = note_of(&a1.root(), &sha).context("no note on the submitted commit")?;
    let (clone, base, head) = landed_in_a_clone(&a1, &sha)?;

    fake.merge_next_at(&base, &head);
    eventually(SHORT, || Ok((pending_notes(&a1)? == 1).then_some(())))
        .await
        .context("a landing whose commits are not here is not kept pending")?;
    assert_eq!(
        a1.status()?["state"]["claims"].as_array().map(Vec::len),
        Some(0)
    );

    fetch_into(&a1, clone.path(), &head)?;
    eventually(SHORT, || {
        a1.status()?;
        Ok(note_of(&a1.root(), &head).filter(|copy| *copy == note))
    })
    .await
    .context("the note never followed once the head was fetched")?;
    assert_eq!(pending_notes(&a1)?, 0);
    Ok(())
}

fn waiting_lines(a1: &Agent) -> usize {
    daemon_log(a1)
        .lines()
        .filter(|line| line.contains("notes after rebase") && line.contains("waiting"))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_waiting_job_logs_once_however_many_heartbeats_pass() -> Result<()> {
    let fake = Fake::start(1_500, &[("a1", TOK1)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    let sha = submitted(&a1).await?;
    let (_clone, base, head) = landed_in_a_clone(&a1, &sha)?;

    fake.merge_next_at(&base, &head);
    eventually(SHORT, || Ok((waiting_lines(&a1) >= 1).then_some(())))
        .await
        .context("a job waiting for its commits was never logged")?;
    let beats = fake.heartbeats("a1");
    eventually(SHORT, || {
        Ok((fake.heartbeats("a1") >= beats + 4).then_some(()))
    })
    .await
    .context("no heartbeats passed")?;
    for _ in 0..3 {
        a1.status()?;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(waiting_lines(&a1), 1, "{}", daemon_log(&a1));
    assert_eq!(pending_notes(&a1)?, 1);
    Ok(())
}

/// A `git` first on PATH that takes three seconds over `git notes list`, which only the notes
/// copy runs without a commit, and is plain git otherwise.
fn slow_git(dir: &Path) -> Result<String> {
    let real = String::from_utf8(
        std::process::Command::new("which")
            .arg("git")
            .output()?
            .stdout,
    )?;
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in *\"notes list\") sleep 3;; esac\nexec {} \"$@\"\n",
        real.trim()
    );
    let path = dir.join("git");
    std::fs::write(&path, script)?;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    Ok(format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_keeps_answering_while_a_slow_copy_runs() -> Result<()> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    let bin = tempfile::tempdir()?;
    let a1 = Agent::new(&fake, "a1", TOK1)?.with_env("PATH", &slow_git(bin.path())?);
    let sha = submitted(&a1).await?;
    let note = note_of(&a1.root(), &sha).context("no note on the submitted commit")?;
    let (clone, base, head) = landed_in_a_clone(&a1, &sha)?;
    fetch_into(&a1, clone.path(), &head)?;

    fake.merge_next_at(&base, &head);
    let began = std::time::Instant::now();
    let mut slowest = Duration::ZERO;
    while began.elapsed() < Duration::from_secs(2) {
        let asked = std::time::Instant::now();
        a1.status()?;
        slowest = slowest.max(asked.elapsed());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        slowest < Duration::from_secs(1),
        "a command waited {slowest:?}"
    );
    eventually(Duration::from_secs(15), || {
        Ok(note_of(&a1.root(), &head).filter(|copy| *copy == note))
    })
    .await
    .context("the slow copy never finished")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_job_survives_a_daemon_restart() -> Result<()> {
    let (fake, a1) = world().await?;
    let sha = submitted(&a1).await?;
    let note = note_of(&a1.root(), &sha).context("no note on the submitted commit")?;
    let (clone, base, head) = landed_in_a_clone(&a1, &sha)?;
    fake.merge_next_at(&base, &head);
    eventually(SHORT, || Ok((pending_notes(&a1)? == 1).then_some(()))).await?;

    assert_eq!(a1.tessel(&["stop"])?.code, 0);
    a1.start_settled("again").await?;
    assert_eq!(pending_notes(&a1)?, 1, "the job is lost on restart");

    fetch_into(&a1, clone.path(), &head)?;
    eventually(SHORT, || {
        a1.status()?;
        Ok(note_of(&a1.root(), &head).filter(|copy| *copy == note))
    })
    .await
    .context("the restarted daemon never copied the note")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_state_file_from_before_pending_notes_still_loads() -> Result<()> {
    let (_fake, a1) = world().await?;
    a1.start("first")?;
    assert_eq!(a1.tessel(&["stop"])?.code, 0);
    let path = a1.root().join(".tessel/state.json");
    let mut state: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    state
        .as_object_mut()
        .context("state is not an object")?
        .remove("pending_notes")
        .context("state.json has no pending_notes")?;
    std::fs::write(&path, serde_json::to_string(&state)?)?;

    a1.start_settled("second").await?;
    assert_eq!(pending_notes(&a1)?, 0);
    Ok(())
}

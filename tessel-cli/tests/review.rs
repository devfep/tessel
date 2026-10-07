//! `tessel review` end to end: a reviewer decides a submission the real core holds. The fake
//! coordinator runs the crate's own `Coordinator`, with `r1` named as the only reviewer.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]
#![expect(
    dead_code,
    reason = "each test crate compiles all of support, which the other test crates also use"
)]

mod support;

use std::time::Duration;

use anyhow::{Context, Result};
use support::{eventually, git, Agent, Fake, Lose};
use tessel_coordinator::protocol::ClientMsg;

const TOK1: &str = "tok-a1-S3CRETvalue";
const TOK_R: &str = "tok-r1-S3CRETvalue";
const SHORT: Duration = Duration::from_secs(8);

/// `a1` has submitted a deletion, which the core holds for review; `r1` is the reviewer.
async fn held_submission() -> Result<(Fake, Agent, Agent, u64)> {
    let fake = Fake::start(30_000, &[("a1", TOK1), ("r1", TOK_R)]).await?;
    fake.set_reviewers(&["r1"]);
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    let r1 = Agent::new(&fake, "r1", TOK_R)?;
    a1.start("delete b")?;
    assert_eq!(
        a1.tessel(&["claim", "src/b.rs", "--mode", "edit-signature"])?
            .code,
        0
    );
    git(&a1.root(), &["rm", "-q", "src/b.rs"])?;
    git(&a1.root(), &["commit", "-q", "-m", "delete b"])?;
    let done = a1.tessel(&["submit", "--evidence", "tests passed"])?;
    assert_eq!(done.code, 7, "{}", done.all());
    let status = a1.status()?;
    let claim = status["state"]["claims"][0]["claim"]
        .as_u64()
        .context("no claim id")?;
    Ok((fake, a1, r1, claim))
}

fn reviews(fake: &Fake, agent: &str) -> Vec<(bool, Option<String>)> {
    fake.received(agent)
        .into_iter()
        .filter_map(|msg| {
            let ClientMsg::Review { approve, note, .. } = msg else {
                return None;
            };
            Some((approve, note))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approval_is_confirmed_from_the_log_and_releases_the_submission() -> Result<()> {
    let (fake, a1, r1, claim) = held_submission().await?;
    let id = claim.to_string();
    let done = r1.tessel(&[
        "review",
        &id,
        "--approve",
        "--note",
        "reviewed the deletion",
    ])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.stdout.contains("approved"), "{}", done.stdout);
    assert_eq!(
        reviews(&fake, "r1"),
        [(true, Some("reviewed the deletion".to_string()))]
    );
    // Only an approved submission is in the merge queue for the steward to take.
    fake.merge_next();
    eventually(SHORT, || {
        let inbox = a1.tessel(&["inbox", "--all"])?;
        Ok(inbox.stdout.contains("[merged]").then_some(()))
    })
    .await
    .context("the approved submission never reached the merge queue")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejection_is_confirmed_too() -> Result<()> {
    let (fake, _a1, r1, claim) = held_submission().await?;
    let id = claim.to_string();
    let done = r1.tessel(&["review", &id, "--reject"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.stdout.contains("rejected"), "{}", done.stdout);
    assert_eq!(reviews(&fake, "r1"), [(false, None)]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn someone_who_is_not_a_reviewer_is_refused_with_exit_8() -> Result<()> {
    let (_fake, a1, _r1, claim) = held_submission().await?;
    let id = claim.to_string();
    let done = a1.tessel(&["review", &id, "--approve"])?;
    assert_eq!(done.code, 8, "{}", done.all());
    assert!(done.stderr.contains("not approved"), "{}", done.stderr);
    assert!(
        done.stderr
            .contains("only a configured reviewer may review"),
        "{}",
        done.stderr
    );
    assert_eq!(done.stdout, "");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_that_is_not_awaiting_review_is_refused() -> Result<()> {
    let (_fake, _a1, r1, claim) = held_submission().await?;
    let done = r1.tessel(&["review", &(claim + 100).to_string(), "--approve"])?;
    assert_eq!(done.code, 8, "{}", done.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decision_that_never_reaches_the_log_is_unconfirmed_with_exit_9() -> Result<()> {
    let (fake, _a1, r1, claim) = held_submission().await?;
    fake.lose_next("r1", Lose::Review);
    let done = r1.tessel(&["review", &claim.to_string(), "--approve"])?;
    assert_eq!(done.code, 9, "{}", done.all());
    assert!(
        done.stderr.contains("not in the event log"),
        "{}",
        done.stderr
    );
    assert_eq!(done.stdout, "");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_over_long_note_is_refused_before_anything_is_sent() -> Result<()> {
    let (fake, _a1, r1, claim) = held_submission().await?;
    let note = "n".repeat(1025);
    let done = r1.tessel(&["review", &claim.to_string(), "--approve", "--note", &note])?;
    assert_eq!(done.code, 1, "{}", done.all());
    assert!(done.stderr.contains("1025 bytes"), "{}", done.stderr);
    assert!(reviews(&fake, "r1").is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exactly_one_of_approve_and_reject_is_required() -> Result<()> {
    let (fake, _a1, r1, claim) = held_submission().await?;
    let id = claim.to_string();
    for args in [
        vec!["review", &id],
        vec!["review", &id, "--approve", "--reject"],
    ] {
        let done = r1.tessel(&args)?;
        assert_eq!(done.code, 2, "{args:?}: {}", done.all());
    }
    assert!(reviews(&fake, "r1").is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_older_decision_on_the_claim_does_not_confirm_a_new_one() -> Result<()> {
    let (fake, _a1, r1, claim) = held_submission().await?;
    let id = claim.to_string();
    assert_eq!(r1.tessel(&["review", &id, "--reject"])?.code, 0);
    fake.lose_next("r1", Lose::Review);
    let done = r1.tessel(&["review", &id, "--reject"])?;
    assert_eq!(
        done.code,
        9,
        "the first rejection is not this one: {}",
        done.all()
    );
    Ok(())
}

fn hello_bases(fake: &Fake, agent: &str) -> Vec<String> {
    fake.received(agent)
        .into_iter()
        .filter_map(|msg| {
            let ClientMsg::Hello { base, .. } = msg else {
                return None;
            };
            Some(base.0)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reviewer_outside_any_git_repository_can_decide() -> Result<()> {
    let (fake, _a1, _r1, claim) = held_submission().await?;
    let nowhere = Agent::without_repo(&fake, "r1", TOK_R)?;
    let done = nowhere.tessel(&["review", &claim.to_string(), "--approve"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.stdout.contains("approved"), "{}", done.stdout);
    assert_eq!(reviews(&fake, "r1"), [(true, None)]);
    assert!(hello_bases(&fake, "r1")
        .iter()
        .all(|base| base == "0".repeat(40).as_str()));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reviewer_in_a_repository_with_no_commit_can_decide() -> Result<()> {
    let (fake, _a1, _r1, claim) = held_submission().await?;
    let unborn = Agent::without_commits(&fake, "r1", TOK_R)?;
    let done = unborn.tessel(&["review", &claim.to_string(), "--reject"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.stdout.contains("rejected"), "{}", done.stdout);
    assert_eq!(reviews(&fake, "r1"), [(false, None)]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reviewer_in_a_worktree_still_sends_its_head_as_the_base() -> Result<()> {
    let (fake, _a1, r1, claim) = held_submission().await?;
    let head = git(&r1.root(), &["rev-parse", "HEAD"])?;
    let done = r1.tessel(&["review", &claim.to_string(), "--approve"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(hello_bases(&fake, "r1")
        .iter()
        .all(|base| base == head.trim()));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reviewer_in_a_subdirectory_uses_the_repository_root_config() -> Result<()> {
    let (fake, _a1, _r1, claim) = held_submission().await?;
    let url = fake.url.clone();
    // The environment names an agent the coordinator does not know: only the root config file
    // names the reviewer.
    let reviewer = Agent::new(&fake, "nobody", "tok-nobody-S3CRETvalue")?;
    let root = reviewer.root();
    std::fs::create_dir_all(root.join(".tessel"))?;
    std::fs::write(
        root.join(".tessel/config.toml"),
        format!(
            "coordinator = \"{url}\"\nrepo = \"{}\"\nagent = \"r1\"\ntoken = \"{TOK_R}\"\n",
            support::REPO
        ),
    )?;
    std::fs::create_dir_all(root.join("src/deep"))?;
    let done = reviewer.tessel_in(&["review", &claim.to_string(), "--approve"], "src/deep")?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(reviews(&fake, "r1"), [(true, None)]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worktree_error_other_than_not_a_repository_is_reported() -> Result<()> {
    let (fake, _a1, _r1, claim) = held_submission().await?;
    let base = tempfile::tempdir()?;
    std::fs::create_dir_all(base.path().join("x".repeat(100)))?;
    let long = base.path().join("x".repeat(100));
    let reviewer =
        Agent::new(&fake, "r1", TOK_R)?.with_env("XDG_RUNTIME_DIR", &long.display().to_string());
    let done = reviewer.tessel(&["review", &claim.to_string(), "--approve"])?;
    assert_eq!(done.code, 1, "{}", done.all());
    assert!(done.stderr.contains("too long"), "{}", done.stderr);
    assert!(reviews(&fake, "r1").is_empty());
    Ok(())
}

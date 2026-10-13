//! The git note `tessel submit` writes on the submitted commit: the real binary and daemon
//! against the fake coordinator, which runs the crate's own `Coordinator` core.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]
#![expect(
    dead_code,
    reason = "each test crate compiles all of support, which the other test crate also uses"
)]

mod support;

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use support::{eventually, git, Agent, Fake};
use tessel_coordinator::protocol::{ClaimId, ClientMsg, Fence, Mode, RequestId, Scope, ScopeClaim};

const TOK1: &str = "tok-a1-S3CRETvalue";
const SHORT: Duration = Duration::from_secs(8);

async fn world() -> Result<(Fake, Agent)> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    Ok((fake, a1))
}

fn commit_a(agent: &Agent) -> Result<String> {
    let root = agent.root();
    std::fs::write(root.join("src/a.rs"), "pub fn a() { 1; }\n")?;
    git(&root, &["add", "src/a.rs"])?;
    git(&root, &["commit", "-q", "-m", "change a"])?;
    Ok(git(&root, &["rev-parse", "HEAD"])?.trim().to_string())
}

fn note_of(agent: &Agent, commit: &str) -> Result<String> {
    git(&agent.root(), &["notes", "show", commit])
}

fn first_claim(agent: &Agent) -> Result<Value> {
    agent.status()?["state"]["claims"][0]
        .as_object()
        .map(|claim| Value::Object(claim.clone()))
        .context("no claim in status")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_writes_the_four_line_note_on_the_commit() -> Result<()> {
    let (_fake, a1) = world().await?;
    a1.start("fix a")?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    let sha = commit_a(&a1)?;
    let done = a1.tessel(&[
        "submit",
        "--evidence",
        "e1",
        "--evidence",
        "e2",
        "--rejected",
        "x::why",
    ])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(
        done.stdout.contains(&format!(
            "note written on {} (refs/notes/commits; push with git push <your fork> \
             refs/notes/commits to share it)",
            &sha[..7]
        )),
        "{}",
        done.stdout
    );
    assert_eq!(
        note_of(&a1, &sha)?,
        "intent: fix a\nassumes: none\nrejected: x::why\nevidence: e1; e2\n"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_that_has_a_note_keeps_it_byte_for_byte() -> Result<()> {
    let (_fake, a1) = world().await?;
    a1.start("fix a")?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    let sha = commit_a(&a1)?;
    git(
        &a1.root(),
        &[
            "notes",
            "add",
            "-m",
            "written by hand\n\n  two lines ",
            &sha,
        ],
    )?;
    let before = note_of(&a1, &sha)?;
    let done = a1.tessel(&["submit", "--evidence", "e1"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(
        done.stdout
            .contains("note: commit already has a note; left as is"),
        "{}",
        done.stdout
    );
    assert!(!done.stdout.contains("note written"), "{}", done.stdout);
    assert_eq!(note_of(&a1, &sha)?, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_submission_the_coordinator_refuses_still_leaves_the_note() -> Result<()> {
    let (fake, a1) = world().await?;
    a1.start("refused work")?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    let sha = commit_a(&a1)?;
    // An amend the daemon does not know about retires the fence it holds, so its submit is stale.
    let claim = first_claim(&a1)?;
    fake.act(
        "a1",
        ClientMsg::Amend {
            req: RequestId(902),
            claim: ClaimId(claim["claim"].as_u64().context("id")?),
            fence: Fence(claim["fence"].as_u64().context("fence")?),
            add: vec![ScopeClaim {
                scope: Scope::File {
                    path: "src/b.rs".into(),
                },
                mode: Mode::EditBody,
            }],
        },
    );
    let done = a1.tessel(&["submit", "--evidence", "e1"])?;
    assert_eq!(done.code, 6, "{}", done.all());
    assert_eq!(
        note_of(&a1, &sha)?,
        "intent: refused work\nassumes: none\nrejected: none\nevidence: e1\n"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_assumptions_of_the_claim_fill_the_assumes_line() -> Result<()> {
    let (_fake, a1) = world().await?;
    a1.start("fix a")?;
    let claimed = a1.tessel(&[
        "claim",
        "src/a.rs",
        "--assume",
        "b() stays pure",
        "--assume",
        "a() is only called once",
    ])?;
    assert_eq!(claimed.code, 0, "{}", claimed.all());
    let sha = commit_a(&a1)?;
    let done = a1.tessel(&["submit", "--evidence", "e1"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(
        note_of(&a1, &sha)?,
        "intent: fix a\nassumes: b() stays pure; a() is only called once\nrejected: none\n\
         evidence: e1\n"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_note_that_cannot_be_written_is_a_warning_and_the_submission_goes_on() -> Result<()> {
    let (fake, a1) = world().await?;
    a1.start("fix a")?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    let sha = commit_a(&a1)?;
    // A ref below refs/notes/commits/ keeps git from creating refs/notes/commits itself.
    git(
        &a1.root(),
        &["update-ref", "refs/notes/commits/blocker", &sha],
    )?;
    let done = a1.tessel(&["submit", "--evidence", "e1"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(
        done.stdout
            .contains("warning: the commit's git note was not written"),
        "{}",
        done.stdout
    );
    let sent = fake
        .received("a1")
        .into_iter()
        .filter(|msg| matches!(msg, ClientMsg::Submit { .. }))
        .count();
    assert_eq!(sent, 1);
    eventually(SHORT, || {
        Ok((first_claim(&a1)?["submitted"] == true).then_some(()))
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_note_exists_while_the_coordinator_has_not_answered_the_submission() -> Result<()> {
    let (fake, a1) = world().await?;
    a1.start("fix a")?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    let sha = commit_a(&a1)?;
    fake.go_silent();
    let mut child = a1.spawn_piped(&["submit", "--evidence", "e1"])?;
    let waited = eventually(SHORT, || Ok(note_of(&a1, &sha).ok())).await;
    let still_waiting = child.try_wait()?.is_none();
    child.kill()?;
    child.wait()?;
    let note = waited.context("no note while the submission was unanswered")?;
    assert!(
        still_waiting,
        "submit returned before the note could be observed"
    );
    assert_eq!(
        note,
        "intent: fix a\nassumes: none\nrejected: none\nevidence: e1\n"
    );
    Ok(())
}

//! Liveness end to end: the real daemon against a fake coordinator whose link goes half-open
//! (open, but silent: no reply, no pong, no close frame), and against one that stays healthy.

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
use support::{eventually, Agent, Fake};
use tessel_coordinator::protocol::{AgentId, ClientMsg, CommitId, ServerMsg};

const TOK1: &str = "tok-a1-S3CRETvalue";
const SHORT: Duration = Duration::from_secs(8);

async fn claimed(lease_ms: u64) -> Result<(Fake, Agent)> {
    let fake = Fake::start(lease_ms, &[("a1", TOK1)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    a1.start("long job")?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    Ok((fake, a1))
}

fn hellos(fake: &Fake) -> usize {
    fake.received("a1")
        .iter()
        .filter(|m| matches!(m, ClientMsg::Hello { .. }))
        .count()
}

fn expiry(agent: &Agent) -> Result<u64> {
    agent.status()?["state"]["claims"][0]["expires_at_ms"]
        .as_u64()
        .context("the agent holds no claim")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_link_is_dropped_after_half_a_lease_and_the_daemon_reconnects() -> Result<()> {
    let (fake, a1) = claimed(3_000).await?;
    let before = hellos(&fake);
    let silent_since = tokio::time::Instant::now();
    fake.go_silent();
    let waited = eventually(SHORT, || Ok((hellos(&fake) > before).then_some(())));
    waited.await.with_context(|| {
        format!(
            "no reconnect after the link went silent:\n{}",
            a1.tessel_files().unwrap_or_default()
        )
    })?;
    // The next heartbeat is sent after the link goes silent and is given half a lease.
    assert!(
        silent_since.elapsed() >= Duration::from_millis(1_400),
        "the link was dropped after {:?}, before a heartbeat had gone unanswered for half a lease",
        silent_since.elapsed()
    );
    eventually(SHORT, || {
        Ok((a1.status()?["state"]["connection"] == "online").then_some(()))
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_healthy_link_is_kept_and_the_local_expiry_follows_the_heartbeats() -> Result<()> {
    let (fake, a1) = claimed(3_000).await?;
    let granted = expiry(&a1)?;
    let baseline = hellos(&fake);
    tokio::time::sleep(Duration::from_millis(4_000)).await;
    assert_eq!(
        hellos(&fake),
        baseline,
        "a healthy link was dropped:\n{}",
        a1.tessel_files().unwrap_or_default()
    );
    assert_eq!(a1.held_claims()?, 1);
    assert!(
        expiry(&a1)? > granted + 2_000,
        "answered heartbeats did not renew the local expiry"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_lost_on_a_silent_link_is_reported_not_assumed_live() -> Result<()> {
    let (fake, a1) = claimed(900).await?;
    fake.go_silent();
    fake.set_accepting(false);
    eventually(SHORT, || Ok((a1.held_claims()? == 0).then_some(())))
        .await
        .with_context(|| {
            format!(
                "the claim stayed live locally:\n{}",
                a1.tessel_files().unwrap_or_default()
            )
        })?;
    let inbox = a1.tessel(&["inbox", "--all"])?;
    assert!(inbox.stdout.contains("[lease_expired]"), "{}", inbox.stdout);
    assert_ne!(a1.status()?["state"]["connection"], "online");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeats_into_a_silent_link_do_not_extend_the_local_expiry() -> Result<()> {
    let (fake, a1) = claimed(3_000).await?;
    fake.go_silent();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let settled = expiry(&a1)?;
    // The window spans a whole heartbeat period, and the link is still up when it is sent.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert_eq!(
        expiry(&a1)?,
        settled,
        "a heartbeat nobody answered renewed the expiry"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_frames_without_a_pong_do_not_extend_the_local_expiry() -> Result<()> {
    let (fake, a1) = claimed(3_000).await?;
    fake.go_deaf();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let settled = expiry(&a1)?;
    // Frames keep arriving across a whole heartbeat period, but none answers a heartbeat.
    for _ in 0..11 {
        fake.push(
            "a1",
            ServerMsg::BaseMoved {
                head: CommitId("d".repeat(40)),
                by: AgentId("a2".into()),
                affected: Vec::new(),
            },
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        expiry(&a1)?,
        settled,
        "a frame that did not answer a heartbeat renewed the expiry"
    );
    Ok(())
}

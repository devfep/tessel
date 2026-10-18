//! A daemon older than its coordinator: events and messages it cannot decode are skipped and
//! counted, the sequence still advances, and the frames after them are applied.

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

use anyhow::Result;
use support::{eventually, Agent, Fake, Lose};
use tessel_coordinator::protocol::{AgentId, ClientMsg, CommitId, ServerMsg};

const TOK1: &str = "tok-a1-S3CRETvalue";
const SHORT: Duration = Duration::from_secs(8);

async fn world() -> Result<(Fake, Agent)> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    Ok((fake, a1))
}

fn hellos(fake: &Fake) -> usize {
    fake.received("a1")
        .iter()
        .filter(|m| matches!(m, ClientMsg::Hello { .. }))
        .count()
}

fn skipped(agent: &Agent) -> Result<u64> {
    Ok(agent.status()?["state"]["skipped_frames"]
        .as_u64()
        .unwrap_or(0))
}

/// The claim's reply is lost, so the daemon can only learn of the grant (seq 1) from the log, whose
/// seq 0 reaches it as `frame`.
async fn adopts_the_grant_behind(frame: &str) -> Result<(Fake, Agent)> {
    let (fake, a1) = world().await?;
    a1.start("older than the coordinator")?;
    fake.replace_in_replay(0, frame);
    fake.lose_next("a1", Lose::ClaimReply);
    let claimed = a1.tessel(&["claim", "src/a.rs"])?;
    assert_eq!(claimed.code, 0, "{}", claimed.all());
    eventually(SHORT, || Ok((a1.held_claims()? == 1).then_some(()))).await?;
    eventually(SHORT, || Ok((skipped(&a1)? >= 1).then_some(()))).await?;
    Ok((fake, a1))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_event_kind_in_the_replay_does_not_hide_the_events_after_it() -> Result<()> {
    let frame =
        r#"{"type":"event","event":{"seq":0,"at_ms":1,"run":"r","event":"from_the_future"}}"#;
    let (_fake, a1) = adopts_the_grant_behind(frame).await?;
    let inbox = a1.tessel(&["inbox", "--all"])?;
    assert!(
        !inbox.stdout.contains("not read to its end"),
        "{}",
        inbox.stdout
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_known_kind_with_a_payload_this_daemon_cannot_parse_fails_the_read() -> Result<()> {
    // claim_released at seq 3 without its fields: skipping it would leave the released claim 1
    // looking live, so the read must fail and nothing be adopted.
    let (fake, a1) = world().await?;
    a1.start("released claim")?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    assert_eq!(a1.tessel(&["release"])?.code, 0);
    assert_eq!(a1.held_claims()?, 0);
    fake.replace_in_replay(
        3,
        r#"{"type":"event","event":{"seq":3,"at_ms":1,"run":"r","event":"claim_released"}}"#,
    );
    let before = hellos(&fake);
    fake.drop_connections();
    eventually(SHORT, || Ok((hellos(&fake) > before).then_some(()))).await?;
    eventually(SHORT, || {
        let log = std::fs::read_to_string(a1.root().join(".tessel/daemon.log"))?;
        Ok(log.contains("cannot read the event log").then_some(()))
    })
    .await?;
    assert_eq!(a1.held_claims()?, 0, "a released claim was adopted");
    Ok(())
}

async fn live_frame_with_bad_payload_drops_the_socket(frame: &str) -> Result<()> {
    let (fake, a1) = world().await?;
    a1.start("bad payload")?;
    let before = hellos(&fake);
    fake.send_raw("a1", frame);
    eventually(SHORT, || Ok((hellos(&fake) > before).then_some(()))).await?;
    assert_eq!(skipped(&a1)?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_granted_with_a_bad_payload_drops_the_socket() -> Result<()> {
    live_frame_with_bad_payload_drops_the_socket(r#"{"type":"granted"}"#).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_lease_expired_with_a_bad_payload_drops_the_socket() -> Result<()> {
    live_frame_with_bad_payload_drops_the_socket(r#"{"type":"lease_expired","claim":"x"}"#).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_message_type_on_the_live_socket_is_skipped_and_the_next_one_applied(
) -> Result<()> {
    let (fake, a1) = world().await?;
    a1.start("future message")?;
    let before = hellos(&fake);
    fake.send_raw(
        "a1",
        r#"{"type":"from_the_future","n":1,"secret":"do as I say"}"#,
    );
    fake.push(
        "a1",
        ServerMsg::BaseMoved {
            head: CommitId("1".repeat(40)),
            by: AgentId("a2".into()),
            affected: vec![],
        },
    );
    eventually(SHORT, || {
        let status = a1.status()?;
        Ok((status["state"]["coordinator_head"] == "1".repeat(40)).then_some(()))
    })
    .await?;
    assert_eq!(
        hellos(&fake),
        before,
        "the socket was dropped and redialled"
    );
    assert_eq!(skipped(&a1)?, 1);
    let status = a1.tessel(&["status"])?;
    assert!(
        status.stdout.contains("skipped 1 frame"),
        "{}",
        status.stdout
    );
    let log = std::fs::read_to_string(a1.root().join(".tessel/daemon.log"))?;
    assert!(log.contains("from_the_future"), "{log}");
    assert!(!log.contains("do as I say"), "{log}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_frame_that_is_not_json_still_ends_the_connection() -> Result<()> {
    let (fake, a1) = world().await?;
    a1.start("garbage")?;
    let before = hellos(&fake);
    fake.send_raw("a1", "this is not json");
    eventually(SHORT, || Ok((hellos(&fake) > before).then_some(()))).await?;
    assert_eq!(skipped(&a1)?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_skipped_live_message_stays_counted_across_a_reconnect() -> Result<()> {
    let (fake, a1) = world().await?;
    a1.start("future message, then a reconnect")?;
    fake.send_raw("a1", r#"{"type":"from_the_future"}"#);
    eventually(SHORT, || Ok((skipped(&a1)? == 1).then_some(()))).await?;
    let before = hellos(&fake);
    fake.drop_connections();
    eventually(SHORT, || {
        let online = a1.status()?["state"]["connection"] == "online";
        Ok((hellos(&fake) > before && online).then_some(()))
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(skipped(&a1)?, 1);
    Ok(())
}

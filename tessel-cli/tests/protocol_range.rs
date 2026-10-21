//! The coordinator announces the protocol versions it speaks in `Welcome`. A daemon whose version
//! is outside the range says so once, in its log and in `tessel status`, and connects anyway.

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
use support::{eventually, Agent, Fake};
use tessel_coordinator::protocol::{ClientMsg, ProtocolRange, PROTOCOL_VERSION};

const TOK1: &str = "tok-a1-S3CRETvalue";
const SHORT: Duration = Duration::from_secs(8);

fn hellos(fake: &Fake) -> usize {
    fake.received("a1")
        .iter()
        .filter(|m| matches!(m, ClientMsg::Hello { .. }))
        .count()
}

fn daemon_log(agent: &Agent) -> Result<String> {
    Ok(std::fs::read_to_string(
        agent.root().join(".tessel/daemon.log"),
    )?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_outside_the_announced_range_warns_once_and_connects_anyway() -> Result<()> {
    let range = ProtocolRange {
        min: PROTOCOL_VERSION + 1,
        max: PROTOCOL_VERSION + 2,
    };
    let fake = Fake::start_announcing(30_000, &[("a1", TOK1)], Some(range)).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    a1.start_settled("outside the range").await?;

    let status = a1.status()?;
    assert_eq!(
        status["state"]["connection"], "online",
        "it connects anyway"
    );
    let warning = status["state"]["protocol_warning"]
        .as_str()
        .unwrap_or_default();
    assert!(warning.contains("speaks protocol v2 to v3"), "{warning}");
    let text = a1.tessel(&["status"])?;
    assert!(
        text.stdout.contains("warning: the coordinator speaks"),
        "{}",
        text.stdout
    );

    let before = hellos(&fake);
    fake.drop_connections();
    eventually(SHORT, || {
        let online = a1.status()?["state"]["connection"] == "online";
        Ok((hellos(&fake) > before && online).then_some(()))
    })
    .await?;
    let log = daemon_log(&a1)?;
    assert_eq!(
        log.matches("warning: the coordinator speaks").count(),
        1,
        "warned once across a reconnect: {log}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_inside_the_announced_range_has_no_warning() -> Result<()> {
    let fake = Fake::start(30_000, &[("a1", TOK1)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    a1.start_settled("inside the range").await?;
    assert!(a1.status()?["state"]["protocol_warning"].is_null());
    assert!(!a1.tessel(&["status"])?.stdout.contains("warning:"));
    assert!(!daemon_log(&a1)?.contains("warning:"));
    Ok(())
}

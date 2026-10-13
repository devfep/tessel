//! A `granted` message carries the trunk head: the real daemon adopts it, `tessel status` shows
//! it, and an older server's grant without the field leaves the head alone.

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
use serde_json::Value;
use support::{eventually, Agent, Fake};
use tessel_coordinator::protocol::{ClaimId, ClientMsg, CommitId, Fence, RequestId, ServerMsg};

const TOK1: &str = "tok-a1-S3CRETvalue";
const TOK2: &str = "tok-a2-S3CRETvalue";
const SHORT: Duration = Duration::from_secs(8);

async fn world() -> Result<(Fake, Agent, Agent)> {
    let fake = Fake::start(30_000, &[("a1", TOK1), ("a2", TOK2)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    let a2 = Agent::new(&fake, "a2", TOK2)?;
    Ok((fake, a1, a2))
}

const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The `granted` message the coordinator sends for `req`, naming `head` when it knows one.
fn grant_naming(req: RequestId, head: Option<&str>) -> ServerMsg {
    ServerMsg::Granted {
        req,
        claim: ClaimId(900),
        fence: Fence(900),
        expires_at_ms: 0,
        race: None,
        at_risk: Vec::new(),
        head: head.map(|head| CommitId(head.into())),
    }
}

fn last_claim_req(fake: &Fake, agent: &str) -> Result<RequestId> {
    let mut found = None;
    for msg in fake.received(agent) {
        if let ClientMsg::Claim { req, .. } = msg {
            found = Some(req);
        }
    }
    found.context("the agent sent no claim")
}

/// a1 holds `src/a.rs` and a2 waits for it. Returns a2's pending request and the head a2 was
/// welcomed with.
fn a2_waits_behind_a1(fake: &Fake, a1: &Agent, a2: &Agent) -> Result<(RequestId, String)> {
    a1.start("hold it")?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    a2.start("need it")?;
    let queued = a2.tessel(&["claim", "--wait", "src/a.rs"])?;
    assert_eq!(queued.code, 4, "{}", queued.all());
    let welcomed = a2.status()?["state"]["coordinator_head"]
        .as_str()
        .context("a2 was welcomed with no head")?
        .to_string();
    assert_ne!(welcomed, HEAD_B);
    Ok((last_claim_req(fake, "a2")?, welcomed))
}

/// The head `state.json` records, read from the file, not from the live daemon.
fn persisted_head(agent: &Agent) -> Result<Option<String>> {
    let text = std::fs::read_to_string(agent.root().join(".tessel/state.json"))?;
    let state: Value = serde_json::from_str(&text)?;
    Ok(state["coordinator_head"].as_str().map(str::to_string))
}

fn status_says_head(agent: &Agent, head: &str) -> Result<bool> {
    let status = agent.tessel(&["status"])?;
    Ok(status
        .stdout
        .contains(&format!("coordinator head: {head}\n")))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_after_a_wait_moves_the_head_status_shows() -> Result<()> {
    let (fake, a1, a2) = world().await?;
    let (req, _) = a2_waits_behind_a1(&fake, &a1, &a2)?;

    fake.push("a2", grant_naming(req, Some(HEAD_B)));

    eventually(SHORT, || Ok((a2.held_claims()? == 1).then_some(()))).await?;
    assert!(
        status_says_head(&a2, HEAD_B)?,
        "{}",
        a2.tessel(&["status"])?.all()
    );
    assert_eq!(persisted_head(&a2)?.as_deref(), Some(HEAD_B));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_without_a_head_leaves_the_head_alone() -> Result<()> {
    let (fake, a1, a2) = world().await?;
    let (req, welcomed) = a2_waits_behind_a1(&fake, &a1, &a2)?;

    let mut old_server = serde_json::to_value(grant_naming(req, Some(HEAD_B)))?;
    let fields = old_server.as_object_mut().context("a grant is an object")?;
    assert!(fields.remove("head").is_some());
    fake.send_raw("a2", &old_server.to_string());

    eventually(SHORT, || Ok((a2.held_claims()? == 1).then_some(()))).await?;
    assert!(
        status_says_head(&a2, &welcomed)?,
        "{}",
        a2.tessel(&["status"])?.all()
    );
    assert_eq!(persisted_head(&a2)?.as_deref(), Some(welcomed.as_str()));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_with_a_null_head_leaves_the_head_alone() -> Result<()> {
    let (fake, a1, a2) = world().await?;
    let (req, welcomed) = a2_waits_behind_a1(&fake, &a1, &a2)?;

    fake.push("a2", grant_naming(req, None));

    eventually(SHORT, || Ok((a2.held_claims()? == 1).then_some(()))).await?;
    assert!(
        status_says_head(&a2, &welcomed)?,
        "{}",
        a2.tessel(&["status"])?.all()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_head_of_a_grant_is_written_to_state_even_when_no_request_awaits_it() -> Result<()> {
    let (fake, a1, _a2) = world().await?;
    a1.start("stray grant")?;

    fake.push("a1", grant_naming(RequestId(9_999), Some(HEAD_B)));

    eventually(SHORT, || Ok(status_says_head(&a1, HEAD_B)?.then_some(()))).await?;
    eventually(SHORT, || {
        Ok((persisted_head(&a1)?.as_deref() == Some(HEAD_B)).then_some(()))
    })
    .await
}

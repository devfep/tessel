//! A `Land` in doubt settled by the operator with the server stopped (`tessel-server admin
//! resolve --landed`): the waiter behind the settled claim is granted by the stored log, with no
//! notice ever sent, and its daemon adopts the claim when it reconnects.

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
use support::{eventually, git, Agent, Fake};
use tessel_coordinator::protocol::ClientMsg;

const TOK1: &str = "tok-a1-S3CRETvalue";
const TOK2: &str = "tok-a2-S3CRETvalue";
const SHORT: Duration = Duration::from_secs(8);

fn hellos(fake: &Fake, agent: &str) -> usize {
    fake.received(agent)
        .iter()
        .filter(|msg| matches!(msg, ClientMsg::Hello { .. }))
        .count()
}

fn commit_in(agent: &Agent, path: &str, text: &str) -> Result<()> {
    std::fs::write(agent.root().join(path), text)?;
    git(&agent.root(), &["add", path])?;
    git(
        &agent.root(),
        &["commit", "-q", "-m", &format!("edit {path}")],
    )?;
    Ok(())
}

fn only_claim(agent: &Agent) -> Result<u64> {
    let status = agent.status()?;
    let claims = status["state"]["claims"]
        .as_array()
        .context("no claims array")?;
    claims
        .first()
        .and_then(|claim| claim["claim"].as_u64())
        .context("the agent holds no claim")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_waiter_adopts_the_claim_an_offline_landed_resolve_granted() -> Result<()> {
    let fake = Fake::start(30_000, &[("a1", TOK1), ("a2", TOK2)]).await?;
    fake.freeze_clock();
    let (a1, a2) = (
        Agent::new(&fake, "a1", TOK1)?,
        Agent::new(&fake, "a2", TOK2)?,
    );
    a1.start("hold it")?;
    assert_eq!(a1.tessel(&["claim", "src/a.rs"])?.code, 0);
    let claim = only_claim(&a1)?;
    commit_in(&a1, "src/a.rs", "pub fn a() { 1; }\n")?;
    assert_eq!(a1.tessel(&["submit", "--evidence", "ok"])?.code, 0);
    a2.start("need it")?;
    let queued = a2.tessel(&["claim", "--wait", "src/a.rs"])?;
    assert_eq!(queued.code, 4, "{}", queued.all());

    fake.land_in_doubt();
    let killed = fake.stored();
    fake.set_accepting(false);
    fake.drop_connections();
    eventually(SHORT, || {
        let both_down = a1.status()?["state"]["connection"] != "online"
            && a2.status()?["state"]["connection"] != "online";
        Ok((both_down && fake.open_sockets() == 0).then_some(()))
    })
    .await
    .context("the daemons never noticed the server stopped")?;
    fake.resolve_offline(killed, (claim, true))?;
    assert_eq!(
        a2.held_claims()?,
        0,
        "nothing reached the waiter while offline"
    );

    let before = hellos(&fake, "a2");
    fake.set_accepting(true);
    eventually(SHORT, || Ok((hellos(&fake, "a2") > before).then_some(())))
        .await
        .context("a2 never reconnected")?;
    eventually(SHORT, || Ok((a2.held_claims()? == 1).then_some(())))
        .await
        .with_context(|| {
            format!(
                "a2 adopted nothing: {}",
                a2.tessel_files().unwrap_or_default()
            )
        })?;
    assert!(a2.status()?["state"]["queued"].is_null());
    let inbox = a2.tessel(&["inbox", "--all"])?;
    assert!(
        inbox.stdout.contains("[reconciled]") && inbox.stdout.contains("it is now tracked"),
        "{}",
        inbox.stdout
    );
    let released = a2.tessel(&["release"])?;
    assert_eq!(
        released.code,
        0,
        "the adopted claim is live: {}",
        released.all()
    );
    eventually(SHORT, || Ok((a1.held_claims()? == 0).then_some(()))).await?;
    Ok(())
}

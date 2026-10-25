//! The server reads time from one clock. A test clock frozen and advanced from outside moves the
//! lease and the expiry wake with it, with no sleeping; the configured lease sets how long a grant
//! lives.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, EventKind, Fence, Intent, Mode, OnConflict,
    ReleaseReason, RequestId, Scope, ScopeClaim, ServerMsg, PROTOCOL_VERSION,
};
use tessel_server::config::{Config, RepositoryConfig};
use tessel_server::id::RepoId;
use tessel_server::repo::NoLander;
use tessel_server::server::{Deps, RunningServer};
use tessel_server::testing::{self, TestClock};
use tessel_server::ws::{Refusal, Verifier};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const WAIT: Duration = Duration::from_secs(2);

struct Tokens;

impl Verifier for Tokens {
    fn verify(&self, token: &str, _repo: &RepoId) -> Result<AgentId, Refusal> {
        token
            .strip_prefix("token-")
            .map(|agent| AgentId(agent.to_string()))
            .ok_or(Refusal {
                reason: "unknown token",
            })
    }
}

async fn start(
    dir: &std::path::Path,
    lease_ms: Option<u64>,
    clock: &TestClock,
) -> Result<RunningServer, Box<dyn std::error::Error>> {
    let config = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        store_dir: dir.join("stores"),
        identity: None,
        lander: None,
        repositories: vec![RepositoryConfig {
            tenant: "acme".to_string(),
            name: "demo".to_string(),
            run: "test".to_string(),
            reviewers: Vec::new(),
            sensitive: Vec::new(),
            remote: None,
            fork: None,
            lease_ms,
            shadow_enabled: false,
        }],
    };
    let deps = Deps {
        verifier: Some(Arc::new(Tokens)),
        calls: Arc::new(NoLander),
    };
    Ok(testing::start_with_clock(&config, deps, None, clock).await?)
}

async fn welcomed(addr: SocketAddr, agent: &str) -> Result<Socket, Box<dyn std::error::Error>> {
    let mut request = format!("ws://{addr}/tenant/acme/repo/demo/ws").into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer token-{agent}").parse()?);
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await?;
    let hello = ClientMsg::Hello {
        agent: AgentId(agent.to_string()),
        base: CommitId("ab".repeat(20)),
        protocol: PROTOCOL_VERSION,
    };
    socket
        .send(Message::text(serde_json::to_string(&hello)?))
        .await?;
    let welcome = next(&mut socket).await?;
    assert!(matches!(welcome, ServerMsg::Welcome { .. }), "{welcome:?}");
    Ok(socket)
}

async fn next(socket: &mut Socket) -> Result<ServerMsg, Box<dyn std::error::Error>> {
    loop {
        let frame = tokio::time::timeout(WAIT, socket.next())
            .await?
            .ok_or("the socket closed")??;
        if let Message::Text(text) = frame {
            return Ok(serde_json::from_str(&text)?);
        }
    }
}

async fn claim(socket: &mut Socket) -> Result<ServerMsg, Box<dyn std::error::Error>> {
    let claim = ClientMsg::Claim {
        req: RequestId(1),
        intent: Intent {
            summary: "edit".to_string(),
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes: vec![ScopeClaim {
            scope: Scope::File {
                path: "src/a.rs".to_string(),
            },
            mode: Mode::EditBody,
        }],
        on_conflict: OnConflict::Fail,
    };
    socket
        .send(Message::text(serde_json::to_string(&claim)?))
        .await?;
    next(socket).await
}

#[tokio::test]
async fn a_frozen_clock_stamps_grants_and_the_configured_lease_sets_their_length() -> TestResult {
    let dir = tempfile::tempdir()?;
    let clock = TestClock::new();
    clock.freeze();
    let server = start(dir.path(), Some(5_000), &clock).await?;
    let mut socket = welcomed(server.local_addr(), "a1").await?;
    let granted = claim(&mut socket).await?;
    let ServerMsg::Granted { expires_at_ms, .. } = granted else {
        return Err(format!("expected a grant, got {granted:?}").into());
    };
    assert_eq!(expires_at_ms, clock.now_ms() + 5_000);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn advancing_the_clock_past_the_lease_expires_the_claim_without_sleeping() -> TestResult {
    let dir = tempfile::tempdir()?;
    let clock = TestClock::new();
    clock.freeze();
    let server = start(dir.path(), Some(5_000), &clock).await?;
    let mut socket = welcomed(server.local_addr(), "a1").await?;
    let ServerMsg::Granted { claim: held, .. } = claim(&mut socket).await? else {
        return Err("expected a grant".into());
    };
    clock.advance(4_000);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), socket.next())
            .await
            .is_err(),
        "the claim lapsed early"
    );
    clock.advance(2_000);
    let lapsed = next(&mut socket).await?;
    assert!(
        matches!(lapsed, ServerMsg::LeaseExpired { claim, .. } if claim == held),
        "{lapsed:?}"
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn without_a_setting_a_grant_lives_thirty_seconds() -> TestResult {
    let dir = tempfile::tempdir()?;
    let clock = TestClock::new();
    clock.freeze();
    let server = start(dir.path(), None, &clock).await?;
    let mut socket = welcomed(server.local_addr(), "a1").await?;
    let ServerMsg::Granted { expires_at_ms, .. } = claim(&mut socket).await? else {
        return Err("expected a grant".into());
    };
    assert_eq!(expires_at_ms, clock.now_ms() + 30_000);
    server.stop().await;
    Ok(())
}

/// A call frame is answered after the expiry step it ran: the lease that ran out is logged, and
/// sent to a watcher, before the answer to the frame is sent to the same socket. The swarm's
/// `settle_expiry` is built on this.
#[tokio::test]
async fn a_call_frame_sent_after_the_clock_moved_is_answered_after_the_expiry_it_ran() -> TestResult
{
    let dir = tempfile::tempdir()?;
    let clock = TestClock::new();
    clock.freeze();
    let server = start(dir.path(), Some(5_000), &clock).await?;
    let mut holder = welcomed(server.local_addr(), "a1").await?;
    let ServerMsg::Granted { claim: held, .. } = claim(&mut holder).await? else {
        return Err("expected a grant".into());
    };
    let mut reader = welcomed(server.local_addr(), "a2").await?;
    let watch = ClientMsg::Watch { from_seq: 0 };
    reader
        .send(Message::text(serde_json::to_string(&watch)?))
        .await?;
    clock.advance(6_000);
    let probe = ClientMsg::Release {
        claim: ClaimId(u64::MAX),
        fence: Fence(1),
        req: Some(RequestId(77)),
    };
    reader
        .send(Message::text(serde_json::to_string(&probe)?))
        .await?;
    let mut expired_before_the_answer = false;
    loop {
        match next(&mut reader).await? {
            ServerMsg::Event { event } => {
                if let EventKind::ClaimReleased { claim, reason } = event.kind {
                    expired_before_the_answer |=
                        claim == held && reason == ReleaseReason::LeaseExpired;
                }
            }
            ServerMsg::Error {
                req: Some(RequestId(77)),
                ..
            } => break,
            _ => {}
        }
    }
    assert!(
        expired_before_the_answer,
        "the answer came before the expiry was logged"
    );
    server.stop().await;
    Ok(())
}

//! `settle_expiry` is a barrier: when it returns, every expiry the clock's move caused is already
//! in the log, with no waiting for it, and a move short of the lease has caused none.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    AgentId, ClientMsg, CommitId, Event, EventKind, Intent, Mode, OnConflict, ReleaseReason,
    RequestId, Scope, ScopeClaim, ServerMsg, PROTOCOL_VERSION,
};
use tessel_swarm::demo;
use tessel_swarm::guard::ScratchRepo;
use tessel_swarm::local::{LocalServer, LocalSetup};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const LEASE_MS: u64 = 1000;
const ROUNDS: usize = 8;

fn expiries(log: &[Event]) -> usize {
    log.iter()
        .filter(|e| {
            matches!(
                e.kind,
                EventKind::ClaimReleased {
                    reason: ReleaseReason::LeaseExpired,
                    ..
                }
            )
        })
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settle_expiry_returns_only_after_the_expiry_is_in_the_log() {
    let scratch = tempfile::tempdir().unwrap();
    let repo = ScratchRepo::parse("swarm-settle").unwrap();
    let names = ["a01".to_string()];
    let base = demo::base_tree();
    let server = LocalServer::start(LocalSetup {
        repo: &repo,
        scratch: scratch.path(),
        base: &base,
        names: &names,
        reviewers: &[],
        shadow_enabled: false,
        lease_ms: LEASE_MS,
    })
    .await
    .unwrap();
    server.freeze_clock();
    let mut request = server
        .endpoint
        .ws_url
        .clone()
        .into_client_request()
        .unwrap();
    let token = server
        .endpoint
        .token_of("a01")
        .unwrap()
        .expose()
        .to_string();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let hello = ClientMsg::Hello {
        agent: AgentId("a01".into()),
        base: CommitId("0".repeat(40)),
        protocol: PROTOCOL_VERSION,
    };
    send(&mut socket, &hello).await;
    for round in 0..ROUNDS {
        let claim = ClientMsg::Claim {
            req: RequestId(round as u64 + 1),
            intent: Intent {
                summary: "work".into(),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes: vec![ScopeClaim {
                scope: Scope::File {
                    path: "src/pricing.ts".into(),
                },
                mode: Mode::EditBody,
            }],
            on_conflict: OnConflict::Fail,
        };
        send(&mut socket, &claim).await;
        loop {
            let msg = next(&mut socket).await;
            if matches!(msg, ServerMsg::Granted { .. }) {
                break;
            }
        }
        server.advance_clock(LEASE_MS - 1);
        server.settle_expiry().await;
        assert_eq!(
            expiries(&server.log()),
            round,
            "round {round}: the lease has not passed yet"
        );
        server.advance_clock(1);
        server.settle_expiry().await;
        assert_eq!(
            expiries(&server.log()),
            round + 1,
            "round {round}: the lease passed"
        );
    }
    server.shutdown().await;
}

async fn send<S>(socket: &mut S, msg: &ClientMsg)
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Debug,
{
    socket
        .send(Message::text(serde_json::to_string(msg).unwrap()))
        .await
        .unwrap();
}

async fn next<S>(socket: &mut S) -> ServerMsg
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = frame {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

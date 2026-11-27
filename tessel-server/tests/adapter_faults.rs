//! The adapter's faults over a real listener (P1-ADAPTER-SUITE, the rest): a watcher that stops
//! reading, a repository task that panics, and a store that fails. Each test names the contract
//! it pins in `docs/proposals/real-use.md` (C4 sends never block, C5 a panicking task, C8 and
//! the store's errors).

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, Fence, Intent, Mode, OnConflict, RequestId, Scope,
    ScopeClaim, ServerMsg, PROTOCOL_VERSION,
};
use tessel_coordinator::shell;
use tessel_server::config::{Config, RepositoryConfig};
use tessel_server::id::RepoId;
use tessel_server::repo::{CallPort, NoLander};
use tessel_server::server::{self, Deps, RunningServer, StartError};
use tessel_server::store::Store;
use tessel_server::ws::{Refusal, Verifier};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

#[expect(
    dead_code,
    reason = "each test crate compiles all of support and uses only some of it"
)]
mod support;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A bound on liveness, not on speed: the Mac this runs on is often loaded ten to sixty times.
const LIVE: Duration = Duration::from_secs(60);
const TOKEN_A1: &str = "token-for-a1";
const TOKEN_W: &str = "token-for-the-watcher";
const DEMO: &str = "/tenant/acme/repo/demo";

struct Tokens;

impl Verifier for Tokens {
    fn verify(&self, token: &str, _repo: &RepoId) -> Result<AgentId, Refusal> {
        let table = HashMap::from([(TOKEN_A1, "a1"), (TOKEN_W, "watcher")]);
        table
            .get(token)
            .map(|agent| AgentId((*agent).to_string()))
            .ok_or(Refusal {
                reason: "unknown token",
            })
    }
}

fn config(dir: &Path) -> Config {
    Config {
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
            lease_ms: None,
            shadow_enabled: false,
        }],
    }
}

async fn start(dir: &Path) -> Result<RunningServer, StartError> {
    let calls: Arc<dyn CallPort> = Arc::new(NoLander);
    let verifier: Arc<dyn Verifier> = Arc::new(Tokens);
    let deps = Deps {
        verifier: Some(verifier),
        calls,
    };
    server::start(&config(dir), deps).await
}

async fn connect(addr: SocketAddr, token: &str) -> Result<Socket, WsError> {
    let mut request = format!("ws://{addr}{DEMO}/ws").into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(socket)
}

async fn send(socket: &mut Socket, msg: &ClientMsg) -> TestResult {
    socket
        .send(Message::text(serde_json::to_string(msg)?))
        .await?;
    Ok(())
}

/// The next server message, or `None` when the socket ended.
async fn next(socket: &mut Socket) -> Result<Option<ServerMsg>, Box<dyn std::error::Error>> {
    loop {
        match tokio::time::timeout(LIVE, socket.next()).await? {
            Some(Ok(Message::Text(text))) => return Ok(Some(serde_json::from_str(&text)?)),
            Some(Ok(Message::Close(_) | Message::Binary(_))) | None => return Ok(None),
            Some(Ok(_)) => {}
            Some(Err(_)) => return Ok(None),
        }
    }
}

fn hello(agent: &str) -> ClientMsg {
    ClientMsg::Hello {
        agent: AgentId(agent.to_string()),
        base: CommitId("ab".repeat(20)),
        protocol: PROTOCOL_VERSION,
    }
}

fn claim(req: u64, path: &str) -> ClientMsg {
    ClientMsg::Claim {
        req: RequestId(req),
        intent: Intent {
            summary: format!("edit {path}"),
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes: vec![ScopeClaim {
            scope: Scope::File {
                path: path.to_string(),
            },
            mode: Mode::EditBody,
        }],
        on_conflict: OnConflict::Fail,
    }
}

fn granted(msg: &ServerMsg) -> Option<(ClaimId, Fence)> {
    if let ServerMsg::Granted { claim, fence, .. } = msg {
        return Some((*claim, *fence));
    }
    None
}

/// Store a log of `total` events of about `pad` bytes each, and the state that knows them, as a
/// server that wrote them would have left the store.
fn seed_log(dir: &Path, total: u64, pad: usize) -> TestResult {
    let id = RepoId::new("acme", "demo")?;
    let file = id.store_file(&dir.join("stores"));
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut store = Store::open(&file)?;
    let padding = "x".repeat(pad);
    let mut batch = Vec::new();
    for seq in 0..total {
        let event = format!(
            r#"{{"seq":{seq},"at_ms":{seq},"run":"r","event":"agent_connected","agent":"{padding}"}}"#
        );
        batch.push((shell::event_key(seq), event));
        if batch.len() == 200 {
            store.commit(&std::mem::take(&mut batch))?;
        }
    }
    let core = shell::load_core(None, Some("test"), None, Vec::new(), Vec::new())
        .map_err(|error| format!("building the first state: {error:?}"))?;
    let mut state = serde_json::to_value(&core)?;
    state["next_seq"] = serde_json::json!(total);
    batch.push((shell::STATE_KEY.to_string(), state.to_string()));
    store.commit(&batch)?;
    Ok(())
}

/// Events seeded for the slow watcher, and the bytes of each. On the Mac this was measured on, 1,150
/// events of 4,000 bytes do not fill the writer queue (1,024 messages) beyond the kernel buffers of
/// a loopback socket nobody reads, and 1,250 do; 3,000 leaves room for larger buffers on Linux.
const SLOW_EVENTS: u64 = 3_000;
const SLOW_PAD: usize = 4_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watcher_that_never_reads_is_dropped_and_the_repo_goes_on() -> TestResult {
    support::capture_logs();
    let dir = tempfile::tempdir()?;
    seed_log(dir.path(), SLOW_EVENTS, SLOW_PAD)?;
    let server = start(dir.path()).await?;
    let addr = server.local_addr();
    let mut watcher = connect(addr, TOKEN_W).await?;
    send(&mut watcher, &ClientMsg::Watch { from_seq: 0 }).await?;
    // The watcher reads nothing from here on. The host logs the failed send once the socket's
    // writer queue was full, so that line is the proof the queue filled and not a kernel buffer.
    let dropped = async {
        loop {
            if !support::log_lines_with("send to a socket failed").is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(LIVE, dropped)
        .await
        .map_err(|_| "the unread watcher was never dropped")?;

    let mut agent = connect(addr, TOKEN_A1).await?;
    send(&mut agent, &hello("a1")).await?;
    let welcome = next(&mut agent).await?;
    assert!(
        matches!(welcome, Some(ServerMsg::Welcome { .. })),
        "{welcome:?}"
    );
    send(&mut agent, &claim(1, "src/a.rs")).await?;
    let reply = next(&mut agent).await?;
    assert!(
        reply.as_ref().and_then(granted).is_some(),
        "the repo stopped serving other agents: {reply:?}"
    );

    let mut events = 0_u64;
    while let Some(msg) = next(&mut watcher).await? {
        events += u64::from(matches!(msg, ServerMsg::Event { .. }));
    }
    assert!(
        events < SLOW_EVENTS,
        "the watcher was sent the whole log ({events} events): its queue never filled"
    );
    server.stop().await;
    Ok(())
}

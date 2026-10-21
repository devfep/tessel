//! The adapter over a real listener: the server runs in process on an ephemeral port and is
//! driven with a WebSocket client and plain HTTP. The tests named `adapter_*` are the rows R2 to
//! R5 and R49 of `docs/reference/runtime-rules.md`.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::host::call::{Call, CallAnswer, Request};
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, DecisionRecord, EventKind, Fence, Intent, Mode,
    OnConflict, ProtocolRange, RequestId, Scope, ScopeClaim, ServerMsg, PROTOCOL_VERSION,
};
use tessel_server::config::{Config, RepositoryConfig};
use tessel_server::id::RepoId;
use tessel_server::identity::{self, Ed25519Verifier, Role, TokenRequest};
use tessel_server::keys;
use tessel_server::repo::{BoxFuture, CallPort, NoLander};
use tessel_server::server::{self, Deps, RunningServer, StartError};
use tessel_server::store::{Store, StoreError};
use tessel_server::ws::{Refusal, Verifier};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderName;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const WAIT: Duration = Duration::from_secs(5);
const TOKEN_A1: &str = "token-for-a1-S3CRETvalue";
const TOKEN_A2: &str = "token-for-a2";
const TOKEN_A3: &str = "token-for-a3";
const TOKEN_LANDER: &str = "token-for-the-lander";
const DEMO: &str = "/tenant/acme/repo/demo";

/// The in-process stand-in for identity: a table from token to agent.
struct Tokens(HashMap<&'static str, &'static str>);

impl Verifier for Tokens {
    fn verify(&self, token: &str, _repo: &RepoId) -> Result<AgentId, Refusal> {
        self.0
            .get(token)
            .map(|agent| AgentId((*agent).to_string()))
            .ok_or(Refusal {
                reason: "unknown token",
            })
    }
}

fn tokens() -> Arc<dyn Verifier> {
    Arc::new(Tokens(HashMap::from([
        (TOKEN_A1, "a1"),
        (TOKEN_A2, "a2"),
        (TOKEN_A3, "a3"),
        (TOKEN_LANDER, "lander"),
    ])))
}

fn config(dir: &Path, repos: &[(&str, &str)]) -> Config {
    Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        store_dir: dir.join("stores"),
        identity: None,
        repositories: repos
            .iter()
            .map(|(tenant, name)| RepositoryConfig {
                tenant: (*tenant).to_string(),
                name: (*name).to_string(),
                run: "test".to_string(),
                reviewers: Vec::new(),
                sensitive: Vec::new(),
            })
            .collect(),
    }
}

async fn start_with(
    dir: &Path,
    repos: &[(&str, &str)],
    verifier: Option<Arc<dyn Verifier>>,
    calls: Arc<dyn CallPort>,
) -> Result<RunningServer, StartError> {
    server::start(&config(dir, repos), Deps { verifier, calls }).await
}

async fn start(dir: &Path) -> Result<RunningServer, StartError> {
    start_with(dir, &[("acme", "demo")], Some(tokens()), Arc::new(NoLander)).await
}

async fn connect(
    addr: SocketAddr,
    path: &str,
    token: Option<&str>,
    extra: &[(&str, &str)],
) -> Result<Socket, WsError> {
    let mut request = format!("ws://{addr}{path}/ws").into_client_request()?;
    if let Some(token) = token {
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse()?);
    }
    for (name, value) in extra {
        let name = HeaderName::from_bytes(name.as_bytes())?;
        request.headers_mut().insert(name, value.parse()?);
    }
    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(socket)
}

/// The status the server refused the upgrade with.
fn refused_with(result: Result<Socket, WsError>) -> Result<u16, Box<dyn std::error::Error>> {
    if let Err(WsError::Http(response)) = &result {
        return Ok(response.status().as_u16());
    }
    Err(format!("expected an HTTP refusal, got {:?}", result.err()).into())
}

async fn send(socket: &mut Socket, msg: &ClientMsg) -> TestResult {
    socket
        .send(Message::text(serde_json::to_string(msg)?))
        .await?;
    Ok(())
}

/// The next server message, or the close frame as `None`.
async fn next(socket: &mut Socket) -> Result<Option<ServerMsg>, Box<dyn std::error::Error>> {
    loop {
        let frame = tokio::time::timeout(WAIT, socket.next()).await?;
        match frame {
            Some(Ok(Message::Text(text))) => return Ok(Some(serde_json::from_str(&text)?)),
            Some(Ok(Message::Close(_))) | None => return Ok(None),
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.into()),
        }
    }
}

async fn expect(socket: &mut Socket) -> Result<ServerMsg, Box<dyn std::error::Error>> {
    next(socket)
        .await?
        .ok_or_else(|| "the socket closed".into())
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
        scopes: vec![edit(path)],
        on_conflict: OnConflict::Fail,
    }
}

fn edit(path: &str) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::File {
            path: path.to_string(),
        },
        mode: Mode::EditBody,
    }
}

/// A submission with test evidence, so the core does not hold it for review.
fn submit(claim: ClaimId, fence: Fence) -> ClientMsg {
    ClientMsg::Submit {
        req: RequestId(2),
        claim,
        fence,
        fork_commit: CommitId("cd".repeat(20)),
        touched: vec![edit("src/a.rs")],
        decisions: DecisionRecord {
            evidence: vec!["cargo test passed".to_string()],
            ..DecisionRecord::default()
        },
    }
}

/// Connect `token`'s agent to `path`, say hello and take the welcome.
async fn welcomed(
    addr: SocketAddr,
    path: &str,
    token: &str,
    agent: &str,
) -> Result<Socket, Box<dyn std::error::Error>> {
    let mut socket = connect(addr, path, Some(token), &[]).await?;
    send(&mut socket, &hello(agent)).await?;
    let welcome = expect(&mut socket).await?;
    assert!(matches!(welcome, ServerMsg::Welcome { .. }), "{welcome:?}");
    Ok(socket)
}

struct Http {
    status: u16,
    head: String,
    body: String,
}

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
) -> Result<Http, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(addr).await?;
    let auth = token.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\n{auth}Content-Length: 0\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    let mut raw = String::new();
    tokio::time::timeout(WAIT, stream.read_to_string(&mut raw)).await??;
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head.split_whitespace().nth(1).unwrap_or("0").parse()?;
    Ok(Http {
        status,
        head: head.to_string(),
        body: body.to_string(),
    })
}

fn events_of(dir: &Path, tenant: &str, repo: &str) -> Result<Vec<EventKind>, StoreError> {
    let file = RepoId::new(tenant, repo)
        .map_err(|e| StoreError::Foreign {
            path: e.to_string(),
        })?
        .store_file(&dir.join("stores"));
    let events = Store::open_read_only(&file)?.read_events(0, 100)?;
    Ok(events.into_iter().map(|event| event.kind).collect())
}

fn granted(msg: &ServerMsg) -> Option<(ClaimId, Fence)> {
    if let ServerMsg::Granted { claim, fence, .. } = msg {
        return Some((*claim, *fence));
    }
    None
}

#[tokio::test]
async fn an_agent_with_a_valid_token_is_welcomed_and_granted_and_the_grant_is_stored_first(
) -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let mut socket = welcomed(server.local_addr(), DEMO, TOKEN_A1, "a1").await?;
    send(&mut socket, &claim(1, "src/a.rs")).await?;
    let reply = expect(&mut socket).await?;
    assert!(granted(&reply).is_some(), "{reply:?}");
    let stored = events_of(dir.path(), "acme", "demo")?;
    assert!(
        stored
            .iter()
            .any(|kind| matches!(kind, EventKind::ClaimGranted { .. })),
        "the grant is in the SQLite file by the time its reply arrived: {stored:?}"
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_ignores_a_client_sent_agent_header() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let addr = server.local_addr();
    let liar = [("x-tessel-verified-agent", "a2"), ("x-agent", "a2")];
    let mut socket = connect(addr, DEMO, Some(TOKEN_A1), &liar).await?;
    send(&mut socket, &hello("a2")).await?;
    let refusal = expect(&mut socket).await?;
    assert!(matches!(refusal, ServerMsg::Error { .. }), "{refusal:?}");
    assert!(next(&mut socket).await?.is_none(), "closed with 1008");
    let mut honest = connect(addr, DEMO, Some(TOKEN_A1), &liar).await?;
    send(&mut honest, &hello("a1")).await?;
    assert!(matches!(
        expect(&mut honest).await?,
        ServerMsg::Welcome { .. }
    ));
    server.stop().await;
    Ok(())
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One subscriber for the whole test binary: a scoped one is lost to the callsites other tests
/// register first, and tests run in parallel. The test looks only for lines of its own repository.
fn captured_log() -> &'static Captured {
    static LOG: OnceLock<Captured> = OnceLock::new();
    LOG.get_or_init(|| {
        let log = Captured::default();
        let sink = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
        log
    })
}

#[tokio::test]
async fn adapter_logs_a_refusal_without_the_token() -> TestResult {
    let log = captured_log();
    let dir = tempfile::tempdir()?;
    let repos = [("acme", "refusal-log")];
    let server = start_with(dir.path(), &repos, Some(tokens()), Arc::new(NoLander)).await?;
    let path = "/tenant/acme/repo/refusal-log";
    let secret = "S3CRET-presented-token";
    assert_eq!(
        refused_with(connect(server.local_addr(), path, Some(secret), &[]).await)?,
        401
    );
    assert_eq!(
        refused_with(connect(server.local_addr(), path, None, &[]).await)?,
        401
    );
    let text = String::from_utf8(log.0.lock().unwrap_or_else(PoisonError::into_inner).clone())?;
    let ours: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("refusal-log") && line.contains("refused"))
        .collect();
    assert!(
        ours.iter().any(|line| line.contains("unknown token")),
        "{ours:?}"
    );
    assert!(
        ours.iter().any(|line| line.contains("no bearer token")),
        "{ours:?}"
    );
    assert!(
        ours.iter()
            .all(|line| line.contains("/tenant/acme/repo/refusal-log/ws")),
        "{ours:?}"
    );
    assert!(!text.contains(secret), "the token is never logged: {text}");
    server.stop().await;
    logs_a_real_refusal_without_the_token(log).await
}

/// A token the real verifier refuses is logged by its reason and path, not by its text.
async fn logs_a_real_refusal_without_the_token(log: &Captured) -> TestResult {
    let dir = tempfile::tempdir()?;
    let keys = dir.path().join("keys");
    keys::generate(&keys, "k1")?;
    let key = keys::load_signing_key(&keys, "k1")?;
    let verifier: Arc<dyn Verifier> = Arc::new(Ed25519Verifier::load(&keys, "tessel-test")?);
    let elsewhere = TokenRequest {
        kid: "k1",
        audience: "tessel-test",
        tenant: "acme",
        repo: "real-refusal-log",
        agent: "a1",
        role: Role::Agent,
        ttl_ms: 60_000,
    };
    let token = identity::mint(&key, &elsewhere, identity::now_ms())?;
    let seed = std::fs::read_to_string(keys.join("k1.key"))?;
    let repos = [("acme", "real-refusal-log"), ("acme", "other-repo")];
    let server = start_with(dir.path(), &repos, Some(verifier), Arc::new(NoLander)).await?;
    let path = "/tenant/acme/repo/other-repo";
    assert_eq!(
        refused_with(connect(server.local_addr(), path, Some(&token), &[]).await)?,
        401
    );
    let text = String::from_utf8(log.0.lock().unwrap_or_else(PoisonError::into_inner).clone())?;
    let ours: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("other-repo") && line.contains("refused"))
        .collect();
    assert!(
        ours.iter()
            .any(|line| line.contains("token is for another repository")),
        "{ours:?}"
    );
    assert!(!text.contains(&token), "the token is never logged");
    assert!(
        !text.contains(seed.trim()),
        "the secret seed is never logged"
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_summary_trunk_poke_and_ws_without_a_verified_agent_are_401() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let addr = server.local_addr();
    for (method, path) in [
        ("GET", format!("{DEMO}/summary")),
        ("POST", format!("{DEMO}/trunk-moved")),
        ("GET", format!("{DEMO}/ws")),
    ] {
        for token in [None, Some("not-a-token")] {
            let answer = http(addr, method, &path, token).await?;
            assert_eq!(
                (answer.status, answer.body.as_str()),
                (401, "unauthorized"),
                "{path}"
            );
        }
    }
    let unconfigured = tempfile::tempdir()?;
    let bare = start_with(
        unconfigured.path(),
        &[("acme", "demo")],
        None,
        Arc::new(NoLander),
    )
    .await?;
    for (method, path) in [
        ("GET", format!("{DEMO}/summary")),
        ("POST", format!("{DEMO}/trunk-moved")),
    ] {
        let answer = http(bare.local_addr(), method, &path, Some(TOKEN_A1)).await?;
        assert_eq!(
            answer.status, 401,
            "a server with no identity answers 401 on {path}"
        );
    }
    assert_eq!(
        refused_with(connect(bare.local_addr(), DEMO, Some(TOKEN_A1), &[]).await)?,
        401
    );
    bare.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_ws_without_an_upgrade_is_426() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let answer = http(
        server.local_addr(),
        "GET",
        &format!("{DEMO}/ws"),
        Some(TOKEN_A1),
    )
    .await?;
    assert_eq!(answer.status, 426, "{}", answer.head);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn adapter_two_repos_get_separate_cores_and_logs() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repos = [("acme", "demo"), ("other", "demo")];
    let server = start_with(dir.path(), &repos, Some(tokens()), Arc::new(NoLander)).await?;
    let addr = server.local_addr();
    let mut first = welcomed(addr, DEMO, TOKEN_A1, "a1").await?;
    let mut second = welcomed(addr, "/tenant/other/repo/demo", TOKEN_A2, "a2").await?;
    for socket in [&mut first, &mut second] {
        send(socket, &claim(1, "src/a.rs")).await?;
        let reply = expect(socket).await?;
        assert!(
            granted(&reply).is_some(),
            "the same scope in two repos does not conflict: {reply:?}"
        );
    }
    let acme = events_of(dir.path(), "acme", "demo")?;
    let other = events_of(dir.path(), "other", "demo")?;
    assert_eq!(acme.len(), 2, "{acme:?}");
    assert_eq!(other.len(), 2, "{other:?}");
    let connected = |events: &[EventKind]| {
        events
            .iter()
            .filter_map(|kind| {
                if let EventKind::AgentConnected { agent } = kind {
                    return Some(agent.0.clone());
                }
                None
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        connected(&acme),
        ["a1"],
        "each log holds only its own repository's events"
    );
    assert_eq!(connected(&other), ["a2"]);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_path_that_names_nothing_is_404_a_wrong_method_405_a_stranger_403() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let addr = server.local_addr();
    assert_eq!(
        http(addr, "GET", "/repo/demo/ws", Some(TOKEN_A1))
            .await?
            .status,
        404
    );
    assert_eq!(
        http(
            addr,
            "GET",
            "/tenant/acme/repo/a%2Fb/summary",
            Some(TOKEN_A1)
        )
        .await?
        .status,
        404
    );
    let wrong = http(addr, "POST", &format!("{DEMO}/summary"), Some(TOKEN_A1)).await?;
    assert_eq!(wrong.status, 405);
    assert!(
        wrong.head.to_lowercase().contains("allow: get"),
        "{}",
        wrong.head
    );
    let wrong = http(addr, "GET", &format!("{DEMO}/trunk-moved"), Some(TOKEN_A1)).await?;
    assert_eq!(wrong.status, 405);
    assert!(
        wrong.head.to_lowercase().contains("allow: post"),
        "{}",
        wrong.head
    );
    let unserved = http(
        addr,
        "GET",
        "/tenant/acme/repo/elsewhere/summary",
        Some(TOKEN_A1),
    )
    .await?;
    assert_eq!(unserved.status, 403);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_summary_counts_the_log_and_a_poke_is_dropped_until_the_repo_has_state() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let addr = server.local_addr();
    let poke = format!("{DEMO}/trunk-moved");
    assert_eq!(
        http(addr, "POST", &poke, Some(TOKEN_LANDER)).await?.status,
        204
    );
    let mut socket = welcomed(addr, DEMO, TOKEN_A1, "a1").await?;
    send(&mut socket, &claim(1, "src/a.rs")).await?;
    expect(&mut socket).await?;
    assert_eq!(
        http(addr, "POST", &poke, Some(TOKEN_LANDER)).await?.status,
        202
    );
    let summary = http(addr, "GET", &format!("{DEMO}/summary"), Some(TOKEN_A1)).await?;
    assert_eq!(summary.status, 200);
    assert!(summary
        .head
        .to_lowercase()
        .contains("content-type: application/json"));
    let body: serde_json::Value = serde_json::from_str(&summary.body)?;
    assert_eq!(body["summary"]["claims_granted"], 1, "{body}");
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_watcher_is_replayed_the_stored_log_then_follows_live() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let addr = server.local_addr();
    let mut agent = welcomed(addr, DEMO, TOKEN_A1, "a1").await?;
    send(&mut agent, &claim(1, "src/a.rs")).await?;
    expect(&mut agent).await?;
    let mut watcher = connect(addr, DEMO, Some(TOKEN_A2), &[]).await?;
    send(&mut watcher, &ClientMsg::Watch { from_seq: 0 }).await?;
    send(&mut watcher, &hello("a2")).await?;
    let mut seqs = Vec::new();
    loop {
        let msg = expect(&mut watcher).await?;
        if let ServerMsg::Event { event } = &msg {
            seqs.push(event.seq);
        } else if matches!(msg, ServerMsg::Welcome { .. }) {
            break;
        } else {
            return Err(format!("unexpected {msg:?}").into());
        }
    }
    assert_eq!(
        seqs,
        [0, 1],
        "the replay is the stored log, in order, before the welcome"
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_restart_over_the_same_directory_keeps_the_claims() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let mut first = welcomed(server.local_addr(), DEMO, TOKEN_A1, "a1").await?;
    send(&mut first, &claim(1, "src/a.rs")).await?;
    expect(&mut first).await?;
    server.stop().await;
    let server = start(dir.path()).await?;
    let mut second = welcomed(server.local_addr(), DEMO, TOKEN_A2, "a2").await?;
    send(&mut second, &claim(1, "src/a.rs")).await?;
    let reply = expect(&mut second).await?;
    assert!(
        matches!(reply, ServerMsg::Denied { .. }),
        "the claim survived the restart: {reply:?}"
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_second_server_on_the_same_directory_is_refused_at_start() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let second = start(dir.path()).await;
    let Err(StartError::Store(StoreError::Locked { .. })) = &second else {
        return Err(format!("expected a lock refusal, got {:?}", second.err()).into());
    };
    let message = second.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        message.contains(&std::process::id().to_string()),
        "{message}"
    );
    server.stop().await;
    start(dir.path()).await?.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_same_repository_listed_twice_is_refused_at_start() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repos = [("acme", "demo"), ("acme", "demo")];
    let result = start_with(dir.path(), &repos, None, Arc::new(NoLander)).await;
    assert!(matches!(result, Err(StartError::Duplicate { .. })));
    Ok(())
}

#[tokio::test]
async fn with_no_lander_a_submission_is_refused_and_never_reported_landed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let mut socket = welcomed(server.local_addr(), DEMO, TOKEN_A1, "a1").await?;
    send(&mut socket, &claim(1, "src/a.rs")).await?;
    let (claim_id, fence) = granted(&expect(&mut socket).await?).ok_or("no grant")?;
    send(&mut socket, &submit(claim_id, fence)).await?;
    let mut saw_accepted = false;
    loop {
        let msg = expect(&mut socket).await?;
        if matches!(msg, ServerMsg::Accepted { .. }) {
            saw_accepted = true;
        } else if let ServerMsg::SubmitRejected { claim, .. } = &msg {
            assert_eq!(*claim, claim_id);
            break;
        } else {
            return Err(format!("expected a refusal, never a landing: {msg:?}").into());
        }
    }
    assert!(saw_accepted);
    server.stop().await;
    Ok(())
}

/// Answers the calls the way the CLI's fake does: a head read with a fixed head, a merge landed.
struct Landing;

impl CallPort for Landing {
    fn answer(&self, call: Call) -> BoxFuture<CallAnswer> {
        let head = CommitId("cc".repeat(20));
        Box::pin(async move {
            match call.request {
                Request::Prepare { .. } => CallAnswer::Prepared {
                    base: CommitId("bb".repeat(20)),
                    head,
                },
                Request::ReadHead { .. } => CallAnswer::Head {
                    main: Some(head),
                    landed: Vec::new(),
                },
                Request::Test { .. }
                | Request::Land { .. }
                | Request::Trial { .. }
                | Request::Reconcile { .. } => CallAnswer::Unknown,
            }
        })
    }
}

#[tokio::test]
async fn a_call_port_that_lands_makes_the_submitter_hear_merged() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start_with(
        dir.path(),
        &[("acme", "demo")],
        Some(tokens()),
        Arc::new(Landing),
    )
    .await?;
    let mut socket = welcomed(server.local_addr(), DEMO, TOKEN_A1, "a1").await?;
    send(&mut socket, &claim(1, "src/a.rs")).await?;
    let (claim_id, fence) = granted(&expect(&mut socket).await?).ok_or("no grant")?;
    send(&mut socket, &submit(claim_id, fence)).await?;
    loop {
        let msg = expect(&mut socket).await?;
        if let ServerMsg::Merged { claim, .. } = &msg {
            assert_eq!(*claim, claim_id);
            break;
        } else if !matches!(msg, ServerMsg::Accepted { .. }) {
            return Err(format!("unexpected {msg:?}").into());
        }
    }
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_binary_frame_is_answered_and_the_socket_stays_open() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let mut socket = welcomed(server.local_addr(), DEMO, TOKEN_A1, "a1").await?;
    socket.send(Message::binary(vec![1, 2, 3])).await?;
    let reply = expect(&mut socket).await?;
    assert!(matches!(reply, ServerMsg::Error { .. }), "{reply:?}");
    send(&mut socket, &claim(1, "src/a.rs")).await?;
    assert!(granted(&expect(&mut socket).await?).is_some());
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_protocol_range_the_host_announces_reaches_the_client() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start(dir.path()).await?;
    let mut socket = connect(server.local_addr(), DEMO, Some(TOKEN_A1), &[]).await?;
    send(&mut socket, &hello("a1")).await?;
    let ServerMsg::Welcome { protocol_range, .. } = expect(&mut socket).await? else {
        return Err("no welcome".into());
    };
    assert_eq!(protocol_range, Some(ProtocolRange::current()));
    server.stop().await;
    Ok(())
}

const SENSITIVE_PATH: &str = "src/auth/token.rs";

/// A server for acme/demo whose configuration lists `reviewers` and holds `src/auth/`.
async fn start_reviewed(dir: &Path, reviewers: &[&str]) -> Result<RunningServer, StartError> {
    let mut config = config(dir, &[("acme", "demo")]);
    config.repositories[0].reviewers = reviewers.iter().map(|name| (*name).to_string()).collect();
    config.repositories[0].sensitive = vec!["src/auth/".to_string()];
    let deps = Deps {
        verifier: Some(tokens()),
        calls: Arc::new(NoLander),
    };
    server::start(&config, deps).await
}

/// a1 claims the sensitive path and submits with test evidence; the core holds it for review.
async fn held_for_review(
    addr: SocketAddr,
) -> Result<(Socket, ClaimId), Box<dyn std::error::Error>> {
    let mut submitter = welcomed(addr, DEMO, TOKEN_A1, "a1").await?;
    send(&mut submitter, &claim(1, SENSITIVE_PATH)).await?;
    let granted = expect(&mut submitter).await?;
    let (claim_id, fence) = granted_claim(&granted)?;
    let mut held = submit(claim_id, fence);
    if let ClientMsg::Submit { touched, .. } = &mut held {
        *touched = vec![edit(SENSITIVE_PATH)];
    }
    send(&mut submitter, &held).await?;
    let reply = expect(&mut submitter).await?;
    assert!(
        matches!(reply, ServerMsg::ReviewRequired { claim, .. } if claim == claim_id),
        "{reply:?}"
    );
    Ok((submitter, claim_id))
}

fn granted_claim(msg: &ServerMsg) -> Result<(ClaimId, Fence), Box<dyn std::error::Error>> {
    granted(msg).ok_or_else(|| format!("expected a grant, got {msg:?}").into())
}

fn review(claim: ClaimId, approve: bool) -> ClientMsg {
    ClientMsg::Review {
        req: RequestId(9),
        claim,
        approve,
        note: None,
    }
}

#[tokio::test]
async fn a_configured_reviewer_approves_a_held_submission_through_the_server() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start_reviewed(dir.path(), &["a2"]).await?;
    let addr = server.local_addr();
    let (mut submitter, claim_id) = held_for_review(addr).await?;
    let mut reviewer = welcomed(addr, DEMO, TOKEN_A2, "a2").await?;
    send(&mut reviewer, &review(claim_id, true)).await?;
    let accepted = expect(&mut submitter).await?;
    assert!(
        matches!(accepted, ServerMsg::Accepted { claim, .. } if claim == claim_id),
        "{accepted:?}"
    );
    let mut decided = Vec::new();
    for kind in events_of(dir.path(), "acme", "demo")? {
        if let EventKind::ReviewDecided {
            claim,
            approve,
            reviewer,
            ..
        } = kind
        {
            decided.push((claim, approve, reviewer));
        }
    }
    assert_eq!(decided, [(claim_id, true, Some(AgentId("a2".to_string())))]);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_name_not_in_the_configured_reviewers_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server = start_reviewed(dir.path(), &["a2"]).await?;
    let addr = server.local_addr();
    let (_submitter, claim_id) = held_for_review(addr).await?;
    let mut outsider = welcomed(addr, DEMO, TOKEN_A3, "a3").await?;
    send(&mut outsider, &review(claim_id, true)).await?;
    let refusal = expect(&mut outsider).await?;
    let ServerMsg::Error { message, .. } = &refusal else {
        return Err(format!("expected a refusal, got {refusal:?}").into());
    };
    assert!(message.contains("configured reviewer"), "{message}");
    let decided = events_of(dir.path(), "acme", "demo")?
        .iter()
        .filter(|kind| matches!(kind, EventKind::ReviewDecided { .. }))
        .count();
    assert_eq!(decided, 0, "the submission is still held");
    server.stop().await;
    Ok(())
}

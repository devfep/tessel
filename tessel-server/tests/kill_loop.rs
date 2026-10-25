//! The kill -9 loop: the real `tessel-server` binary runs on one store while scripted agents
//! claim, amend, release and heartbeat and a watcher reads the log. The server is killed with
//! SIGKILL at a random moment and started again on the same store, over and over. After every
//! kill, with the server dead, every message the clients received is checked against what the
//! store holds (invariant I1: each grant and event a client received is in the stored state and
//! log), and no fence, claim id or sequence number was issued twice (I9), and the stored state
//! round-trips (I5). The checkers are the simulator's: `tessel_coordinator::sim::check`.
//!
//! The test is `#[ignore]`: it kills a process many times, so it runs only in the CI job that
//! names it (`cargo test -p tessel-server --test kill_loop -- --ignored`), never in `cargo test`.
//! `TESSEL_KILL_LOOP_ITERATIONS` sets how many kills (default 25); two are enough for a smoke
//! run.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::host::{BatchId, SocketId};
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, Fence, Intent, Mode, OnConflict, RequestId, Scope,
    ScopeClaim, ServerMsg, PROTOCOL_VERSION,
};
use tessel_coordinator::shell::{event_key, STATE_KEY};
use tessel_coordinator::sim::check;
use tessel_coordinator::sim::driver::{Delivery, RequestKind, Sent};
use tessel_coordinator::sim::store::Store as SimStore;
use tessel_coordinator::sim::Rng;
use tessel_server::admin::{self, TokenArgs};
use tessel_server::config::IdentityConfig;
use tessel_server::id::RepoId;
use tessel_server::identity;
use tessel_server::keys;
use tessel_server::store::Store;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BINARY: &str = env!("CARGO_BIN_EXE_tessel-server");
const REPO: &str = "killloop";
const AGENTS: [&str; 3] = ["a1", "a2", "a3"];
const WATCHER: &str = "w1";
const PATHS: [&str; 5] = ["src/a.rs", "src/b.rs", "src/c.rs", "lib/d.rs", "lib/e.rs"];
const DEFAULT_ITERATIONS: u64 = 25;

/// What the clients saw, shared by every client task across every restart.
#[derive(Default)]
struct Traffic {
    transcript: Mutex<Vec<Delivery>>,
    sent: Mutex<Vec<Sent>>,
    next_req: AtomicU64,
    next_sock: AtomicU64,
    stopped: AtomicBool,
}

impl Traffic {
    fn record(&self, sock: u64, agent: &str, msg: ServerMsg) {
        let delivery = Delivery {
            sock: SocketId(sock),
            agent: AgentId(agent.to_string()),
            msg,
        };
        self.transcript
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(delivery);
    }

    fn sent(&self, sock: u64, req: RequestId, kind: RequestKind, claim: Option<ClaimId>) {
        let sent = Sent {
            sock: SocketId(sock),
            req,
            kind,
            claim,
            releases_before: 0,
            accepted: false,
        };
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sent);
    }

    fn req(&self) -> RequestId {
        RequestId(self.next_req.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

/// A server process that is killed when the test ends, however it ends.
struct Server(Child);

impl Server {
    fn start(config: &Path, port: u16, home: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut child = Command::new(BINARY)
            .args(["serve", "--config"])
            .arg(config)
            .env_clear()
            .env("PATH", std::env::var("PATH")?)
            .env("HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let (lines, heard) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    return;
                }
            }
        });
        let mut seen = Vec::new();
        loop {
            let line = heard
                .recv_timeout(Duration::from_secs(30))
                .map_err(|_| format!("the server never listened on {port}; it said {seen:?}"))?;
            if line.contains("listening addr=") {
                break;
            }
            seen.push(line);
        }
        // Keeps reading so the server never blocks on a full pipe.
        std::thread::spawn(move || for _ in heard {});
        Ok(Self(child))
    }

    /// SIGKILL: the process gets no chance to clean up.
    fn kill9(mut self) -> TestResult {
        self.0.kill()?;
        self.0.wait()?;
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> Result<u16, Box<dyn std::error::Error>> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn scope(rng: &mut Rng) -> ScopeClaim {
    let modes = [
        Mode::Depend,
        Mode::EditBody,
        Mode::EditSignature,
        Mode::Create,
    ];
    ScopeClaim {
        scope: Scope::File {
            path: PATHS[rng.below(PATHS.len())].to_string(),
        },
        mode: modes[rng.below(modes.len())],
    }
}

/// Connects to the server (as often as it takes), says hello and acts at random until the socket
/// ends, then connects again, until the test stops it.
async fn agent(port: u16, name: &'static str, token: String, traffic: Arc<Traffic>, seed: u64) {
    let mut rng = Rng::new(seed);
    while !traffic.stopped.load(Ordering::SeqCst) {
        let Some(mut socket) = connect(port, &token).await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let sock = traffic.next_sock.fetch_add(1, Ordering::SeqCst);
        let mut held: BTreeMap<u64, Fence> = BTreeMap::new();
        let hello = ClientMsg::Hello {
            agent: AgentId(name.to_string()),
            base: CommitId("ab".repeat(20)),
            protocol: PROTOCOL_VERSION,
        };
        if send(&mut socket, &hello).await.is_err() {
            continue;
        }
        loop {
            let pause = Duration::from_millis(5 + rng.below(35) as u64);
            tokio::select! {
                frame = socket.next() => {
                    let Some(Ok(Message::Text(text))) = frame else {
                        if matches!(frame, Some(Ok(_))) { continue }
                        break;
                    };
                    if let Ok(msg) = serde_json::from_str::<ServerMsg>(&text) {
                        learn(&mut held, &msg);
                        traffic.record(sock, name, msg);
                    }
                }
                () = tokio::time::sleep(pause) => {
                    let Some(msg) = act(&mut rng, &mut held, &traffic, sock) else { continue };
                    if send(&mut socket, &msg).await.is_err() {
                        break;
                    }
                }
            }
            if traffic.stopped.load(Ordering::SeqCst) {
                return;
            }
        }
    }
}

fn learn(held: &mut BTreeMap<u64, Fence>, msg: &ServerMsg) {
    if let ServerMsg::Granted { claim, fence, .. } = msg {
        held.insert(claim.0, *fence);
    }
    if let ServerMsg::LeaseExpired { claim, .. } = msg {
        held.remove(&claim.0);
    }
}

/// One random request: a claim, an amend, a release or a heartbeat.
fn act(
    rng: &mut Rng,
    held: &mut BTreeMap<u64, Fence>,
    traffic: &Traffic,
    sock: u64,
) -> Option<ClientMsg> {
    let pick = |rng: &mut Rng, held: &BTreeMap<u64, Fence>| {
        let index = rng.below(held.len());
        held.iter()
            .nth(index)
            .map(|(claim, fence)| (ClaimId(*claim), *fence))
    };
    match rng.below(10) {
        0..=3 => {
            let req = traffic.req();
            traffic.sent(sock, req, RequestKind::Claim, None);
            Some(ClientMsg::Claim {
                req,
                intent: Intent {
                    summary: "kill loop".to_string(),
                    task_ref: None,
                    assumptions: Vec::new(),
                },
                scopes: vec![scope(rng)],
                on_conflict: OnConflict::Fail,
            })
        }
        4..=5 => {
            let (claim, fence) = pick(rng, held)?;
            let req = traffic.req();
            traffic.sent(sock, req, RequestKind::Amend, Some(claim));
            Some(ClientMsg::Amend {
                req,
                claim,
                fence,
                add: vec![scope(rng)],
            })
        }
        6..=7 => {
            let (claim, fence) = pick(rng, held)?;
            held.remove(&claim.0);
            Some(ClientMsg::Release {
                claim,
                fence,
                req: Some(traffic.req()),
            })
        }
        _ => Some(ClientMsg::Heartbeat),
    }
}

/// Connects the watcher the same way and records the events it is sent.
async fn watcher(port: u16, token: String, traffic: Arc<Traffic>) {
    while !traffic.stopped.load(Ordering::SeqCst) {
        let Some(mut socket) = connect(port, &token).await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let sock = traffic.next_sock.fetch_add(1, Ordering::SeqCst);
        if send(&mut socket, &ClientMsg::Watch { from_seq: 0 })
            .await
            .is_err()
        {
            continue;
        }
        while let Some(Ok(frame)) = socket.next().await {
            let Message::Text(text) = frame else { continue };
            if let Ok(msg) = serde_json::from_str::<ServerMsg>(&text) {
                traffic.record(sock, WATCHER, msg);
            }
            if traffic.stopped.load(Ordering::SeqCst) {
                return;
            }
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(port: u16, token: &str) -> Option<Socket> {
    let mut request = format!("ws://127.0.0.1:{port}/tenant/acme/repo/{REPO}/ws")
        .into_client_request()
        .ok()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().ok()?);
    let connected = tokio::time::timeout(
        Duration::from_secs(2),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .ok()?;
    connected.ok().map(|(socket, _)| socket)
}

async fn send(socket: &mut Socket, msg: &ClientMsg) -> Result<(), ()> {
    let text = serde_json::to_string(msg).map_err(|_| ())?;
    socket.send(Message::text(text)).await.map_err(|_| ())
}

/// The stored state and log, as a simulator store the checkers can read.
fn stored(file: &Path) -> Result<SimStore, Box<dyn std::error::Error>> {
    let store = Store::open_read_only(file)?;
    let mut entries = Vec::new();
    if let Some(state) = store.load()? {
        entries.push((STATE_KEY.to_string(), state));
    }
    for event in store.read_events(0, usize::MAX >> 1)? {
        entries.push((event_key(event.seq), serde_json::to_string(&event)?));
    }
    let mut sim = SimStore::default();
    sim.stage(BatchId(0), entries);
    assert!(sim.apply(BatchId(0)));
    Ok(sim)
}

/// Every violation of I1, I5 and I9 the transcript has against the store.
fn violations(traffic: &Traffic, store: &SimStore) -> Vec<String> {
    let transcript = traffic
        .transcript
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let sent = traffic.sent.lock().unwrap_or_else(PoisonError::into_inner);
    let mut found = check::i1(&transcript, store);
    found.extend(check::i5(store));
    found.extend(check::i9(&transcript, &sent, &|_| false));
    found
}

fn iterations() -> u64 {
    std::env::var("TESSEL_KILL_LOOP_ITERATIONS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(DEFAULT_ITERATIONS)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "kills a server many times: runs in the CI job that names it"]
async fn a_server_killed_again_and_again_keeps_every_grant_event_and_fence() -> TestResult {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    std::fs::create_dir_all(&home)?;
    let identity = IdentityConfig {
        keys: root.path().join("keys"),
        audience: "kill-loop".to_string(),
    };
    keys::generate(&identity.keys, "k1")?;
    let token = |agent: &str| -> Result<String, Box<dyn std::error::Error>> {
        let args = TokenArgs {
            kid: "k1",
            tenant: "acme",
            repo: REPO,
            agent,
            ttl_ms: 3_600_000,
            service: false,
        };
        Ok(admin::token(&identity, &args, identity::now_ms())?)
    };
    let port = free_port()?;
    let config = root.path().join("tessel-server.toml");
    let stores = root.path().join("stores");
    std::fs::write(
        &config,
        format!(
            "listen = \"127.0.0.1:{port}\"\nstore_dir = {stores:?}\n\n[[repository]]\n\
             tenant = \"acme\"\nname = {REPO:?}\nrun = \"killloop\"\n\n\
             [identity]\nkeys = {keys:?}\naudience = \"kill-loop\"\n",
            stores = stores.display().to_string(),
            keys = identity.keys.display().to_string(),
        ),
    )?;
    let file = RepoId::new("acme", REPO)?.store_file(&stores);
    let traffic = Arc::new(Traffic::default());
    let mut tasks = Vec::new();
    for (index, name) in AGENTS.iter().enumerate() {
        let (token, traffic) = (token(name)?, Arc::clone(&traffic));
        tasks.push(tokio::spawn(agent(
            port,
            name,
            token,
            traffic,
            index as u64 + 1,
        )));
    }
    tasks.push(tokio::spawn(watcher(
        port,
        token(WATCHER)?,
        Arc::clone(&traffic),
    )));
    let mut rng = Rng::new(7);
    let rounds = iterations();
    for round in 0..rounds {
        let server = Server::start(&config, port, &home)?;
        tokio::time::sleep(Duration::from_millis(400 + rng.below(900) as u64)).await;
        server.kill9()?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        let found = violations(&traffic, &stored(&file)?);
        assert!(found.is_empty(), "after kill {round}: {found:#?}");
    }
    traffic.stopped.store(true, Ordering::SeqCst);
    for task in tasks {
        task.abort();
    }
    let transcript = traffic
        .transcript
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let grants = transcript
        .iter()
        .filter(|d| matches!(d.msg, ServerMsg::Granted { .. }))
        .count();
    let events = transcript
        .iter()
        .filter(|d| matches!(d.msg, ServerMsg::Event { .. }))
        .count();
    assert!(
        grants as u64 >= rounds && events as u64 >= rounds,
        "the traffic was too thin to prove anything: {grants} grants and {events} events in {rounds} kills"
    );
    report(rounds, grants, events);
    Ok(())
}

#[expect(
    clippy::print_stdout,
    reason = "the CI log shows how much traffic the kills were checked against"
)]
fn report(rounds: u64, grants: usize, events: usize) {
    println!("{rounds} kills, {grants} grants, {events} events checked");
}

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

use std::collections::{BTreeMap, BTreeSet};
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
    AgentId, ClaimId, ClientMsg, CommitId, ErrorCode, Event, EventKind, Fence, Intent, Mode,
    OnConflict, ReleaseReason, RequestId, Scope, ScopeClaim, ServerMsg, PROTOCOL_VERSION,
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
    /// Claims and amends sent and not yet answered: a kill that finds some in flight fell inside
    /// the window between a request and its (persisted) answer.
    outstanding: Mutex<BTreeSet<u64>>,
    /// Things the watcher found wrong as it read, such as a gap after a resume.
    faults: Mutex<Vec<String>>,
}

impl Traffic {
    fn record(&self, sock: u64, agent: &str, msg: ServerMsg) {
        if let Some(req) = answered_request(&msg) {
            self.outstanding
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&req.0);
        }
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

    /// A new request id for a claim or an amend, recorded as sent and as unanswered.
    fn send_tracked(&self, sock: u64, kind: RequestKind, claim: Option<ClaimId>) -> RequestId {
        let req = self.req();
        self.sent(sock, req, kind, claim);
        self.outstanding
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(req.0);
        req
    }

    fn fault(&self, what: String) {
        self.faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(what);
    }

    fn in_flight(&self) -> usize {
        self.outstanding
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
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

/// The request a server message answers, for the replies a claim or an amend gets.
fn answered_request(msg: &ServerMsg) -> Option<RequestId> {
    if let ServerMsg::Granted { req, .. }
    | ServerMsg::Denied { req, .. }
    | ServerMsg::Queued { req, .. }
    | ServerMsg::Shadowed { req, .. } = msg
    {
        return Some(*req);
    }
    if let ServerMsg::Error { req, .. } = msg {
        return *req;
    }
    None
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

/// What an agent knows, kept across its reconnects: the claims it holds with the fence it was last
/// granted, the fences an amend retired, and the claims it has probed with a retired fence.
#[derive(Default)]
struct Held {
    fences: BTreeMap<u64, Fence>,
    retired: Vec<(ClaimId, Fence)>,
    probed: BTreeSet<u64>,
}

impl Held {
    fn learn(&mut self, msg: &ServerMsg) {
        if let ServerMsg::Granted { claim, fence, .. } = msg {
            if let Some(old) = self.fences.insert(claim.0, *fence) {
                self.retired.push((*claim, old));
            }
        }
        if let ServerMsg::LeaseExpired { claim, .. } = msg {
            self.fences.remove(&claim.0);
        }
    }

    /// A claim to amend or release: never one already probed, whose release would then be
    /// ambiguous.
    fn pick(&self, rng: &mut Rng) -> Option<(ClaimId, Fence)> {
        let open: Vec<_> = self
            .fences
            .iter()
            .filter(|(claim, _)| !self.probed.contains(claim))
            .collect();
        let (claim, fence) = open.get(rng.below(open.len()))?;
        Some((ClaimId(**claim), **fence))
    }

    /// A release that presents a fence an amend retired on a claim still held, once per claim.
    fn probe(&mut self) -> Option<(ClaimId, Fence)> {
        let found = self
            .retired
            .iter()
            .find(|(claim, _)| {
                self.fences.contains_key(&claim.0) && !self.probed.contains(&claim.0)
            })
            .copied()?;
        self.probed.insert(found.0 .0);
        Some(found)
    }
}

/// Connects to the server (as often as it takes), says hello and acts at random until the socket
/// ends, then connects again, until the test stops it. What the agent holds survives the
/// reconnect, so after a restart it presents fences the killed server issued; after each hello it
/// also sends one stale release.
async fn agent(port: u16, name: &'static str, token: String, traffic: Arc<Traffic>, seed: u64) {
    let mut rng = Rng::new(seed);
    let mut held = Held::default();
    while !traffic.stopped.load(Ordering::SeqCst) {
        let Some(mut socket) = connect(port, &token).await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let sock = traffic.next_sock.fetch_add(1, Ordering::SeqCst);
        let hello = ClientMsg::Hello {
            agent: AgentId(name.to_string()),
            base: CommitId("ab".repeat(20)),
            protocol: PROTOCOL_VERSION,
        };
        if send(&mut socket, &hello).await.is_err() {
            continue;
        }
        let mut probe = true;
        loop {
            let pause = Duration::from_millis(5 + rng.below(35) as u64);
            tokio::select! {
                frame = socket.next() => {
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(msg) = serde_json::from_str::<ServerMsg>(&text) {
                                held.learn(&msg);
                                traffic.record(sock, name, msg);
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => break,
                    }
                }
                () = tokio::time::sleep(pause) => {
                    let msg = if probe {
                        probe = false;
                        stale_release(&mut held, &traffic, sock)
                    } else {
                        None
                    };
                    let Some(msg) = msg.or_else(|| act(&mut rng, &mut held, &traffic, sock)) else {
                        continue;
                    };
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

fn stale_release(held: &mut Held, traffic: &Traffic, sock: u64) -> Option<ClientMsg> {
    let (claim, fence) = held.probe()?;
    let req = traffic.req();
    traffic.sent(sock, req, RequestKind::StaleRelease, Some(claim));
    Some(ClientMsg::Release {
        claim,
        fence,
        req: Some(req),
    })
}

/// One random request: a claim, an amend, a release or a heartbeat.
fn act(rng: &mut Rng, held: &mut Held, traffic: &Traffic, sock: u64) -> Option<ClientMsg> {
    match rng.below(10) {
        0..=3 => {
            let req = traffic.send_tracked(sock, RequestKind::Claim, None);
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
            let (claim, fence) = held.pick(rng)?;
            let req = traffic.send_tracked(sock, RequestKind::Amend, Some(claim));
            Some(ClientMsg::Amend {
                req,
                claim,
                fence,
                add: vec![scope(rng)],
            })
        }
        6..=7 => {
            let (claim, fence) = held.pick(rng)?;
            held.fences.remove(&claim.0);
            Some(ClientMsg::Release {
                claim,
                fence,
                req: Some(traffic.req()),
            })
        }
        _ => Some(ClientMsg::Heartbeat),
    }
}

/// Reads the log through a watch socket and records what it is sent. After a restart it watches
/// from the next sequence number it has not seen, so the events it gets must continue the log
/// exactly: a gap or a repeat is a fault.
async fn watcher(port: u16, token: String, traffic: Arc<Traffic>) {
    let mut next = 0_u64;
    while !traffic.stopped.load(Ordering::SeqCst) {
        let Some(mut socket) = connect(port, &token).await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let sock = traffic.next_sock.fetch_add(1, Ordering::SeqCst);
        if send(&mut socket, &ClientMsg::Watch { from_seq: next })
            .await
            .is_err()
        {
            continue;
        }
        while let Some(Ok(frame)) = socket.next().await {
            let Message::Text(text) = frame else { continue };
            let Ok(msg) = serde_json::from_str::<ServerMsg>(&text) else {
                continue;
            };
            if let ServerMsg::Event { event } = &msg {
                if event.seq != next {
                    traffic.fault(format!(
                        "the watcher got seq {} where {next} was next",
                        event.seq
                    ));
                }
                next = event.seq + 1;
            }
            traffic.record(sock, WATCHER, msg);
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

/// The stored state and log, as a simulator store the checkers can read, and the events in it.
fn stored(file: &Path) -> Result<(SimStore, Vec<Event>), Box<dyn std::error::Error>> {
    let store = Store::open_read_only(file)?;
    let mut entries = Vec::new();
    if let Some(state) = store.load()? {
        entries.push((STATE_KEY.to_string(), state));
    }
    let events = store.read_events(0, usize::MAX >> 1)?;
    for event in &events {
        entries.push((event_key(event.seq), serde_json::to_string(event)?));
    }
    let mut sim = SimStore::default();
    sim.stage(BatchId(0), entries);
    assert!(sim.apply(BatchId(0)));
    Ok((sim, events))
}

/// Every violation of I1, I5 and I9 the transcript has against the store, and the watcher's own
/// faults. A stale release counts as accepted when the log holds an agent's release of its claim:
/// the agent never releases a probed claim itself.
fn violations(traffic: &Traffic, store: &SimStore, events: &[Event]) -> Vec<String> {
    let transcript = traffic
        .transcript
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let mut sent = traffic
        .sent
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    for probe in sent
        .iter_mut()
        .filter(|s| s.kind == RequestKind::StaleRelease)
    {
        probe.accepted = probe.claim.is_some_and(|claim| {
            events.iter().any(|event| {
                if let EventKind::ClaimReleased {
                    claim: released,
                    reason,
                } = &event.kind
                {
                    return *released == claim && *reason == ReleaseReason::Agent;
                }
                false
            })
        });
    }
    let mut found = check::i1(&transcript, store);
    found.extend(check::i5(store));
    found.extend(check::i9(&transcript, &sent, &|_| false));
    found.extend(
        traffic
            .faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned(),
    );
    found
}

fn iterations() -> u64 {
    std::env::var("TESSEL_KILL_LOOP_ITERATIONS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(DEFAULT_ITERATIONS)
}

fn grants(traffic: &Traffic) -> usize {
    let transcript = traffic
        .transcript
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let mut count = 0;
    for delivery in transcript.iter() {
        if let ServerMsg::Granted { .. } = delivery.msg {
            count += 1;
        }
    }
    count
}

fn refusals(traffic: &Traffic) -> usize {
    let probes: BTreeSet<u64> = traffic
        .sent
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|s| s.kind == RequestKind::StaleRelease)
        .map(|s| s.req.0)
        .collect();
    let transcript = traffic
        .transcript
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let mut count = 0;
    for delivery in transcript.iter() {
        if let ServerMsg::Error {
            req: Some(req),
            code,
            ..
        } = &delivery.msg
        {
            let refused = *code == ErrorCode::StaleFence || *code == ErrorCode::UnknownClaim;
            count += usize::from(refused && probes.contains(&req.0));
        }
    }
    count
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
    let (mut mid_request, mut last_grants, mut last_events) = (0_u64, 0_usize, 0_usize);
    for round in 0..rounds {
        let server = Server::start(&config, port, &home)?;
        tokio::time::sleep(Duration::from_millis(400 + rng.below(900) as u64)).await;
        mid_request += u64::from(traffic.in_flight() > 0);
        server.kill9()?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        let (store, events) = stored(&file)?;
        let found = violations(&traffic, &store, &events);
        assert!(found.is_empty(), "after kill {round}: {found:#?}");
        let (now_grants, now_events) = (grants(&traffic), events.len());
        assert!(
            now_grants > last_grants && now_events > last_events,
            "kill {round}: no new grant ({last_grants} to {now_grants}) or the log did not grow \
             ({last_events} to {now_events}), so this round checked nothing new"
        );
        (last_grants, last_events) = (now_grants, now_events);
    }
    traffic.stopped.store(true, Ordering::SeqCst);
    for task in tasks {
        task.abort();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (store, events) = stored(&file)?;
    let found = violations(&traffic, &store, &events);
    assert!(found.is_empty(), "after the last round: {found:#?}");
    report(
        rounds,
        mid_request,
        refusals(&traffic),
        last_grants,
        last_events,
    );
    Ok(())
}

#[expect(
    clippy::print_stdout,
    reason = "the CI log shows how much traffic the kills were checked against"
)]
fn report(rounds: u64, mid_request: u64, refused: usize, grants: usize, events: usize) {
    println!(
        "{rounds} kills, {mid_request} of them with a claim or amend unanswered; {grants} grants \
         received, {events} events stored, {refused} stale releases refused"
    );
}

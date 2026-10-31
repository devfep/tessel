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
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::host::{BatchId, SocketId};
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, ErrorCode, Event, EventKind, Fence, Intent, Mode,
    OnConflict, ReleaseReason, RequestId, RunId, Scope, ScopeClaim, ServerMsg, PROTOCOL_VERSION,
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
/// How many claims one agent holds at most; at the limit a claim becomes a release. Every denial
/// logs all the claims it conflicts with, so claims piling up on five files made the average
/// event grow from 0.5 KB to 5 KB in 25 rounds (denials reached 8 KB) and each round's check cost
/// more than the last, which made the whole run quadratic.
const MAX_HELD_PER_AGENT: usize = 6;
/// How often one claim is amended. Every amend adds a scope that the claim, its events and the
/// stored state then carry for good, so an amend without a limit makes each event cost more than
/// the last and the whole run quadratic: the average event grew from 0.5 KB to 5 KB in 25 rounds.
const MAX_AMENDS_PER_CLAIM: usize = 4;
/// How long the server may take to print that it listens, to die after SIGKILL, and how long the
/// checks of one round may take. A wait that passes its bound fails the test and names the round;
/// the check runs on a thread the test does not join, so a stuck check cannot stop the test from
/// ending.
const START_BOUND: Duration = Duration::from_secs(30);
const KILL_BOUND: Duration = Duration::from_secs(10);
const CHECK_BOUND: Duration = Duration::from_secs(120);
/// How long after its minimum run time a round may wait for the clients' first new grant. Clients
/// that reconnect slowly on a loaded runner would otherwise make the round check nothing.
const PROGRESS_BOUND: Duration = Duration::from_secs(20);

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

    fn clear_in_flight(&self) {
        self.outstanding
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
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
    fn start(
        config: &Path,
        port: u16,
        home: &Path,
        round: u64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let child = Command::new(BINARY)
            .args(["serve", "--config"])
            .arg(config)
            .env_clear()
            .env("PATH", std::env::var("PATH")?)
            .env("HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        // Owned by a `Server` from here on, so every error path below kills and reaps it.
        let mut server = Self(child);
        let stderr = server.0.stderr.take().ok_or("no stderr")?;
        let (lines, heard) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    return;
                }
            }
        });
        let mut seen = Vec::new();
        let deadline = Instant::now() + START_BOUND;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = heard.recv_timeout(left).map_err(|why| {
                format!(
                    "round {round}: waited {START_BOUND:?} for the server to print \"listening\" \
                     on {port} ({why}); it said {seen:?}"
                )
            })?;
            if line.contains("listening addr=") {
                break;
            }
            seen.push(line);
        }
        // Keeps reading so the server never blocks on a full pipe.
        std::thread::spawn(move || for _ in heard {});
        Ok(server)
    }

    /// SIGKILL: the process gets no chance to clean up.
    fn kill9(mut self, round: u64) -> TestResult {
        self.0.kill()?;
        reaped_within(&mut self.0, KILL_BOUND)
            .map_err(|why| format!("round {round}: killed server: {why}").into())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = reaped_within(&mut self.0, KILL_BOUND);
    }
}

/// Waits for `child` to exit, polling, and fails with the process id after `bound`.
fn reaped_within(child: &mut Child, bound: Duration) -> Result<(), String> {
    let deadline = Instant::now() + bound;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => return Err(format!("could not wait for pid {}: {error}", child.id())),
        }
        if Instant::now() >= deadline {
            return Err(format!("waited {bound:?} for pid {} to exit", child.id()));
        }
        std::thread::sleep(Duration::from_millis(10));
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
/// granted, the fences an amend retired, and the claims it has probed with a retired fence. A
/// probed claim is released once its probe was refused, so it stops counting toward the cap.
#[derive(Default)]
struct Held {
    fences: BTreeMap<u64, Fence>,
    retired: Vec<(ClaimId, Fence)>,
    probed: BTreeSet<u64>,
    /// The claim each stale release was sent for, by request.
    probe_reqs: BTreeMap<u64, u64>,
    /// The probed claims whose stale release the server refused.
    refused: BTreeSet<u64>,
    /// How many amends each claim has been sent, to stop at `MAX_AMENDS_PER_CLAIM`.
    amends: BTreeMap<u64, usize>,
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
        if let ServerMsg::Error {
            req: Some(req),
            code,
            ..
        } = msg
        {
            let refused = *code == ErrorCode::StaleFence || *code == ErrorCode::UnknownClaim;
            if let Some(claim) = self.probe_reqs.get(&req.0).filter(|_| refused) {
                self.refused.insert(*claim);
            }
        }
    }

    /// A claim to release: never one probed whose refusal has not arrived, whose release would
    /// then be ambiguous.
    fn pick(&self, rng: &mut Rng) -> Option<(ClaimId, Fence)> {
        let open: Vec<_> = self
            .fences
            .iter()
            .filter(|(claim, _)| !self.probed.contains(claim) || self.refused.contains(claim))
            .collect();
        let (claim, fence) = open.get(rng.below(open.len()))?;
        Some((ClaimId(**claim), **fence))
    }

    /// A claim to amend: one never probed that has not used up its amends.
    fn pick_amendable(&mut self, rng: &mut Rng) -> Option<(ClaimId, Fence)> {
        let open: Vec<_> = self
            .fences
            .iter()
            .filter(|(claim, _)| !self.probed.contains(claim))
            .filter(|(claim, _)| {
                self.amends.get(claim).copied().unwrap_or(0) < MAX_AMENDS_PER_CLAIM
            })
            .collect();
        let (claim, fence) = open.get(rng.below(open.len()))?;
        *self.amends.entry(**claim).or_default() += 1;
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
    held.probe_reqs.insert(req.0, claim.0);
    traffic.sent(sock, req, RequestKind::StaleRelease, Some(claim));
    Some(ClientMsg::Release {
        claim,
        fence,
        req: Some(req),
    })
}

/// One random request: a claim, an amend, a release or a heartbeat.
fn act(rng: &mut Rng, held: &mut Held, traffic: &Traffic, sock: u64) -> Option<ClientMsg> {
    let mut roll = rng.below(10);
    if roll < 4 && held.fences.len() >= MAX_HELD_PER_AGENT {
        roll = 6;
    }
    match roll {
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
            let (claim, fence) = held.pick_amendable(rng)?;
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
/// faults. A stale release counts as accepted when the log holds an agent's release of its claim
/// and the server sent no refusal for the probe: the agent releases a probed claim only after that
/// refusal arrived.
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
    let mut refused = BTreeSet::new();
    for delivery in transcript.iter() {
        if let ServerMsg::Error {
            req: Some(req),
            code,
            ..
        } = &delivery.msg
        {
            if *code == ErrorCode::StaleFence || *code == ErrorCode::UnknownClaim {
                refused.insert(req.0);
            }
        }
    }
    for probe in sent
        .iter_mut()
        .filter(|s| s.kind == RequestKind::StaleRelease)
    {
        probe.accepted = !refused.contains(&probe.req.0)
            && probe.claim.is_some_and(|claim| {
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

fn write_config(
    config: &Path,
    port: u16,
    stores: &Path,
    keys: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(
        config,
        format!(
            "listen = \"127.0.0.1:{port}\"\nstore_dir = {stores:?}\n\n[[repository]]\n\
             tenant = \"acme\"\nname = {REPO:?}\nrun = \"killloop\"\n\n\
             [identity]\nkeys = {keys:?}\naudience = \"kill-loop\"\n",
            stores = stores.display().to_string(),
            keys = keys.display().to_string(),
        ),
    )?;
    Ok(())
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
    write_config(&config, port, &stores, &identity.keys)?;
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
    let test_began = Instant::now();
    let (mut mid_request, mut last_grants, mut last_events) = (0_u64, 0_usize, 0_usize);
    for round in 0..rounds {
        let (round_began, before) = (Instant::now(), Counts::of(&traffic));
        let server = Server::start(&config, port, &home, round)?;
        let started_in = round_began.elapsed();
        tokio::time::sleep(Duration::from_millis(400 + rng.below(900) as u64)).await;
        let first_grant = wait_for_progress(&traffic, (last_grants, &before), round).await?;
        mid_request += u64::from(traffic.in_flight() > 0);
        server.kill9(round)?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        let check_began = Instant::now();
        let (found, now_events) = checked_in_time(&file, &traffic, &format!("round {round}"))?;
        let check_took = check_began.elapsed();
        assert!(found.is_empty(), "after kill {round}: {found:#?}");
        // A request sent to the server that was just killed will never be answered: it must not
        // count as in flight at the next kill.
        traffic.clear_in_flight();
        let now_grants = grants(&traffic);
        assert!(
            now_grants > last_grants && now_events > last_events,
            "kill {round}: no new grant ({last_grants} to {now_grants}) or the log did not grow \
             ({last_events} to {now_events}), so this round checked nothing new"
        );
        let times = RoundTimes {
            test: test_began.elapsed(),
            round: round_began.elapsed(),
            started: started_in,
            check: check_took,
            first_grant,
            new_events: now_events - last_events,
        };
        progress_line(round, rounds, &times, (now_grants, now_events));
        (last_grants, last_events) = (now_grants, now_events);
    }
    traffic.stopped.store(true, Ordering::SeqCst);
    for task in tasks {
        task.abort();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (found, _) = checked_in_time(&file, &traffic, "the final check")?;
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

/// Reads the store and checks the transcript against it on a thread, and gives up after
/// `CHECK_BOUND`, so a check that never returns fails the test and names `what`. A thread that
/// overran is left running: joining it would hang the test the way the check did. Gives the
/// violations and the number of events stored.
fn checked_in_time(
    file: &Path,
    traffic: &Arc<Traffic>,
    what: &str,
) -> Result<(Vec<String>, usize), Box<dyn std::error::Error>> {
    let entries = traffic
        .transcript
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .len();
    let (file, shared) = (file.to_path_buf(), Arc::clone(traffic));
    let found = run_within(CHECK_BOUND, move || {
        let (store, events) = stored(&file).map_err(|e| e.to_string())?;
        Ok::<_, String>((violations(&shared, &store, &events), events.len()))
    })
    .map_err(|why| {
        format!(
            "{what}: the stored state and log were not read and checked: {why} \
             ({entries} transcript entries when it began)"
        )
    })?;
    Ok(found.map_err(|e| format!("{what}: reading the store failed: {e}"))?)
}

/// Runs `work` on its own thread and waits `bound` for its result.
fn run_within<T: Send + 'static>(
    bound: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(work());
    });
    result
        .recv_timeout(bound)
        .map_err(|why| format!("waited {bound:?} ({why})"))
}

#[test]
fn a_check_that_never_returns_fails_within_its_bound() {
    let began = Instant::now();
    let outcome = run_within(Duration::from_millis(200), || {
        std::thread::sleep(Duration::from_secs(30));
    });
    let message = outcome.expect_err("the check overran its bound");
    assert!(message.contains("waited 200ms"), "{message}");
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "the bound did not fire in time"
    );
}

#[test]
fn a_process_that_will_not_exit_fails_within_its_bound() -> TestResult {
    let mut child = Command::new("sleep").arg("30").spawn()?;
    let began = Instant::now();
    let outcome = reaped_within(&mut child, Duration::from_millis(200));
    let pid = child.id();
    child.kill()?;
    child.wait()?;
    let message = outcome.expect_err("the process was still running");
    assert!(
        message.contains(&format!("waited 200ms for pid {pid}")),
        "{message}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "the bound did not fire in time"
    );
    Ok(())
}

/// What the clients had done at a moment: the sockets they opened and the requests they sent.
struct Counts {
    sockets: u64,
    requests: usize,
}

impl Counts {
    fn of(traffic: &Traffic) -> Self {
        Self {
            sockets: traffic.next_sock.load(Ordering::SeqCst),
            requests: traffic
                .sent
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
        }
    }

    /// What the clients did since `self`, for a failure message.
    fn since(&self, traffic: &Traffic) -> String {
        let now = Self::of(traffic);
        format!(
            "{} sockets opened since the server began to start (0: the clients never reconnected), \
             {} requests sent, {} unanswered (many: the server is not answering), {} grants in all",
            now.sockets - self.sockets,
            now.requests - self.requests,
            traffic.in_flight(),
            grants(traffic)
        )
    }
}

/// Waits for the round's first new grant and gives the time it took since the round began. With
/// no grant it says what the clients did since `since`, a snapshot from before the server started.
async fn wait_for_progress(
    traffic: &Traffic,
    (last_grants, since): (usize, &Counts),
    round: u64,
) -> Result<Duration, Box<dyn std::error::Error>> {
    let began = Instant::now();
    progress_within(traffic, last_grants, PROGRESS_BOUND)
        .await
        .map_err(|why| format!("round {round}: {why}; {}", since.since(traffic)))?;
    Ok(began.elapsed())
}

/// Waits until the clients have been granted something new since `last_grants`, or fails after
/// `bound` with what it waited for. A round the clients spent reconnecting checks nothing, so the
/// round waits for them instead of assuming they were quick.
async fn progress_within(
    traffic: &Traffic,
    last_grants: usize,
    bound: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + bound;
    while grants(traffic) <= last_grants {
        if Instant::now() >= deadline {
            return Err(format!(
                "waited {bound:?} for a grant beyond the {last_grants} already received"
            ));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

#[tokio::test]
async fn a_round_in_which_the_clients_get_nothing_fails_within_its_bound() {
    let traffic = Traffic::default();
    let began = Instant::now();
    let message = progress_within(&traffic, 0, Duration::from_millis(200))
        .await
        .expect_err("no client was granted anything");
    assert!(
        message.contains("waited 200ms for a grant beyond the 0"),
        "{message}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "the bound did not fire in time"
    );
}

#[tokio::test]
async fn a_grant_that_has_arrived_ends_the_wait_at_once() {
    let traffic = Traffic::default();
    let granted = ServerMsg::Granted {
        req: RequestId(1),
        claim: ClaimId(1),
        fence: Fence(1),
        expires_at_ms: 0,
        race: None,
        at_risk: Vec::new(),
        head: None,
    };
    traffic.record(1, "a1", granted);
    let began = Instant::now();
    progress_within(&traffic, 0, Duration::from_secs(120))
        .await
        .expect("a grant beyond the 0 already received");
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "the wait did not end at once"
    );
    progress_within(&traffic, 1, Duration::from_millis(100))
        .await
        .expect_err("that grant was already counted");
}

/// How long each part of a round took.
struct RoundTimes {
    test: Duration,
    round: Duration,
    started: Duration,
    check: Duration,
    /// How long after the minimum run the first new grant came.
    first_grant: Duration,
    /// Events the round stored: with the round time, the server's rate.
    new_events: usize,
}

fn progress_line(round: u64, rounds: u64, times: &RoundTimes, totals: (usize, usize)) {
    progress(&format!(
        "round {round}/{rounds}: elapsed {:?}, round {:?} (server up in {:?}, first grant {:?} \
         after the minimum run, {} events stored this round, checks {:?}), {} grants, {} events",
        times.test,
        times.round,
        times.started,
        times.first_grant,
        times.new_events,
        times.check,
        totals.0,
        totals.1
    ));
}

#[expect(
    clippy::print_stdout,
    reason = "the CI log shows each round, so a hung run says where it stopped"
)]
fn progress(line: &str) {
    println!("{line}");
}

#[expect(
    clippy::print_stdout,
    reason = "the CI log shows how much traffic the kills were checked against"
)]
fn report(rounds: u64, mid_request: u64, refused: usize, grants: usize, events: usize) {
    println!(
        "{rounds} kills, {mid_request} of them with a claim or amend that had no answer yet; \
         {grants} grants received, {events} events stored, {refused} stale releases refused"
    );
}

#[test]
fn an_agent_at_its_limit_releases_instead_of_claiming() {
    let (mut rng, traffic) = (Rng::new(5), Traffic::default());
    for _ in 0..200 {
        let mut held = Held::default();
        for claim in 0..MAX_HELD_PER_AGENT as u64 {
            held.fences.insert(claim, Fence(claim));
        }
        let msg = act(&mut rng, &mut held, &traffic, 1);
        assert!(
            !matches!(msg, Some(ClientMsg::Claim { .. })),
            "claimed at the limit of {MAX_HELD_PER_AGENT}"
        );
    }
}

#[test]
fn a_claim_is_amended_at_most_a_fixed_number_of_times() {
    let mut held = Held::default();
    held.fences.insert(1, Fence(1));
    let mut rng = Rng::new(3);
    let mut sent = 0;
    for _ in 0..1_000 {
        sent += usize::from(held.pick_amendable(&mut rng).is_some());
    }
    assert_eq!(sent, MAX_AMENDS_PER_CLAIM);
}

#[test]
fn an_agent_whose_claims_are_all_probed_and_refused_still_claims_or_releases() {
    let (mut rng, traffic) = (Rng::new(7), Traffic::default());
    let mut held = Held::default();
    for claim in 0..MAX_HELD_PER_AGENT as u64 {
        held.fences.insert(claim, Fence(claim + 100));
        held.retired.push((ClaimId(claim), Fence(claim)));
    }
    while let Some(ClientMsg::Release { req: Some(req), .. }) =
        stale_release(&mut held, &traffic, 1)
    {
        held.learn(&ServerMsg::Error {
            req: Some(req),
            code: ErrorCode::StaleFence,
            message: String::new(),
        });
    }
    assert_eq!(
        held.probed.len(),
        MAX_HELD_PER_AGENT,
        "every claim is probed"
    );
    let mut acted = 0;
    for _ in 0..1_000 {
        let msg = act(&mut rng, &mut held, &traffic, 1);
        acted += usize::from(matches!(
            msg,
            Some(ClientMsg::Claim { .. } | ClientMsg::Release { .. })
        ));
    }
    assert!(acted > 0, "an agent holding only refused probes stalled");
}

/// A claim release as the log holds it.
fn release_event(claim: ClaimId, reason: ReleaseReason) -> Event {
    Event {
        seq: 0,
        at_ms: 0,
        run: RunId("run".to_string()),
        kind: EventKind::ClaimReleased { claim, reason },
    }
}

/// A traffic record with one stale-release probe of claim 1 (request 9) that no answer is
/// recorded for, an empty store (so i1 and i5 report nothing) and a log holding the one `release`.
fn probed_then_released(release: Option<Event>) -> (Traffic, SimStore, Vec<Event>) {
    let traffic = Traffic::default();
    traffic.sent(1, RequestId(9), RequestKind::StaleRelease, Some(ClaimId(1)));
    (traffic, SimStore::default(), release.into_iter().collect())
}

#[test]
fn a_stale_release_with_no_refusal_and_an_agent_release_is_flagged_as_accepted() {
    let release = release_event(ClaimId(1), ReleaseReason::Agent);
    let (traffic, store, events) = probed_then_released(Some(release));
    let found = violations(&traffic, &store, &events);
    assert_eq!(
        found,
        vec!["retired fence accepted (request 9)".to_string()]
    );
}

#[test]
fn a_stale_release_the_server_refused_is_not_flagged() {
    for code in [ErrorCode::StaleFence, ErrorCode::UnknownClaim] {
        let release = release_event(ClaimId(1), ReleaseReason::Agent);
        let (traffic, store, events) = probed_then_released(Some(release));
        traffic.record(
            1,
            "a1",
            ServerMsg::Error {
                req: Some(RequestId(9)),
                code,
                message: String::new(),
            },
        );
        let found = violations(&traffic, &store, &events);
        assert!(found.is_empty(), "{code:?}: {found:?}");
    }
}

#[test]
fn a_stale_release_without_an_agent_release_of_its_claim_is_not_flagged() {
    let releases = [
        ("no release in the log", None),
        (
            "a lease expiry of the claim",
            Some(release_event(ClaimId(1), ReleaseReason::LeaseExpired)),
        ),
        (
            "an agent release of another claim",
            Some(release_event(ClaimId(2), ReleaseReason::Agent)),
        ),
    ];
    for (what, release) in releases {
        let (traffic, store, events) = probed_then_released(release);
        let found = violations(&traffic, &store, &events);
        assert!(found.is_empty(), "{what}: {found:?}");
    }
}

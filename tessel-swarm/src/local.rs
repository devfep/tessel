//! The local target: a real `tessel-server` started in this process, with its own lander and
//! runner, on a git remote and one bare fork per agent. The scripted agents reach it through
//! a small relay on 127.0.0.1 that can do what a network does to a socket (reset it, cut it with
//! close code 1011, refuse a new connection) and that counts heartbeats.
//!
//! The server reads one clock, which a test can freeze and advance (`freeze_clock`,
//! `advance_clock`, `settle_expiry`) so that a lease lapses only when the test says so. The agents
//! stay on real time. The configured lease (`LocalSetup::lease_ms`) is the server's own setting.
//!
//! The server lands each submission itself: it rebases the commit onto main, runs the repository's
//! `tessel.toml` test command (`node --test` here) on the rebased head and moves main by
//! compare-and-swap. The coordinator's event log, for checking what a Watch reader saw, is read
//! through a Watch socket of its own that bypasses the relay.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, ErrorCode, Event, Fence, RequestId, ServerMsg,
    PROTOCOL_VERSION,
};
use tessel_server::config::{Config, LanderConfig, RepositoryConfig};
use tessel_server::id::RepoId;
use tessel_server::repo::NoLander;
use tessel_server::server::{Deps, RunningServer};
use tessel_server::testing::{self, TestClock};
use tessel_server::ws::{Refusal, Verifier};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

use crate::demo::Tree;
use crate::endpoint::{Endpoint, Remote, Token};
use crate::git::{self, Git};
use crate::guard::ScratchRepo;

/// The lease the local coordinator grants.
pub const LEASE_MS: u64 = 30_000;

/// The tenant every local repository lives under.
const TENANT: &str = "swarm";

/// The agent name of the socket that reads the log.
const LOG_READER: &str = "swarm-log";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What happens to a relayed connection.
#[derive(Clone, Copy)]
enum Fate {
    /// Dropped with no close frame, as the network does.
    Reset,
    /// Closed from the coordinator's side with code 1011.
    Cut,
}

struct Conn {
    id: u64,
    agent: String,
    fate: mpsc::UnboundedSender<Fate>,
}

/// Resets every connection when an event satisfies `when` (see `Cutter::reset_on`).
struct Tripwire {
    when: Box<dyn Fn(&Event) -> bool + Send>,
    /// Agents whose new connections are refused from then on.
    refuse: Vec<String>,
    /// Fire on every matching event, not just the first.
    repeat: bool,
}

#[derive(Default)]
struct Relay {
    conns: Vec<Conn>,
    next_conn: u64,
    /// Agents whose new connections get a 401 at the handshake.
    refused: Vec<String>,
    /// Handshakes turned away because the agent was in `refused`.
    turned_away: usize,
    tripwire: Option<Tripwire>,
    /// Heartbeats relayed, by agent.
    heartbeats: HashMap<String, usize>,
    /// The events the log reader has seen.
    events: Vec<Event>,
    /// Why the log reader ended, if it did before the target was shut down.
    dead: Option<String>,
    /// Heartbeats passed on to the server whose handling has not been confirmed yet.
    pending_beats: usize,
}

struct Hub {
    relay: Mutex<Relay>,
    /// Asks the log reader for a barrier; see `Hub::barrier`.
    barriers: mpsc::UnboundedSender<oneshot::Sender<()>>,
    /// Token to agent name.
    tokens: HashMap<String, String>,
    path: String,
}

impl Relay {
    /// Fires the tripwire if `event` satisfies it: resets every connection and refuses the agents
    /// it names. A tripwire that does not repeat is spent.
    fn check(&mut self, event: &Event) {
        let Some(tripwire) = self.tripwire.as_ref() else {
            return;
        };
        if !(tripwire.when)(event) {
            return;
        }
        let (refuse, repeat) = (tripwire.refuse.clone(), tripwire.repeat);
        if !repeat {
            self.tripwire = None;
        }
        self.refused = refuse;
        self.reset_all();
    }

    fn reset_all(&mut self) -> usize {
        let conns = std::mem::take(&mut self.conns);
        for conn in &conns {
            let _ = conn.fate.send(Fate::Reset);
        }
        conns.len()
    }
}

/// The identity port of the in-process server: a token names an agent.
struct Tokens(HashMap<String, String>);

impl Verifier for Tokens {
    fn verify(&self, token: &str, _repo: &RepoId) -> Result<AgentId, Refusal> {
        self.0
            .get(token)
            .map(|agent| AgentId(agent.clone()))
            .ok_or(Refusal {
                reason: "unknown token",
            })
    }
}

/// A running local coordinator. Dropping it without `shutdown` leaves the tasks to end with the
/// runtime; `shutdown` closes the relay and stops the server first.
pub struct LocalServer {
    pub endpoint: Endpoint,
    hub: Arc<Hub>,
    clock: TestClock,
    server: RunningServer,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    relay_addr: SocketAddr,
}

#[derive(Clone)]
pub struct LogReader(Arc<Hub>);

/// Closes an agent's sockets from the relay's side, the way a failing coordinator or network
/// does: `cut` ends them with close code 1011, the others with no close frame at all.
#[derive(Clone)]
pub struct Cutter(Arc<Hub>);

impl Cutter {
    /// Closes every socket bound to `agent` with close code 1011. Returns whether there was one.
    pub fn cut(&self, agent: &str) -> bool {
        let mut relay = lock(&self.0.relay);
        let mut cut = false;
        relay.conns.retain(|conn| {
            let bound = conn.agent == agent;
            if bound {
                let _ = conn.fate.send(Fate::Cut);
                cut = true;
            }
            !bound
        });
        cut
    }

    /// Resets every socket at once, as the network does: no close frame, the connection just
    /// ends, for the agents, the reviewer and the watchers alike. The server withdraws each
    /// agent's queued request when it sees the socket close; claims stay under their lease.
    /// Returns how many sockets were reset.
    pub fn reset_all(&self) -> usize {
        lock(&self.0.relay).reset_all()
    }

    /// Resets every socket (as `reset_all`) when the log reader sees the first event satisfying
    /// `when`, and from then on refuses new connections from the agents in `refuse` (turned away
    /// at the handshake). The cut happens once. The agents may already hold what the step that
    /// logged the event sent them: the log reader is a socket of its own.
    pub fn reset_on(&self, when: impl Fn(&Event) -> bool + Send + 'static, refuse: Vec<String>) {
        lock(&self.0.relay).tripwire = Some(Tripwire {
            when: Box::new(when),
            refuse,
            repeat: false,
        });
    }

    /// Like `reset_on`, but cuts again at every event that satisfies `when`: a network that keeps
    /// failing.
    pub fn reset_on_each(&self, when: impl Fn(&Event) -> bool + Send + 'static) {
        lock(&self.0.relay).tripwire = Some(Tripwire {
            when: Box::new(when),
            refuse: Vec::new(),
            repeat: true,
        });
    }

    /// How many connection attempts were turned away because the agent was refused.
    pub fn turned_away(&self) -> usize {
        lock(&self.0.relay).turned_away
    }

    /// Lets the agents in `refuse` open no new connection (turned away at the handshake); an
    /// empty list lets everyone in again. Sockets that are already open are not touched.
    pub fn refuse_new(&self, refuse: Vec<String>) {
        lock(&self.0.relay).refused = refuse;
    }
}

impl LogReader {
    pub fn events(&self) -> Vec<Event> {
        let relay = lock(&self.0.relay);
        assert!(relay.dead.is_none(), "{}", dead_message(&relay));
        relay.events.clone()
    }
}

/// What the local target is made of on disk, inside `scratch`.
pub struct LocalSetup<'a> {
    pub repo: &'a ScratchRepo,
    pub scratch: &'a std::path::Path,
    pub base: &'a Tree,
    /// Every name that needs a token: the agents, the reviewer and the observer.
    pub names: &'a [String],
    pub reviewers: &'a [String],
    /// Allow `OnConflict::Shadow` and run the trials it queues: for the shadow policy only.
    pub shadow_enabled: bool,
    /// How long a claim lives without a heartbeat.
    pub lease_ms: u64,
}

/// The bare remote, the forks and the files the server is configured with.
struct Layout {
    main: PathBuf,
    forks: PathBuf,
    config: Config,
}

fn lay_out(setup: &LocalSetup<'_>) -> Result<Layout> {
    let (main, forks) = (setup.scratch.join("trunk"), setup.scratch.join("forks"));
    let lander = setup.scratch.join("lander");
    for dir in [&main, &forks, &lander] {
        std::fs::create_dir_all(dir)?;
    }
    // The trunk is a working repository tests can edit; `updateInstead` lets the lander push to
    // its checked-out branch, which moves the working tree along.
    let trunk = Git::new(&main);
    git::init_repo(&trunk, setup.base)?;
    trunk.run(&["config", "receive.denyCurrentBranch", "updateInstead"])?;
    for name in setup.names {
        let fork = forks.join(format!("{name}.git"));
        std::fs::create_dir_all(&fork)?;
        Git::new(&fork).run(&["init", "-q", "--bare", "-b", "main"])?;
    }
    let runner = setup.scratch.join("tessel-runner.toml");
    let user = tessel_runner::effective_login()
        .context("the effective user has no login name; the runner needs one")?;
    std::fs::write(&runner, format!("[runner]\nuser = \"{user}\"\n"))?;
    std::fs::set_permissions(&runner, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    let config = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        store_dir: setup.scratch.join("stores"),
        identity: None,
        lander: Some(LanderConfig {
            work_dir: lander,
            runner_config: runner,
            token_file: None,
            sudo_runner: None,
        }),
        repositories: vec![RepositoryConfig {
            tenant: TENANT.to_string(),
            name: setup.repo.as_str().to_ascii_lowercase(),
            run: "swarm-on".to_string(),
            reviewers: setup.reviewers.to_vec(),
            sensitive: Vec::new(),
            remote: Some(main.to_string_lossy().into_owned()),
            fork: Some(format!("{}/{{agent}}.git", forks.display())),
            lease_ms: Some(setup.lease_ms),
            shadow_enabled: setup.shadow_enabled,
        }],
    };
    Ok(Layout {
        main,
        forks,
        config,
    })
}

impl LocalServer {
    pub async fn start(setup: LocalSetup<'_>) -> Result<Self> {
        let layout = lay_out(&setup)?;
        let name = layout.config.repositories[0].name.clone();
        let path = format!("/tenant/{TENANT}/repo/{name}/ws");
        let mut tokens = HashMap::new();
        let mut issued = HashMap::new();
        for agent in setup.names.iter().map(String::as_str).chain([LOG_READER]) {
            let token = format!("local-{agent}");
            tokens.insert(token.clone(), agent.to_string());
            issued.insert(agent.to_string(), Token::new(token));
        }
        let deps = Deps {
            verifier: Some(Arc::new(Tokens(tokens.clone()))),
            calls: Arc::new(NoLander),
        };
        let clock = TestClock::new();
        let server = testing::start_with_clock(&layout.config, deps, None, &clock).await?;
        let backend = server.local_addr();
        let (barriers, requests) = mpsc::unbounded_channel();
        let hub = Arc::new(Hub {
            relay: Mutex::new(Relay::default()),
            barriers,
            tokens,
            path: path.clone(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let relay_addr = listener.local_addr()?;
        let (stop, stop_rx) = watch::channel(false);
        let log_token = format!("local-{LOG_READER}");
        let (ready, subscribed) = oneshot::channel();
        let tasks = vec![
            tokio::spawn(accept(listener, Arc::clone(&hub), backend, stop_rx)),
            tokio::spawn(read_log(
                Arc::clone(&hub),
                (backend, log_token),
                requests,
                ready,
            )),
        ];
        issued.remove(LOG_READER);
        let subscribed = match tokio::time::timeout(BARRIER_WAIT, subscribed).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(why))) => Err(anyhow::anyhow!("the log reader could not subscribe: {why}")),
            Ok(Err(_)) => Err(anyhow::anyhow!("the log reader ended before it subscribed")),
            Err(_) => Err(anyhow::anyhow!("the log reader did not subscribe in time")),
        };
        if let Err(error) = subscribed {
            for task in &tasks {
                task.abort();
            }
            server.stop().await;
            return Err(error);
        }
        Ok(Self {
            hub,
            clock,
            server,
            endpoint: Endpoint {
                ws_url: format!("ws://{relay_addr}{path}"),
                tokens: issued,
                remote: Remote {
                    trunk: layout.main,
                    forks: layout.forks,
                },
            },
            stop,
            tasks,
            relay_addr,
        })
    }

    /// The coordinator's own copy of the event log, for checking what a Watch reader saw.
    pub fn log(&self) -> Vec<Event> {
        self.reader().events()
    }

    /// A handle that reads the log from another task while the server runs.
    pub fn reader(&self) -> LogReader {
        LogReader(Arc::clone(&self.hub))
    }

    /// A handle that closes agents' sockets from the relay's side while the server runs.
    pub fn cutter(&self) -> Cutter {
        Cutter(Arc::clone(&self.hub))
    }

    /// Stops the coordinator's clock: from now on it moves only with `advance_clock`, so no lease
    /// ends because the machine was slow. Call it before any claim is made.
    pub fn freeze_clock(&self) {
        self.clock.freeze();
    }

    /// Moves the coordinator's clock forward by `ms`. Follow it with `settle_expiry` before
    /// asserting on what expired.
    pub fn advance_clock(&self, ms: u64) {
        self.clock.advance(ms);
    }

    /// Waits until the server has acted on the latest clock move: a frame sent on the log socket
    /// now runs the expiry step first, as its own write, so every expiry it logged has reached
    /// the log reader by the time that frame is answered (see `Hub::barrier`).
    pub async fn settle_expiry(&self) {
        self.hub.barrier().await;
    }

    /// Waits until the server has confirmed handling every heartbeat the relay passed on, so that
    /// `heartbeats` counts the ones it handled before now. A test that moves the clock after a
    /// heartbeat calls it, or a heartbeat handled before the move could be counted after it.
    pub async fn settle_heartbeats(&self) {
        let started = tokio::time::Instant::now();
        while lock(&self.hub.relay).pending_beats > 0 {
            assert!(
                started.elapsed() < BARRIER_WAIT,
                "heartbeats passed to the server were never confirmed"
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// How many heartbeats the relay has passed on from `agent`.
    pub fn heartbeats(&self, agent: &str) -> usize {
        lock(&self.hub.relay)
            .heartbeats
            .get(agent)
            .copied()
            .unwrap_or(0)
    }

    pub fn addr(&self) -> SocketAddr {
        self.relay_addr
    }

    /// Closes every socket and the relay, stops the server and ends the background tasks.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        lock(&self.hub.relay).reset_all();
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks {
            let _ = task.await;
        }
        self.server.stop().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn accept(
    listener: TcpListener,
    hub: Arc<Hub>,
    backend: SocketAddr,
    stop: watch::Receiver<bool>,
) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(relay(stream, Arc::clone(&hub), backend, stop.clone()));
    }
}

fn dead_message(relay: &Relay) -> String {
    format!(
        "the log reader ended: {}",
        relay.dead.as_deref().unwrap_or("unknown")
    )
}

/// How long a barrier waits for the log socket's answer.
const BARRIER_WAIT: Duration = Duration::from_secs(10);

impl Hub {
    /// Returns once the server has handled everything it was sent before this call and the log
    /// reader has seen every event that produced. The log socket sends a call frame; the server
    /// runs the expiry step before it handles one, so any expiry already due is logged first, and
    /// it answers on the same socket after the events it wrote to it, in order.
    async fn barrier(&self) {
        let (ack, answered) = oneshot::channel();
        let sent = self.barriers.send(ack).is_ok();
        let answered = sent
            && tokio::time::timeout(BARRIER_WAIT, answered)
                .await
                .is_ok_and(|a| a.is_ok());
        assert!(answered, "{}", dead_message(&lock(&self.relay)));
    }
}

/// The frame the log socket sends for a barrier: a release of a claim that cannot exist, which
/// the server answers with an error carrying the request id.
fn barrier_frame(req: u64) -> Message {
    let release = ClientMsg::Release {
        claim: ClaimId(u64::MAX),
        fence: Fence(0),
        req: Some(RequestId(req)),
    };
    Message::text(serde_json::to_string(&release).unwrap_or_default())
}

/// Reads the log through a socket that bypasses the relay: it says hello as the log reader,
/// watches from the start, tells `ready` when a first barrier came back, then records events,
/// fires the tripwire and answers barrier requests. When it ends it says why in `Relay::dead`.
async fn read_log(
    hub: Arc<Hub>,
    (backend, token): (SocketAddr, String),
    mut requests: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
    ready: oneshot::Sender<Result<(), String>>,
) {
    let outcome = log_session(&hub, backend, &token, &mut requests, ready).await;
    let why = match outcome {
        Ok(()) => "its socket closed".to_string(),
        Err(why) => why,
    };
    lock(&hub.relay).dead = Some(why);
}

async fn log_session(
    hub: &Arc<Hub>,
    backend: SocketAddr,
    token: &str,
    requests: &mut mpsc::UnboundedReceiver<oneshot::Sender<()>>,
    ready: oneshot::Sender<Result<(), String>>,
) -> Result<(), String> {
    let mut socket = connect_backend(backend, &hub.path, token)
        .await
        .ok_or("it could not connect")?;
    let hello = ClientMsg::Hello {
        agent: AgentId(LOG_READER.to_string()),
        base: CommitId("0".repeat(40)),
        protocol: PROTOCOL_VERSION,
    };
    for msg in [hello, ClientMsg::Watch { from_seq: 0 }] {
        let text = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
        socket
            .send(Message::text(text))
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut ready = Some(ready);
    let mut next_req = 1_u64;
    let mut waiting: VecDeque<(u64, oneshot::Sender<()>)> = VecDeque::new();
    // The first barrier: the replay of the log is done when its answer comes.
    let (first, _) = oneshot::channel();
    waiting.push_back((0, first));
    socket
        .send(barrier_frame(0))
        .await
        .map_err(|e| e.to_string())?;
    loop {
        tokio::select! {
            request = requests.recv() => {
                let Some(ack) = request else { return Ok(()) };
                waiting.push_back((next_req, ack));
                socket.send(barrier_frame(next_req)).await.map_err(|e| e.to_string())?;
                next_req += 1;
            }
            frame = socket.next() => {
                let message = match frame {
                    Some(Ok(message)) => message,
                    Some(Err(error)) => return Err(error.to_string()),
                    None => return Ok(()),
                };
                let Message::Text(text) = message else { continue };
                match serde_json::from_str(&text) {
                    Ok(ServerMsg::Event { event }) => {
                        let mut relay = lock(&hub.relay);
                        relay.check(&event);
                        relay.events.push(event);
                    }
                    Ok(ServerMsg::Error { req: Some(RequestId(req)), .. }) => {
                        if waiting.front().is_some_and(|(wanted, _)| *wanted == req) {
                            let (_, ack) = waiting.pop_front().ok_or("no barrier waits")?;
                            let _ = ack.send(());
                            if let Some(ready) = ready.take() {
                                let _ = ready.send(Ok(()));
                            }
                        }
                    }
                    Ok(_) | Err(_) => {}
                }
            }
        }
    }
}

type ClientSocket = tokio_tungstenite::WebSocketStream<TcpStream>;
type ServerSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// Accepts an agent's upgrade if its token is known and it is not refused; returns the socket, the
/// agent and the token.
async fn handshake(
    stream: TcpStream,
    hub: &Arc<Hub>,
    stop: &mut watch::Receiver<bool>,
) -> Option<(ClientSocket, String, String)> {
    let seen: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let record = Arc::clone(&seen);
    let known = Arc::clone(hub);
    #[expect(
        clippy::result_large_err,
        reason = "the handshake callback fixes this signature"
    )]
    let check = move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let bearer = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let agent = bearer.and_then(|token| known.tokens.get(token));
        let open = |agent: &&String| {
            let mut relay = lock(&known.relay);
            let refused = relay.refused.contains(agent);
            relay.turned_away += usize::from(refused);
            !refused
        };
        match (bearer, agent.filter(open)) {
            (Some(token), Some(agent)) if request.uri().path() == known.path => {
                *lock(&record) = Some((agent.clone(), token.to_string()));
                Ok(response)
            }
            _ => {
                let mut refusal = ErrorResponse::new(Some("unauthorized".to_string()));
                *refusal.status_mut() = StatusCode::UNAUTHORIZED;
                Err(refusal)
            }
        }
    };
    let accepted = tokio::select! {
        result = tokio_tungstenite::accept_hdr_async(stream, check) => result,
        _ = stop.changed() => return None,
    };
    let client = accepted.ok()?;
    let (agent, token) = lock(&seen).clone()?;
    Some((client, agent, token))
}

/// Serves one agent connection: accepts its upgrade, opens one to the server with the same token
/// and passes the messages both ways until either side ends or the relay's fate says otherwise.
async fn relay(
    stream: TcpStream,
    hub: Arc<Hub>,
    backend: SocketAddr,
    mut stop: watch::Receiver<bool>,
) {
    let Some((mut client, agent, token)) = handshake(stream, &hub, &mut stop).await else {
        return;
    };
    let Some(mut server) = connect_backend(backend, &hub.path, &token).await else {
        return;
    };
    let (fate, fates) = mpsc::unbounded_channel();
    let id = {
        let mut relay = lock(&hub.relay);
        relay.next_conn += 1;
        let id = relay.next_conn;
        relay.conns.push(Conn {
            id,
            agent: agent.clone(),
            fate,
        });
        id
    };
    let unanswered = pump(&mut client, &mut server, fates, (&hub, &agent), stop).await;
    let mut relay = lock(&hub.relay);
    relay.pending_beats = relay.pending_beats.saturating_sub(unanswered);
    relay.conns.retain(|conn| conn.id != id);
}

/// A connection that is about to end will not confirm the heartbeats it has passed on: they stop
/// counting as pending, so `settle_heartbeats` does not wait for a connection that is parked.
fn forget_heartbeats(hub: &Hub, unanswered: &mut usize) {
    let mut relay = lock(&hub.relay);
    relay.pending_beats = relay.pending_beats.saturating_sub(*unanswered);
    *unanswered = 0;
}

/// Ends a connection by its fate. First it passes the agent whatever the server has already sent
/// it (everything before a junk frame's answer), so that a reset after a step finds the answer
/// that step sent already delivered.
async fn end_connection(
    client: &mut ClientSocket,
    server: &mut ServerSocket,
    fate: Option<Fate>,
    stop: &mut watch::Receiver<bool>,
) {
    if server.send(Message::text(SENTINEL)).await.is_ok() {
        let flush = async {
            while let Some(Ok(message)) = server.next().await {
                if is_sentinel_answer(&message) {
                    return;
                }
                if client.send(message).await.is_err() {
                    return;
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(1), flush).await;
    }
    if let Some(Fate::Cut) = fate {
        cut(client).await;
        park(server, stop).await;
    }
}

/// Sends `message` to the server; a heartbeat is followed by the junk frame whose answer shows the
/// server handled it. False when the server's socket is gone.
async fn forward(
    (server, hub): (&mut ServerSocket, &Hub),
    message: Message,
    unanswered: &mut usize,
) -> bool {
    let beat = matches!(&message, Message::Text(text) if is_heartbeat(text));
    if beat {
        // Pending before the heartbeat leaves, so no moment exists in which the server may have
        // it and `settle_heartbeats` sees none. If a send fails the connection ends, and its
        // cleanup takes `unanswered` back out of the pending count.
        *unanswered += 1;
        lock(&hub.relay).pending_beats += 1;
    }
    if server.send(message).await.is_err() {
        return false;
    }
    if beat && server.send(Message::text(SENTINEL)).await.is_err() {
        return false;
    }
    true
}

/// Passes messages both ways, and returns how many heartbeats it passed on that the server had not
/// confirmed handling. A heartbeat is followed by a junk frame to the server, whose answer
/// marks that the server has handled the heartbeat, and the heartbeat counts then.
async fn pump(
    client: &mut ClientSocket,
    server: &mut ServerSocket,
    mut fates: mpsc::UnboundedReceiver<Fate>,
    (hub, agent): (&Arc<Hub>, &str),
    mut stop: watch::Receiver<bool>,
) -> usize {
    let mut unanswered = 0_usize;
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => return unanswered,
            end = fates.recv() => {
                forget_heartbeats(hub, &mut unanswered);
                end_connection(client, server, end, &mut stop).await;
                return unanswered;
            }
            frame = client.next() => {
                let Some(Ok(message)) = frame else { return unanswered };
                if lock(&hub.relay).tripwire.is_some() {
                    // Everything the server has logged so far reaches the log reader, and so the
                    // tripwire, before this frame goes on: the agent keeps the answer the step
                    // sent and then finds the connection gone, as it would if the cut were inside
                    // the step.
                    hub.barrier().await;
                    if let Ok(end) = fates.try_recv() {
                        forget_heartbeats(hub, &mut unanswered);
                        end_connection(client, server, Some(end), &mut stop).await;
                        return unanswered;
                    }
                }
                if !forward((server, hub), message, &mut unanswered).await {
                    return unanswered;
                }
            }
            frame = server.next() => {
                let Some(Ok(message)) = frame else {
                    let _ = client.close(None).await;
                    return unanswered;
                };
                if unanswered > 0 && is_sentinel_answer(&message) {
                    unanswered -= 1;
                    let mut relay = lock(&hub.relay);
                    relay.pending_beats = relay.pending_beats.saturating_sub(1);
                    *relay.heartbeats.entry(agent.to_string()).or_default() += 1;
                } else if client.send(message).await.is_err() {
                    return unanswered;
                }
            }
        }
    }
}

async fn connect_backend(backend: SocketAddr, path: &str, token: &str) -> Option<ServerSocket> {
    let mut request = format!("ws://{backend}{path}").into_client_request().ok()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().ok()?);
    tokio_tungstenite::connect_async(request)
        .await
        .ok()
        .map(|(socket, _)| socket)
}

/// A frame the server cannot parse; its `Malformed` answer marks that everything sent before it
/// has been handled.
const SENTINEL: &str = "{";

fn is_sentinel_answer(message: &Message) -> bool {
    let Message::Text(text) = message else {
        return false;
    };
    matches!(
        serde_json::from_str(text),
        Ok(ServerMsg::Error {
            req: None,
            code: ErrorCode::Malformed,
            ..
        })
    )
}

fn is_heartbeat(text: &str) -> bool {
    matches!(serde_json::from_str(text), Ok(ClientMsg::Heartbeat))
}

/// Keeps the server's end of a cut connection open and unread, as a host does that believes the
/// agent is still there: its queued request is not withdrawn, and what it sends the agent is
/// lost. Ends when the server closes the socket or the relay stops.
async fn park(server: &mut ServerSocket, stop: &mut watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            frame = server.next() => if !matches!(frame, Some(Ok(_))) { return },
        }
    }
}

/// Ends the client's socket the way a failing coordinator does: close code 1011, then read until
/// the agent answers the close, so that its unread heartbeats do not reset the connection and
/// hide the close frame.
async fn cut(client: &mut ClientSocket) {
    let frame = CloseFrame {
        code: CloseCode::Error,
        reason: "coordinator error".into(),
    };
    let _ = client.close(Some(frame)).await;
    let drained = async { while let Some(Ok(_)) = client.next().await {} };
    let _ = tokio::time::timeout(Duration::from_secs(5), drained).await;
}

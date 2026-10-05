//! The per-worktree daemon. It holds the one WebSocket to the coordinator, sends heartbeats,
//! reconnects with backoff, answers CLI commands on a Unix socket and keeps `.tessel/` current.
//!
//! One task owns all state. Reader/writer work for the socket runs in a helper task that
//! exchanges messages with the owner through channels, so no lock is ever held across an await.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    uncovered, AgentId, Assumption, ClaimId, ClientMsg, CommitId, DecisionRecord, ErrorCode, Event,
    EventKind, Intent, OnConflict, RequestId, ScopeClaim, ServerMsg, PROTOCOL_VERSION,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, AUTHORIZATION};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::config::Config;
use crate::plan::escalate;
use crate::reconcile::{self, Local, Plan, ServerClaim};
use crate::rpc::{self, ClaimOutcome, Reply, Request, SubmitOutcome};
use crate::state::{append_notice, Connection, HeldClaim, Notice, NoticeKind, QueuedWait, State};
use crate::worktree::Worktree;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(1);
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// How long the event-log read after a reconnect may take to reach its end marker. A read that
/// does not is treated as truncated.
const SNAPSHOT_LIMIT: Duration = Duration::from_secs(5);
/// Until the coordinator's `Welcome` says otherwise.
const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(10);
/// How many times, and for how long in all, `stop` reads the event log before it reports a claim as
/// not released. The client gives up on `stop` after 30 s.
const CONFIRM_READS: u32 = 3;
const CONFIRM_DEADLINE: Duration = Duration::from_secs(15);
const CONFIRM_PAUSE: Duration = Duration::from_millis(150);

pub(crate) type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// What `tessel start` hands the daemon.
#[derive(Debug, Clone)]
pub struct Args {
    pub summary: String,
    pub task_ref: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A request from a CLI connection, with the channel for its reply.
struct Command {
    request: Request,
    reply: oneshot::Sender<Reply>,
}

enum Incoming {
    Msg {
        generation: u64,
        msg: ServerMsg,
    },
    Closed {
        generation: u64,
        reason: String,
    },
    /// The coordinator's event log, read right after a reconnect.
    Snapshot {
        generation: u64,
        log: Result<LogRead, String>,
    },
}

/// What the event-log read returned.
pub(crate) struct LogRead {
    pub(crate) events: Vec<Event>,
    /// The read reached its end marker, with no gap in `seq`.
    pub(crate) complete: bool,
}

struct Conn {
    out: mpsc::UnboundedSender<ClientMsg>,
    task: JoinHandle<()>,
}

struct PendingClaim {
    scopes: Vec<ScopeClaim>,
    /// The coordinator answered `Queued`; the grant will arrive later.
    queued: bool,
    /// The held claim this request adds its scopes to, when it is an `Amend`.
    amend: Option<ClaimId>,
    replies: Vec<oneshot::Sender<Reply>>,
}

/// A `Submit` sent and not yet answered by `Accepted`, `Uncovered`, `ReviewRequired` or `Error`.
struct PendingSubmit {
    claim: ClaimId,
    fork_commit: String,
    reply: oneshot::Sender<Reply>,
}

/// Why a connection attempt failed.
pub(crate) enum ConnectFailure {
    /// Retrying cannot help (bad token, wrong repo or URL).
    Fatal(String),
    Transient(String),
}

enum Flow {
    Continue,
    Exit,
}

struct Daemon {
    worktree: Worktree,
    config: Config,
    state: State,
    conn: Option<Conn>,
    generation: u64,
    next_req: u64,
    pending: HashMap<u64, PendingClaim>,
    release_reqs: HashMap<u64, ClaimId>,
    submits: HashMap<u64, PendingSubmit>,
    /// Claim requests waiting for the one in flight to be answered.
    deferred: VecDeque<(Request, oneshot::Sender<Reply>)>,
    /// Claim requests whose answer was lost with the socket; their callers are still waiting.
    lost_requests: Vec<PendingClaim>,
    /// Claims released just before the socket dropped; the release may not have arrived.
    lost_releases: HashSet<ClaimId>,
    /// Claims granted since the current connection was welcomed.
    fresh: HashSet<ClaimId>,
    /// The task reading the event log. Aborted when the working socket drops: a bound read
    /// connection that outlived it would stop the coordinator withdrawing a queued wait.
    snapshot: Option<JoinHandle<()>>,
    in_tx: mpsc::UnboundedSender<Incoming>,
    backoff: Duration,
    reconnect_at: Option<Instant>,
    next_heartbeat: Instant,
    heartbeat_every: Duration,
    next_housekeeping: Instant,
    ever_online: bool,
}

/// Runs the daemon for `worktree` until `tessel stop` or a fatal error. Returns `Ok` without
/// doing anything if another daemon already holds this worktree's lock.
pub async fn run(worktree: Worktree, config: Config, args: Args) -> anyhow::Result<()> {
    // Fails only if a provider is already installed, which is what we want anyway.
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        log(
            &worktree,
            &config,
            "a TLS crypto provider was already installed",
        );
    }
    worktree.prepare_dir()?;
    // Held for the daemon's whole life: the kernel drops it if the process dies, so a crash
    // never leaves a lock behind, and a second daemon exits before touching the socket.
    let Some(_lock) = acquire_lock(&worktree)? else {
        log(
            &worktree,
            &config,
            "another daemon holds the lock for this worktree; exiting",
        );
        return Ok(());
    };
    let sock = worktree.sock();
    match std::fs::remove_file(&sock) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot remove stale {}", sock.display())),
    }
    let listener = UnixListener::bind(&sock)
        .with_context(|| format!("cannot listen on {}", sock.display()))?;
    restrict_permissions(&sock)?;
    std::fs::write(worktree.pid_path(), std::process::id().to_string())
        .context("cannot write daemon.pid")?;

    let base = worktree.head()?;
    // The diff base is pinned at the first start in a worktree and then only moves when a merge
    // lands (`on_merged`): not on a restart, a reconnect, `stop` or a lapsed lease. Deleting
    // `.tessel/state.json` resets it to HEAD.
    let persisted = State::read(&worktree)?
        .map(|prior| prior.start_base)
        .filter(|pinned| !pinned.is_empty());
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let accept = tokio::spawn(accept_loop(listener, cmd_tx, shutdown_rx));
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let state = State {
        pid: std::process::id(),
        agent: config.agent.clone(),
        repo: config.repo.clone(),
        summary: args.summary,
        task_ref: args.task_ref,
        start_base: persisted.unwrap_or_else(|| base.clone()),
        coordinator_head: None,
        base: base.clone(),
        socket: sock.display().to_string(),
        connection: Connection::Connecting,
        lease_ms: None,
        last_error: None,
        claims: Vec::new(),
        queued: None,
        updated_at_ms: now_ms(),
    };
    let mut daemon = Daemon::new(worktree.clone(), config, state, in_tx);
    daemon.log("daemon started");
    let result = daemon.main_loop(cmd_rx, in_rx).await;
    if let Err(e) = &result {
        daemon.log(&format!("daemon failed: {e:#}"));
    }
    daemon.finish().await;
    drop(daemon);
    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(CLOSE_GRACE, accept).await;
    result
}

fn acquire_lock(worktree: &Worktree) -> anyhow::Result<Option<std::fs::File>> {
    let path = worktree.lock_path();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}

fn restrict_permissions(sock: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot restrict {}", sock.display()))
}

/// Appends a timestamped line to `daemon.log`, with the token removed.
fn log(worktree: &Worktree, config: &Config, message: &str) {
    let line = format!("{} {}\n", now_ms(), config.token.redact(message));
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(worktree.log_path())
        .and_then(|mut file| file.write_all(line.as_bytes()));
    if let Err(e) = written {
        let _ = writeln!(std::io::stderr(), "tessel daemon: cannot write log: {e}");
    }
}

impl Daemon {
    fn new(
        worktree: Worktree,
        config: Config,
        state: State,
        in_tx: mpsc::UnboundedSender<Incoming>,
    ) -> Self {
        let now = Instant::now();
        Self {
            worktree,
            config,
            state,
            conn: None,
            generation: 0,
            next_req: 1,
            pending: HashMap::new(),
            release_reqs: HashMap::new(),
            submits: HashMap::new(),
            deferred: VecDeque::new(),
            lost_requests: Vec::new(),
            lost_releases: HashSet::new(),
            fresh: HashSet::new(),
            snapshot: None,
            in_tx,
            backoff: FIRST_BACKOFF,
            reconnect_at: Some(now),
            next_heartbeat: now + DEFAULT_HEARTBEAT,
            heartbeat_every: DEFAULT_HEARTBEAT,
            next_housekeeping: now + HOUSEKEEPING_EVERY,
            ever_online: false,
        }
    }

    fn log(&self, message: &str) {
        log(&self.worktree, &self.config, message);
    }

    fn is_online(&self) -> bool {
        self.conn.is_some() && self.state.connection == Connection::Online
    }

    fn persist(&mut self) {
        self.state.updated_at_ms = now_ms();
        if let Err(e) = self.state.write(&self.worktree) {
            self.log(&format!("cannot write state.json: {e}"));
        }
    }

    fn notify(&self, kind: NoticeKind, note: &str, server: Option<ServerMsg>) {
        let notice = Notice {
            at_ms: now_ms(),
            kind,
            note: note.to_string(),
            server,
        };
        let redact = |text: &str| self.config.token.redact(text);
        if let Err(e) = append_notice(&self.worktree, &notice, &redact) {
            self.log(&format!("cannot write inbox: {e}"));
        }
    }

    async fn main_loop(
        &mut self,
        mut cmd_rx: mpsc::Receiver<Command>,
        mut in_rx: mpsc::UnboundedReceiver<Incoming>,
    ) -> anyhow::Result<()> {
        self.persist();
        loop {
            let online = self.is_online();
            let reconnect_at = self.reconnect_at;
            tokio::select! {
                Some(command) = cmd_rx.recv() => {
                    if let Flow::Exit = self.on_command(command).await {
                        return Ok(());
                    }
                }
                Some(incoming) = in_rx.recv() => self.on_incoming(incoming),
                () = tokio::time::sleep_until(self.next_heartbeat), if online => {
                    self.send_heartbeat();
                }
                () = sleep_until_some(reconnect_at), if reconnect_at.is_some() => {
                    self.try_connect().await?;
                }
                () = tokio::time::sleep_until(self.next_housekeeping) => {
                    if let Flow::Exit = self.housekeeping() {
                        return Ok(());
                    }
                }
            }
        }
    }

    // ---------- connection ----------

    async fn try_connect(&mut self) -> anyhow::Result<()> {
        self.reconnect_at = None;
        match open_socket(&self.config).await {
            Ok(socket) => {
                self.generation += 1;
                self.start_conn(socket);
                Ok(())
            }
            Err(ConnectFailure::Fatal(message)) if !self.ever_online => bail!("{message}"),
            Err(ConnectFailure::Fatal(message) | ConnectFailure::Transient(message)) => {
                self.log(&format!("connect failed: {message}"));
                self.state.last_error = Some(message);
                self.schedule_reconnect();
                self.persist();
                Ok(())
            }
        }
    }

    fn start_conn(&mut self, socket: Socket) {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(socket_task(
            socket,
            out_rx,
            self.in_tx.clone(),
            self.generation,
        ));
        match self.worktree.head() {
            Ok(head) => self.state.base = head,
            Err(e) => self.log(&format!(
                "cannot read HEAD, keeping {}: {e}",
                self.state.base
            )),
        }
        let hello = ClientMsg::Hello {
            agent: AgentId(self.config.agent.clone()),
            base: CommitId(self.state.base.clone()),
            protocol: PROTOCOL_VERSION,
        };
        let _ = out_tx.send(hello);
        self.conn = Some(Conn { out: out_tx, task });
        self.log("connected; hello sent");
    }

    fn schedule_reconnect(&mut self) {
        self.reconnect_at = Some(Instant::now() + self.backoff);
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
        self.state.connection = if self.ever_online {
            Connection::Reconnecting
        } else {
            Connection::Connecting
        };
    }

    fn send(&mut self, msg: ClientMsg) -> bool {
        match &self.conn {
            Some(conn) => conn.out.send(msg).is_ok(),
            None => false,
        }
    }

    fn send_heartbeat(&mut self) {
        self.next_heartbeat = Instant::now() + self.heartbeat_every;
        if !self.send(ClientMsg::Heartbeat) {
            return;
        }
        let renewed = now_ms().saturating_add(self.state.lease_ms.unwrap_or(0));
        for held in &mut self.state.claims {
            held.expires_at_ms = renewed;
        }
        self.persist();
    }

    fn housekeeping(&mut self) -> Flow {
        self.next_housekeeping = Instant::now() + HOUSEKEEPING_EVERY;
        if !self.worktree.dir().exists() {
            return Flow::Exit;
        }
        let now = now_ms();
        let (live, lapsed): (Vec<_>, Vec<_>) = std::mem::take(&mut self.state.claims)
            .into_iter()
            .partition(|held| held.submitted || held.expires_at_ms > now);
        self.state.claims = live;
        for held in lapsed {
            self.notify(
                NoticeKind::LeaseExpired,
                &format!(
                    "claim {} presumed expired: no heartbeat reached the coordinator before its \
                     lease ended",
                    held.claim.0
                ),
                None,
            );
            self.persist();
        }
        Flow::Continue
    }

    // ---------- messages from the coordinator ----------

    fn on_incoming(&mut self, incoming: Incoming) {
        match incoming {
            Incoming::Msg { generation, msg } if generation == self.generation => {
                self.on_server_msg(msg);
                self.drain_deferred();
            }
            Incoming::Closed { generation, reason } if generation == self.generation => {
                self.on_closed(&reason);
                self.drain_deferred();
            }
            Incoming::Snapshot { generation, log } if generation == self.generation => {
                self.on_snapshot(log);
            }
            Incoming::Msg { .. } | Incoming::Closed { .. } | Incoming::Snapshot { .. } => {}
        }
    }

    fn on_closed(&mut self, reason: &str) {
        self.conn = None;
        if let Some(read) = self.snapshot.take() {
            read.abort();
        }
        self.log(&format!("connection closed: {reason}"));
        self.state.last_error = Some(reason.to_string());
        // A claim request that was in flight may have been granted without us hearing of it.
        // Its caller keeps waiting; the event log read after the next welcome decides.
        for (_, pending) in std::mem::take(&mut self.pending) {
            if pending.queued {
                self.notify(
                    NoticeKind::WaitWithdrawn,
                    "connection lost while a --wait claim was queued; the coordinator withdraws \
                     it, claim again after reconnecting",
                    None,
                );
            } else if pending.amend.is_some() {
                // The amend may have been applied; the log read after the next welcome repairs the
                // claim's fence and scopes (`reconcile`), so the caller is told to look.
                let message = "the connection dropped before the coordinator answered; the \
                               scopes may have been added to your claim, so run `tessel status` \
                               and claim again if they are missing";
                answer(pending.replies, &refused(None, message));
            } else {
                self.lost_requests.push(pending);
            }
        }
        // A submission in flight may have been accepted without us hearing of it; the event log
        // read after the next welcome marks the claim submitted if it was.
        let message = "the connection dropped before the coordinator answered; the submission may \
                       still have arrived, so run `tessel status`: a claim shown as submitted was \
                       accepted";
        for (_, pending) in std::mem::take(&mut self.submits) {
            let _ = pending.reply.send(submit_refused(None, message));
        }
        self.lost_releases
            .extend(self.release_reqs.drain().map(|(_, claim)| claim));
        self.state.queued = None;
        self.schedule_reconnect();
        self.persist();
    }

    fn on_server_msg(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome { head, lease_ms, .. } => self.on_welcome(head.0, lease_ms),
            ServerMsg::Granted {
                req,
                claim,
                fence,
                expires_at_ms,
                race,
                at_risk,
            } => match self.pending.remove(&req.0) {
                Some(pending) => self.on_granted(pending, claim, fence, race, at_risk),
                None => self.unexpected(
                    "grant for an unknown request",
                    &ServerMsg::Granted {
                        req,
                        claim,
                        fence,
                        expires_at_ms,
                        race,
                        at_risk,
                    },
                ),
            },
            ServerMsg::Denied { req, conflicts } => {
                let msg = ServerMsg::Denied {
                    req,
                    conflicts: conflicts.clone(),
                };
                self.notify(NoticeKind::Denied, "claim denied", Some(msg));
                if let Some(pending) = self.pending.remove(&req.0) {
                    answer(
                        pending.replies,
                        &Reply::Claim {
                            outcome: ClaimOutcome::Denied { conflicts },
                        },
                    );
                }
            }
            ServerMsg::Queued { req, position } => self.on_queued(req, position),
            ServerMsg::BaseMoved { ref head, .. } => {
                self.state.coordinator_head = Some(head.0.clone());
                self.notify(
                    NoticeKind::BaseMoved,
                    "main moved under your claims",
                    Some(msg),
                );
                self.persist();
            }
            ServerMsg::AssumptionChallenged { .. } => {
                self.notify(
                    NoticeKind::AssumptionChallenged,
                    "work submitted by another agent touches something you assume",
                    Some(msg),
                );
            }
            ServerMsg::LeaseExpired { claim, .. } => {
                self.state.claims.retain(|held| held.claim != claim);
                self.notify(NoticeKind::LeaseExpired, "a claim's lease ended", Some(msg));
                self.persist();
            }
            ServerMsg::Error {
                req,
                code,
                ref message,
            } => {
                let message = message.clone();
                self.on_error(req, code, &message, &msg);
            }
            ServerMsg::Accepted {
                req,
                claim,
                queue_position,
            } => self.on_accepted(req, claim, queue_position),
            ServerMsg::Merged { claim, ref head } => {
                self.state.coordinator_head = Some(head.0.clone());
                self.on_merged(claim, msg);
            }
            ServerMsg::SubmitRejected { claim, .. } => self.on_submit_rejected(claim, msg),
            ServerMsg::Uncovered {
                req,
                claim,
                ref scopes,
            } => {
                let scopes = scopes.clone();
                self.on_uncovered(req, claim, scopes, msg);
            }
            ServerMsg::ReviewRequired { claim, ref reasons } => {
                let reasons = reasons.clone();
                self.on_review_required(claim, reasons, msg);
            }
            ServerMsg::Shadowed { .. }
            | ServerMsg::RaceOpened { .. }
            | ServerMsg::RaceResult { .. }
            | ServerMsg::Event { .. } => self.unexpected("message this CLI does not use", &msg),
        }
    }

    fn unexpected(&self, note: &str, msg: &ServerMsg) {
        self.notify(NoticeKind::Unexpected, note, Some(msg.clone()));
    }

    fn on_welcome(&mut self, head: String, lease_ms: u64) {
        self.state.coordinator_head = Some(head);
        self.ever_online = true;
        self.backoff = FIRST_BACKOFF;
        self.state.connection = Connection::Online;
        self.state.lease_ms = Some(lease_ms);
        self.state.last_error = None;
        self.heartbeat_every = Duration::from_millis((lease_ms / 3).max(1));
        self.next_heartbeat = Instant::now() + self.heartbeat_every;
        self.fresh.clear();
        if let Some(read) = self.snapshot.take() {
            read.abort();
        }
        self.snapshot = Some(tokio::spawn(snapshot_task(
            self.config.clone(),
            self.state.base.clone(),
            self.in_tx.clone(),
            self.generation,
        )));
        self.log(&format!("welcomed; lease {lease_ms} ms"));
        self.persist();
    }

    fn on_granted(
        &mut self,
        pending: PendingClaim,
        claim: ClaimId,
        fence: tessel_coordinator::protocol::Fence,
        race: Option<tessel_coordinator::protocol::RaceId>,
        at_risk: Vec<tessel_coordinator::protocol::HeldAssumption>,
    ) {
        let lease = self.state.lease_ms.unwrap_or(0);
        let amended = pending.amend.is_some();
        let held = if let Some(target) = pending.amend {
            let Some(held) = self.extend_claim(target, fence, &pending.scopes) else {
                let message = format!("claim {} is no longer held here", target.0);
                answer(pending.replies, &refused(None, &message));
                return;
            };
            held
        } else {
            let held = HeldClaim {
                claim,
                fence,
                expires_at_ms: now_ms().saturating_add(lease),
                race,
                scopes: pending.scopes,
                submitted: false,
                submitted_commit: None,
            };
            self.state.claims.push(held.clone());
            held
        };
        self.fresh.insert(claim);
        if pending.queued {
            self.state.queued = None;
            self.notify(
                NoticeKind::GrantedAfterWait,
                &format!("your queued claim was granted as claim {}", claim.0),
                None,
            );
        }
        if !at_risk.is_empty() {
            let msg = ServerMsg::Granted {
                req: RequestId(0),
                claim,
                fence,
                expires_at_ms: held.expires_at_ms,
                race,
                at_risk: at_risk.clone(),
            };
            self.notify(
                NoticeKind::AtRisk,
                "this claim could break assumptions other agents declared",
                Some(msg),
            );
        }
        self.persist();
        answer(
            pending.replies,
            &Reply::Claim {
                outcome: ClaimOutcome::Granted {
                    claim: held,
                    at_risk,
                    amended,
                },
            },
        );
    }

    /// Adds `scopes` to the held claim `target` under its new `fence`, as an `Amend` grant did.
    fn extend_claim(
        &mut self,
        target: ClaimId,
        fence: tessel_coordinator::protocol::Fence,
        scopes: &[ScopeClaim],
    ) -> Option<HeldClaim> {
        let held = self
            .state
            .claims
            .iter_mut()
            .find(|held| held.claim == target)?;
        held.fence = fence;
        for scope in scopes {
            if !held.scopes.contains(scope) {
                held.scopes.push(scope.clone());
            }
        }
        Some(held.clone())
    }

    /// Compares this agent's claims in the coordinator's log with the local ones, and repairs
    /// the difference. The rules are in `reconcile`.
    fn on_snapshot(&mut self, log: Result<LogRead, String>) {
        let lost = std::mem::take(&mut self.lost_requests);
        let lost_releases = std::mem::take(&mut self.lost_releases);
        let LogRead { events, complete } = match log {
            Ok(log) => log,
            Err(why) => {
                self.log(&format!("cannot read the event log: {why}"));
                self.notify(
                    NoticeKind::Error,
                    "could not read the coordinator's event log after reconnecting, so claims \
                     were not compared; see daemon.log",
                    None,
                );
                let message = "the connection dropped before the coordinator answered, and the \
                               outcome could not be checked afterwards; run `tessel status`";
                for pending in lost {
                    answer(pending.replies, &refused(None, message));
                }
                self.lost_releases = lost_releases;
                return;
            }
        };
        let live = reconcile::live_claims(&AgentId(self.config.agent.clone()), &events);
        self.advance_base_from_log(&events);
        self.log(&format!(
            "event log read: {} events ({}), {} live claims for this agent",
            events.len(),
            if complete { "complete" } else { "TRUNCATED" },
            live.len()
        ));
        let scopes: Vec<Vec<ScopeClaim>> = lost.iter().map(|p| p.scopes.clone()).collect();
        let local = Local {
            claims: &self.state.claims,
            fresh: &self.fresh,
            lost_requests: &scopes,
            lost_releases: &lost_releases,
            complete,
        };
        let plan = reconcile::plan(&local, &live, &events);
        if complete {
            self.apply_plan(plan, lost);
            return;
        }
        self.notify(
            NoticeKind::Error,
            "the coordinator's event log was not read to its end after reconnecting, so no claim \
             was adopted; only claims the log shows as ended were dropped",
            None,
        );
        self.lost_releases = lost_releases;
        let message = "the connection dropped before the coordinator answered, and the outcome \
                       could not be checked afterwards; run `tessel status`, then run it again";
        for pending in lost {
            answer(pending.replies, &refused(None, message));
        }
        self.apply_plan(plan, Vec::new());
    }

    /// Moves the diff base forward to the newest of this agent's submissions that the log shows
    /// merged, for a `Merged` that arrived while the daemon was offline or stopped. A submitted
    /// claim can only end by merging, and only a commit that descends from the base moves it.
    fn advance_base_from_log(&mut self, events: &[Event]) {
        let me = AgentId(self.config.agent.clone());
        let root = self.worktree.root.clone();
        let mut moved = false;
        for commit in reconcile::landed_commits(&me, events) {
            let ahead = commit != self.state.start_base
                && crate::submit::is_ancestor(&root, &self.state.start_base, &commit);
            if ahead {
                self.state.start_base = commit;
                moved = true;
            }
        }
        if moved {
            self.log("diff base advanced to a submission the log shows merged");
            self.persist();
        }
    }

    /// Brings the fences and scopes of local claims up to what the log shows.
    fn repair_local_claims(
        &mut self,
        refresh: Vec<(ClaimId, tessel_coordinator::protocol::Fence)>,
        rescope: Vec<(ClaimId, Vec<ScopeClaim>)>,
    ) {
        for (claim, scopes) in rescope {
            if let Some(held) = self.state.claims.iter_mut().find(|h| h.claim == claim) {
                held.scopes = scopes;
            }
        }
        for (claim, fence) in refresh {
            if let Some(held) = self.state.claims.iter_mut().find(|h| h.claim == claim) {
                held.fence = fence;
            }
        }
    }

    fn apply_submitted(&mut self, set_submitted: Vec<(ClaimId, bool, Option<String>)>) {
        for (claim, submitted, commit) in set_submitted {
            if let Some(held) = self.state.claims.iter_mut().find(|h| h.claim == claim) {
                held.submitted = submitted;
                held.submitted_commit = commit;
            }
            let note = if submitted {
                format!(
                    "claim {} is submitted at the coordinator; it is now shown as submitted",
                    claim.0
                )
            } else {
                format!(
                    "the merge of claim {} was refused; the claim is active again",
                    claim.0
                )
            };
            self.notify(NoticeKind::Reconciled, &note, None);
        }
    }

    fn apply_plan(&mut self, plan: Plan, lost: Vec<PendingClaim>) {
        let Plan {
            forget,
            refresh,
            rescope,
            set_submitted,
            answer_lost,
            release_again,
            adopt,
        } = plan;
        for claim in forget {
            self.state.claims.retain(|held| held.claim != claim);
            let note = format!("claim {} is gone from the coordinator; forgot it", claim.0);
            self.notify(NoticeKind::Reconciled, &note, None);
        }
        self.repair_local_claims(refresh, rescope);
        self.apply_submitted(set_submitted);
        let mut lost: Vec<Option<PendingClaim>> = lost.into_iter().map(Some).collect();
        for (index, claim, server) in answer_lost {
            let held = self.adopt(claim, server);
            let note = format!(
                "claim {} was granted just before the connection dropped; it is now tracked",
                claim.0
            );
            self.notify(NoticeKind::Reconciled, &note, None);
            if let Some(pending) = lost.get_mut(index).and_then(Option::take) {
                let outcome = ClaimOutcome::Granted {
                    claim: held,
                    at_risk: Vec::new(),
                    amended: false,
                };
                answer(pending.replies, &Reply::Claim { outcome });
            }
        }
        for (claim, server) in adopt {
            self.adopt(claim, server);
            let note = format!(
                "the coordinator holds claim {} for this agent and no request here explains it; \
                 it is now tracked, and `tessel release {}` drops it",
                claim.0, claim.0
            );
            self.notify(NoticeKind::Reconciled, &note, None);
        }
        for (claim, fence) in release_again {
            let held = HeldClaim {
                claim,
                fence,
                expires_at_ms: 0,
                race: None,
                scopes: Vec::new(),
                submitted: false,
                submitted_commit: None,
            };
            self.send_release(&held);
            let note = format!(
                "a release of claim {} was lost with the connection; sent it again",
                claim.0
            );
            self.notify(NoticeKind::Reconciled, &note, None);
        }
        let message = "the connection dropped before the coordinator answered; it did not grant \
                       this claim, so run it again";
        for pending in lost.into_iter().flatten() {
            answer(pending.replies, &refused(None, message));
        }
        self.persist();
    }

    fn adopt(&mut self, claim: ClaimId, server: ServerClaim) -> HeldClaim {
        let held = HeldClaim {
            claim,
            fence: server.fence,
            expires_at_ms: now_ms().saturating_add(self.state.lease_ms.unwrap_or(0)),
            race: server.race,
            scopes: server.scopes,
            submitted: server.submitted,
            submitted_commit: server.submitted_commit,
        };
        self.state.claims.push(held.clone());
        held
    }

    fn on_queued(&mut self, req: RequestId, position: u32) {
        let Some(pending) = self.pending.get_mut(&req.0) else {
            self.unexpected(
                "queue position for an unknown request",
                &ServerMsg::Queued { req, position },
            );
            return;
        };
        pending.queued = true;
        let replies = std::mem::take(&mut pending.replies);
        self.state.queued = Some(QueuedWait {
            req,
            scopes: pending.scopes.clone(),
            position,
        });
        self.notify(
            NoticeKind::WaitQueued,
            &format!("claim queued at position {position}"),
            None,
        );
        self.persist();
        answer(
            replies,
            &Reply::Claim {
                outcome: ClaimOutcome::Queued { position },
            },
        );
    }

    /// The pending submit that `req` (or, from an old coordinator that omits it, `claim`) names.
    fn take_submit(&mut self, req: Option<RequestId>, claim: ClaimId) -> Option<PendingSubmit> {
        let key = match req {
            Some(req) => Some(req.0),
            None => self
                .submits
                .iter()
                .find(|(_, pending)| pending.claim == claim)
                .map(|(key, _)| *key),
        };
        key.and_then(|key| self.submits.remove(&key))
    }

    /// Records that the coordinator holds `claim` as submitted (or, after a rejection, active
    /// again). A submitted claim does not expire; a rejected one gets a fresh lease estimate.
    fn set_submitted(&mut self, claim: ClaimId, submitted: bool, commit: Option<String>) {
        let renewed = now_ms().saturating_add(self.state.lease_ms.unwrap_or(0));
        if let Some(held) = self.state.claims.iter_mut().find(|h| h.claim == claim) {
            held.submitted = submitted;
            held.submitted_commit = if submitted {
                commit.or_else(|| held.submitted_commit.take())
            } else {
                None
            };
            if !submitted {
                held.expires_at_ms = renewed;
            }
        }
        self.fresh.insert(claim);
        self.persist();
    }

    /// A merge is the only thing that moves the diff base: everything up to the merged
    /// submission's commit has landed on main (as rebased copies), so later work is diffed from it.
    fn on_merged(&mut self, claim: ClaimId, msg: ServerMsg) {
        let landed = self
            .state
            .claims
            .iter()
            .find(|held| held.claim == claim)
            .and_then(|held| held.submitted_commit.clone());
        if let Some(commit) = landed {
            self.state.start_base = commit;
        }
        self.state.claims.retain(|held| held.claim != claim);
        self.notify(
            NoticeKind::Merged,
            &format!("claim {} was merged and is no longer held", claim.0),
            Some(msg),
        );
        self.persist();
    }

    fn on_submit_rejected(&mut self, claim: ClaimId, msg: ServerMsg) {
        self.set_submitted(claim, false, None);
        self.notify(
            NoticeKind::SubmitRejected,
            &format!(
                "the merge of claim {} was refused; the claim is active again with the same fence",
                claim.0
            ),
            Some(msg),
        );
    }

    /// The first of `Accepted` and `ReviewRequired` for a claim decides the submit reply, in
    /// whichever order the coordinator sends them; the second only reaches the inbox.
    fn on_accepted(&mut self, req: RequestId, claim: ClaimId, queue_position: u32) {
        let known = self.submits.get(&req.0).map(|pending| pending.claim);
        match known {
            Some(pending_claim) if pending_claim == claim => {}
            Some(_) => {
                let msg = ServerMsg::Accepted {
                    req,
                    claim,
                    queue_position,
                };
                self.unexpected(
                    "acceptance names a different claim than the submission",
                    &msg,
                );
                return;
            }
            None => {
                let already = self
                    .state
                    .claims
                    .iter()
                    .any(|h| h.claim == claim && h.submitted);
                if already {
                    self.log(&format!(
                        "claim {} accepted after its review notice",
                        claim.0
                    ));
                } else {
                    let msg = ServerMsg::Accepted {
                        req,
                        claim,
                        queue_position,
                    };
                    self.unexpected("acceptance for an unknown request", &msg);
                }
                return;
            }
        }
        let commit = self.submits.remove(&req.0).map(|pending| {
            let outcome = SubmitOutcome::Accepted { queue_position };
            let _ = pending.reply.send(Reply::Submit { outcome });
            pending.fork_commit
        });
        self.set_submitted(claim, true, commit);
    }

    fn on_uncovered(
        &mut self,
        req: Option<RequestId>,
        claim: ClaimId,
        scopes: Vec<ScopeClaim>,
        msg: ServerMsg,
    ) {
        if let Some(pending) = self.take_submit(req, claim) {
            let outcome = SubmitOutcome::Uncovered { scopes };
            let _ = pending.reply.send(Reply::Submit { outcome });
        }
        self.notify(
            NoticeKind::Uncovered,
            &format!(
                "the coordinator found scopes touched by claim {} that the claim does not \
                 cover; the claim is not submitted",
                claim.0
            ),
            Some(msg),
        );
    }

    fn on_review_required(
        &mut self,
        claim: ClaimId,
        reasons: Vec<tessel_coordinator::protocol::ReviewReason>,
        msg: ServerMsg,
    ) {
        if let Some(pending) = self.take_submit(None, claim) {
            self.set_submitted(claim, true, Some(pending.fork_commit.clone()));
            let outcome = SubmitOutcome::ReviewRequired { reasons };
            let _ = pending.reply.send(Reply::Submit { outcome });
        }
        self.notify(
            NoticeKind::ReviewRequired,
            &format!(
                "claim {} is held for human review; a reviewer must approve it before it \
                 merges",
                claim.0
            ),
            Some(msg),
        );
    }

    /// The fence to submit `claim` with, or why it cannot be submitted now.
    fn submit_fence(&self, claim: ClaimId) -> Result<tessel_coordinator::protocol::Fence, String> {
        if !self.is_online() {
            let why = self
                .state
                .last_error
                .clone()
                .unwrap_or_else(|| "not welcomed yet".to_string());
            return Err(format!(
                "not connected to the coordinator ({why}); retrying"
            ));
        }
        let Some(held) = self.state.claims.iter().find(|held| held.claim == claim) else {
            return Err(format!("no held claim {}", claim.0));
        };
        if held.submitted {
            return Err(format!("claim {} is already submitted", claim.0));
        }
        Ok(held.fence)
    }

    fn start_submit(
        &mut self,
        claim: ClaimId,
        fork_commit: String,
        touched: Vec<ScopeClaim>,
        decisions: DecisionRecord,
        reply: oneshot::Sender<Reply>,
    ) {
        let fence = match self.submit_fence(claim) {
            Ok(fence) => fence,
            Err(message) => {
                let _ = reply.send(submit_refused(None, &message));
                return;
            }
        };
        let req = self.next_req;
        self.next_req += 1;
        let msg = ClientMsg::Submit {
            req: RequestId(req),
            claim,
            fence,
            fork_commit: CommitId(fork_commit.clone()),
            touched,
            decisions,
        };
        if !self.send(msg) {
            let message = "the connection just dropped; retrying";
            let _ = reply.send(submit_refused(None, message));
            return;
        }
        self.submits.insert(
            req,
            PendingSubmit {
                claim,
                fork_commit,
                reply,
            },
        );
    }

    fn on_error(
        &mut self,
        req: Option<RequestId>,
        code: ErrorCode,
        message: &str,
        msg: &ServerMsg,
    ) {
        if let Some(req) = req {
            if let Some(pending) = self.pending.remove(&req.0) {
                self.notify(
                    NoticeKind::Error,
                    "the coordinator refused a claim",
                    Some(msg.clone()),
                );
                answer(pending.replies, &refused(Some(code), message));
                return;
            }
            if let Some(pending) = self.submits.remove(&req.0) {
                self.notify(
                    NoticeKind::Error,
                    "the coordinator refused a submission",
                    Some(msg.clone()),
                );
                let _ = pending.reply.send(submit_refused(Some(code), message));
                return;
            }
            if let Some(claim) = self.release_reqs.remove(&req.0) {
                let note = format!("the coordinator refused to release claim {}", claim.0);
                self.notify(NoticeKind::Error, &note, Some(msg.clone()));
                return;
            }
        }
        self.log(&format!("coordinator error {code:?}"));
        self.notify(
            NoticeKind::Error,
            "error from the coordinator",
            Some(msg.clone()),
        );
    }

    // ---------- commands from the CLI ----------

    async fn on_command(&mut self, command: Command) -> Flow {
        let Command { request, reply } = command;
        match request {
            Request::Status => {
                let _ = reply.send(Reply::Status {
                    state: Box::new(self.state.clone()),
                });
            }
            request @ (Request::Claim { .. } | Request::Ensure { .. }) => {
                self.handle_claim(request, reply);
            }
            Request::Submit {
                claim,
                fork_commit,
                touched,
                decisions,
            } => self.start_submit(claim, fork_commit, touched, decisions, reply),
            Request::Release { claim } => {
                let _ = reply.send(self.release(claim));
            }
            Request::Stop => {
                let (mut unreleased, submitted, sent) = self.release_all_for_stop();
                unreleased.extend(self.confirm_released(sent).await);
                self.pending.clear();
                self.submits.clear();
                self.deferred.clear();
                self.lost_requests.clear();
                self.close_connection().await;
                let _ = reply.send(Reply::Stopping {
                    unreleased,
                    submitted,
                });
                return Flow::Exit;
            }
        }
        Flow::Continue
    }

    /// Runs a `Claim` or `Ensure` request. While another claim request is waiting for its answer
    /// the request is held back and run when that answer arrives: two requests sent at once
    /// would otherwise both create a claim, or both amend with the same fence.
    fn handle_claim(&mut self, request: Request, reply: oneshot::Sender<Reply>) {
        match request {
            Request::Claim {
                scopes,
                wait,
                assumptions,
                new,
            } => {
                if !new && self.claim_in_flight() {
                    let request = Request::Claim {
                        scopes,
                        wait,
                        assumptions,
                        new,
                    };
                    self.deferred.push_back((request, reply));
                    return;
                }
                self.start_claim(scopes, wait, &assumptions, new, reply);
            }
            Request::Ensure { wanted } => self.ensure(&wanted, reply),
            Request::Status | Request::Release { .. } | Request::Submit { .. } | Request::Stop => {
                let _ = reply.send(refused(None, "not a claim request"));
            }
        }
    }

    fn claim_in_flight(&self) -> bool {
        self.pending.values().any(|pending| !pending.queued)
    }

    fn drain_deferred(&mut self) {
        while !self.claim_in_flight() {
            let Some((request, reply)) = self.deferred.pop_front() else {
                return;
            };
            self.handle_claim(request, reply);
        }
    }

    /// The one claim that new scopes are added to, if the agent holds exactly one open claim.
    fn amend_target(&self) -> Option<(ClaimId, tessel_coordinator::protocol::Fence)> {
        // Amend assumes no race entries (they cannot amend: `RaceScopeFixed`) until races ship.
        let mut open = self.state.claims.iter().filter(|held| !held.submitted);
        let only = open.next()?;
        if open.next().is_some() {
            return None;
        }
        Some((only.claim, only.fence))
    }

    fn ensure(&mut self, wanted: &[ScopeClaim], reply: oneshot::Sender<Reply>) {
        let held: Vec<ScopeClaim> = self
            .state
            .claims
            .iter()
            .flat_map(|held| held.scopes.iter().cloned())
            .collect();
        let missing = uncovered(&held, wanted);
        // Escalation counts only open claims: a submitted claim's scopes leave with its merge.
        let open: Vec<ScopeClaim> = self
            .state
            .claims
            .iter()
            .filter(|held| !held.submitted)
            .flat_map(|held| held.scopes.iter().cloned())
            .collect();
        let wanted = uncovered(&held, &escalate(&open, &missing));
        if wanted.is_empty() {
            let _ = reply.send(Reply::Claim {
                outcome: ClaimOutcome::Covered,
            });
            return;
        }
        let same = self
            .pending
            .values_mut()
            .find(|p| !p.queued && p.scopes == wanted);
        if let Some(pending) = same {
            pending.replies.push(reply);
            return;
        }
        if self.claim_in_flight() {
            self.deferred.push_back((Request::Ensure { wanted }, reply));
            return;
        }
        self.start_claim(wanted, false, &[], false, reply);
    }

    fn start_claim(
        &mut self,
        scopes: Vec<ScopeClaim>,
        wait: bool,
        assumptions: &[String],
        new: bool,
        reply: oneshot::Sender<Reply>,
    ) {
        if !self.is_online() {
            let why = self
                .state
                .last_error
                .clone()
                .unwrap_or_else(|| "not welcomed yet".to_string());
            let message = format!("not connected to the coordinator ({why}); retrying");
            let _ = reply.send(refused(None, &message));
            return;
        }
        let Some(first) = scopes.first() else {
            let _ = reply.send(refused(None, "no scopes to claim"));
            return;
        };
        // An amend cannot carry assumptions or wait, so those make a claim of their own.
        let target = if new || wait || !assumptions.is_empty() {
            None
        } else {
            self.amend_target()
        };
        if let Some((claim, fence)) = target {
            self.start_amend(claim, fence, scopes, reply);
            return;
        }
        let intent = Intent {
            summary: self.state.summary.clone(),
            task_ref: self.state.task_ref.clone(),
            assumptions: assumptions
                .iter()
                .map(|statement| Assumption {
                    scope: first.scope.clone(),
                    statement: statement.clone(),
                })
                .collect(),
        };
        let req = self.next_req;
        self.next_req += 1;
        let msg = ClientMsg::Claim {
            req: RequestId(req),
            intent,
            scopes: scopes.clone(),
            on_conflict: if wait {
                OnConflict::Wait
            } else {
                OnConflict::Fail
            },
        };
        if !self.send(msg) {
            let _ = reply.send(refused(None, "the connection just dropped; retrying"));
            return;
        }
        self.pending.insert(
            req,
            PendingClaim {
                scopes,
                queued: false,
                amend: None,
                replies: vec![reply],
            },
        );
    }

    /// Adds `scopes` to the open claim `claim` with an `Amend`, which is atomic and never waits.
    fn start_amend(
        &mut self,
        claim: ClaimId,
        fence: tessel_coordinator::protocol::Fence,
        scopes: Vec<ScopeClaim>,
        reply: oneshot::Sender<Reply>,
    ) {
        let held: &[ScopeClaim] = self
            .state
            .claims
            .iter()
            .find(|held| held.claim == claim)
            .map_or(&[], |held| held.scopes.as_slice());
        let add: Vec<ScopeClaim> = scopes
            .into_iter()
            .filter(|scope| !held.contains(scope))
            .collect();
        if add.is_empty() {
            let _ = reply.send(Reply::Claim {
                outcome: ClaimOutcome::Covered,
            });
            return;
        }
        let req = self.next_req;
        self.next_req += 1;
        let msg = ClientMsg::Amend {
            req: RequestId(req),
            claim,
            fence,
            add: add.clone(),
        };
        if !self.send(msg) {
            let _ = reply.send(refused(None, "the connection just dropped; retrying"));
            return;
        }
        self.pending.insert(
            req,
            PendingClaim {
                scopes: add,
                queued: false,
                amend: Some(claim),
                replies: vec![reply],
            },
        );
    }

    fn release(&mut self, claim: Option<ClaimId>) -> Reply {
        let targets: Vec<HeldClaim> = self
            .state
            .claims
            .iter()
            .filter(|held| claim.is_none_or(|id| held.claim == id))
            .cloned()
            .collect();
        if let Some(id) = claim.filter(|_| targets.is_empty()) {
            return Reply::Failed {
                message: format!("no held claim {}", id.0),
            };
        }
        if let Some(held) = claim.and_then(|_| targets.iter().find(|held| held.submitted)) {
            return Reply::Failed {
                message: format!(
                    "claim {} is submitted: the coordinator holds it until it merges or is \
                     rejected and refuses to release it; watch `tessel inbox`",
                    held.claim.0
                ),
            };
        }
        let (kept, targets): (Vec<_>, Vec<_>) =
            targets.into_iter().partition(|held| held.submitted);
        let kept_submitted: Vec<ClaimId> = kept.iter().map(|held| held.claim).collect();
        if !targets.is_empty() && !self.is_online() {
            return Reply::Failed {
                message: "not connected to the coordinator; claims stay held until their lease \
                          ends"
                    .to_string(),
            };
        }
        let mut released = Vec::new();
        for held in targets {
            self.send_release(&held);
            self.state.claims.retain(|other| other.claim != held.claim);
            released.push(held.claim);
        }
        self.persist();
        Reply::Released {
            claims: released,
            kept_submitted,
        }
    }

    fn send_release(&mut self, held: &HeldClaim) {
        let req = self.next_req;
        self.next_req += 1;
        self.release_reqs.insert(req, held.claim);
        let _ = self.send(ClientMsg::Release {
            claim: held.claim,
            fence: held.fence,
            req: Some(RequestId(req)),
        });
    }

    /// Releases every unsubmitted claim if online. Returns the claims it could not release, the
    /// submitted ones it left with the coordinator (which refuses to release them), and the
    /// claims whose release it sent, which `confirm_released` then checks.
    fn release_all_for_stop(&mut self) -> (Vec<ClaimId>, Vec<ClaimId>, Vec<ClaimId>) {
        let (submitted, held): (Vec<_>, Vec<_>) = std::mem::take(&mut self.state.claims)
            .into_iter()
            .partition(|claim| claim.submitted);
        let submitted = submitted.iter().map(|claim| claim.claim).collect();
        if !self.is_online() {
            let offline = held.iter().map(|claim| claim.claim).collect();
            return (offline, submitted, Vec::new());
        }
        for claim in &held {
            self.send_release(claim);
        }
        let sent = held.iter().map(|claim| claim.claim).collect();
        (Vec::new(), submitted, sent)
    }

    /// A `Release` has no reply, and closing the socket right after sending it can lose it, so
    /// before the socket closes the coordinator's event log is read until it shows every claim
    /// ended, within `CONFIRM_DEADLINE` in all. Returns the claims it could not confirm released.
    async fn confirm_released(&mut self, sent: Vec<ClaimId>) -> Vec<ClaimId> {
        let mut remaining = sent;
        let deadline = Instant::now() + CONFIRM_DEADLINE;
        for _ in 0..CONFIRM_READS {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            if remaining.is_empty() {
                break;
            }
            let live = tokio::time::timeout(left, self.live_claims_in_log())
                .await
                .unwrap_or(None);
            if let Some(live) = live {
                remaining.retain(|claim| live.contains_key(&claim.0));
            }
            if remaining.is_empty() {
                break;
            }
            tokio::time::sleep(CONFIRM_PAUSE).await;
        }
        remaining
    }

    /// This agent's live claims according to a complete read of the event log, or `None`.
    async fn live_claims_in_log(&self) -> Option<BTreeMap<u64, ServerClaim>> {
        match read_log(&self.config, self.state.base.clone()).await {
            Ok(LogRead {
                events,
                complete: true,
            }) => Some(reconcile::live_claims(
                &AgentId(self.config.agent.clone()),
                &events,
            )),
            Ok(LogRead { .. }) => {
                self.log("stop: the event log was cut short, so releases are unconfirmed");
                None
            }
            Err(why) => {
                self.log(&format!("stop: cannot read the event log: {why}"));
                None
            }
        }
    }

    /// Lets queued messages go out, then closes the socket and waits for its task.
    async fn close_connection(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        let Conn { out, task, .. } = conn;
        drop(out);
        let _ = tokio::time::timeout(CLOSE_GRACE, task).await;
    }

    /// Final bookkeeping on the way out: no socket, no files that say the daemon is alive.
    async fn finish(&mut self) {
        self.close_connection().await;
        self.state.connection = Connection::Stopped;
        self.state.claims.clear();
        self.state.queued = None;
        self.persist();
        for path in [self.worktree.sock(), self.worktree.pid_path()] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => self.log(&format!("cannot remove {}: {e}", path.display())),
            }
        }
        self.log("daemon stopped");
    }
}

fn submit_refused(code: Option<ErrorCode>, message: &str) -> Reply {
    Reply::Submit {
        outcome: SubmitOutcome::Refused {
            code,
            message: message.to_string(),
        },
    }
}

fn refused(code: Option<ErrorCode>, message: &str) -> Reply {
    Reply::Claim {
        outcome: ClaimOutcome::Refused {
            code,
            message: message.to_string(),
        },
    }
}

fn answer(replies: Vec<oneshot::Sender<Reply>>, reply: &Reply) {
    for tx in replies {
        let _ = tx.send(reply.clone());
    }
}

async fn sleep_until_some(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

// ---------- WebSocket ----------

pub(crate) async fn open_socket(config: &Config) -> Result<Socket, ConnectFailure> {
    let url = config.socket_url();
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| ConnectFailure::Fatal(format!("bad coordinator URL: {e}")))?;
    let mut bearer = HeaderValue::from_str(&format!("Bearer {}", config.token.expose()))
        .map_err(|_| ConnectFailure::Fatal("TESSEL_TOKEN is not a valid header value".into()))?;
    bearer.set_sensitive(true);
    request.headers_mut().insert(AUTHORIZATION, bearer);
    let attempt = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request));
    match attempt.await {
        Err(_) => Err(ConnectFailure::Transient(format!(
            "no answer from {} within {CONNECT_TIMEOUT:?}",
            config.coordinator
        ))),
        Ok(Ok((socket, _response))) => Ok(socket),
        Ok(Err(WsError::Http(response))) => {
            let status = response.status();
            let message = format!(
                "the coordinator refused the connection with HTTP {status}; check \
                 TESSEL_TOKEN, TESSEL_REPO and TESSEL_COORDINATOR"
            );
            if matches!(status.as_u16(), 400 | 401 | 403 | 404) {
                Err(ConnectFailure::Fatal(message))
            } else {
                Err(ConnectFailure::Transient(message))
            }
        }
        // Only the error kind is shown: a handshake error must never echo request headers.
        Ok(Err(e)) => Err(ConnectFailure::Transient(format!("cannot connect: {e}"))),
    }
}

/// Reads the whole event log on a short-lived second connection. `Watch { from_seq: 0 }`
/// replays the log and then follows it live; the replay has no end of its own. So the read then
/// sends `Hello`, which the coordinator handles after the replay is done (its Durable Object
/// takes one message at a time while it reads storage; see `watch` in the Worker). Its `Welcome`
/// reply is sent before the events it caused, so the first event after the `Welcome` must be the
/// `AgentConnected` that `Hello` logged: it comes after every event the replay held. Any other
/// first event, a missing marker, a closed socket, `SNAPSHOT_LIMIT` or a gap in `seq` marks the
/// read truncated.
///
/// This depends on the coordinator never running the `Hello` mid-replay. If it did, the read
/// would see a gap-free prefix of the log with a marker at its end, which none of these checks
/// can tell from the whole log.
///
/// The read connection is bound to the agent while it lives, so the coordinator withdraws a
/// queued wait only when the last of the agent's sockets closes; `on_closed` aborts this task
/// so that it is never the last.
async fn snapshot_task(
    config: Config,
    base: String,
    tx: mpsc::UnboundedSender<Incoming>,
    generation: u64,
) {
    let log = tokio::time::timeout(
        CONNECT_TIMEOUT + SNAPSHOT_LIMIT * 2,
        read_log(&config, base),
    )
    .await
    .unwrap_or_else(|_| Err("the event log read hung".to_string()));
    let _ = tx.send(Incoming::Snapshot { generation, log });
}

pub(crate) async fn read_log(config: &Config, base: String) -> Result<LogRead, String> {
    let me = AgentId(config.agent.clone());
    let mut socket = open_socket(config).await.map_err(|failure| match failure {
        ConnectFailure::Fatal(message) | ConnectFailure::Transient(message) => message,
    })?;
    for msg in [
        ClientMsg::Watch { from_seq: 0 },
        ClientMsg::Hello {
            agent: me.clone(),
            base: CommitId(base),
            protocol: PROTOCOL_VERSION,
        },
    ] {
        let text = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
        socket
            .send(Message::text(text))
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut events = Vec::new();
    let mut welcomed = false;
    let mut marker = false;
    let deadline = Instant::now() + SNAPSHOT_LIMIT;
    while !marker {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        match tokio::time::timeout(left, socket.next()).await {
            // A closed or failed socket mid-read leaves a truncated log, not an unreadable one.
            Err(_) | Ok(None | Some(Err(_))) => break,
            Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ServerMsg>(&text) {
                Ok(ServerMsg::Event { event }) if welcomed => {
                    // The first event after the Welcome is the marker or the read is not whole.
                    marker = is_connection_of(&event, &me);
                    events.push(event);
                    if !marker {
                        break;
                    }
                }
                Ok(ServerMsg::Event { event }) => events.push(event),
                Ok(ServerMsg::Welcome { .. }) => welcomed = true,
                Ok(ServerMsg::Error { code, .. }) => return Err(format!("refused: {code:?}")),
                Ok(_) => {}
                Err(e) => return Err(format!("unreadable message: {e}")),
            },
            Ok(Some(Ok(_))) => {}
        }
    }
    let _ = socket.close(None).await;
    let gapless = events
        .iter()
        .zip(0u64..)
        .all(|(event, seq)| event.seq == seq);
    Ok(LogRead {
        complete: marker && gapless,
        events,
    })
}

fn is_connection_of(event: &Event, agent: &AgentId) -> bool {
    match &event.kind {
        EventKind::AgentConnected { agent: who } => who == agent,
        EventKind::ClaimGranted { .. }
        | EventKind::ClaimDenied { .. }
        | EventKind::ClaimShadowed { .. }
        | EventKind::ClaimAmended { .. }
        | EventKind::ClaimReleased { .. }
        | EventKind::WaitQueued { .. }
        | EventKind::WaitWithdrawn { .. }
        | EventKind::Submitted { .. }
        | EventKind::Merged { .. }
        | EventKind::SubmitRejected { .. }
        | EventKind::ReviewRequested { .. }
        | EventKind::ReviewDecided { .. }
        | EventKind::BaseMoved { .. }
        | EventKind::AssumptionChallenged { .. }
        | EventKind::RaceOpened { .. }
        | EventKind::RaceDecided { .. }
        | EventKind::DenialVerified { .. }
        | EventKind::AssumptionVerified { .. }
        | EventKind::ReplayMerged { .. } => false,
    }
}

/// Owns the socket: forwards frames to the daemon and daemon messages to the socket. Ends when
/// the socket closes or the daemon drops its sender, and then reports `Closed`.
async fn socket_task(
    mut socket: Socket,
    mut out_rx: mpsc::UnboundedReceiver<ClientMsg>,
    in_tx: mpsc::UnboundedSender<Incoming>,
    generation: u64,
) {
    let reason = loop {
        tokio::select! {
            frame = socket.next() => {
                if let Some(reason) = forward_frame(frame, &in_tx, generation) {
                    break reason;
                }
            },
            outgoing = out_rx.recv() => {
                if let Some(msg) = outgoing {
                    let Ok(text) = serde_json::to_string(&msg) else { continue };
                    if let Err(e) = socket.send(Message::text(text)).await {
                        break format!("send failed: {e}");
                    }
                } else {
                    let _ = socket.close(None).await;
                    break "closed locally".to_string();
                }
            },
        }
    };
    let _ = in_tx.send(Incoming::Closed { generation, reason });
}

/// Passes a text frame to the daemon. Returns why the socket is over, if this frame ended it.
fn forward_frame(
    frame: Option<Result<Message, WsError>>,
    in_tx: &mpsc::UnboundedSender<Incoming>,
    generation: u64,
) -> Option<String> {
    match frame {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMsg>(&text) {
            Ok(msg) => {
                let _ = in_tx.send(Incoming::Msg { generation, msg });
                None
            }
            Err(e) => Some(format!("unreadable message from the coordinator: {e}")),
        },
        Some(Ok(Message::Close(_))) | None => Some("closed by the coordinator".to_string()),
        Some(Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {
            None
        }
        Some(Err(e)) => Some(format!("socket error: {e}")),
    }
}

// ---------- CLI connections ----------

async fn accept_loop(
    listener: UnixListener,
    cmd_tx: mpsc::Sender<Command>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut handlers = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    handlers.spawn(serve_connection(stream, cmd_tx.clone()));
                }
            }
            Some(_) = handlers.join_next(), if !handlers.is_empty() => {}
            _ = shutdown.changed() => break,
        }
    }
    while handlers.join_next().await.is_some() {}
}

async fn serve_connection(stream: UnixStream, cmd_tx: mpsc::Sender<Command>) {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let mut reader = BufReader::new(read).take(rpc::MAX_LINE_BYTES as u64);
    let reply = match reader.read_line(&mut line).await {
        Ok(0) | Err(_) => return,
        Ok(_) => match serde_json::from_str::<Request>(&line) {
            Ok(request) => dispatch(request, &cmd_tx).await,
            Err(e) => Reply::Failed {
                message: format!("unreadable request: {e}"),
            },
        },
    };
    if let Ok(mut bytes) = serde_json::to_vec(&reply) {
        bytes.push(b'\n');
        let _ = write.write_all(&bytes).await;
    }
}

async fn dispatch(request: Request, cmd_tx: &mpsc::Sender<Command>) -> Reply {
    let (reply_tx, reply_rx) = oneshot::channel();
    if cmd_tx
        .send(Command {
            request,
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return Reply::Failed {
            message: "the daemon is stopping".to_string(),
        };
    }
    reply_rx.await.unwrap_or_else(|_| Reply::Failed {
        message: "the daemon stopped before answering".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_coordinator::protocol::Fence;

    fn held(claim: u64) -> HeldClaim {
        HeldClaim {
            claim: ClaimId(claim),
            fence: Fence(claim),
            expires_at_ms: u64::MAX,
            race: None,
            scopes: Vec::new(),
            submitted: false,
            submitted_commit: None,
        }
    }

    fn daemon_in(dir: &std::path::Path) -> Daemon {
        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-q"])
            .status();
        assert!(init.is_ok_and(|status| status.success()));
        let worktree = Worktree::discover(dir).unwrap();
        worktree.prepare_dir().unwrap();
        let config = Config::load(dir, |name| match name {
            "TESSEL_COORDINATOR" => Some("ws://127.0.0.1:1".to_string()),
            "TESSEL_REPO" => Some("demo".to_string()),
            "TESSEL_AGENT" => Some("a1".to_string()),
            "TESSEL_TOKEN" => Some("tok".to_string()),
            _ => None,
        })
        .unwrap();
        let state = State {
            pid: 1,
            agent: "a1".into(),
            repo: "demo".into(),
            summary: String::new(),
            task_ref: None,
            start_base: String::new(),
            coordinator_head: None,
            base: String::new(),
            socket: String::new(),
            connection: Connection::Online,
            lease_ms: None,
            last_error: None,
            claims: vec![held(1), held(2)],
            queued: None,
            updated_at_ms: 0,
        };
        let (in_tx, _in_rx) = mpsc::unbounded_channel();
        Daemon::new(worktree, config, state, in_tx)
    }

    #[test]
    fn an_accepted_for_another_claim_does_not_answer_or_submit_the_pending_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let (reply, mut answer_rx) = oneshot::channel();
        daemon.submits.insert(
            5,
            PendingSubmit {
                claim: ClaimId(1),
                fork_commit: "f".into(),
                reply,
            },
        );

        daemon.on_accepted(RequestId(5), ClaimId(2), 4);
        assert!(
            answer_rx.try_recv().is_err(),
            "answered by a mismatched Accepted"
        );
        assert!(daemon.submits.contains_key(&5));
        assert!(daemon.state.claims.iter().all(|held| !held.submitted));

        daemon.on_accepted(RequestId(5), ClaimId(1), 3);
        let answered = answer_rx.try_recv().unwrap();
        assert!(
            matches!(
                answered,
                Reply::Submit {
                    outcome: SubmitOutcome::Accepted { queue_position: 3 }
                }
            ),
            "{answered:?}"
        );
        assert!(daemon.state.claims[0].submitted);
        assert!(!daemon.state.claims[1].submitted);
    }
}

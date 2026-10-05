//! The per-worktree daemon. It holds the one WebSocket to the coordinator, sends heartbeats,
//! reconnects with backoff, answers CLI commands on a Unix socket and keeps `.tessel/` current.
//!
//! One task owns all state. Reader/writer work for the socket runs in a helper task that
//! exchanges messages with the owner through channels, so no lock is ever held across an await.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    uncovered, Assumption, ClaimId, ClientMsg, CommitId, ErrorCode, Intent, Mode, OnConflict,
    RequestId, Scope, ScopeClaim, ServerMsg, PROTOCOL_VERSION,
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
use crate::rpc::{self, ClaimOutcome, Reply, Request};
use crate::state::{append_notice, Connection, HeldClaim, Notice, NoticeKind, QueuedWait, State};
use crate::worktree::Worktree;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(1);
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// Until the coordinator's `Welcome` says otherwise.
const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(10);

type Socket =
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
    Msg { generation: u64, msg: ServerMsg },
    Closed { generation: u64, reason: String },
}

struct Conn {
    out: mpsc::UnboundedSender<ClientMsg>,
    task: JoinHandle<()>,
}

struct PendingClaim {
    scopes: Vec<ScopeClaim>,
    sent_at_ms: u64,
    /// The coordinator answered `Queued`; the grant will arrive later.
    queued: bool,
    replies: Vec<oneshot::Sender<Reply>>,
}

/// Why a connection attempt failed.
enum ConnectFailure {
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
    in_tx: mpsc::UnboundedSender<Incoming>,
    backoff: Duration,
    reconnect_at: Option<Instant>,
    next_heartbeat: Instant,
    heartbeat_every: Duration,
    next_housekeeping: Instant,
    ever_online: bool,
}

/// Runs the daemon for `worktree` until `tessel stop` or a fatal error. Returns `Ok` without
/// doing anything if another daemon already serves this worktree.
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
    let sock = worktree.sock();
    if rpc::call(&sock, &Request::Status, Duration::from_secs(1))
        .await
        .is_ok()
    {
        log(
            &worktree,
            &config,
            "another daemon already serves this worktree; exiting",
        );
        return Ok(());
    }
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
        base,
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
        let hello = ClientMsg::Hello {
            agent: tessel_coordinator::protocol::AgentId(self.config.agent.clone()),
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
            .partition(|held| held.expires_at_ms > now);
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
            }
            Incoming::Closed { generation, reason } if generation == self.generation => {
                self.on_closed(&reason);
            }
            Incoming::Msg { .. } | Incoming::Closed { .. } => {}
        }
    }

    fn on_closed(&mut self, reason: &str) {
        self.conn = None;
        self.log(&format!("connection closed: {reason}"));
        self.state.last_error = Some(reason.to_string());
        for (_, pending) in std::mem::take(&mut self.pending) {
            if pending.queued {
                self.notify(
                    NoticeKind::WaitWithdrawn,
                    "connection lost while a --wait claim was queued; the coordinator withdraws \
                     it, claim again after reconnecting",
                    None,
                );
                continue;
            }
            let message = "connection lost before the coordinator answered; run `tessel status` \
                           to see what is held";
            answer(pending.replies, &refused(None, message));
        }
        self.state.queued = None;
        self.schedule_reconnect();
        self.persist();
    }

    fn on_server_msg(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome { lease_ms, .. } => self.on_welcome(lease_ms),
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
            ServerMsg::BaseMoved { .. } => {
                self.notify(
                    NoticeKind::BaseMoved,
                    "main moved under your claims",
                    Some(msg),
                );
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
            ServerMsg::Shadowed { .. }
            | ServerMsg::Accepted { .. }
            | ServerMsg::Merged { .. }
            | ServerMsg::SubmitRejected { .. }
            | ServerMsg::Uncovered { .. }
            | ServerMsg::ReviewRequired { .. }
            | ServerMsg::RaceOpened { .. }
            | ServerMsg::RaceResult { .. }
            | ServerMsg::Event { .. } => self.unexpected("message this CLI does not use", &msg),
        }
    }

    fn unexpected(&self, note: &str, msg: &ServerMsg) {
        self.notify(NoticeKind::Unexpected, note, Some(msg.clone()));
    }

    fn on_welcome(&mut self, lease_ms: u64) {
        self.ever_online = true;
        self.backoff = FIRST_BACKOFF;
        self.state.connection = Connection::Online;
        self.state.lease_ms = Some(lease_ms);
        self.state.last_error = None;
        self.heartbeat_every = Duration::from_millis((lease_ms / 3).max(1));
        self.next_heartbeat = Instant::now() + self.heartbeat_every;
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
        let held = HeldClaim {
            claim,
            fence,
            expires_at_ms: pending.sent_at_ms.saturating_add(lease),
            race,
            scopes: pending.scopes,
        };
        self.state.claims.push(held.clone());
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
                },
            },
        );
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
            Request::Claim {
                scopes,
                wait,
                assumptions,
            } => {
                self.start_claim(scopes, wait, &assumptions, reply);
            }
            Request::Ensure { path, create } => self.ensure(&path, create, reply),
            Request::Release { claim } => {
                let _ = reply.send(self.release(claim));
            }
            Request::Stop => {
                self.release_all_for_stop();
                self.pending.clear();
                self.close_connection().await;
                let _ = reply.send(Reply::Stopping);
                return Flow::Exit;
            }
        }
        Flow::Continue
    }

    fn ensure(&mut self, path: &str, create: bool, reply: oneshot::Sender<Reply>) {
        let mode = if create { Mode::Create } else { Mode::EditBody };
        let wanted = ScopeClaim {
            scope: Scope::File {
                path: path.to_string(),
            },
            mode,
        };
        let held: Vec<ScopeClaim> = self
            .state
            .claims
            .iter()
            .flat_map(|held| held.scopes.iter().cloned())
            .collect();
        if uncovered(&held, std::slice::from_ref(&wanted)).is_empty() {
            let _ = reply.send(Reply::Claim {
                outcome: ClaimOutcome::Covered,
            });
            return;
        }
        let same = self
            .pending
            .values_mut()
            .find(|p| !p.queued && p.scopes == [wanted.clone()]);
        if let Some(pending) = same {
            pending.replies.push(reply);
            return;
        }
        self.start_claim(vec![wanted], false, &[], reply);
    }

    fn start_claim(
        &mut self,
        scopes: Vec<ScopeClaim>,
        wait: bool,
        assumptions: &[String],
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
                sent_at_ms: now_ms(),
                queued: false,
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
        Reply::Released { claims: released }
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

    fn release_all_for_stop(&mut self) {
        let held = std::mem::take(&mut self.state.claims);
        if self.is_online() {
            for claim in &held {
                self.send_release(claim);
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

async fn open_socket(config: &Config) -> Result<Socket, ConnectFailure> {
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

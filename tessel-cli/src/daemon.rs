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
    uncovered, AgentId, Assumption, ClaimId, ClaimInDoubt, ClientMsg, CommitId, DecisionRecord,
    ErrorCode, Event, EventKind, Intent, OnConflict, ProtocolRange, RequestId, ScopeClaim,
    ServerMsg, PROTOCOL_VERSION,
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
use crate::notes::{self, Step};
use crate::plan::escalate;
use crate::reconcile::{self, Local, Plan, ServerClaim};
use crate::rpc::{self, ClaimOutcome, Reply, Request, SubmitOutcome};
use crate::state::{
    append_notice, Connection, HeldClaim, NoteJob, Notice, NoticeKind, QueuedWait, State,
};
use crate::worktree::{Worktree, WorktreeError};

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
/// How long a socket may stay open without a `Welcome` before the link is declared dead: the
/// silence limit (3/2 of a heartbeat) that the default heartbeat would give, since the lease is
/// not known until the `Welcome` arrives.
const WELCOME_LIMIT: Duration = Duration::from_secs(15);
/// How long one write to the socket may block, on a half-open link whose send buffer is full,
/// before the link is declared dead. Without it the silence timer would never be polled.
const WRITE_LIMIT: Duration = Duration::from_secs(15);
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
    /// A frame this daemon cannot decode arrived on the live socket.
    SkippedFrame {
        generation: u64,
        envelope: UnknownEnvelope,
    },
    /// A pong echoed the heartbeat written at `sent_ms`.
    HeartbeatAnswered {
        generation: u64,
        sent_ms: u64,
    },
    /// The note copies started by `start_notes` finished, each with what became of it.
    NotesDone {
        results: Vec<(NoteJob, Step)>,
    },
}

/// What the event-log read returned.
pub(crate) struct LogRead {
    pub(crate) events: Vec<Event>,
    /// The read reached its end marker, with no gap in `seq`. Skipped frames do not make it
    /// incomplete: an event kind added after this daemon was built is not a claim event it knows,
    /// and refusing to reconcile because of one would disable reconciliation for good.
    pub(crate) complete: bool,
    /// Frames this daemon could not decode. They are not in `events`.
    pub(crate) unknown: Vec<UnknownEnvelope>,
    /// The highest `seq` read, skipped events included.
    pub(crate) last_seq: Option<u64>,
}

/// How much of an unknown frame's kind or type is kept for the log; it is the coordinator's text.
const KIND_LOG_CHARS: usize = 64;

/// A frame this daemon cannot decode as a `ServerMsg`: an event or message added after this
/// daemon was built, or one whose payload differs from what this daemon expects. Only its `type`
/// and, for an event, its `seq` and kind are read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UnknownEnvelope {
    /// Present for an event, whose `seq` the replay still counts.
    pub(crate) seq: Option<u64>,
    /// The event's kind, or for any other message its `type`.
    pub(crate) kind: Option<String>,
}

impl std::fmt::Display for UnknownEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.seq, &self.kind) {
            (Some(seq), Some(kind)) => write!(f, "unknown event seq {seq} kind {kind:?}"),
            (Some(seq), None) => write!(f, "unknown event seq {seq} without a kind string"),
            (None, Some(kind)) => write!(f, "unknown message type {kind:?}"),
            (None, None) => write!(f, "unknown message"),
        }
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(KIND_LOG_CHARS).collect()
}

/// The `type` of every `ServerMsg` this daemon can decode. A test checks it against the enum.
const KNOWN_MESSAGE_TYPES: &[&str] = &[
    "welcome",
    "granted",
    "denied",
    "shadowed",
    "queued",
    "accepted",
    "merged",
    "merge_doubt",
    "submit_rejected",
    "uncovered",
    "review_required",
    "base_moved",
    "assumption_challenged",
    "lease_expired",
    "race_opened",
    "race_result",
    "event",
    "error",
];

/// The kind of every `EventKind` this daemon can decode. A test checks it against the enum.
const KNOWN_EVENT_KINDS: &[&str] = &[
    "agent_connected",
    "claim_granted",
    "claim_denied",
    "claim_shadowed",
    "claim_amended",
    "claim_released",
    "wait_queued",
    "wait_withdrawn",
    "submitted",
    "merged",
    "submit_rejected",
    "review_requested",
    "review_decided",
    "base_moved",
    "assumption_challenged",
    "race_opened",
    "race_decided",
    "denial_verified",
    "assumption_verified",
    "verification_dropped",
    "replay_merged",
];

/// Reads `text` as a frame to skip: a JSON object with a string `type` that this daemon has no
/// name for, or an event of a kind it has no name for. An event must also carry a numeric `seq`,
/// because skipping one without counting it would hide a gap.
///
/// A name this daemon knows with a payload it cannot parse is `None`, as is text that is not
/// JSON, not an object, or has no `type`: skipping it could drop a grant, a release or a lease
/// end this daemon acts on, so it stays an error.
fn unknown_envelope(text: &str) -> Option<UnknownEnvelope> {
    if serde_json::from_str::<ServerMsg>(text).is_ok() {
        return None;
    }
    let frame: serde_json::Value = serde_json::from_str(text).ok()?;
    let kind = frame.get("type")?.as_str()?;
    if kind != "event" {
        if KNOWN_MESSAGE_TYPES.contains(&kind) {
            return None;
        }
        return Some(UnknownEnvelope {
            seq: None,
            kind: Some(bounded(kind)),
        });
    }
    let event = frame.get("event")?;
    let seq = event.get("seq")?.as_u64()?;
    let kind = event.get("event").and_then(serde_json::Value::as_str);
    if kind.is_some_and(|kind| KNOWN_EVENT_KINDS.contains(&kind)) {
        return None;
    }
    Some(UnknownEnvelope {
        seq: Some(seq),
        kind: kind.map(bounded),
    })
}

/// What the daemon asks the socket task to write.
enum Outgoing {
    Msg(ClientMsg),
    /// A heartbeat, with a ping behind it. The link is dead if nothing at all arrives within
    /// `silence_limit`.
    Heartbeat {
        silence_limit: Duration,
    },
}

struct Conn {
    out: mpsc::UnboundedSender<Outgoing>,
    task: JoinHandle<()>,
}

struct PendingClaim {
    scopes: Vec<ScopeClaim>,
    /// The coordinator answered `Queued`; the grant will arrive later.
    queued: bool,
    /// The held claim this request adds its scopes to, when it is an `Amend`.
    amend: Option<ClaimId>,
    /// The `--assume` statements sent with the request; the held claim keeps them for the note.
    assumptions: Vec<String>,
    /// Claims released here while the request was on the way. A grant for one of them is not
    /// held: the release already reached the coordinator.
    released_meanwhile: Vec<ClaimId>,
    /// A live claim in the event log that this request is expected to be answered with, found
    /// before the grant arrived. A reply that names another claim, or none, leaves it unexplained.
    awaited: Option<(ClaimId, ServerClaim)>,
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

/// Landings kept waiting for their commits or the notes ref; older ones are given up.
const MAX_PENDING_NOTES: usize = 32;

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
    /// Frames skipped in the last event-log read.
    log_skips: u64,
    /// Skipped live events the last read did not cover (they came after its end marker).
    live_event_skips: Vec<u64>,
    /// The highest `seq` of the last read; a live skip at or below it is already in `log_skips`.
    read_last_seq: Option<u64>,
    /// Skipped live messages that are not events; the log never holds them.
    message_skips: u64,
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
    /// When the link is declared dead if the current socket has not been welcomed by then.
    welcome_deadline: Option<Instant>,
    welcome_limit: Duration,
    /// `sent_ms` of the newest heartbeat a pong answered, kept for the lapse log line.
    last_pong_sent_ms: Option<u64>,
    ever_online: bool,
    /// A batch of note copies is running on a blocking thread; at most one is.
    notes_running: bool,
    /// A retry was asked for while a batch ran; one more batch starts when it ends.
    notes_again: bool,
    /// Jobs whose waiting has been logged, so a retry does not log it again.
    waiting_logged: Vec<NoteJob>,
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
    let prior = State::read(&worktree)?;
    let persisted = prior
        .as_ref()
        .map(|prior| prior.start_base.clone())
        .filter(|pinned| !pinned.is_empty());
    let pending_notes = prior.map(|prior| prior.pending_notes).unwrap_or_default();
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
        skipped_frames: 0,
        protocol_warning: None,
        pending_notes,
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

/// Takes the worktree's daemon lock, or returns `None` when another daemon holds it.
///
/// The lock is a POSIX record lock, not `flock`. `flock` locks the open file description, and a
/// child the daemon forks (git, a hook) shares it until the child execs, so a daemon that
/// exited could still refuse its successor for that window. A record lock belongs to the process
/// and a fork child does not inherit it. Its two hazards cannot arise here: one process runs one
/// daemon (`commands.rs` calls `run` once), and nothing else opens `daemon.lock`, so no second
/// holder takes the lock inside this process and no other descriptor closes it early.
fn acquire_lock(worktree: &Worktree) -> anyhow::Result<Option<std::fs::File>> {
    let path = worktree.lock_path();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match held_by_flock(&file) {
        Ok(true) => return Ok(None),
        Ok(false) => {}
        Err(e) => return Err(e).with_context(|| format!("cannot lock {}", path.display())),
    }
    match try_record_lock(&file) {
        Ok(true) => Ok(Some(file)),
        Ok(false) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot lock {}", path.display())),
    }
}

/// Whether a daemon of an older `tessel`, which held `daemon.lock` with `flock`, is still running.
///
/// On Linux a `flock` and a record lock do not exclude each other, so after an upgrade a daemon
/// from the new binary could start beside one from the old binary that is still alive. Nothing
/// else in `daemon.lock` names that daemon, so the probe takes the `flock` itself: it fails only
/// while another open file description holds it. A held lock is given back at once, so the probe
/// leaves nothing for a forked child to keep. It runs before the record lock because on macOS
/// the two kinds exclude each other and the probe would otherwise meet this process's own lock.
fn held_by_flock(file: &std::fs::File) -> std::io::Result<bool> {
    match file.try_lock() {
        Ok(()) => file.unlock().map(|()| false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Takes a write lock over the whole file without waiting. `Ok(false)` means another process
/// holds it.
fn try_record_lock(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;

    // SAFETY: an all-zero `flock` is a valid value (zero integers). Zero in `l_whence`, `l_start`
    // and `l_len` is `SEEK_SET`, offset 0, length 0, which locks the whole file; only the lock
    // type is set below.
    let mut request: libc::flock = unsafe { std::mem::zeroed() };
    request.l_type = libc::c_short::try_from(libc::F_WRLCK)
        .map_err(|source| std::io::Error::new(std::io::ErrorKind::InvalidInput, source))?;
    // SAFETY: `fcntl` with `F_SETLK` reads `request`, which is live for the call, and the
    // descriptor is open for as long as `file` is borrowed.
    let status = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &raw mut request) };
    if status == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EACCES | libc::EAGAIN) => Ok(false),
        Some(_) | None => Err(error),
    }
}

fn restrict_permissions(sock: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot restrict {}", sock.display()))
}

/// Appends a timestamped line to `daemon.log`, with the token removed.
/// What a daemon outside the coordinator's protocol range says, in its log and in `tessel status`.
fn protocol_warning(range: ProtocolRange) -> String {
    let speaks = if range.min == range.max {
        format!("v{}", range.min)
    } else {
        format!("v{} to v{}", range.min, range.max)
    };
    format!(
        "the coordinator speaks protocol {speaks} and this daemon speaks v{PROTOCOL_VERSION}; it \
         connects anyway and may misread what it is sent, so update the older of the two"
    )
}

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
            log_skips: 0,
            live_event_skips: Vec::new(),
            read_last_seq: None,
            message_skips: 0,
            fresh: HashSet::new(),
            snapshot: None,
            in_tx,
            backoff: FIRST_BACKOFF,
            reconnect_at: Some(now),
            next_heartbeat: now + DEFAULT_HEARTBEAT,
            heartbeat_every: DEFAULT_HEARTBEAT,
            next_housekeeping: now + HOUSEKEEPING_EVERY,
            welcome_deadline: None,
            welcome_limit: WELCOME_LIMIT,
            last_pong_sent_ms: None,
            ever_online: false,
            notes_running: false,
            notes_again: false,
            waiting_logged: Vec::new(),
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
        self.start_notes();
        loop {
            let online = self.is_online();
            let reconnect_at = self.reconnect_at;
            let welcome_at = self.welcome_deadline;
            tokio::select! {
                Some(command) = cmd_rx.recv() => {
                    if let Flow::Exit = self.on_command(command).await {
                        return Ok(());
                    }
                }
                Some(incoming) = in_rx.recv() => self.on_incoming(incoming).await,
                () = tokio::time::sleep_until(self.next_heartbeat), if online => {
                    self.send_heartbeat();
                }
                () = sleep_until_some(reconnect_at), if reconnect_at.is_some() => {
                    self.try_connect().await?;
                }
                () = sleep_until_some(welcome_at), if welcome_at.is_some() => {
                    self.welcome_overdue();
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
                self.start_conn(socket).await;
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

    async fn start_conn(&mut self, socket: Socket) {
        let head = self.read_head().await;
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(socket_task(
            socket,
            out_rx,
            self.in_tx.clone(),
            self.generation,
        ));
        match head {
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
        let _ = out_tx.send(Outgoing::Msg(hello));
        self.conn = Some(Conn { out: out_tx, task });
        self.welcome_deadline = Some(Instant::now() + self.welcome_limit);
        self.log("connected; hello sent");
    }

    /// `git rev-parse HEAD`, run on a blocking thread so the loop and the socket task keep
    /// running while git is slow.
    async fn read_head(&self) -> Result<String, WorktreeError> {
        let worktree = self.worktree.clone();
        match tokio::task::spawn_blocking(move || worktree.head()).await {
            Ok(head) => head,
            Err(e) => Err(WorktreeError::Git {
                args: "rev-parse HEAD".into(),
                message: e.to_string(),
            }),
        }
    }

    /// The socket opened but no `Welcome` came, so heartbeats never started: drop it and
    /// reconnect with the usual backoff. The task is aborted so that a stalled write cannot keep
    /// it alive.
    fn welcome_overdue(&mut self) {
        if let Some(conn) = &self.conn {
            conn.task.abort();
        }
        // A `Closed` the aborted task may still have queued belongs to the old generation.
        self.generation += 1;
        let reason = format!(
            "no Welcome within {:?} of the handshake; link dead",
            self.welcome_limit
        );
        self.on_closed(&reason);
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
        self.write(Outgoing::Msg(msg))
    }

    /// Hands `outgoing` to the socket task. A task that is gone (it ended without reporting
    /// `Closed`, as a panic does) has closed the connection: run the close path now.
    fn write(&mut self, outgoing: Outgoing) -> bool {
        let Some(conn) = &self.conn else {
            return false;
        };
        if conn.out.send(outgoing).is_ok() {
            return true;
        }
        // A `Closed` the dead task may still have queued belongs to the old generation.
        self.generation += 1;
        self.on_closed("the socket task ended without closing the connection");
        false
    }

    fn send_heartbeat(&mut self) {
        self.next_heartbeat = Instant::now() + self.heartbeat_every;
        // A link is declared dead lease/2 after an unanswered heartbeat, before the local expiry
        // lapses.
        let silence_limit = self.heartbeat_every * 3 / 2;
        self.write(Outgoing::Heartbeat { silence_limit });
    }

    /// Moves the local expiry only when a pong echoes the heartbeat, measured from when it was
    /// written: the coordinator renewed no earlier than that. The transport may answer pings
    /// itself, so a pong proves delivery, not that the host persisted the renewal;
    /// a failed persist closes the socket, and the local expiry may then run ahead by at most one
    /// heartbeat interval.
    fn on_heartbeat_answered(&mut self, sent_ms: u64) {
        let renewed = sent_ms.saturating_add(self.state.lease_ms.unwrap_or(0));
        self.last_pong_sent_ms = Some(sent_ms);
        let rtt_ms = now_ms().saturating_sub(sent_ms);
        if u128::from(rtt_ms) > self.heartbeat_every.as_millis() {
            self.log(&format!(
                "slow pong: heartbeat {sent_ms} answered after {rtt_ms} ms"
            ));
        }
        for held in &mut self.state.claims {
            held.expires_at_ms = held.expires_at_ms.max(renewed);
        }
        self.persist();
    }

    fn housekeeping(&mut self) -> Flow {
        let tick_late_ms = Instant::now()
            .saturating_duration_since(self.next_housekeeping)
            .as_millis();
        self.next_housekeeping = Instant::now() + HOUSEKEEPING_EVERY;
        if !self.worktree.dir().exists() {
            return Flow::Exit;
        }
        let now = now_ms();
        // While the link is up the coordinator renews every claim on each heartbeat and says so
        // when a lease ends (`LeaseExpired`), so a local expiry would only drop a live claim.
        let online = self.is_online();
        let (live, lapsed): (Vec<_>, Vec<_>) = std::mem::take(&mut self.state.claims)
            .into_iter()
            .partition(|held| online || held.submitted || held.expires_at_ms > now);
        self.state.claims = live;
        for held in lapsed {
            self.log(&format!(
                "claim {} lapsed locally: expiry {} now {now} last pong for heartbeat {:?} \
                 tick late {tick_late_ms} ms",
                held.claim.0, held.expires_at_ms, self.last_pong_sent_ms
            ));
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

    async fn on_incoming(&mut self, incoming: Incoming) {
        let retry_notes = matches!(
            incoming,
            Incoming::Msg {
                msg: ServerMsg::Merged { .. } | ServerMsg::BaseMoved { .. },
                ..
            } | Incoming::Snapshot { .. }
        );
        self.handle_incoming(incoming).await;
        if retry_notes {
            self.start_notes();
        }
    }

    async fn handle_incoming(&mut self, incoming: Incoming) {
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
                self.on_snapshot(log).await;
            }
            Incoming::SkippedFrame {
                generation,
                envelope,
            } if generation == self.generation => {
                match envelope.seq {
                    Some(seq) if self.read_last_seq.is_some_and(|last| seq <= last) => {}
                    Some(seq) => self.live_event_skips.push(seq),
                    None => self.message_skips += 1,
                }
                self.refresh_skipped();
                self.log(&format!("skipped {envelope} on the live socket"));
            }
            Incoming::HeartbeatAnswered {
                generation,
                sent_ms,
            } if generation == self.generation => {
                self.on_heartbeat_answered(sent_ms);
            }
            Incoming::NotesDone { results } => self.finish_notes(results),
            Incoming::Msg { .. }
            | Incoming::Closed { .. }
            | Incoming::Snapshot { .. }
            | Incoming::SkippedFrame { .. }
            | Incoming::HeartbeatAnswered { .. } => {}
        }
    }

    fn on_closed(&mut self, reason: &str) {
        self.conn = None;
        self.welcome_deadline = None;
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
            ServerMsg::Welcome {
                head,
                lease_ms,
                protocol_range,
                merge_doubts,
                ..
            } => {
                self.on_welcome(head.0, lease_ms, protocol_range);
                self.apply_welcome_doubts(&merge_doubts);
            }
            ServerMsg::Granted {
                req,
                claim,
                fence,
                race,
                ref at_risk,
                ref head,
                ..
            } => {
                if let Some(head) = head {
                    self.state.coordinator_head = Some(head.0.clone());
                    self.persist();
                }
                match self.pending.remove(&req.0) {
                    Some(mut pending) => {
                        let awaited = pending.awaited.take();
                        self.settle_awaited(awaited, Some(claim));
                        self.on_granted(pending, claim, fence, race, at_risk.clone());
                    }
                    None => self.unexpected("grant for an unknown request", &msg),
                }
            }
            ServerMsg::Denied { req, conflicts } => self.on_denied(req, conflicts),
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
            ServerMsg::Merged {
                claim,
                ref head,
                ref base,
            } => {
                self.state.coordinator_head = Some(head.0.clone());
                let (head, base) = (head.0.clone(), base.as_ref().map(|base| base.0.clone()));
                self.queue_notes_of_submission(claim, base, head);
                self.on_merged(claim, msg);
            }
            ServerMsg::MergeDoubt { claim, since_ms } => self.on_merge_doubt(claim, since_ms),
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
            msg @ ServerMsg::RaceResult { .. } => self.on_race_result(msg),
            // `RaceOpened` answers an `OpenRace`, which `tessel race open` sends on a connection
            // of its own, never through the daemon.
            ServerMsg::Shadowed { .. } | ServerMsg::RaceOpened { .. } | ServerMsg::Event { .. } => {
                self.unexpected("message this CLI does not use", &msg);
            }
        }
    }

    /// A race this agent entered or opened was ranked or decided: an inbox notice that says where
    /// this agent's entry placed. The coordinator sends each losing entry a `SubmitRejected` after.
    fn on_race_result(&self, msg: ServerMsg) {
        let ServerMsg::RaceResult {
            race,
            winner,
            ref ranking,
            ref entries,
        } = msg
        else {
            return;
        };
        let me = AgentId(self.config.agent.clone());
        let note = crate::race::result_note(race, winner, ranking, entries, &me);
        self.notify(NoticeKind::RaceResult, &note, Some(msg));
    }

    fn unexpected(&self, note: &str, msg: &ServerMsg) {
        self.notify(NoticeKind::Unexpected, note, Some(msg.clone()));
    }

    fn on_welcome(&mut self, head: String, lease_ms: u64, range: Option<ProtocolRange>) {
        self.note_protocol_range(range);
        self.state.coordinator_head = Some(head);
        self.ever_online = true;
        self.backoff = FIRST_BACKOFF;
        self.state.connection = Connection::Online;
        self.welcome_deadline = None;
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

    /// Sets or clears each claim's merge doubt from what `Welcome` lists, on every connection.
    /// A claim listed keeps the local time it already has, or gets now; one not listed loses it.
    fn apply_welcome_doubts(&mut self, doubts: &[ClaimInDoubt]) {
        for held in &mut self.state.claims {
            let listed = doubts.iter().any(|doubt| doubt.claim == held.claim);
            held.merge_doubt_since_ms =
                listed.then(|| held.merge_doubt_since_ms.unwrap_or_else(now_ms));
        }
        self.persist();
    }

    /// Warns, once for as long as the same range is announced, when the coordinator's range does
    /// not include this daemon's version. The daemon connects anyway. A coordinator that announces
    /// no range, or one that includes this version, clears the warning.
    fn note_protocol_range(&mut self, range: Option<ProtocolRange>) {
        let warning = range
            .filter(|range| !range.contains(PROTOCOL_VERSION))
            .map(protocol_warning);
        if warning == self.state.protocol_warning {
            return;
        }
        if let Some(text) = &warning {
            self.log(&format!("warning: {text}"));
        }
        self.state.protocol_warning = warning;
    }

    /// Records a granted claim. A log read that finished between the coordinator's grant and its
    /// arrival here has already adopted the claim; the grant, being the answer to this agent's own
    /// request, replaces that copy (its lease, scopes and assumptions) rather than adding a second.
    fn replace_or_push(&mut self, held: HeldClaim) {
        match self
            .state
            .claims
            .iter_mut()
            .find(|existing| existing.claim == held.claim)
        {
            Some(existing) => *existing = held,
            None => self.state.claims.push(held),
        }
    }

    /// A request that was waiting for a claim the log showed has been answered. If the reply is
    /// not a grant of that claim, nothing else explains it, so it is tracked and reported.
    fn settle_awaited(
        &mut self,
        awaited: Option<(ClaimId, ServerClaim)>,
        granted: Option<ClaimId>,
    ) {
        let Some((claim, server)) = awaited else {
            return;
        };
        if granted == Some(claim) || self.state.claims.iter().any(|held| held.claim == claim) {
            return;
        }
        self.adopt_unexplained(claim, server);
        self.persist();
    }

    fn adopt_unexplained(&mut self, claim: ClaimId, server: ServerClaim) {
        self.adopt(claim, server);
        let note = format!(
            "the coordinator holds claim {} for this agent and no request here explains it; \
             it is now tracked, and `tessel release {}` drops it",
            claim.0, claim.0
        );
        self.notify(NoticeKind::Reconciled, &note, None);
    }

    /// Answers a grant for a claim this agent released while its request was on the way. A new
    /// claim's release was sent with the logged fence, which is the grant's, so nothing more is
    /// sent. An amend moved the fence before the release arrived and the coordinator refuses a
    /// release with the old one, so the release is sent again with the grant's fence.
    fn refuse_released_grant(
        &mut self,
        pending: PendingClaim,
        claim: ClaimId,
        fence: tessel_coordinator::protocol::Fence,
    ) {
        if pending.queued {
            self.state.queued = None;
            self.persist();
        }
        let message = if pending.amend.is_some() {
            self.release_by_id(claim, fence);
            format!(
                "claim {} was released here while an amend of it was on the way; the amend was \
                 granted, so its release was sent again with the new fence. Claim again to get \
                 it back",
                claim.0
            )
        } else {
            format!(
                "claim {} was released here before its grant arrived; its release was sent when \
                 it was released, and the claim is not held here. Claim again to get it back",
                claim.0
            )
        };
        answer(pending.replies, &refused(None, &message));
    }

    fn on_granted(
        &mut self,
        pending: PendingClaim,
        claim: ClaimId,
        fence: tessel_coordinator::protocol::Fence,
        race: Option<tessel_coordinator::protocol::RaceId>,
        at_risk: Vec<tessel_coordinator::protocol::HeldAssumption>,
    ) {
        if pending.released_meanwhile.contains(&claim) {
            self.refuse_released_grant(pending, claim, fence);
            return;
        }
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
                awaiting_review: false,
                merge_doubt_since_ms: None,
                assumptions: pending.assumptions,
            };
            self.replace_or_push(held.clone());
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
                head: None,
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

    /// Puts the skipped-frame total, which `tessel status` shows, into the saved state.
    fn refresh_skipped(&mut self) {
        let live = u64::try_from(self.live_event_skips.len()).unwrap_or(u64::MAX);
        self.state.skipped_frames = self
            .log_skips
            .saturating_add(live)
            .saturating_add(self.message_skips);
        self.persist();
    }

    /// Compares this agent's claims in the coordinator's log with the local ones, and repairs
    /// the difference. The rules are in `reconcile`.
    async fn on_snapshot(&mut self, log: Result<LogRead, String>) {
        let lost = std::mem::take(&mut self.lost_requests);
        let lost_releases = std::mem::take(&mut self.lost_releases);
        let LogRead {
            events,
            complete,
            unknown,
            last_seq,
        } = match log {
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
        self.log_skips = u64::try_from(unknown.len()).unwrap_or(u64::MAX);
        self.read_last_seq = last_seq;
        self.live_event_skips
            .retain(|seq| last_seq.is_none_or(|last| *seq > last));
        self.refresh_skipped();
        for envelope in &unknown {
            self.log(&format!("skipped {envelope} in the event log"));
        }
        let live = reconcile::live_claims(&AgentId(self.config.agent.clone()), &events);
        self.advance_base_from_log(&events).await;
        self.log(&format!(
            "event log read: {} events ({}), {} live claims for this agent",
            events.len(),
            if complete { "complete" } else { "TRUNCATED" },
            live.len()
        ));
        let scopes: Vec<Vec<ScopeClaim>> = lost.iter().map(|p| p.scopes.clone()).collect();
        let (in_flight_keys, in_flight): (Vec<u64>, Vec<Vec<ScopeClaim>>) = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.amend.is_none())
            .map(|(key, pending)| (*key, pending.scopes.clone()))
            .unzip();
        let local = Local {
            claims: &self.state.claims,
            fresh: &self.fresh,
            lost_requests: &scopes,
            in_flight: &in_flight,
            lost_releases: &lost_releases,
            complete,
        };
        let plan = reconcile::plan(&local, &live, &events);
        if complete {
            self.apply_plan(plan, lost, &in_flight_keys);
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
        self.apply_plan(plan, Vec::new(), &in_flight_keys);
    }

    /// Moves the diff base forward to the newest of this agent's submissions that the log shows
    /// merged, for a `Merged` that arrived while the daemon was offline or stopped. A submitted
    /// claim can only end by merging, and only a commit that descends from the base moves it.
    async fn advance_base_from_log(&mut self, events: &[Event]) {
        let me = AgentId(self.config.agent.clone());
        let root = self.worktree.root.clone();
        let mut moved = false;
        for landed in reconcile::landed_submissions(&me, events) {
            let commit = landed.commit.clone();
            let ahead = commit != self.state.start_base
                && is_ancestor_off_loop(&root, &self.state.start_base, &commit).await;
            if ahead {
                self.queue_notes(landed.claim, Some(commit.clone()), landed.base, landed.head);
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
                if held.submitted != submitted {
                    held.merge_doubt_since_ms = None;
                }
                held.submitted = submitted;
                held.submitted_commit = commit;
                if !submitted {
                    held.awaiting_review = false;
                }
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

    fn apply_plan(&mut self, plan: Plan, lost: Vec<PendingClaim>, in_flight_keys: &[u64]) {
        let Plan {
            forget,
            refresh,
            rescope,
            set_submitted,
            set_awaiting_review,
            answer_lost,
            await_reply,
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
        for (claim, awaiting) in set_awaiting_review {
            self.set_awaiting_review(claim, awaiting);
            self.log(&format!(
                "claim {} is {} review according to the event log",
                claim.0,
                if awaiting {
                    "awaiting"
                } else {
                    "no longer awaiting"
                }
            ));
        }
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
        for (index, claim, server) in await_reply {
            self.log(&format!(
                "claim {} in the event log answers a request still on the way; its reply will \
                 track it",
                claim.0
            ));
            if let Some(pending) = in_flight_keys
                .get(index)
                .and_then(|key| self.pending.get_mut(key))
            {
                pending.awaited = Some((claim, server));
            }
        }
        for (claim, server) in adopt {
            self.adopt_unexplained(claim, server);
        }
        for (claim, fence) in release_again {
            self.release_by_id(claim, fence);
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
            awaiting_review: server.awaiting_review,
            merge_doubt_since_ms: None,
            assumptions: server.assumptions,
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
            held.merge_doubt_since_ms = None;
            held.awaiting_review = false;
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

    /// Records whether `claim` waits for a reviewer, so `status` and the stop hook can tell a
    /// claim in the merge queue from one a human still has to approve.
    fn set_awaiting_review(&mut self, claim: ClaimId, awaiting: bool) {
        if let Some(held) = self.state.claims.iter_mut().find(|h| h.claim == claim) {
            held.awaiting_review = awaiting;
        }
        self.fresh.insert(claim);
        self.persist();
    }

    /// A landing of the submission held for `claim`: its commits' notes are to follow the commits
    /// the lander made from them. Must run before `on_merged` forgets the claim.
    fn queue_notes_of_submission(&mut self, claim: ClaimId, base: Option<String>, head: String) {
        let source = self
            .state
            .claims
            .iter()
            .find(|held| held.claim == claim)
            .and_then(|held| held.submitted_commit.clone());
        self.queue_notes(claim, source, base, head);
    }

    /// Queues the copy of the notes of `source` (the submitted commit) onto `base..head`. A
    /// landing that names no base (an older coordinator, or a span the coordinator does not know)
    /// copies nothing: `base..head` would be unbounded.
    fn queue_notes(
        &mut self,
        claim: ClaimId,
        source: Option<String>,
        base: Option<String>,
        head: String,
    ) {
        let Some(source) = source else { return };
        let Some(base) = base else {
            self.log(&format!(
                "claim {} landed without a base, so no notes were copied",
                claim.0
            ));
            return;
        };
        let job = NoteJob {
            claim,
            source,
            floor: Some(self.state.start_base.clone()).filter(|floor| !floor.is_empty()),
            base,
            head,
        };
        if self.state.pending_notes.contains(&job) {
            return;
        }
        self.state.pending_notes.push(job);
        let excess = self
            .state
            .pending_notes
            .len()
            .saturating_sub(MAX_PENDING_NOTES);
        if excess > 0 {
            self.state.pending_notes.drain(..excess);
            self.log(&format!(
                "{excess} landings waited too long for their notes and were given up"
            ));
        }
        self.persist();
    }

    /// Starts the queued note copies on a blocking thread, unless a batch is already running (then
    /// one more follows it). The batch reports back as `Incoming::NotesDone`, so the loop keeps
    /// serving the socket and commands meanwhile. Called on `Merged`, `BaseMoved`, a reconnect's
    /// log read, every command and at start, not on a heartbeat.
    fn start_notes(&mut self) {
        if self.state.pending_notes.is_empty() {
            return;
        }
        if self.notes_running {
            self.notes_again = true;
            return;
        }
        self.notes_running = true;
        let jobs = self.state.pending_notes.clone();
        let root = self.worktree.root.clone();
        let tx = self.in_tx.clone();
        tokio::spawn(async move {
            let batch = tokio::task::spawn_blocking(move || {
                jobs.into_iter()
                    .map(|job| {
                        let step = notes::copy_notes(&root, &job);
                        (job, step)
                    })
                    .collect()
            });
            let results = batch.await.unwrap_or_default();
            let _ = tx.send(Incoming::NotesDone { results });
        });
    }

    /// A batch ended. A job that finished or was refused leaves the queue; one still waiting for
    /// its commits or the notes ref stays, and is logged the first time only. `state.json` is
    /// written only when the queue changed.
    fn finish_notes(&mut self, results: Vec<(NoteJob, Step)>) {
        self.notes_running = false;
        let mut changed = false;
        for (job, step) in results {
            if let Step::Waiting(_) = step {
                if !self.waiting_logged.contains(&job) {
                    self.log(&notes::describe(&job, &step));
                    self.waiting_logged.push(job);
                }
                continue;
            }
            let line = notes::describe(&job, &step);
            self.log(&line);
            let clean =
                matches!(&step, Step::Done(report) if report.not_landed + report.ambiguous == 0);
            if !clean {
                self.notify(NoticeKind::Reconciled, &line, None);
            }
            self.state.pending_notes.retain(|queued| *queued != job);
            self.waiting_logged.retain(|logged| *logged != job);
            changed = true;
        }
        if changed {
            self.persist();
        }
        if std::mem::take(&mut self.notes_again) {
            self.start_notes();
        }
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

    /// The coordinator cannot say whether the landing of `claim` took effect (`since_ms` set), or
    /// says it is over. The age shown in status counts from when this daemon was told, on its own
    /// clock; a daemon that was offline then is not told later.
    fn on_merge_doubt(&mut self, claim: ClaimId, since_ms: Option<u64>) {
        let Some(held) = self
            .state
            .claims
            .iter_mut()
            .find(|held| held.claim == claim)
        else {
            return;
        };
        let begun = since_ms.map(|_| held.merge_doubt_since_ms.unwrap_or_else(now_ms));
        held.merge_doubt_since_ms = begun;
        self.persist();
    }

    /// The first of `Accepted` and `ReviewRequired` for a claim decides the submit reply; the
    /// second only reaches the inbox. The core sends `ReviewRequired` before `Accepted` and never
    /// the other way round. On approval, `Accepted` finds no pending submit and clears the hold.
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
                    self.set_awaiting_review(claim, false);
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
        self.set_awaiting_review(claim, true);
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

    fn on_denied(
        &mut self,
        req: RequestId,
        conflicts: Vec<tessel_coordinator::protocol::Conflict>,
    ) {
        let msg = ServerMsg::Denied {
            req,
            conflicts: conflicts.clone(),
        };
        self.notify(NoticeKind::Denied, "claim denied", Some(msg));
        if let Some(mut pending) = self.pending.remove(&req.0) {
            let awaited = pending.awaited.take();
            self.settle_awaited(awaited, None);
            answer(
                pending.replies,
                &Reply::Claim {
                    outcome: ClaimOutcome::Denied { conflicts },
                },
            );
        }
    }

    fn on_error(
        &mut self,
        req: Option<RequestId>,
        code: ErrorCode,
        message: &str,
        msg: &ServerMsg,
    ) {
        if let Some(req) = req {
            if let Some(mut pending) = self.pending.remove(&req.0) {
                let awaited = pending.awaited.take();
                self.settle_awaited(awaited, None);
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
        self.start_notes();
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
            Request::JoinRace { race, scopes } => self.start_join(race, scopes, reply),
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
            Request::Status
            | Request::Release { .. }
            | Request::JoinRace { .. }
            | Request::Submit { .. }
            | Request::Stop => {
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

    /// The one claim that new scopes are added to, if the agent holds exactly one open claim
    /// besides its race entries.
    fn amend_target(&self) -> Option<(ClaimId, tessel_coordinator::protocol::Fence)> {
        // A race entry holds exactly the race's scopes and cannot amend (`RaceScopeFixed`), so
        // new scopes go to the one other open claim, or to a claim of their own.
        let mut open = self
            .state
            .claims
            .iter()
            .filter(|held| !held.submitted && held.race.is_none());
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
                assumptions: assumptions.to_vec(),
                released_meanwhile: Vec::new(),
                awaited: None,
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
                assumptions: Vec::new(),
                released_meanwhile: Vec::new(),
                awaited: None,
                replies: vec![reply],
            },
        );
    }

    /// Enters `race` with a `JoinRace`, which never waits. The grant does not repeat the race's
    /// scopes, so the entry is recorded with `scopes`, which the command read from the race's
    /// `RaceOpened` event.
    fn start_join(
        &mut self,
        race: tessel_coordinator::protocol::RaceId,
        scopes: Vec<ScopeClaim>,
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
        if scopes.is_empty() {
            let _ = reply.send(refused(None, "the race names no scopes"));
            return;
        }
        let req = self.next_req;
        self.next_req += 1;
        let msg = ClientMsg::JoinRace {
            req: RequestId(req),
            race,
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
                assumptions: Vec::new(),
                released_meanwhile: Vec::new(),
                awaited: None,
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
            for pending in self.pending.values_mut() {
                pending.released_meanwhile.push(held.claim);
            }
            released.push(held.claim);
        }
        self.persist();
        Reply::Released {
            claims: released,
            kept_submitted,
        }
    }

    fn release_by_id(&mut self, claim: ClaimId, fence: tessel_coordinator::protocol::Fence) {
        let held = HeldClaim {
            claim,
            fence,
            expires_at_ms: 0,
            race: None,
            scopes: Vec::new(),
            submitted: false,
            submitted_commit: None,
            awaiting_review: false,
            merge_doubt_since_ms: None,
            assumptions: Vec::new(),
        };
        self.send_release(&held);
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
                ..
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

/// `submit::is_ancestor` on a blocking thread. A git that cannot run, or a task that fails,
/// answers `false`, as `is_ancestor` does for a commit it cannot find.
async fn is_ancestor_off_loop(root: &std::path::Path, ancestor: &str, descendant: &str) -> bool {
    let (root, ancestor, descendant) = (
        root.to_path_buf(),
        ancestor.to_string(),
        descendant.to_string(),
    );
    tokio::task::spawn_blocking(move || crate::submit::is_ancestor(&root, &ancestor, &descendant))
        .await
        .unwrap_or(false)
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
/// sends `Hello`, which the coordinator handles after the replay is done (its host runs no other
/// input while it reads storage; see the input gate in `src/host/mod.rs`). Its `Welcome`
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
    let mut unknown = Vec::new();
    let mut seqs = Vec::new();
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
                    seqs.push(event.seq);
                    events.push(event);
                    if !marker {
                        break;
                    }
                }
                Ok(ServerMsg::Event { event }) => {
                    seqs.push(event.seq);
                    events.push(event);
                }
                Ok(ServerMsg::Welcome { .. }) => welcomed = true,
                Ok(ServerMsg::Error { code, .. }) => return Err(format!("refused: {code:?}")),
                Ok(_) => {}
                Err(e) => {
                    let Some(envelope) = unknown_envelope(&text) else {
                        return Err(format!("unreadable message: {e}"));
                    };
                    seqs.extend(envelope.seq);
                    unknown.push(envelope);
                }
            },
            Ok(Some(Ok(_))) => {}
        }
    }
    let _ = socket.close(None).await;
    let gapless = seqs.iter().zip(0u64..).all(|(seen, seq)| *seen == seq);
    Ok(LogRead {
        complete: marker && gapless,
        events,
        unknown,
        last_seq: seqs.iter().copied().max(),
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
        | EventKind::VerificationDropped { .. }
        | EventKind::ReplayMerged { .. } => false,
    }
}

/// Heartbeats written on one socket whose pong has not come back. The link is declared dead when
/// the oldest of them has gone unanswered for its silence limit, however many other frames arrive
/// meanwhile: only a pong echoing a ping proves the heartbeats get through.
#[derive(Default)]
struct Probes {
    /// `sent_ms` of each unanswered heartbeat, which is also the payload of its ping, with the
    /// instant at which an unanswered heartbeat declares the link dead.
    outstanding: Vec<(u64, Instant)>,
}

impl Probes {
    fn sent(&mut self, sent_ms: u64, silence_limit: Duration) {
        self.outstanding
            .push((sent_ms, Instant::now() + silence_limit));
    }

    /// When the link is declared dead: the deadline of the oldest unanswered heartbeat.
    fn silent_from(&self) -> Option<Instant> {
        self.outstanding.first().map(|&(_, deadline)| deadline)
    }

    /// When a write must have finished: `WRITE_LIMIT` from now, or the silence deadline if that
    /// comes first, so a blocked write never delays declaring the link dead.
    fn write_deadline(&self) -> Instant {
        let limit = Instant::now() + WRITE_LIMIT;
        self.silent_from().map_or(limit, |silent| silent.min(limit))
    }

    /// The heartbeat a pong proves delivered, if its payload echoes an outstanding ping. That
    /// heartbeat and every older one, by position in the order sent, are answered; an unknown
    /// payload proves nothing.
    fn answered_by(&mut self, payload: &[u8]) -> Option<u64> {
        let sent_ms = u64::from_be_bytes(payload.try_into().ok()?);
        let position = self
            .outstanding
            .iter()
            .position(|&(pending, _)| pending == sent_ms)?;
        self.outstanding.drain(..=position);
        Some(sent_ms)
    }
}

/// Owns the socket: forwards frames to the daemon and daemon messages to the socket. Ends when
/// the socket closes, the daemon drops its sender or a heartbeat goes unanswered for its silence
/// limit, and then reports `Closed`.
async fn socket_task(
    mut socket: Socket,
    mut out_rx: mpsc::UnboundedReceiver<Outgoing>,
    in_tx: mpsc::UnboundedSender<Incoming>,
    generation: u64,
) {
    let mut probes = Probes::default();
    let reason = loop {
        tokio::select! {
            frame = socket.next() => {
                if let Some(Ok(Message::Pong(payload))) = &frame {
                    if let Some(sent_ms) = probes.answered_by(payload) {
                        let _ = in_tx.send(Incoming::HeartbeatAnswered { generation, sent_ms });
                    }
                }
                if let Some(reason) = forward_frame(frame, &in_tx, generation) {
                    break reason;
                }
            },
            outgoing = out_rx.recv() => match outgoing {
                Some(Outgoing::Msg(msg)) => {
                    let Ok(text) = serde_json::to_string(&msg) else { continue };
                    let write = socket.send(Message::text(text));
                    if let Err(reason) = within_write_limit(probes.write_deadline(), write).await {
                        break reason;
                    }
                }
                Some(Outgoing::Heartbeat { silence_limit }) => {
                    let sent_ms = now_ms();
                    let write = write_heartbeat(&mut socket, sent_ms);
                    if let Err(reason) = within_write_limit(probes.write_deadline(), write).await {
                        break reason;
                    }
                    probes.sent(sent_ms, silence_limit);
                }
                None => {
                    let _ = within_write_limit(probes.write_deadline(), socket.close(None)).await;
                    break "closed locally".to_string();
                }
            },
            () = sleep_until_some(probes.silent_from()), if probes.silent_from().is_some() => {
                break "no pong from the coordinator since a heartbeat; link dead".to_string();
            },
        }
    };
    let _ = in_tx.send(Incoming::Closed { generation, reason });
}

/// Runs one write, ending the link if it fails or is still blocked at `deadline`.
async fn within_write_limit(
    deadline: Instant,
    write: impl std::future::Future<Output = Result<(), WsError>>,
) -> Result<(), String> {
    match tokio::time::timeout_at(deadline, write).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("send failed: {e}")),
        Err(_) => Err("a write did not finish by its deadline; link dead".to_string()),
    }
}

/// The heartbeat, then a ping carrying `sent_ms`, whose pong names the heartbeat it answers.
async fn write_heartbeat(socket: &mut Socket, sent_ms: u64) -> Result<(), WsError> {
    let text = serde_json::to_string(&ClientMsg::Heartbeat).map_err(|e| WsError::Io(e.into()))?;
    socket.send(Message::text(text)).await?;
    socket
        .send(Message::Ping(sent_ms.to_be_bytes().to_vec().into()))
        .await
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
            Err(e) => {
                let Some(envelope) = unknown_envelope(&text) else {
                    return Some(format!("unreadable message from the coordinator: {e}"));
                };
                let _ = in_tx.send(Incoming::SkippedFrame {
                    generation,
                    envelope,
                });
                None
            }
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

    fn envelope(kind: &str) -> String {
        format!(
            r#"{{"type":"event","event":{{"seq":7,"at_ms":1,"run":"r","event":"{kind}","x":1}}}}"#
        )
    }

    #[test]
    fn a_known_server_message_is_not_an_unknown_envelope() {
        let known = r#"{"type":"event","event":{"seq":7,"at_ms":1,"run":"r",
            "event":"agent_connected","agent":"a1"}}"#;
        assert!(serde_json::from_str::<ServerMsg>(known).is_ok());
        assert!(unknown_envelope(known).is_none());
    }

    #[test]
    fn an_event_of_an_unknown_kind_is_an_envelope_with_its_seq_and_kind() {
        let found = unknown_envelope(&envelope("claim_teleported"));
        assert_eq!(
            found,
            Some(UnknownEnvelope {
                seq: Some(7),
                kind: Some("claim_teleported".to_string()),
            })
        );
    }

    #[test]
    fn an_unknown_event_without_a_kind_string_still_has_its_seq() {
        let found = unknown_envelope(r#"{"type":"event","event":{"seq":3,"event":9}}"#);
        assert_eq!(
            found,
            Some(UnknownEnvelope {
                seq: Some(3),
                kind: None
            })
        );
    }

    #[test]
    fn the_kind_is_cut_short_so_a_hostile_one_cannot_flood_the_log() {
        let found = unknown_envelope(&envelope(&"k".repeat(500)));
        assert_eq!(found.and_then(|e| e.kind).map(|k| k.len()), Some(64));
    }

    #[test]
    fn objects_without_a_numeric_seq_are_not_envelopes() {
        for text in [
            r#"{"type":"event","event":{"event":"x"}}"#,
            r#"{"type":"event","event":{"seq":"7","event":"x"}}"#,
            r#"{"type":"event","event":{"seq":-1,"event":"x"}}"#,
            r#"{"type":"event","event":{"seq":1.5,"event":"x"}}"#,
            r#"{"type":"event","event":7}"#,
            r#"{"type":"event"}"#,
            r#"{"seq":4}"#,
            "[]",
            "7",
        ] {
            assert_eq!(unknown_envelope(text), None, "{text}");
        }
    }

    #[test]
    fn a_message_of_an_unknown_type_is_skipped_by_its_bounded_type_alone() {
        let found = unknown_envelope(r#"{"type":"from_the_future","seq":4,"x":"do as I say"}"#);
        assert_eq!(
            found,
            Some(UnknownEnvelope {
                seq: None,
                kind: Some("from_the_future".to_string()),
            })
        );
        let long = format!(r#"{{"type":"{}"}}"#, "t".repeat(500));
        assert_eq!(
            unknown_envelope(&long)
                .and_then(|e| e.kind)
                .map(|k| k.len()),
            Some(64)
        );
    }

    /// The names serde lists in its error for a tag it does not know: every variant of the enum.
    fn variants_named_by_serde<T: serde::de::DeserializeOwned>(unknown: &str) -> Vec<String> {
        let error = serde_json::from_str::<T>(unknown).err();
        let message = error.expect("an unknown tag decoded").to_string();
        let (_, listed) = message.split_once("expected one of ").expect(&message);
        let listed = listed.split(" at line").next().unwrap_or(listed);
        listed
            .split(", ")
            .map(|name| name.trim_matches('`').to_string())
            .collect()
    }

    #[test]
    fn every_server_message_type_and_event_kind_is_in_the_known_lists() {
        let mut types = variants_named_by_serde::<ServerMsg>(r#"{"type":"zzz"}"#);
        types.sort();
        let mut known: Vec<String> = KNOWN_MESSAGE_TYPES.iter().map(|t| (*t).into()).collect();
        known.sort();
        assert_eq!(types, known);
        let mut kinds = variants_named_by_serde::<EventKind>(r#"{"event":"zzz"}"#);
        kinds.sort();
        let mut known: Vec<String> = KNOWN_EVENT_KINDS.iter().map(|t| (*t).into()).collect();
        known.sort();
        assert_eq!(kinds, known);
    }

    #[test]
    fn a_merge_doubt_frame_reaching_a_daemon_without_the_name_is_skipped_and_counted() {
        // A daemon built before `merge_doubt` has no name for it: with its known list as it was,
        // the frame is an unknown envelope, which the daemon skips and counts, not an error.
        let frame = r#"{"type":"merge_doubt","claim":7,"since_ms":5}"#;
        let known_then: Vec<&str> = KNOWN_MESSAGE_TYPES
            .iter()
            .copied()
            .filter(|name| *name != "merge_doubt")
            .collect();
        let frame_value: serde_json::Value = serde_json::from_str(frame).unwrap();
        let kind = frame_value["type"].as_str().unwrap();
        assert!(
            !known_then.contains(&kind),
            "the old daemon has no name for it"
        );
        assert!(
            serde_json::from_str::<ServerMsg>(frame.replace("merge_doubt", "zzz").as_str())
                .is_err(),
            "and cannot decode it"
        );
        let skipped =
            unknown_envelope(&frame.replace("merge_doubt", "merge_doubt_from_the_future"));
        assert_eq!(
            skipped,
            Some(UnknownEnvelope {
                seq: None,
                kind: Some("merge_doubt_from_the_future".to_string())
            }),
            "a type with no name is skipped and named for the count"
        );
        assert!(ServerMsg::MergeDoubt {
            claim: ClaimId(7),
            since_ms: Some(5)
        }
        .skippable());
    }

    #[test]
    fn a_known_name_with_a_payload_that_does_not_parse_is_not_skipped() {
        for text in [
            r#"{"type":"welcome"}"#,
            r#"{"type":"granted"}"#,
            r#"{"type":"lease_expired","claim":"x"}"#,
            r#"{"type":"error","code":"a_code_from_the_future","message":"m"}"#,
            r#"{"type":"event","event":{"seq":3,"at_ms":1,"run":"r","event":"claim_released"}}"#,
            r#"{"type":"event","event":{"seq":0,"at_ms":1,"run":"r","event":"agent_connected"}}"#,
        ] {
            assert_eq!(unknown_envelope(text), None, "{text}");
        }
    }

    #[test]
    fn frames_without_a_string_type_are_not_skipped() {
        for text in [r#"{"type":7}"#, r#"{"n":1}"#, "{}"] {
            assert_eq!(unknown_envelope(text), None, "{text}");
        }
    }

    #[test]
    fn text_that_is_not_json_is_not_an_envelope() {
        for text in ["", "not json", r#"{"type":"event","event":{"seq":1"#] {
            assert_eq!(unknown_envelope(text), None, "{text:?}");
        }
    }

    fn held(claim: u64) -> HeldClaim {
        HeldClaim {
            claim: ClaimId(claim),
            fence: Fence(claim),
            expires_at_ms: u64::MAX,
            race: None,
            scopes: Vec::new(),
            submitted: false,
            submitted_commit: None,
            awaiting_review: false,
            merge_doubt_since_ms: None,
            assumptions: Vec::new(),
        }
    }

    fn daemon_in(dir: &std::path::Path) -> Daemon {
        daemon_for(dir, "ws://127.0.0.1:1").0
    }

    fn daemon_for(
        dir: &std::path::Path,
        coordinator: &str,
    ) -> (Daemon, mpsc::UnboundedReceiver<Incoming>) {
        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-q"])
            .status();
        assert!(init.is_ok_and(|status| status.success()));
        let worktree = Worktree::discover(dir).unwrap();
        worktree.prepare_dir().unwrap();
        let config = Config::load(dir, |name| match name {
            "TESSEL_COORDINATOR" => Some(coordinator.to_string()),
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
            skipped_frames: 0,
            protocol_warning: None,
            pending_notes: Vec::new(),
            claims: vec![held(1), held(2)],
            queued: None,
            updated_at_ms: 0,
        };
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        (Daemon::new(worktree, config, state, in_tx), in_rx)
    }

    fn go_online(daemon: &mut Daemon) {
        let (out, _out_rx) = mpsc::unbounded_channel();
        daemon.conn = Some(Conn {
            out,
            task: tokio::spawn(async {}),
        });
    }

    /// Like `go_online`, with the receiving end kept so that sending does not fail.
    fn go_online_listening(daemon: &mut Daemon) -> mpsc::UnboundedReceiver<Outgoing> {
        let (out, out_rx) = mpsc::unbounded_channel();
        daemon.conn = Some(Conn {
            out,
            task: tokio::spawn(async {}),
        });
        out_rx
    }

    #[tokio::test]
    async fn an_adopted_claim_keeps_the_review_flag_the_log_showed() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let server = |awaiting_review| ServerClaim {
            fence: Fence(9),
            scopes: Vec::new(),
            race: None,
            submitted: true,
            submitted_commit: Some("f".into()),
            awaiting_review,
            assumptions: vec!["b is pure".into()],
        };
        assert!(daemon.adopt(ClaimId(7), server(true)).awaiting_review);
        assert!(!daemon.adopt(ClaimId(8), server(false)).awaiting_review);
        let stored = daemon.state.claims.iter().find(|h| h.claim == ClaimId(7));
        assert!(stored.is_some_and(|held| held.awaiting_review));
        let adopted = daemon.adopt(ClaimId(9), server(false));
        assert_eq!(adopted.assumptions, vec!["b is pure".to_string()]);
    }

    fn lapsed_claim(claim: u64) -> HeldClaim {
        HeldClaim {
            expires_at_ms: 5,
            ..held(claim)
        }
    }

    fn daemon_log(daemon: &Daemon) -> String {
        std::fs::read_to_string(daemon.worktree.log_path()).unwrap_or_default()
    }

    fn inbox(daemon: &Daemon) -> String {
        std::fs::read_to_string(daemon.worktree.inbox_path()).unwrap_or_default()
    }

    #[tokio::test]
    async fn an_online_daemon_keeps_a_claim_past_its_local_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        go_online(&mut daemon);
        daemon.state.claims = vec![lapsed_claim(7)];
        assert!(matches!(daemon.housekeeping(), Flow::Continue));
        assert_eq!(daemon.state.claims.len(), 1);
        assert!(!daemon_log(&daemon).contains("lapsed"));
        assert!(!inbox(&daemon).contains("lease_expired"));
    }

    #[tokio::test]
    async fn the_coordinators_lease_expiry_drops_a_claim_with_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        go_online(&mut daemon);
        daemon.state.claims = vec![held(7), held(8)];
        daemon.on_server_msg(ServerMsg::LeaseExpired {
            claim: ClaimId(7),
            fence: Fence(7),
        });
        let kept: Vec<u64> = daemon.state.claims.iter().map(|h| h.claim.0).collect();
        assert_eq!(kept, vec![8]);
        assert!(inbox(&daemon).contains("lease_expired"));
    }

    #[tokio::test]
    async fn a_merge_doubt_is_kept_on_the_claim_from_when_it_was_told_and_cleared_when_over() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        go_online(&mut daemon);
        daemon.state.claims = vec![held(7), held(8)];
        let doubt = |since_ms| ServerMsg::MergeDoubt {
            claim: ClaimId(7),
            since_ms,
        };
        daemon.on_server_msg(doubt(Some(5)));
        let since = daemon.state.claims[0].merge_doubt_since_ms;
        assert!(
            since.is_some_and(|at| at > 5),
            "counted on this clock, not the server's"
        );
        assert_eq!(daemon.state.claims[1].merge_doubt_since_ms, None);
        daemon.on_server_msg(doubt(Some(9)));
        assert_eq!(
            daemon.state.claims[0].merge_doubt_since_ms, since,
            "a repeat does not restart the age"
        );
        daemon.on_server_msg(doubt(None));
        assert_eq!(daemon.state.claims[0].merge_doubt_since_ms, None);
        daemon.on_server_msg(ServerMsg::MergeDoubt {
            claim: ClaimId(99),
            since_ms: Some(1),
        });
        assert!(daemon
            .state
            .claims
            .iter()
            .all(|h| h.merge_doubt_since_ms.is_none()));
    }

    fn welcome_listing(doubts: Vec<ClaimInDoubt>) -> ServerMsg {
        ServerMsg::Welcome {
            head: CommitId("a".repeat(40)),
            lease_ms: 30_000,
            protocol: PROTOCOL_VERSION,
            protocol_range: None,
            merge_doubts: doubts,
        }
    }

    #[tokio::test]
    async fn a_daemon_offline_across_the_end_of_a_doubt_shows_none_after_it_reconnects() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let mut doubtful = held(7);
        doubtful.merge_doubt_since_ms = Some(5);
        daemon.state.claims = vec![doubtful, held(8)];
        daemon.on_server_msg(welcome_listing(Vec::new()));
        assert!(daemon
            .state
            .claims
            .iter()
            .all(|h| h.merge_doubt_since_ms.is_none()));
    }

    #[tokio::test]
    async fn a_welcome_that_lists_a_doubt_sets_it_and_keeps_the_first_local_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let mut known = held(8);
        known.merge_doubt_since_ms = Some(11);
        daemon.state.claims = vec![held(7), known];
        let listed = |claim| ClaimInDoubt {
            claim: ClaimId(claim),
            since_ms: 1,
        };
        daemon.on_server_msg(welcome_listing(vec![listed(7), listed(8), listed(99)]));
        assert!(daemon.state.claims[0]
            .merge_doubt_since_ms
            .is_some_and(|at| at > 11));
        assert_eq!(daemon.state.claims[1].merge_doubt_since_ms, Some(11));
    }

    #[tokio::test]
    async fn a_claim_whose_submission_changes_forgets_its_merge_doubt() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        go_online(&mut daemon);
        daemon.state.claims = vec![held(7)];
        for submitted in [false, true] {
            daemon.on_server_msg(ServerMsg::MergeDoubt {
                claim: ClaimId(7),
                since_ms: Some(5),
            });
            assert!(daemon.state.claims[0].merge_doubt_since_ms.is_some());
            daemon.set_submitted(ClaimId(7), submitted, None);
            assert_eq!(
                daemon.state.claims[0].merge_doubt_since_ms, None,
                "set_submitted({submitted})"
            );
        }
    }

    #[test]
    fn an_offline_daemon_lapses_a_claim_past_its_local_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.state.claims = vec![lapsed_claim(7), held(8)];
        assert!(matches!(daemon.housekeeping(), Flow::Continue));
        let kept: Vec<u64> = daemon.state.claims.iter().map(|h| h.claim.0).collect();
        assert_eq!(kept, vec![8]);
        assert!(inbox(&daemon).contains("lease_expired"));
    }

    #[test]
    fn a_lapse_is_logged_with_the_pong_and_tick_times_that_explain_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.state.lease_ms = Some(30_000);
        let sent_ms = now_ms() - 1_000;
        daemon.on_heartbeat_answered(sent_ms);
        assert_eq!(daemon.last_pong_sent_ms, Some(sent_ms));
        assert!(
            !daemon_log(&daemon).contains("slow pong"),
            "a prompt pong is not logged"
        );
        daemon.state.claims = vec![lapsed_claim(7)];
        daemon.next_housekeeping = Instant::now() - Duration::from_millis(2_000);
        daemon.housekeeping();
        let log = daemon_log(&daemon);
        let (_, after) = log
            .split_once("claim 7 lapsed locally: expiry 5 now ")
            .unwrap();
        assert!(
            after.contains(&format!("last pong for heartbeat Some({sent_ms})")),
            "{log}"
        );
        let (_, late) = after.split_once("tick late ").unwrap();
        let late_ms: u64 = late.split_whitespace().next().unwrap().parse().unwrap();
        assert!(late_ms >= 2_000, "{log}");
    }

    #[tokio::test]
    async fn a_heartbeat_to_a_gone_socket_task_closes_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.ever_online = true;
        go_online(&mut daemon);
        assert!(daemon.is_online());
        let generation = daemon.generation;
        daemon.send_heartbeat();
        assert!(!daemon.is_online());
        assert!(daemon.conn.is_none());
        assert_eq!(daemon.state.connection, Connection::Reconnecting);
        assert!(daemon.reconnect_at.is_some());
        assert!(
            daemon.generation > generation,
            "a late Closed must be ignored"
        );
        assert!(daemon_log(&daemon).contains("connection closed: the socket task ended"));
    }

    #[tokio::test]
    async fn a_message_to_a_gone_socket_task_closes_the_connection_and_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.ever_online = true;
        go_online(&mut daemon);
        assert!(!daemon.send(ClientMsg::Heartbeat));
        assert!(!daemon.is_online());
        assert_eq!(daemon.state.connection, Connection::Reconnecting);
    }

    #[test]
    fn a_pong_slower_than_a_heartbeat_interval_is_logged_with_its_delay() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.on_heartbeat_answered(now_ms() - 11_000);
        let log = daemon_log(&daemon);
        let (_, after) = log.split_once("answered after ").unwrap();
        let rtt: u64 = after.split_whitespace().next().unwrap().parse().unwrap();
        assert!(rtt >= 11_000, "{log}");
    }

    fn commit_in(dir: &std::path::Path, message: &str) -> String {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["commit", "-q", "--allow-empty", "-m", message]);
        git(&["rev-parse", "HEAD"])
    }

    #[tokio::test]
    async fn git_reads_made_off_the_loop_give_the_answers_the_direct_calls_give() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = daemon_in(dir.path());
        let first = commit_in(dir.path(), "one");
        let second = commit_in(dir.path(), "two");
        assert_eq!(daemon.read_head().await.unwrap(), second);
        let root = dir.path();
        assert!(is_ancestor_off_loop(root, &first, &second).await);
        assert!(!is_ancestor_off_loop(root, &second, &first).await);
        assert!(!is_ancestor_off_loop(root, "not-a-commit", &second).await);
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

    const PROBE_LIMIT: Duration = Duration::from_millis(600);

    fn base_moved() -> Message {
        let msg = ServerMsg::BaseMoved {
            head: CommitId("d".repeat(40)),
            by: AgentId("a2".into()),
            affected: Vec::new(),
        };
        Message::text(serde_json::to_string(&msg).unwrap())
    }

    /// A client socket as `socket_task` sees it, and the server's end of the same link.
    async fn linked() -> (
        Socket,
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        });
        let (client, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        (client, server.await.unwrap())
    }

    /// Starts a `socket_task` on a fresh link and sends one heartbeat with `silence_limit`.
    async fn heartbeating(
        silence_limit: Duration,
    ) -> (
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        mpsc::UnboundedReceiver<Incoming>,
        mpsc::UnboundedSender<Outgoing>,
        Instant,
    ) {
        let (client, server) = linked().await;
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        tokio::spawn(socket_task(client, out_rx, in_tx, 1));
        let heartbeat_at = Instant::now();
        out_tx.send(Outgoing::Heartbeat { silence_limit }).unwrap();
        (server, in_rx, out_tx, heartbeat_at)
    }

    /// Why the link closed, if it does within `within`; frames and pongs seen meanwhile are
    /// dropped.
    async fn closed_within(
        in_rx: &mut mpsc::UnboundedReceiver<Incoming>,
        within: Duration,
    ) -> Option<String> {
        let wait = async {
            loop {
                if let Incoming::Closed { reason, .. } = in_rx.recv().await? {
                    return Some(reason);
                }
            }
        };
        tokio::time::timeout(within, wait).await.ok().flatten()
    }

    /// Answers nothing, but keeps sending server frames and `extra` (a pong of the wrong
    /// payload, say) every 50 ms: the link looks alive and no heartbeat gets through.
    fn chatter(
        mut server: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        extra: Option<Message>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let mut frames = vec![base_moved()];
                frames.extend(extra.clone());
                for frame in frames {
                    if server.send(frame).await.is_err() {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    }

    #[tokio::test]
    async fn server_frames_without_a_pong_do_not_keep_a_link_alive() {
        let (server, mut in_rx, _out, heartbeat_at) = heartbeating(PROBE_LIMIT).await;
        let _chatter = chatter(server, None);
        let reason = closed_within(&mut in_rx, Duration::from_secs(10)).await;
        assert!(
            reason.is_some_and(|r| r.contains("link dead")),
            "not declared dead"
        );
        assert!(heartbeat_at.elapsed() >= PROBE_LIMIT, "declared dead early");
    }

    #[tokio::test]
    async fn a_pong_that_echoes_no_outstanding_ping_does_not_keep_a_link_alive() {
        let (server, mut in_rx, _out, heartbeat_at) = heartbeating(PROBE_LIMIT).await;
        let stale = Message::Pong(1_u64.to_be_bytes().to_vec().into());
        let _chatter = chatter(server, Some(stale));
        let reason = closed_within(&mut in_rx, Duration::from_secs(10)).await;
        assert!(
            reason.is_some_and(|r| r.contains("link dead")),
            "not declared dead"
        );
        assert!(heartbeat_at.elapsed() >= PROBE_LIMIT, "declared dead early");
    }

    #[tokio::test]
    async fn a_matching_pong_keeps_a_link_alive() {
        // A long limit: both ends share this runtime, so a stall must not read as silence.
        let (mut server, mut in_rx, _out, _) = heartbeating(Duration::from_secs(30)).await;
        // Reading the ping makes the server answer it with a pong of the same payload.
        let reader = tokio::spawn(async move { while server.next().await.is_some() {} });
        let answered = async {
            loop {
                if let Incoming::HeartbeatAnswered { .. } = in_rx.recv().await? {
                    return Some(());
                }
            }
        };
        let answered = tokio::time::timeout(Duration::from_secs(10), answered).await;
        assert!(answered.is_ok_and(|seen| seen.is_some()), "no pong arrived");
        reader.abort();
    }

    #[test]
    fn the_silence_deadline_follows_the_oldest_unanswered_heartbeat() {
        let mut probes = Probes::default();
        assert!(probes.silent_from().is_none());
        probes.sent(1, Duration::from_secs(60));
        let first = Instant::now() + Duration::from_secs(60);
        std::thread::sleep(Duration::from_millis(5));
        probes.sent(2, Duration::from_secs(60));
        let oldest = probes.silent_from().unwrap();
        assert!(oldest <= first, "a newer heartbeat moved the deadline");
        assert_eq!(probes.answered_by(&1_u64.to_be_bytes()), Some(1));
        assert!(
            probes.silent_from().is_some_and(|next| next > oldest),
            "the deadline did not move to the next heartbeat"
        );
        assert_eq!(probes.answered_by(&1_u64.to_be_bytes()), None);
        assert_eq!(probes.answered_by(&9_u64.to_be_bytes()), None);
        assert_eq!(probes.answered_by(b"short"), None);
        assert!(probes.silent_from().is_some());
        assert_eq!(probes.answered_by(&2_u64.to_be_bytes()), Some(2));
        assert!(probes.silent_from().is_none());

        probes.sent(1, Duration::from_secs(60));
        probes.sent(2, Duration::from_secs(60));
        assert_eq!(probes.answered_by(&2_u64.to_be_bytes()), Some(2));
        assert!(
            probes.silent_from().is_none(),
            "a pong for the newest ping left an older one outstanding"
        );
    }

    #[test]
    fn a_write_is_cut_off_at_the_silence_deadline_when_that_comes_before_the_write_limit() {
        let mut probes = Probes::default();
        let before = Instant::now();
        let unbounded = probes.write_deadline();
        assert!(
            unbounded >= before + WRITE_LIMIT,
            "no outstanding ping: now + limit"
        );
        assert!(unbounded <= Instant::now() + WRITE_LIMIT);
        probes.sent(1, Duration::from_secs(1));
        let silent = probes.silent_from().unwrap();
        assert_eq!(probes.write_deadline(), silent);
        let mut late = Probes::default();
        late.sent(1, WRITE_LIMIT * 2);
        assert!(
            late.write_deadline() <= Instant::now() + WRITE_LIMIT,
            "capped at the limit"
        );
    }

    #[test]
    fn heartbeats_are_answered_in_the_order_sent_even_if_the_clock_steps_back() {
        let mut probes = Probes::default();
        probes.sent(500, Duration::from_secs(60));
        probes.sent(100, Duration::from_secs(60));
        probes.sent(200, Duration::from_secs(60));
        assert_eq!(probes.answered_by(&100_u64.to_be_bytes()), Some(100));
        assert_eq!(probes.outstanding.len(), 1);
        assert_eq!(probes.outstanding[0].0, 200);
    }

    #[tokio::test]
    async fn repeated_welcome_timeouts_back_off_instead_of_reconnecting_hot() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.ever_online = true;
        daemon.welcome_overdue();
        daemon.welcome_overdue();
        assert_eq!(daemon.backoff, FIRST_BACKOFF * 4);
        assert!(daemon.reconnect_at.is_some());
    }

    #[tokio::test]
    async fn a_link_that_never_welcomes_is_dropped_and_reconnected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (accepted_tx, mut accepted_rx) = mpsc::unbounded_channel();
        // Completes every handshake, then says nothing and never closes.
        let server = tokio::spawn(async move {
            let mut open = Vec::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                open.push(tokio_tungstenite::accept_async(stream).await.unwrap());
                let _ = accepted_tx.send(Instant::now());
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, in_rx) = daemon_for(dir.path(), &url);
        daemon.state.connection = Connection::Connecting;
        daemon.state.claims.clear();
        daemon.welcome_limit = PROBE_LIMIT;
        daemon.reconnect_at = Some(Instant::now());
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let two_handshakes = async {
            let first = accepted_rx.recv().await.unwrap();
            let second = accepted_rx.recv().await.unwrap();
            second.duration_since(first)
        };
        let gap = tokio::select! {
            result = daemon.main_loop(cmd_rx, in_rx) => panic!("daemon ended: {result:?}"),
            gap = tokio::time::timeout(Duration::from_secs(10), two_handshakes) => gap,
        };
        server.abort();
        let gap = gap.expect("no reconnect after a socket that was never welcomed");
        assert!(
            gap >= PROBE_LIMIT,
            "reconnected after {gap:?}, before the Welcome limit"
        );
        assert!(daemon_log(&daemon).contains("no Welcome within"));
    }

    #[tokio::test]
    async fn a_welcome_ends_the_wait_for_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.welcome_deadline = Some(Instant::now() + PROBE_LIMIT);
        daemon.on_welcome("a".repeat(40), 30_000, None);
        assert!(daemon.welcome_deadline.is_none());
    }

    #[tokio::test]
    async fn a_welcome_without_a_range_or_with_one_around_this_version_warns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.on_welcome("a".repeat(40), 30_000, None);
        let around = ProtocolRange {
            min: PROTOCOL_VERSION,
            max: PROTOCOL_VERSION + 2,
        };
        daemon.on_welcome("a".repeat(40), 30_000, Some(around));
        assert_eq!(daemon.state.protocol_warning, None);
        assert!(!daemon_log(&daemon).contains("warning"));
    }

    #[tokio::test]
    async fn a_range_that_excludes_this_version_warns_once_and_a_later_fit_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let above = ProtocolRange {
            min: PROTOCOL_VERSION + 1,
            max: PROTOCOL_VERSION + 3,
        };
        daemon.on_welcome("a".repeat(40), 30_000, Some(above));
        daemon.on_welcome("a".repeat(40), 30_000, Some(above));
        let log = daemon_log(&daemon);
        assert_eq!(
            log.matches("warning: the coordinator speaks").count(),
            1,
            "{log}"
        );
        assert!(log.contains("v2 to v4 and this daemon speaks v1"), "{log}");
        assert!(daemon.state.protocol_warning.is_some());
        daemon.on_welcome("a".repeat(40), 30_000, None);
        assert_eq!(daemon.state.protocol_warning, None);
    }

    const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// A `granted` as it arrives on the wire; `head` is absent from the object when `None`.
    fn granted_json(head: Option<serde_json::Value>) -> ServerMsg {
        let mut msg = serde_json::json!({
            "type": "granted", "req": 5, "claim": 9, "fence": 9,
            "expires_at_ms": 0, "race": null,
        });
        if let Some(head) = head {
            msg["head"] = head;
        }
        serde_json::from_value(msg).unwrap()
    }

    fn pending_claim() -> PendingClaim {
        PendingClaim {
            scopes: Vec::new(),
            queued: false,
            amend: None,
            assumptions: Vec::new(),
            released_meanwhile: Vec::new(),
            awaited: None,
            replies: Vec::new(),
        }
    }

    fn persisted_head(daemon: &Daemon) -> Option<String> {
        State::read(&daemon.worktree)
            .unwrap()
            .and_then(|state| state.coordinator_head)
    }

    fn daemon_knowing_head_a(dir: &std::path::Path) -> Daemon {
        let mut daemon = daemon_in(dir);
        daemon.state.coordinator_head = Some(HEAD_A.to_string());
        daemon
    }

    #[test]
    fn a_grant_for_a_pending_claim_adopts_the_head_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_knowing_head_a(dir.path());
        daemon.pending.insert(5, pending_claim());

        daemon.on_server_msg(granted_json(Some(HEAD_B.into())));

        assert_eq!(daemon.state.coordinator_head.as_deref(), Some(HEAD_B));
        assert_eq!(persisted_head(&daemon).as_deref(), Some(HEAD_B));
    }

    fn adopted_by_the_first_log_read(daemon: &mut Daemon) {
        let server = ServerClaim {
            fence: Fence(9),
            scopes: Vec::new(),
            race: None,
            submitted: false,
            submitted_commit: None,
            awaiting_review: false,
            assumptions: Vec::new(),
        };
        daemon.adopt(ClaimId(9), server);
    }

    #[tokio::test]
    async fn a_grant_for_a_claim_released_in_the_meantime_is_not_held() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let _wire = go_online_listening(&mut daemon);
        let (tx, mut rx) = oneshot::channel();
        daemon.pending.insert(
            5,
            PendingClaim {
                replies: vec![tx],
                ..pending_claim()
            },
        );
        adopted_by_the_first_log_read(&mut daemon);

        let released = daemon.release(Some(ClaimId(9)));
        daemon.on_server_msg(granted_json(None));

        assert!(matches!(released, Reply::Released { claims, .. } if claims == [ClaimId(9)]));
        assert!(daemon
            .state
            .claims
            .iter()
            .all(|held| held.claim != ClaimId(9)));
        let reply = rx.try_recv();
        assert!(
            matches!(
                &reply,
                Ok(Reply::Claim { outcome: ClaimOutcome::Refused { message, .. } })
                    if message.contains("claim 9 was released here")
            ),
            "{reply:?}"
        );
    }

    #[tokio::test]
    async fn a_grant_is_held_when_only_another_claim_was_released_in_the_meantime() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let _wire = go_online_listening(&mut daemon);
        daemon.pending.insert(5, pending_claim());

        daemon.release(Some(ClaimId(1)));
        daemon.on_server_msg(granted_json(None));

        let held: Vec<u64> = daemon.state.claims.iter().map(|h| h.claim.0).collect();
        assert_eq!(held, vec![2, 9]);
    }

    fn sent_releases(wire: &mut mpsc::UnboundedReceiver<Outgoing>) -> Vec<(u64, u64)> {
        let mut sent = Vec::new();
        while let Ok(outgoing) = wire.try_recv() {
            if let Outgoing::Msg(ClientMsg::Release { claim, fence, .. }) = outgoing {
                sent.push((claim.0, fence.0));
            }
        }
        sent
    }

    fn granted_for(claim: u64, fence: u64) -> ServerMsg {
        serde_json::from_value(serde_json::json!({
            "type": "granted", "req": 5, "claim": claim, "fence": fence,
            "expires_at_ms": 0, "race": null,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn an_amend_granted_after_the_claim_was_released_sends_the_release_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let mut wire = go_online_listening(&mut daemon);
        let (tx, mut rx) = oneshot::channel();
        daemon.pending.insert(
            5,
            PendingClaim {
                amend: Some(ClaimId(1)),
                replies: vec![tx],
                ..pending_claim()
            },
        );

        daemon.release(Some(ClaimId(1)));
        daemon.on_server_msg(granted_for(1, 4));

        assert_eq!(sent_releases(&mut wire), vec![(1, 1), (1, 4)]);
        let reply = rx.try_recv();
        assert!(
            matches!(
                &reply,
                Ok(Reply::Claim { outcome: ClaimOutcome::Refused { message, .. } })
                    if message.contains("release was sent again with the new fence")
            ),
            "{reply:?}"
        );
    }

    #[tokio::test]
    async fn a_new_claim_granted_after_its_release_is_not_released_a_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let mut wire = go_online_listening(&mut daemon);
        daemon.pending.insert(5, pending_claim());
        adopted_by_the_first_log_read(&mut daemon);

        daemon.release(Some(ClaimId(9)));
        daemon.on_server_msg(granted_json(None));

        assert_eq!(sent_releases(&mut wire), vec![(9, 9)]);
    }

    #[tokio::test]
    async fn a_released_grant_ends_a_queued_wait() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let _wire = go_online_listening(&mut daemon);
        daemon.state.queued = Some(QueuedWait {
            req: RequestId(5),
            scopes: Vec::new(),
            position: 1,
        });
        daemon.pending.insert(
            5,
            PendingClaim {
                queued: true,
                ..pending_claim()
            },
        );
        adopted_by_the_first_log_read(&mut daemon);

        daemon.release(Some(ClaimId(9)));
        daemon.on_server_msg(granted_json(None));

        assert_eq!(daemon.state.queued, None);
    }

    fn awaiting(claim: u64) -> PendingClaim {
        let server = ServerClaim {
            fence: Fence(claim),
            scopes: Vec::new(),
            race: None,
            submitted: false,
            submitted_commit: None,
            awaiting_review: false,
            assumptions: Vec::new(),
        };
        PendingClaim {
            awaited: Some((ClaimId(claim), server)),
            ..pending_claim()
        }
    }

    fn held_ids(daemon: &Daemon) -> Vec<u64> {
        daemon.state.claims.iter().map(|h| h.claim.0).collect()
    }

    #[tokio::test]
    async fn a_grant_of_another_claim_leaves_the_awaited_one_reported_and_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.pending.insert(5, awaiting(7));

        daemon.on_server_msg(granted_json(None));

        assert_eq!(held_ids(&daemon), vec![1, 2, 7, 9]);
        assert!(inbox(&daemon).contains("no request here explains it"));
    }

    #[tokio::test]
    async fn a_grant_of_the_awaited_claim_is_held_once_and_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.pending.insert(5, awaiting(9));

        daemon.on_server_msg(granted_json(None));

        assert_eq!(held_ids(&daemon), vec![1, 2, 9]);
        assert!(!inbox(&daemon).contains("no request here explains it"));
    }

    #[tokio::test]
    async fn a_denied_request_leaves_the_awaited_claim_reported_and_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.pending.insert(5, awaiting(7));

        daemon.on_server_msg(ServerMsg::Denied {
            req: RequestId(5),
            conflicts: Vec::new(),
        });

        assert_eq!(held_ids(&daemon), vec![1, 2, 7]);
        assert!(inbox(&daemon).contains("no request here explains it"));
    }

    #[tokio::test]
    async fn a_refused_request_leaves_the_awaited_claim_reported_and_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        daemon.pending.insert(5, awaiting(7));

        daemon.on_server_msg(ServerMsg::Error {
            req: Some(RequestId(5)),
            code: ErrorCode::Malformed,
            message: "no".to_string(),
        });

        assert_eq!(held_ids(&daemon), vec![1, 2, 7]);
        assert!(inbox(&daemon).contains("no request here explains it"));
    }

    #[tokio::test]
    async fn a_claim_found_by_the_log_read_is_tracked_when_the_reply_names_another() {
        use tessel_coordinator::protocol::{Event, EventKind, Intent, Mode, RunId, Scope};
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_in(dir.path());
        let scopes = vec![ScopeClaim {
            scope: Scope::File {
                path: "a.rs".into(),
            },
            mode: Mode::EditBody,
        }];
        daemon.pending.insert(
            5,
            PendingClaim {
                scopes: scopes.clone(),
                ..pending_claim()
            },
        );
        let orphan = Event {
            seq: 0,
            at_ms: 0,
            run: RunId("r".into()),
            kind: EventKind::ClaimGranted {
                agent: AgentId("a1".into()),
                claim: ClaimId(7),
                fence: Fence(7),
                scopes,
                intent: Intent {
                    summary: String::new(),
                    task_ref: None,
                    assumptions: Vec::new(),
                },
                race: None,
                at_risk: Vec::new(),
            },
        };

        daemon
            .on_snapshot(Ok(LogRead {
                events: vec![orphan],
                complete: true,
                unknown: Vec::new(),
                last_seq: Some(0),
            }))
            .await;
        assert_eq!(held_ids(&daemon), vec![1, 2], "left for the reply");
        assert!(!inbox(&daemon).contains("no request here explains it"));
        daemon.on_server_msg(granted_json(None));

        assert_eq!(held_ids(&daemon), vec![1, 2, 7, 9]);
        assert!(inbox(&daemon).contains("no request here explains it"));
    }

    #[test]
    fn a_grant_for_an_unknown_request_still_adopts_and_writes_the_head_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon_knowing_head_a(dir.path());
        assert_eq!(persisted_head(&daemon), None, "nothing was written yet");

        daemon.on_server_msg(granted_json(Some(HEAD_B.into())));

        assert_eq!(daemon.state.coordinator_head.as_deref(), Some(HEAD_B));
        assert_eq!(persisted_head(&daemon).as_deref(), Some(HEAD_B));
    }

    #[test]
    fn a_grant_without_a_head_leaves_the_head_alone() {
        for head in [None, Some(serde_json::Value::Null)] {
            let dir = tempfile::tempdir().unwrap();
            let mut daemon = daemon_knowing_head_a(dir.path());
            daemon.pending.insert(5, pending_claim());

            daemon.on_server_msg(granted_json(head.clone()));

            assert_eq!(
                daemon.state.coordinator_head.as_deref(),
                Some(HEAD_A),
                "{head:?}"
            );
            assert_eq!(persisted_head(&daemon).as_deref(), Some(HEAD_A), "{head:?}");
        }
    }

    /// A child between `fork` and `exec` holds a copy of every descriptor the daemon had open. A
    /// lock that lives in an open file description (`flock`) then outlives the daemon's own
    /// release, so a daemon started in that window is refused. The child here sleeps in that
    /// window and the lock is taken again inside it.
    #[test]
    fn a_forked_child_before_exec_does_not_hold_the_worktree_lock() {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        const WINDOW_NANOS: libc::c_long = 300_000_000;

        let dir = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["init", "-q"])
            .status();
        assert!(init.is_ok_and(|status| status.success()));
        let worktree = Worktree::discover(dir.path()).unwrap();
        worktree.prepare_dir().unwrap();
        let first = acquire_lock(&worktree).unwrap();
        assert!(first.is_some(), "the first daemon takes the lock");

        let (mut forked, signal) = std::io::pipe().unwrap();
        let signal_fd = signal.as_raw_fd();
        let spawner = std::thread::spawn(move || {
            let mut command = std::process::Command::new("true");
            // SAFETY: the closure runs between fork and exec and calls only `write` and
            // `nanosleep`, which POSIX lists as async-signal-safe; the `timespec` is built on the
            // child's stack from zeroed memory and no memory another thread owns is touched.
            unsafe {
                command.pre_exec(move || {
                    let mut window: libc::timespec = std::mem::zeroed();
                    window.tv_nsec = WINDOW_NANOS;
                    libc::write(signal_fd, b"x".as_ptr().cast(), 1);
                    libc::nanosleep(&raw const window, std::ptr::null_mut());
                    Ok(())
                });
            }
            let status = command.status();
            drop(signal);
            status
        });
        let mut byte = [0_u8; 1];
        forked.read_exact(&mut byte).unwrap();
        drop(first);
        let second = acquire_lock(&worktree).unwrap();
        let status = spawner.join().unwrap().unwrap();
        assert!(status.success(), "the child ran: {status}");
        assert!(
            second.is_some(),
            "a daemon started inside the exec window is refused by the released lock"
        );
    }

    /// A daemon of the previous `tessel` held `daemon.lock` with `flock`. On Linux that does not
    /// exclude the record lock, so the new daemon asks for the `flock` as well and gives it back.
    #[test]
    fn a_flock_held_by_an_older_daemon_is_seen_and_given_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        let open = || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .unwrap()
        };
        let older = open();
        older.lock().unwrap();
        let probe = open();

        assert!(held_by_flock(&probe).unwrap(), "the older daemon holds it");

        older.unlock().unwrap();
        assert!(!held_by_flock(&probe).unwrap(), "released, so free");
        assert!(!held_by_flock(&open()).unwrap(), "the probe kept nothing");
    }

    #[test]
    fn a_daemon_refuses_the_lock_while_an_older_daemons_flock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["init", "-q"])
            .status();
        assert!(init.is_ok_and(|status| status.success()));
        let worktree = Worktree::discover(dir.path()).unwrap();
        worktree.prepare_dir().unwrap();
        let older = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(worktree.lock_path())
            .unwrap();
        older.lock().unwrap();

        assert!(acquire_lock(&worktree).unwrap().is_none());

        older.unlock().unwrap();
        assert!(acquire_lock(&worktree).unwrap().is_some());
    }
}

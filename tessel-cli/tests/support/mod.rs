//! Test support: a fake coordinator that drives the crate's real `Coordinator` core behind a
//! local WebSocket server, and helpers that run the real `tessel` binary in temp git repos.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tessel_coordinator::coordinator::{Config, Coordinator};
use tessel_coordinator::merge::MergeOutcome;
use tessel_coordinator::protocol::{AgentId, ClientMsg, CommitId, Event, RunId, ServerMsg};
use tessel_coordinator::shell::{self, Action, Outbound, Session};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

pub const REPO: &str = "demo";

fn now_ms() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

struct SocketEntry {
    id: u64,
    bound: Option<AgentId>,
    watching: bool,
    tx: mpsc::UnboundedSender<ServerMsg>,
}

struct Inner {
    core: Mutex<Coordinator>,
    sockets: Mutex<Vec<SocketEntry>>,
    received: Mutex<Vec<(AgentId, ClientMsg)>>,
    tokens: HashMap<String, String>,
    next_socket: Mutex<u64>,
    events: Mutex<Vec<Event>>,
    accepting: AtomicBool,
    cut_replay: AtomicUsize,
    skip_seq: AtomicU64,
    stall_live: AtomicBool,
    review_first: AtomicBool,
    hold_claim_reply: AtomicBool,
    held: Mutex<Vec<(u64, Vec<Outbound>)>>,
    lose: Mutex<Vec<(String, Lose)>>,
}

/// A message to lose on its way, as a dropped network connection would.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lose {
    /// The coordinator handles the claim, but the reply is lost and the socket closes.
    ClaimReply,
    /// The coordinator never sees the release; the socket closes.
    Release,
    /// The coordinator never sees the review; the socket closes.
    Review,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Inner {
    /// Records events in the log and sends them to the sockets that watch it.
    fn publish(&self, events: Vec<Event>) {
        let mut log = lock(&self.events);
        let sockets = lock(&self.sockets);
        for event in events {
            let live = !self.stall_live.load(Ordering::SeqCst);
            for entry in sockets.iter().filter(|s| s.watching && live) {
                let _ = entry.tx.send(ServerMsg::Event {
                    event: event.clone(),
                });
            }
            log.push(event);
        }
    }

    /// Runs `msg` on the core as `agent`, outside any socket.
    fn act(&self, agent: &AgentId, msg: ClientMsg) {
        let effects = lock(&self.core).handle(agent, msg, now_ms());
        let (events, outbound) = shell::split_effects(effects);
        self.deliver(None, outbound);
        self.publish(events);
    }

    fn deliver(&self, origin: Option<u64>, outbound: Vec<Outbound>) {
        let sockets = self
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for item in outbound {
            match item {
                Outbound::Reply(msg) => {
                    if let Some(entry) = sockets.iter().find(|s| Some(s.id) == origin) {
                        let _ = entry.tx.send(msg);
                    }
                }
                Outbound::Notify { agent, msg } => {
                    for entry in sockets.iter().filter(|s| s.bound.as_ref() == Some(&agent)) {
                        let _ = entry.tx.send(msg.clone());
                    }
                }
            }
        }
    }
}

/// A coordinator on 127.0.0.1 that accepts exactly the tokens it was given.
pub struct Fake {
    pub url: String,
    inner: Arc<Inner>,
    kill: watch::Sender<(u64, bool)>,
    mute: watch::Sender<(u64, bool)>,
}

impl Fake {
    /// `agents` maps each agent name to the token the fake accepts for it.
    pub async fn start(lease_ms: u64, agents: &[(&str, &str)]) -> Result<Self> {
        let config = Config {
            run: RunId("test".into()),
            lease_ms,
            shadow_enabled: false,
        };
        let inner = Arc::new(Inner {
            core: Mutex::new(Coordinator::new(config)?),
            sockets: Mutex::new(Vec::new()),
            received: Mutex::new(Vec::new()),
            tokens: agents
                .iter()
                .map(|(agent, token)| ((*token).to_string(), (*agent).to_string()))
                .collect(),
            next_socket: Mutex::new(0),
            events: Mutex::new(Vec::new()),
            accepting: AtomicBool::new(true),
            cut_replay: AtomicUsize::new(0),
            skip_seq: AtomicU64::new(u64::MAX),
            stall_live: AtomicBool::new(false),
            review_first: AtomicBool::new(false),
            hold_claim_reply: AtomicBool::new(false),
            held: Mutex::new(Vec::new()),
            lose: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let (kill, kill_rx) = watch::channel((0, false));
        let (mute, mute_rx) = watch::channel((0, false));
        tokio::spawn(accept(listener, Arc::clone(&inner), kill_rx, mute_rx));
        tokio::spawn(expire_loop(Arc::clone(&inner)));
        Ok(Self {
            url,
            inner,
            kill,
            mute,
        })
    }

    /// Keeps the reply to the next `Claim` or `Amend` back until `release_held`.
    pub fn hold_next_claim_reply(&self) {
        self.inner.hold_claim_reply.store(true, Ordering::SeqCst);
    }

    /// Delivers the replies kept back by `hold_next_claim_reply`.
    pub fn release_held(&self) {
        let held = std::mem::take(&mut *lock(&self.inner.held));
        for (origin, outbound) in held {
            self.inner.deliver(Some(origin), outbound);
        }
    }

    /// Names the agents who may review (the core's `REVIEWERS`).
    pub fn set_reviewers(&self, reviewers: &[&str]) {
        let ids = reviewers.iter().map(|id| AgentId((*id).into())).collect();
        lock(&self.inner.core).set_reviewers(ids);
    }

    /// Makes the coordinator send `ReviewRequired` before `Accepted`, as REVIEW-1 will.
    pub fn review_before_accepted(&self, on: bool) {
        self.inner.review_first.store(on, Ordering::SeqCst);
    }

    /// Runs the next queued merge to completion on the real core, as the steward would, and
    /// sends `Merged` to the submitter if it has a socket open.
    pub fn merge_next(&self) {
        let effects = {
            let mut core = lock(&self.inner.core);
            let Some(dispatch) = core.begin_merge(now_ms()) else {
                return;
            };
            let landed = MergeOutcome::Merged {
                base: CommitId("b".repeat(40)),
                head: CommitId("c".repeat(40)),
            };
            core.merge_outcome(dispatch.claim, &landed, now_ms())
        };
        let (events, outbound) = shell::split_effects(effects);
        self.inner.deliver(None, outbound);
        self.inner.publish(events);
    }

    /// Cuts every open socket without a close frame, as a network failure would.
    pub fn drop_connections(&self) {
        self.kill.send_modify(|kill| *kill = (kill.0 + 1, false));
    }

    /// Leaves every open socket open but stops reading from it and writing to it, as a link that
    /// went half-open would: no reply, no pong, no close frame. New connections are unaffected.
    pub fn go_silent(&self) {
        self.mute.send_modify(|mute| *mute = (mute.0 + 1, false));
    }

    /// Stops reading from every open socket, so no ping or heartbeat is ever answered, but keeps
    /// writing to it, as on a link that carries traffic in one direction only. Each frame it writes
    /// is followed by pongs that answer no ping.
    pub fn go_deaf(&self) {
        self.mute.send_modify(|mute| *mute = (mute.0 + 1, true));
    }

    /// Refuses (or accepts again) new connections with HTTP 503. Open sockets are unaffected.
    pub fn set_accepting(&self, accepting: bool) {
        self.inner.accepting.store(accepting, Ordering::SeqCst);
    }

    /// Cuts the sockets that are not reading the event log, as the loss of agents' working
    /// connections would, and leaves the log readers open.
    pub fn drop_main_connections(&self) {
        self.kill.send_modify(|kill| *kill = (kill.0 + 1, true));
    }

    /// How many sockets are reading the event log right now.
    pub fn watching_sockets(&self) -> usize {
        lock(&self.inner.sockets)
            .iter()
            .filter(|s| s.watching)
            .count()
    }

    /// Withholds live events from event-log readers, so a reader never sees its end marker.
    pub fn stall_live_events(&self, stall: bool) {
        self.inner.stall_live.store(stall, Ordering::SeqCst);
    }

    /// Leaves the event with this `seq` out of every replay, so a reader sees a gap.
    pub fn skip_in_replay(&self, seq: u64) {
        self.inner.skip_seq.store(seq, Ordering::SeqCst);
    }

    /// Makes every event-log replay stop `n` events short and close, as a dropped connection
    /// would. 0 turns it off.
    pub fn cut_replay(&self, n: usize) {
        self.inner.cut_replay.store(n, Ordering::SeqCst);
    }

    /// Loses the next message of this kind from `agent`.
    pub fn lose_next(&self, agent: &str, what: Lose) {
        lock(&self.inner.lose).push((agent.to_string(), what));
    }

    /// Makes `agent` send `msg` to the coordinator without any socket, as if from elsewhere.
    pub fn act(&self, agent: &str, msg: ClientMsg) {
        self.inner.act(&AgentId(agent.into()), msg);
    }

    /// Sends `msg` to every open socket bound to `agent`.
    pub fn push(&self, agent: &str, msg: ServerMsg) {
        let notify = Outbound::Notify {
            agent: AgentId(agent.into()),
            msg,
        };
        self.inner.deliver(None, vec![notify]);
    }

    /// Every message `agent` has sent, in order.
    pub fn received(&self, agent: &str) -> Vec<ClientMsg> {
        let log = self
            .inner
            .received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        log.iter()
            .filter(|(who, _)| who.0 == agent)
            .map(|(_, msg)| msg.clone())
            .collect()
    }
}

async fn expire_loop(inner: Arc<Inner>) {
    loop {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let effects = {
            let mut core = inner
                .core
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            core.expire(now_ms())
        };
        let (events, outbound) = shell::split_effects(effects);
        inner.deliver(None, outbound);
        inner.publish(events);
    }
}

async fn accept(
    listener: TcpListener,
    inner: Arc<Inner>,
    kill: watch::Receiver<(u64, bool)>,
    mute: watch::Receiver<(u64, bool)>,
) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(serve(
            stream,
            Arc::clone(&inner),
            kill.clone(),
            mute.clone(),
        ));
    }
}

async fn serve(
    stream: TcpStream,
    inner: Arc<Inner>,
    mut kill: watch::Receiver<(u64, bool)>,
    mut mute: watch::Receiver<(u64, bool)>,
) {
    // Only a drop or a mute requested after this socket opened may affect it.
    kill.borrow_and_update();
    mute.borrow_and_update();
    let (mut deaf, mut mute_writes) = (false, false);
    let verified: Arc<Mutex<Option<AgentId>>> = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&verified);
    let tokens = inner.tokens.clone();
    let open = Arc::clone(&inner);
    #[expect(
        clippy::result_large_err,
        reason = "the handshake callback fixes this signature"
    )]
    let check = move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let expected = format!("/repo/{REPO}/ws");
        let bearer = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if !open.accepting.load(Ordering::SeqCst) {
            let mut busy = ErrorResponse::new(Some("busy".to_string()));
            *busy.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
            return Err(busy);
        }
        let agent = bearer.and_then(|token| tokens.get(token));
        match agent {
            Some(agent) if request.uri().path() == expected => {
                *seen
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(AgentId(agent.clone()));
                Ok(response)
            }
            Some(_) | None => {
                let mut refusal = ErrorResponse::new(Some("unauthorized".to_string()));
                *refusal.status_mut() = StatusCode::UNAUTHORIZED;
                Err(refusal)
            }
        }
    };
    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, check).await else {
        return;
    };
    let Some(agent) = verified.lock().ok().and_then(|v| v.clone()) else {
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    let id = {
        let mut next = inner
            .next_socket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *next += 1;
        *next
    };
    inner
        .sockets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(SocketEntry {
            id,
            bound: None,
            watching: false,
            tx,
        });
    let mut session = Session {
        verified: Some(agent),
        ..Session::default()
    };
    loop {
        tokio::select! {
            _ = mute.changed() => {
                (deaf, mute_writes) = (true, !mute.borrow_and_update().1);
            }
            _ = kill.changed() => {
                if !keeps_open(&inner, id, *kill.borrow_and_update()) {
                    break;
                }
            }
            outgoing = rx.recv(), if !mute_writes => {
                let Some(msg) = outgoing else { break };
                if write_server_msg(&mut socket, &msg, deaf).await.is_err() {
                    break;
                }
            }
            frame = socket.next(), if !deaf => match frame {
                Some(Ok(Message::Text(text))) => {
                    if let Some(parsed) = handle_text(&inner, id, &mut session, &text) {
                        if parsed.close {
                            break;
                        }
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
    }
    close_socket(&inner, id, &session);
}

/// Writes `msg`, followed by stray pongs when the link is one-way.
async fn write_server_msg(
    socket: &mut WebSocketStream<TcpStream>,
    msg: &ServerMsg,
    deaf: bool,
) -> Result<(), WsError> {
    let text = serde_json::to_string(msg).map_err(|e| WsError::Io(e.into()))?;
    socket.send(Message::text(text)).await?;
    if deaf {
        stray_pongs(socket).await?;
    }
    Ok(())
}

/// Pongs that echo no ping the daemon sent: one whose payload is a time far ahead (which would
/// renew a lease far ahead if believed), and one too short to hold a payload.
async fn stray_pongs(socket: &mut WebSocketStream<TcpStream>) -> Result<(), WsError> {
    let ahead = now_ms() + 600_000;
    socket
        .send(Message::Pong(ahead.to_be_bytes().to_vec().into()))
        .await?;
    socket.send(Message::Pong(vec![1, 2, 3].into())).await
}

/// Whether a drop request spares this socket: only a request for the main connections does, and
/// only for a socket that is reading the event log.
fn keeps_open(inner: &Inner, id: u64, (_, main_only): (u64, bool)) -> bool {
    let reading = lock(&inner.sockets)
        .iter()
        .any(|s| s.id == id && s.watching);
    main_only && reading
}

struct Handled {
    close: bool,
}

fn handle_text(inner: &Inner, id: u64, session: &mut Session, text: &str) -> Option<Handled> {
    let msg = match shell::parse_client_msg(text) {
        Ok(msg) => msg,
        Err(fault) => {
            inner.deliver(Some(id), vec![Outbound::Reply(fault.reply())]);
            return None;
        }
    };
    match shell::decide(session, &msg) {
        Action::Reject(reply) => inner.deliver(Some(id), vec![Outbound::Reply(reply)]),
        Action::RejectAndClose(reply) => {
            inner.deliver(Some(id), vec![Outbound::Reply(reply)]);
            return Some(Handled { close: true });
        }
        Action::Watch { from_seq } => {
            let log = lock(&inner.events);
            let cut = inner.cut_replay.load(Ordering::SeqCst);
            let mut sockets = lock(&inner.sockets);
            if let Some(entry) = sockets.iter_mut().find(|s| s.id == id) {
                let shown = log.len().saturating_sub(cut);
                let skip = inner.skip_seq.load(Ordering::SeqCst);
                for event in log[..shown]
                    .iter()
                    .filter(|e| e.seq >= from_seq && e.seq != skip)
                {
                    let _ = entry.tx.send(ServerMsg::Event {
                        event: event.clone(),
                    });
                }
                entry.watching = true;
            }
            if cut > 0 {
                return Some(Handled { close: true });
            }
        }
        Action::Call { agent } => {
            if let Some(lost) = take_loss(inner, &agent, &msg) {
                if lost == Lose::ClaimReply {
                    inner.act(&agent, msg);
                }
                return Some(Handled { close: true });
            }
            inner
                .received
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((agent.clone(), msg.clone()));
            let holds_claim_reply =
                matches!(msg, ClientMsg::Claim { .. } | ClientMsg::Amend { .. });
            let effects = {
                let mut core = inner
                    .core
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                core.handle(&agent, msg, now_ms())
            };
            let (events, mut outbound) = shell::split_effects(effects);
            if inner.review_first.load(Ordering::SeqCst) {
                let review =
                    |o: &Outbound| matches!(o, Outbound::Reply(ServerMsg::ReviewRequired { .. }));
                outbound.sort_by_key(|o| !review(o));
            }
            if let Some(bound) = shell::bind_on_welcome(session, &agent, &outbound) {
                *session = bound;
                let mut sockets = inner
                    .sockets
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(entry) = sockets.iter_mut().find(|s| s.id == id) {
                    entry.bound.clone_from(&session.agent);
                }
            }
            // As the Durable Object does: the reply first, then the events it caused.
            if holds_claim_reply && inner.hold_claim_reply.swap(false, Ordering::SeqCst) {
                lock(&inner.held).push((id, outbound));
            } else {
                inner.deliver(Some(id), outbound);
            }
            inner.publish(events);
        }
    }
    None
}

fn close_socket(inner: &Inner, id: u64, session: &Session) {
    let others_bound = {
        let mut sockets = inner
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sockets.retain(|s| s.id != id);
        session
            .agent
            .as_ref()
            .is_some_and(|agent| sockets.iter().any(|s| s.bound.as_ref() == Some(agent)))
    };
    let Some(agent) = session.agent.as_ref().filter(|_| !others_bound) else {
        return;
    };
    let effects = {
        let mut core = inner
            .core
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        core.disconnect(agent, now_ms())
    };
    let (events, outbound) = shell::split_effects(effects);
    inner.deliver(None, outbound);
    inner.publish(events);
}

fn take_loss(inner: &Inner, agent: &AgentId, msg: &ClientMsg) -> Option<Lose> {
    let wanted = if let ClientMsg::Claim { .. } | ClientMsg::Amend { .. } = msg {
        Lose::ClaimReply
    } else if let ClientMsg::Release { .. } = msg {
        Lose::Release
    } else if let ClientMsg::Review { .. } = msg {
        Lose::Review
    } else {
        return None;
    };
    let mut pending = lock(&inner.lose);
    let at = pending
        .iter()
        .position(|(who, what)| *who == agent.0 && *what == wanted)?;
    Some(pending.remove(at).1)
}

// ---------- agents: real git repos running the real binary ----------

/// One agent's worktree: a temp git repo with two source files and its own `tessel` config.
/// Longest any test waits for one `tessel` command with piped input.
const COMMAND_DEADLINE: Duration = Duration::from_secs(30);

pub struct Agent {
    pub name: String,
    pub token: String,
    dir: TempDir,
    /// The git worktree: `dir`, or a nested directory of it.
    path: PathBuf,
    env: Vec<(String, String)>,
    url: String,
}

impl Agent {
    pub fn new(fake: &Fake, name: &str, token: &str) -> Result<Self> {
        Self::nested(fake, name, token, "")
    }

    /// Like `new`, with the worktree `nest` levels below the temp directory.
    pub fn nested(fake: &Fake, name: &str, token: &str, nest: &str) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join(nest);
        let root = path.as_path();
        std::fs::create_dir_all(root.join("src"))?;
        std::fs::write(root.join("src/a.rs"), "pub fn a() {}\n")?;
        std::fs::write(root.join("src/b.rs"), "pub fn b() {}\n")?;
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.test"],
            vec!["config", "user.name", "Test"],
            vec!["add", "src"],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            git(root, &args)?;
        }
        Ok(Self {
            name: name.into(),
            token: token.into(),
            dir,
            path,
            env: Vec::new(),
            url: fake.url.clone(),
        })
    }

    /// An agent whose directory is a git repository with no commit.
    pub fn without_commits(fake: &Fake, name: &str, token: &str) -> Result<Self> {
        let agent = Self::without_repo(fake, name, token)?;
        git(&agent.path, &["init", "-q"])?;
        Ok(agent)
    }

    /// An agent whose directory is not inside any git repository.
    pub fn without_repo(fake: &Fake, name: &str, token: &str) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().to_path_buf();
        Ok(Self {
            name: name.into(),
            token: token.into(),
            dir,
            path,
            env: Vec::new(),
            url: fake.url.clone(),
        })
    }

    /// Sets an environment variable for every command this agent runs.
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn root(&self) -> PathBuf {
        self.path
            .canonicalize()
            .unwrap_or_else(|_| self.path.clone())
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tessel"));
        command
            .args(args)
            .current_dir(&self.path)
            .env_remove("CLAUDE_PROJECT_DIR")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .env("TESSEL_COORDINATOR", &self.url)
            .env("TESSEL_REPO", REPO)
            .env("TESSEL_AGENT", &self.name)
            .env("TESSEL_TOKEN", &self.token)
            .stdin(Stdio::null());
        command
    }

    /// Runs `tessel args...` and waits for it to finish.
    pub fn tessel(&self, args: &[&str]) -> Result<Done> {
        let output = self.command(args).output().context("cannot run tessel")?;
        Ok(Done::from(output))
    }

    /// Runs `tessel args...` from `subdir` of the agent's directory.
    pub fn tessel_in(&self, args: &[&str], subdir: &str) -> Result<Done> {
        let output = self
            .command(args)
            .current_dir(self.path.join(subdir))
            .output()
            .context("cannot run tessel")?;
        Ok(Done::from(output))
    }

    /// Runs `tessel args...` with `stdin` as its standard input.
    pub fn tessel_with_stdin(&self, args: &[&str], stdin: &str) -> Result<Done> {
        self.tessel_with_stdin_bytes(args, stdin.as_bytes())
    }

    /// Like `tessel_with_stdin`, for input that need not be valid UTF-8.
    pub fn tessel_with_stdin_bytes(&self, args: &[&str], stdin: &[u8]) -> Result<Done> {
        use std::io::Write;
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(stdin)?;
        }
        // A command that never ends fails the test instead of hanging it.
        let pid = child.id();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(child.wait_with_output());
        });
        let Ok(output) = receiver.recv_timeout(COMMAND_DEADLINE) else {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            bail!("tessel {args:?} did not finish within {COMMAND_DEADLINE:?}");
        };
        Ok(Done::from(output?))
    }

    /// Runs the daemon in the foreground of a child process, as `tessel start` would detach it.
    pub fn spawn_daemon(&self, summary: &str) -> Result<std::process::Child> {
        let child = self
            .command(&["daemon", "--summary", summary])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(child)
    }

    /// Starts the daemon and requires it to come online.
    pub fn start(&self, intent: &str) -> Result<()> {
        let done = self.tessel(&["start", intent])?;
        if done.code != 0 {
            bail!(
                "start failed ({}): {}{}",
                done.code,
                done.stdout,
                done.stderr
            );
        }
        Ok(())
    }

    /// The hook JSON Claude Code would send for `tool` editing `path`.
    pub fn hook(&self, tool: &str, key: &str, path: &str) -> Result<Done> {
        let root = self.root().display().to_string();
        self.hook_at(tool, key, path, &self.path, &["--root", &root])
    }

    /// The hook JSON for `tool` with a full `tool_input`, as Claude Code sends an `Edit` or a
    /// `Write`.
    pub fn hook_input(&self, tool: &str, input: &serde_json::Value) -> Result<Done> {
        let root = self.root().display().to_string();
        let event = serde_json::json!({
            "tool_name": tool,
            "tool_input": input,
            "cwd": self.path,
        });
        self.tessel_with_stdin(&["hook", "pre-edit", "--root", &root], &event.to_string())
    }

    /// The hook JSON with no `cwd` field, run from `run_from`, which may be outside the worktree.
    pub fn hook_without_cwd(
        &self,
        tool: &str,
        key: &str,
        path: &str,
        run_from: &Path,
    ) -> Result<Done> {
        use std::io::Write;
        let root = self.root().display().to_string();
        let event = serde_json::json!({ "tool_name": tool, "tool_input": { key: path } });
        let mut child = self
            .command(&["hook", "pre-edit", "--root", &root])
            .current_dir(run_from)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(event.to_string().as_bytes())?;
        }
        Ok(Done::from(child.wait_with_output()?))
    }

    /// Like `hook`, with the event's `cwd` and the hook's extra arguments chosen by the caller.
    pub fn hook_at(
        &self,
        tool: &str,
        key: &str,
        path: &str,
        cwd: &Path,
        extra: &[&str],
    ) -> Result<Done> {
        let event = serde_json::json!({
            "tool_name": tool,
            "tool_input": { key: path },
            "cwd": cwd,
        });
        let mut args = vec!["hook", "pre-edit"];
        args.extend_from_slice(extra);
        self.tessel_with_stdin(&args, &event.to_string())
    }

    /// Runs `tessel args...` with `stdin` connected to a directory, which fails every read.
    pub fn tessel_with_unreadable_stdin(&self, args: &[&str]) -> Result<Done> {
        let output = self
            .command(args)
            .stdin(std::fs::File::open(self.dir.path())?)
            .output()?;
        Ok(Done::from(output))
    }

    /// Runs `tessel args...` from a directory that is deleted before `tessel` starts.
    pub fn tessel_in_deleted_cwd(&self, args: &[&str], stdin: &str) -> Result<Done> {
        use std::io::Write;
        let gone = self.dir.path().join("gone");
        std::fs::create_dir_all(&gone)?;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("cd \"$1\" && rmdir \"$1\" && shift && exec \"$@\"")
            .arg("sh")
            .arg(&gone)
            .arg(env!("CARGO_BIN_EXE_tessel"))
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .env("TESSEL_COORDINATOR", &self.url)
            .env("TESSEL_REPO", REPO)
            .env("TESSEL_AGENT", &self.name)
            .env("TESSEL_TOKEN", &self.token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(stdin.as_bytes())?;
        }
        Ok(Done::from(child.wait_with_output()?))
    }

    /// `tessel status --json`, parsed.
    pub fn status(&self) -> Result<serde_json::Value> {
        let done = self.tessel(&["status", "--json"])?;
        Ok(serde_json::from_str(&done.stdout)?)
    }

    pub fn held_claims(&self) -> Result<usize> {
        let status = self.status()?;
        Ok(status["state"]["claims"].as_array().map_or(0, Vec::len))
    }

    /// Everything under `.tessel/` as text, for scanning.
    pub fn tessel_files(&self) -> Result<String> {
        let mut all = String::new();
        for entry in std::fs::read_dir(self.root().join(".tessel"))? {
            let path = entry?.path();
            if path.is_file() {
                all.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
        Ok(all)
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        // Best effort: a failed test must not leave a daemon behind.
        let _ = self.command(&["stop"]).output();
    }
}

pub struct Done {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl From<Output> for Done {
    fn from(output: Output) -> Self {
        Self {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

impl Done {
    pub fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !output.status.success() {
        bail!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Polls `check` every 25 ms for up to `limit`, returning the first `Some`.
pub async fn eventually<T>(limit: Duration, check: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    eventually_every(limit, Duration::from_millis(25), check).await
}

/// Like `eventually`, polling every `every`. A probe that itself logs an event at the
/// coordinator (a denied claim) should not poll faster than the daemon's event-log read ends.
pub async fn eventually_every<T>(
    limit: Duration,
    every: Duration,
    mut check: impl FnMut() -> Result<Option<T>>,
) -> Result<T> {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if let Some(found) = check()? {
            return Ok(found);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("condition not met within {limit:?}");
        }
        tokio::time::sleep(every).await;
    }
}

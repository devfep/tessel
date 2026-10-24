//! Test support: a fake coordinator that runs the crate's real `Host` behind a local WebSocket
//! server, and helpers that run the real `tessel` binary in temp git repos.
//!
//! The fake is the adapter. It owns the WebSocket plumbing (listener, token check, path check,
//! 503 refusal, one task per socket) and the fault-injection seams. Everything a coordinator
//! decides is the host's: each wire event becomes an `Input` stamped from the fake's clock, goes
//! through `Host::step`, and each `Output` is carried out here the way an adapter would: a store,
//! sends to the sockets' channels, closes, the wake timer, and calls left for `merge_next` and
//! `verify_next` to answer. One lock covers the host and everything it touches, and the host's own
//! storage and replay requests are answered before the next input, so inputs reach it one at a
//! time with nothing outstanding.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tessel_coordinator::host::call::{Call, CallAnswer, Request as CallRequest, TrialResult};
use tessel_coordinator::host::{Host, HostConfig, Input, Output as HostOutput, SocketId};
use tessel_coordinator::protocol::{
    AgentId, ClientMsg, CommitId, Event, ProtocolRange, ServerMsg, PROTOCOL_VERSION,
};
use tessel_coordinator::shell::{self, Action, Session, EVENT_PREFIX, STATE_KEY};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch, Notify};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

pub const REPO: &str = "demo";

/// The most inputs one wire event may cause before the fake gives up: far more than any needs.
const FEED_LIMIT: usize = 10_000;

/// `cut_replay`'s value while no replay is cut.
const NO_CUT: usize = usize::MAX;

/// The longest the wake timer sleeps before it looks at the clock again.
const WAKE_RECHECK: Duration = Duration::from_secs(1);

pub fn now_ms() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

/// What a socket task writes: a message the protocol knows, a text frame as given, or a close
/// frame that ends the socket.
#[derive(Clone)]
enum Wire {
    Msg(ServerMsg),
    Raw(String),
    Close { code: u16, reason: String },
}

/// A socket the fake serves, as the host has last said it stands.
struct SocketEntry {
    session: Session,
    /// The socket has seen a `go_silent` or `go_deaf` request and reads nothing more.
    muted: bool,
    /// The log is being replayed to it: events written now are replay pages, not live events.
    replaying: bool,
    /// Its replay was cut short: the socket is dropped as soon as the replay is over.
    cut: bool,
    tx: mpsc::UnboundedSender<Wire>,
}

/// What becomes of the sends one frame causes to the socket it came on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Replies {
    Deliver,
    /// Kept until `release_held`.
    Park,
    Lose,
}

/// The socket whose frame is being handled and what to do with the sends it causes to it.
struct Origin {
    sock: SocketId,
    replies: Replies,
}

/// Everything the host touches, behind one lock.
struct State {
    host: Host,
    config: HostConfig,
    /// Inputs fed so far.
    fed: u64,
    store: BTreeMap<String, String>,
    sockets: BTreeMap<SocketId, SocketEntry>,
    next_socket: u64,
    /// Calls the host has handed out and nobody has answered.
    calls: Vec<Call>,
    /// The head of the last merge `merge_next` landed, which is what a head read answers.
    landed: Option<CommitId>,
    /// The wake the host last asked for, on the fake's clock.
    wake_at: Option<u64>,
    logs: Vec<String>,
    received: Vec<(AgentId, ClientMsg)>,
    parked: Vec<(SocketId, ServerMsg)>,
    origin: Option<Origin>,
    origin_sends: Vec<ServerMsg>,
}

struct Inner {
    state: Mutex<State>,
    tokens: HashMap<String, String>,
    accepting: AtomicBool,
    cut_replay: AtomicUsize,
    skip_seq: AtomicU64,
    raw_in_replay: Mutex<Option<(u64, String)>>,
    stall_live: AtomicBool,
    hold_claim_reply: AtomicBool,
    /// Event-log readers' `Watch` frames wait here while `hold_log_reads` is on.
    hold_watch: AtomicBool,
    watch_gate: Notify,
    lose: Mutex<Vec<(String, Lose)>>,
    /// Milliseconds the coordinator clock runs ahead of the real one (`Fake::advance_clock`).
    skew_ms: AtomicU64,
    /// When `Fake::freeze_clock` stopped the coordinator clock; zero while it runs.
    frozen_at_ms: AtomicU64,
    /// The wake changed, or the clock did: the wake timer looks again.
    wake_changed: Notify,
    /// Why the wake timer's task stopped, if it did: the panic that ended it, naming the call or
    /// the check that failed. The task cannot fail a test itself, so the fake reports it.
    timer_failure: Mutex<Option<String>>,
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The commit id of 40 copies of `digit`, which stands for a commit the fake never made.
fn sha(digit: char) -> CommitId {
    CommitId(digit.to_string().repeat(40))
}

/// The text frame a client message goes out as.
fn frame_text(msg: &ClientMsg) -> String {
    let text = serde_json::to_string(msg);
    assert!(text.is_ok(), "a client message serializes: {text:?}");
    text.unwrap_or_default()
}

/// Every stored event, by `seq`.
fn stored_events(store: &BTreeMap<String, String>) -> Vec<Event> {
    let mut events = Vec::new();
    for (key, json) in store {
        if key.starts_with(EVENT_PREFIX) {
            let parsed = serde_json::from_str::<Event>(json);
            assert!(
                parsed.is_ok(),
                "the stored event {key} does not parse: {parsed:?}"
            );
            events.extend(parsed);
        }
    }
    events.sort_by_key(|event| event.seq);
    events
}

/// The state of a repository with nothing in it yet, with the lease the test asked for. The host
/// builds a fresh core with the production lease, so the store starts with this one instead.
fn initial_state(lease_ms: u64) -> Result<String> {
    ensure!(
        lease_ms > 0,
        "a lease of zero ms would end every grant at once"
    );
    let core = shell::load_core(None, Some("test"), Some("false"), Vec::new(), Vec::new())
        .map_err(|e| anyhow::anyhow!("building the first state: {e}"))?;
    let mut state = serde_json::to_value(&core)?;
    *state
        .pointer_mut("/config/lease_ms")
        .context("the core's state has no config.lease_ms")? = lease_ms.into();
    Ok(state.to_string())
}

impl Inner {
    /// The time the host is told, in milliseconds: the real time, or the instant the clock was
    /// frozen at, plus whatever `advance_clock` has added.
    fn clock(&self) -> u64 {
        let frozen = self.frozen_at_ms.load(Ordering::SeqCst);
        let base = if frozen == 0 { now_ms() } else { frozen };
        base + self.skew_ms.load(Ordering::SeqCst)
    }

    async fn wait_if_log_read(&self, text: &str) {
        if !matches!(shell::parse_client_msg(text), Ok(ClientMsg::Watch { .. })) {
            return;
        }
        loop {
            let released = self.watch_gate.notified();
            if !self.hold_watch.load(Ordering::SeqCst) {
                return;
            }
            released.await;
        }
    }

    fn mark_muted(&self, sock: SocketId) {
        if let Some(entry) = lock(&self.state).sockets.get_mut(&sock) {
            entry.muted = true;
        }
    }

    /// Hands `input` to the host and carries out what it asks, then answers what the host asks of
    /// storage and of the calls only the fake makes, until the host waits for nothing.
    fn feed(&self, st: &mut State, input: Input, origin: Option<Origin>) {
        st.origin = origin;
        st.fed += 1;
        let mut pending = VecDeque::from([input]);
        let mut steps = 0;
        while let Some(input) = pending.pop_front() {
            assert!(
                steps < FEED_LIMIT,
                "the host still asks for answers after {FEED_LIMIT} inputs; it logged {:?}",
                st.logs
            );
            steps += 1;
            let outputs = st.host.step(input);
            self.execute(st, outputs, &mut pending);
        }
        self.finish_replies(st);
    }

    fn execute(&self, st: &mut State, outputs: Vec<HostOutput>, pending: &mut VecDeque<Input>) {
        let now_ms = self.clock();
        for output in outputs {
            match output {
                HostOutput::Load => {
                    let stored = st.store.get(STATE_KEY).cloned();
                    pending.push_back(Input::Loaded { now_ms, stored });
                }
                HostOutput::Commit { batch, entries } => {
                    st.store.extend(entries);
                    pending.push_back(Input::Committed { now_ms, batch });
                }
                HostOutput::Send { sock, msg } => self.send(st, sock, msg),
                HostOutput::SetSession { sock, session } => set_session(st, sock, session),
                HostOutput::Close { sock, code, reason } => {
                    if let Some(entry) = st.sockets.get(&sock) {
                        let _ = entry.tx.send(Wire::Close { code, reason });
                    }
                }
                HostOutput::ReadEvents {
                    sock,
                    from_seq,
                    limit,
                } => {
                    let (events, done) = self.page(st, sock, from_seq, limit);
                    pending.push_back(Input::ReplayPage {
                        now_ms,
                        sock,
                        events,
                        done,
                    });
                }
                HostOutput::SetWake { at_ms } => {
                    st.wake_at = at_ms;
                    self.wake_changed.notify_one();
                }
                HostOutput::Call(call) => take_call(st, call, now_ms, pending),
                HostOutput::Log { line } => st.logs.push(line),
                HostOutput::Metric { .. } => {}
            }
        }
    }

    /// A page of the stored log for the replay of `sock`; `done` when the socket is gone.
    ///
    /// Two things here are the fake's own. The cut (`cut_replay`) keeps the first events of the
    /// log and is seen by the daemon as a read that never reaches its end. The page size is not
    /// seen at all: a fake that handed back the whole log in one page would be read the same way,
    /// because the host asks again for the rest and is told there is none, so no CLI test can
    /// tell `take(limit)` from its absence.
    fn page(
        &self,
        st: &mut State,
        sock: SocketId,
        from_seq: u64,
        limit: usize,
    ) -> (Vec<Event>, bool) {
        let Some(entry) = st.sockets.get_mut(&sock).filter(|e| !e.session.closed) else {
            return (Vec::new(), true);
        };
        let keep = self.cut_replay.load(Ordering::SeqCst);
        entry.replaying = true;
        entry.cut = keep != NO_CUT;
        let mut log = stored_events(&st.store);
        log.truncate(keep);
        let page = log
            .into_iter()
            .filter(|event| event.seq >= from_seq)
            .take(limit)
            .collect();
        (page, false)
    }

    /// Writes `msg` to `sock`, unless it is the socket whose frame is being handled and its
    /// sends are kept for `finish_replies`.
    fn send(&self, st: &mut State, sock: SocketId, msg: ServerMsg) {
        if st.origin.as_ref().is_some_and(|origin| origin.sock == sock) {
            st.origin_sends.push(msg);
            return;
        }
        self.write(st, sock, msg);
    }

    fn write(&self, st: &State, sock: SocketId, msg: ServerMsg) {
        let Some(entry) = st.sockets.get(&sock) else {
            return;
        };
        let wire = if let ServerMsg::Event { event } = msg {
            let Some(wire) = self.event_wire(entry.replaying, event) else {
                return;
            };
            wire
        } else {
            Wire::Msg(msg)
        };
        let _ = entry.tx.send(wire);
    }

    /// A live event is held back while `stall_live_events` is on. A replayed one may be left
    /// out, or sent as a frame of the test's own.
    fn event_wire(&self, replaying: bool, event: Event) -> Option<Wire> {
        if !replaying {
            let live = !self.stall_live.load(Ordering::SeqCst);
            return live.then_some(Wire::Msg(ServerMsg::Event { event }));
        }
        if event.seq == self.skip_seq.load(Ordering::SeqCst) {
            return None;
        }
        match lock(&self.raw_in_replay).as_ref() {
            Some((seq, text)) if *seq == event.seq => Some(Wire::Raw(text.clone())),
            Some(_) | None => Some(Wire::Msg(ServerMsg::Event { event })),
        }
    }

    /// The sends the handled frame caused to its own socket, delivered, kept or lost as the
    /// frame's origin says.
    fn finish_replies(&self, st: &mut State) {
        let Some(origin) = st.origin.take() else {
            return;
        };
        let sends = std::mem::take(&mut st.origin_sends);
        match origin.replies {
            Replies::Deliver => {
                for msg in sends {
                    self.write(st, origin.sock, msg);
                }
            }
            Replies::Park => st
                .parked
                .extend(sends.into_iter().map(|msg| (origin.sock, msg))),
            Replies::Lose => {}
        }
    }

    /// A socket opened for `agent`, whose token the handshake accepted.
    fn open_socket(&self, agent: AgentId, tx: mpsc::UnboundedSender<Wire>) -> SocketId {
        let mut st = lock(&self.state);
        let sock = SocketId(st.next_socket);
        st.next_socket += 1;
        let session = Session {
            verified: Some(agent.clone()),
            ..Session::default()
        };
        st.sockets.insert(
            sock,
            SocketEntry {
                session,
                muted: false,
                replaying: false,
                cut: false,
                tx,
            },
        );
        let now_ms = self.clock();
        let opened = Input::Opened {
            now_ms,
            sock,
            verified: agent,
        };
        self.feed(&mut st, opened, None);
        sock
    }

    /// A text frame arrived on `sock`. True when the socket must end now: the frame was lost on
    /// its way, its reply was, or its replay was cut.
    fn handle_text(&self, sock: SocketId, text: &str) -> bool {
        let mut guard = lock(&self.state);
        let st = &mut *guard;
        let now_ms = self.clock();
        let frame = Input::Frame {
            now_ms,
            sock,
            text: text.to_string(),
        };
        let Ok(msg) = shell::parse_client_msg(text) else {
            self.feed(st, frame, None);
            return false;
        };
        let Some(session) = st.sockets.get(&sock).map(|entry| entry.session.clone()) else {
            return true;
        };
        let lost = session
            .agent
            .as_ref()
            .and_then(|agent| self.take_loss(agent, &msg));
        let replies = match lost {
            Some(Lose::Release | Lose::Review) => return true,
            Some(Lose::ClaimReply) => Replies::Lose,
            None if self.takes_held_reply(&msg) => Replies::Park,
            None => Replies::Deliver,
        };
        if let Action::Call { agent } = shell::decide(&session, &msg) {
            st.received.push((agent, msg));
        }
        let kept = replies != Replies::Deliver;
        self.feed(st, frame, kept.then_some(Origin { sock, replies }));
        replies == Replies::Lose || st.sockets.get(&sock).is_some_and(|entry| entry.cut)
    }

    /// Whether `msg` is the claim or amend whose reply `hold_next_claim_reply` keeps back.
    fn takes_held_reply(&self, msg: &ClientMsg) -> bool {
        let claims = matches!(msg, ClientMsg::Claim { .. } | ClientMsg::Amend { .. });
        claims && self.hold_claim_reply.swap(false, Ordering::SeqCst)
    }

    fn take_loss(&self, agent: &AgentId, msg: &ClientMsg) -> Option<Lose> {
        let wanted = if let ClientMsg::Claim { .. } | ClientMsg::Amend { .. } = msg {
            Lose::ClaimReply
        } else if let ClientMsg::Release { .. } = msg {
            Lose::Release
        } else if let ClientMsg::Review { .. } = msg {
            Lose::Review
        } else {
            return None;
        };
        let mut pending = lock(&self.lose);
        let at = pending
            .iter()
            .position(|(who, what)| *who == agent.0 && *what == wanted)?;
        Some(pending.remove(at).1)
    }

    fn handle_binary(&self, sock: SocketId) {
        let mut st = lock(&self.state);
        let now_ms = self.clock();
        self.feed(&mut st, Input::Binary { now_ms, sock }, None);
    }

    /// The socket's task ended, however it did.
    fn close_socket(&self, sock: SocketId) {
        let mut st = lock(&self.state);
        st.sockets.remove(&sock);
        let now_ms = self.clock();
        self.feed(&mut st, Input::Closed { now_ms, sock }, None);
    }

    /// Fails the test, naming the call or the check, if the wake timer's task has died.
    fn assert_timer_alive(&self) {
        let failure = lock(&self.timer_failure).clone();
        assert!(
            failure.is_none(),
            "the fake's wake timer failed: {}",
            failure.unwrap_or_default()
        );
    }

    fn wake_is_due(&self, st: &State) -> bool {
        st.wake_at.is_some_and(|at| at <= self.clock())
    }

    /// Runs the wake if the host's is due on the fake's clock. True if it ran.
    fn fire_due_wake(&self) -> bool {
        let mut st = lock(&self.state);
        if !self.wake_is_due(&st) {
            return false;
        }
        let now_ms = self.clock();
        st.wake_at = None;
        self.feed(&mut st, Input::Wake { now_ms }, None);
        true
    }

    /// How long the timer may sleep before it looks again; none while there is no wake, or while
    /// a frozen clock cannot reach it until `advance_clock` moves it.
    fn wake_wait(&self) -> Option<Duration> {
        let at = lock(&self.state).wake_at?;
        let now_ms = self.clock();
        if at > now_ms && self.frozen_at_ms.load(Ordering::SeqCst) != 0 {
            return None;
        }
        let wait = Duration::from_millis(at.saturating_sub(now_ms).max(1));
        Some(wait.min(WAKE_RECHECK))
    }

    /// The outstanding call `wanted` picks. A wake is run first if no call is out, since only a
    /// wake issues one.
    fn call_of(&self, st: &mut State, wanted: fn(&CallRequest) -> bool) -> Option<Call> {
        let find = |st: &State| st.calls.iter().find(|call| wanted(&call.request)).cloned();
        if let Some(call) = find(st) {
            return Some(call);
        }
        if !st.calls.is_empty() {
            return None;
        }
        st.wake_at = None;
        let now_ms = self.clock();
        self.feed(st, Input::Wake { now_ms }, None);
        find(st)
    }

    fn answer(&self, st: &mut State, call: &Call, answer: CallAnswer) {
        st.calls.retain(|out| out.id != call.id);
        let now_ms = self.clock();
        let replied = Input::Replied {
            now_ms,
            call: call.id,
            answer,
        };
        self.feed(st, replied, None);
    }
}

fn set_session(st: &mut State, sock: SocketId, session: Session) {
    let Some(entry) = st.sockets.get_mut(&sock) else {
        return;
    };
    if session.watch_from.is_some() {
        entry.replaying = false;
    }
    entry.session = session;
}

/// Whether `request` is the merge preparation that `merge_next` answers.
fn is_merge_call(request: &CallRequest) -> bool {
    match request {
        CallRequest::Prepare { .. } => true,
        CallRequest::Test { .. }
        | CallRequest::Land { .. }
        | CallRequest::Trial { .. }
        | CallRequest::ReadHead { .. }
        | CallRequest::Reconcile { .. } => false,
    }
}

/// Whether `request` is the verification trial that `verify_next` answers.
fn is_trial_call(request: &CallRequest) -> bool {
    match request {
        CallRequest::Trial { .. } => true,
        CallRequest::Prepare { .. }
        | CallRequest::Test { .. }
        | CallRequest::Land { .. }
        | CallRequest::ReadHead { .. }
        | CallRequest::Reconcile { .. } => false,
    }
}

/// A call the host handed out. A head read is answered at once, with the head of the last merge
/// the fake landed; a merge or a trial waits for `merge_next` or `verify_next`. The host does not
/// issue any other call yet, so one that arrives fails the test instead of waiting unanswered.
///
/// The head-read answer is fake-only. A real lander reads a trunk that anything can move; the
/// fake has no trunk and never pokes one, so the head it reports is the one `merge_next` last
/// landed, or none. The host turns that answer into the trunk moves it announces, so no CLI test
/// can see a trunk that moved by any means other than a landing the fake made.
fn take_call(st: &mut State, call: Call, now_ms: u64, pending: &mut VecDeque<Input>) {
    match &call.request {
        CallRequest::Prepare { .. } | CallRequest::Trial { .. } => st.calls.push(call),
        CallRequest::ReadHead { .. } => {
            let answer = CallAnswer::Head {
                main: st.landed.clone(),
                landed: Vec::new(),
            };
            let call = call.id;
            pending.push_back(Input::Replied {
                now_ms,
                call,
                answer,
            });
        }
        CallRequest::Test { .. } | CallRequest::Land { .. } | CallRequest::Reconcile { .. } => {
            unreachable!("the host issued a call the fake does not answer: {call:?}");
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
        Self::start_announcing(lease_ms, agents, None).await
    }

    /// Like `start`, with the host announcing `range` in every `Welcome` (its `HostConfig` says
    /// so); `None` announces the range of this build.
    pub async fn start_announcing(
        lease_ms: u64,
        agents: &[(&str, &str)],
        range: Option<ProtocolRange>,
    ) -> Result<Self> {
        let config = HostConfig {
            repo: REPO.to_string(),
            run: Some("test".to_string()),
            shadow_enabled: Some("false".to_string()),
            protocol_range: range,
            ..HostConfig::default()
        };
        let store = BTreeMap::from([(STATE_KEY.to_string(), initial_state(lease_ms)?)]);
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                host: Host::new(config.clone()),
                config,
                fed: 0,
                store,
                sockets: BTreeMap::new(),
                next_socket: 0,
                calls: Vec::new(),
                landed: None,
                wake_at: None,
                logs: Vec::new(),
                received: Vec::new(),
                parked: Vec::new(),
                origin: None,
                origin_sends: Vec::new(),
            }),
            tokens: agents
                .iter()
                .map(|(agent, token)| ((*token).to_string(), (*agent).to_string()))
                .collect(),
            accepting: AtomicBool::new(true),
            cut_replay: AtomicUsize::new(NO_CUT),
            skip_seq: AtomicU64::new(u64::MAX),
            raw_in_replay: Mutex::new(None),
            stall_live: AtomicBool::new(false),
            hold_claim_reply: AtomicBool::new(false),
            hold_watch: AtomicBool::new(false),
            watch_gate: Notify::new(),
            lose: Mutex::new(Vec::new()),
            skew_ms: AtomicU64::new(0),
            frozen_at_ms: AtomicU64::new(0),
            wake_changed: Notify::new(),
            timer_failure: Mutex::new(None),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let (kill, kill_rx) = watch::channel((0, false));
        let (mute, mute_rx) = watch::channel((0, false));
        tokio::spawn(accept(listener, Arc::clone(&inner), kill_rx, mute_rx));
        tokio::spawn(wake_loop(Arc::clone(&inner)));
        Ok(Self {
            url,
            inner,
            kill,
            mute,
        })
    }

    /// Parks every `Watch` frame, and so the `Hello` queued behind it, until `release_log_reads`.
    /// The daemon's event-log read then stays in flight while other sockets are served.
    pub fn hold_log_reads(&self) {
        self.inner.hold_watch.store(true, Ordering::SeqCst);
    }

    /// Lets the parked `Watch` frames, and every later one, through.
    pub fn release_log_reads(&self) {
        self.inner.hold_watch.store(false, Ordering::SeqCst);
        self.inner.watch_gate.notify_waiters();
    }

    /// Keeps the reply to the next `Claim` or `Amend` back until `release_held`.
    pub fn hold_next_claim_reply(&self) {
        self.inner.hold_claim_reply.store(true, Ordering::SeqCst);
    }

    /// Delivers the replies kept back by `hold_next_claim_reply`.
    pub fn release_held(&self) {
        let mut st = lock(&self.inner.state);
        for (sock, msg) in std::mem::take(&mut st.parked) {
            self.inner.write(&st, sock, msg);
        }
    }

    /// Names the agents who may review (the host's `reviewers`). The host reads them when it
    /// loads the core, so this must come before the first socket opens.
    pub fn set_reviewers(&self, reviewers: &[&str]) {
        let mut st = lock(&self.inner.state);
        assert_eq!(
            st.fed, 0,
            "set_reviewers must come before any socket opens: the host reads it on load"
        );
        st.config.reviewers = Some(reviewers.join(","));
        st.host = Host::new(st.config.clone());
    }

    /// Answers the merge the host has asked the lander for with a landing on main, as the
    /// lander would, and so sends `Merged` to the submitter if it has a socket open. If no merge
    /// is out, a wake runs first; if none is queued, nothing happens.
    pub fn merge_next(&self) {
        let mut guard = lock(&self.inner.state);
        let st = &mut *guard;
        let Some(call) = self.inner.call_of(st, is_merge_call) else {
            let trial_out = st.calls.iter().any(|call| is_trial_call(&call.request));
            assert!(
                !trial_out,
                "merge_next: a trial is outstanding, so no wake can start a merge; answer it with \
                 verify_next first"
            );
            return;
        };
        let (base, head) = (sha('b'), sha('c'));
        st.landed = Some(head.clone());
        self.inner
            .answer(st, &call, CallAnswer::Prepared { base, head });
    }

    /// Answers the next verification (a race entry's trial) the host has asked the lander for,
    /// with its tests passing or failing. Returns false when nothing was ready to run.
    pub fn verify_next(&self, passed: bool) -> bool {
        let mut guard = lock(&self.inner.state);
        let st = &mut *guard;
        let Some(call) = self.inner.call_of(st, is_trial_call) else {
            return false;
        };
        let result = if passed {
            TrialResult::Clean
        } else {
            TrialResult::TestsFailed
        };
        let answer = CallAnswer::Trialled {
            before: None,
            after: Some(result),
        };
        self.inner.answer(st, &call, answer);
        true
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

    /// Every open socket has taken up a `go_silent` or `go_deaf` request. The request itself only
    /// queues the change; until this is true a socket may still read a frame and answer it.
    pub fn all_sockets_muted(&self) -> bool {
        let st = lock(&self.inner.state);
        !st.sockets.is_empty() && st.sockets.values().all(|entry| entry.muted)
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
        self.inner.assert_timer_alive();
        lock(&self.inner.state)
            .sockets
            .values()
            .filter(|entry| entry.session.watch_from.is_some())
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

    /// Sends this text frame, as given, in place of the event with this `seq` in every replay.
    pub fn replace_in_replay(&self, seq: u64, frame: &str) {
        *lock(&self.inner.raw_in_replay) = Some((seq, frame.to_string()));
    }

    /// Sends this text frame, as given, to every open socket bound to `agent`.
    pub fn send_raw(&self, agent: &str, frame: &str) {
        self.to_agent(agent, &Wire::Raw(frame.to_string()));
    }

    /// Makes every event-log replay carry only the first `keep` events of the log and then close,
    /// as a dropped connection would: the reader gets those events and no end marker. `None` turns
    /// it off. Counting from the start keeps the cut where the test put it, whatever the reconnect
    /// itself adds to the end of the log.
    pub fn cut_replay(&self, keep: Option<usize>) {
        self.inner
            .cut_replay
            .store(keep.unwrap_or(NO_CUT), Ordering::SeqCst);
    }

    /// Loses the next message of this kind from `agent`.
    pub fn lose_next(&self, agent: &str, what: Lose) {
        lock(&self.inner.lose).push((agent.to_string(), what));
    }

    /// Makes `agent` send `msg` to the coordinator from a socket of its own that has no peer: the
    /// fake opens it, says hello, sends `msg`, and closes it, so the host sees what it would see
    /// from a connection that came and went. Its replies go nowhere, and neither the hello nor
    /// `msg` counts in `received`. The hello is one more `AgentConnected` in the log, and the
    /// close withdraws the agent's queued request only if the agent has no other socket open.
    pub fn act(&self, agent: &str, msg: ClientMsg) {
        let agent = AgentId(agent.into());
        let hello = ClientMsg::Hello {
            agent: agent.clone(),
            base: CommitId("0".repeat(40)),
            protocol: PROTOCOL_VERSION,
        };
        let mut st = lock(&self.inner.state);
        let sock = SocketId(st.next_socket);
        st.next_socket += 1;
        let opened = Input::Opened {
            now_ms: self.inner.clock(),
            sock,
            verified: agent,
        };
        self.inner.feed(&mut st, opened, None);
        for frame in [hello, msg] {
            let text = frame_text(&frame);
            let now_ms = self.inner.clock();
            self.inner
                .feed(&mut st, Input::Frame { now_ms, sock, text }, None);
        }
        let closed = Input::Closed {
            now_ms: self.inner.clock(),
            sock,
        };
        self.inner.feed(&mut st, closed, None);
    }

    /// Sends `msg` to every open socket bound to `agent`.
    pub fn push(&self, agent: &str, msg: ServerMsg) {
        self.to_agent(agent, &Wire::Msg(msg));
    }

    fn to_agent(&self, agent: &str, wire: &Wire) {
        let agent = AgentId(agent.into());
        let st = lock(&self.inner.state);
        for entry in st
            .sockets
            .values()
            .filter(|entry| entry.session.agent.as_ref() == Some(&agent))
        {
            let _ = entry.tx.send(wire.clone());
        }
    }

    /// Moves the coordinator's clock `ms` ahead of the real one, as if that long had passed
    /// there. Leases end on this clock; the daemons, which are real processes, keep the real
    /// one. Follow it with `settle_expiry` before asserting on what expired.
    pub fn advance_clock(&self, ms: u64) {
        self.inner.skew_ms.fetch_add(ms, Ordering::SeqCst);
        self.inner.wake_changed.notify_one();
    }

    /// Stops the coordinator clock: from now on it moves only with `advance_clock`, so no lease
    /// ends because the machine was slow. Call it before any claim is made.
    pub fn freeze_clock(&self) {
        self.inner.frozen_at_ms.store(now_ms(), Ordering::SeqCst);
        self.inner.wake_changed.notify_one();
    }

    /// Runs every wake that is due on the coordinator's clock, so what was due at the current
    /// clock has been applied when it returns: leases that ended are expired, and their grants
    /// made. The timer does the same on its own; this makes the moment certain.
    pub async fn settle_expiry(&self) {
        // A wake sets the next one for later; a host that wakes itself again and again is stuck.
        const WAKES_MAX: usize = 100;
        self.inner.assert_timer_alive();
        tokio::task::yield_now().await;
        for _ in 0..WAKES_MAX {
            if !self.inner.fire_due_wake() {
                return;
            }
        }
        let st = lock(&self.inner.state);
        assert!(
            !self.inner.wake_is_due(&st),
            "the host is still due a wake after {WAKES_MAX}; it logged {:?}",
            st.logs
        );
    }

    /// How many heartbeats `agent` has sent.
    pub fn heartbeats(&self, agent: &str) -> usize {
        self.received(agent)
            .iter()
            .filter(|msg| matches!(msg, ClientMsg::Heartbeat))
            .count()
    }

    /// The coordinator's event log so far: what the host has stored.
    pub fn events(&self) -> Vec<Event> {
        self.inner.assert_timer_alive();
        stored_events(&lock(&self.inner.state).store)
    }

    /// Every message `agent` has sent the host, in order.
    pub fn received(&self, agent: &str) -> Vec<ClientMsg> {
        self.inner.assert_timer_alive();
        let st = lock(&self.inner.state);
        st.received
            .iter()
            .filter(|(who, _)| who.0 == agent)
            .map(|(_, msg)| msg.clone())
            .collect()
    }
}

impl Drop for Fake {
    /// A test that waited on something the dead timer would have caused ends in a timeout; this
    /// makes the run say why. It stays quiet while another panic is already unwinding.
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.inner.assert_timer_alive();
        }
    }
}

/// Fires the host's wake when the fake's clock reaches it, and looks again whenever the wake or
/// the clock changes. If the wake panics (a call the fake cannot answer, a host that never settles)
/// the panic is kept in `timer_failure` and the task ends.
async fn wake_loop(inner: Arc<Inner>) {
    loop {
        let fired = std::panic::catch_unwind(AssertUnwindSafe(|| inner.fire_due_wake()));
        if let Err(panic) = fired {
            let why = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("a panic with no message");
            *lock(&inner.timer_failure) = Some(why.to_string());
            return;
        }
        match inner.wake_wait() {
            Some(wait) => {
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = inner.wake_changed.notified() => {}
                }
            }
            None => inner.wake_changed.notified().await,
        }
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
                *lock(&seen) = Some(AgentId(agent.clone()));
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
    let Some(agent) = lock(&verified).clone() else {
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<Wire>();
    let id = inner.open_socket(agent, tx);
    loop {
        tokio::select! {
            _ = mute.changed() => {
                (deaf, mute_writes) = (true, !mute.borrow_and_update().1);
                inner.mark_muted(id);
            }
            _ = kill.changed() => {
                if !keeps_open(&inner, id, *kill.borrow_and_update()) {
                    break;
                }
            }
            outgoing = rx.recv(), if !mute_writes => {
                let Some(msg) = outgoing else { break };
                match write_wire(&mut socket, msg, deaf).await {
                    Ok(true) => {}
                    Ok(false) | Err(_) => break,
                }
            }
            frame = socket.next(), if !deaf => match frame {
                Some(Ok(Message::Text(text))) => {
                    inner.wait_if_log_read(&text).await;
                    if inner.handle_text(id, &text) {
                        flush_queued(&mut socket, &mut rx, deaf).await;
                        break;
                    }
                }
                Some(Ok(Message::Binary(_))) => inner.handle_binary(id),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
    }
    inner.close_socket(id);
}

/// Writes what the host already queued for the socket before the fake cuts it, then closes it,
/// as a connection that drops after the events sent so far have been carried: the events of a cut
/// replay arrive, and the end marker does not. The close is a frame, and the peer's answer is
/// awaited, so that its unread events are not lost to a reset.
async fn flush_queued(
    socket: &mut WebSocketStream<TcpStream>,
    rx: &mut mpsc::UnboundedReceiver<Wire>,
    deaf: bool,
) {
    while let Ok(wire) = rx.try_recv() {
        if !matches!(write_wire(socket, wire, deaf).await, Ok(true)) {
            return;
        }
    }
    let _ = socket.close(None).await;
    // The reader sends its `Hello` right behind its `Watch`. Closing with that frame unread makes
    // the OS reset the connection, and a reset throws away the events the reader has not read yet,
    // so the close handshake is finished before the socket is dropped. A passing run returns as
    // soon as the daemon answers the close; the 2 s only bounds a daemon that never does. Its
    // expiry is not recorded: the drop that follows is the same cut a network gives, and the
    // lost events then show as the failing assertion on what the read held.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while socket.next().await.is_some() {}
    })
    .await;
}

/// Writes `wire`, followed by stray pongs when the link is one-way. False once the socket is to
/// end: the host closed it, or the fake cut it.
async fn write_wire(
    socket: &mut WebSocketStream<TcpStream>,
    wire: Wire,
    deaf: bool,
) -> Result<bool, WsError> {
    let text = match wire {
        Wire::Msg(msg) => serde_json::to_string(&msg).map_err(|e| WsError::Io(e.into()))?,
        Wire::Raw(text) => text,
        Wire::Close { code, reason } => {
            let frame = CloseFrame {
                code: CloseCode::from(code),
                reason: reason.into(),
            };
            socket.send(Message::Close(Some(frame))).await?;
            return Ok(false);
        }
    };
    socket.send(Message::text(text)).await?;
    if deaf {
        stray_pongs(socket).await?;
    }
    Ok(true)
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
fn keeps_open(inner: &Inner, sock: SocketId, (_, main_only): (u64, bool)) -> bool {
    let reading = lock(&inner.state)
        .sockets
        .get(&sock)
        .is_some_and(|entry| entry.session.watch_from.is_some());
    main_only && reading
}

// ---------- agents: real git repos running the real binary ----------

/// Longest any test waits for one `tessel` command with piped input.
const COMMAND_DEADLINE: Duration = Duration::from_secs(30);

/// One agent's worktree: a temp git repo with two source files and its own `tessel` config.
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
        Self::nested_at(&fake.url, name, token, nest)
    }

    /// Like `new`, for a coordinator that is not the fake: `url` is its address.
    pub fn at(url: &str, name: &str, token: &str) -> Result<Self> {
        Self::nested_at(url, name, token, "")
    }

    fn nested_at(url: &str, name: &str, token: &str, nest: &str) -> Result<Self> {
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
            url: url.to_string(),
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

    /// Spawns `tessel args...` with piped stdin and stdout and no stderr, for a long-lived
    /// session.
    pub fn spawn_piped(&self, args: &[&str]) -> Result<std::process::Child> {
        Ok(self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?)
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

    /// Starts the daemon, then waits until its first event-log read has completed and it is
    /// online. `start` returns once the working socket is welcomed; the daemon then opens a second
    /// connection whose own `Hello` reads the log. The `(complete)` line is logged only after the
    /// coordinator answered that `Hello` and the read reached its marker, so from then on every
    /// further hello at the coordinator is a redial. A truncated read logs a different line and
    /// does not prove the `Hello` was handled.
    pub async fn start_settled(&self, intent: &str) -> Result<()> {
        self.start(intent)?;
        let log = self.root().join(".tessel/daemon.log");
        eventually(Duration::from_secs(8), || {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let online = self.status()?["state"]["connection"] == "online";
            let read = text
                .lines()
                .any(|l| l.contains("event log read:") && l.contains("(complete)"));
            Ok((read && online).then_some(()))
        })
        .await
        .context("the first event-log read never completed with the daemon online")
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
        let child = self.spawn_in_deleted_cwd(args)?;
        send_stdin(child, stdin)
    }

    /// Spawns `tessel args...` from a directory that is deleted before `tessel` starts.
    pub fn spawn_in_deleted_cwd(&self, args: &[&str]) -> Result<Child> {
        let gone = self.dir.path().join("gone");
        std::fs::create_dir_all(&gone)?;
        let child = Command::new("sh")
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
        Ok(child)
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

/// Writes `stdin` to the child, closes its input and waits for it to finish.
pub fn send_stdin(mut child: Child, stdin: &str) -> Result<Done> {
    use std::io::Write;
    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(stdin.as_bytes())?;
    }
    Ok(Done::from(child.wait_with_output()?))
}

/// Polls until the child has exited, failing after 30 seconds.
pub fn wait_until_exited(child: &mut Child) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while child.try_wait()?.is_none() {
        ensure!(std::time::Instant::now() < deadline, "the child did not exit");
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
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

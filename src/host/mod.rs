//! The sans-IO host: what goes into the step machine and what comes out of it. Adapters do only
//! I/O: they turn what happens (a socket opens, a commit finishes, a call is answered) into an
//! `Input` and carry out each `Output`. `Host::step` is the step machine; it moves the call path
//! of `src/runtime.rs` (see `docs/reference/runtime-rules.md`). Frozen design:
//! `docs/proposals/real-use.md`, Architecture.
//!
//! The input gate. While a storage operation is outstanding (a `Load` or a `Commit`) no other
//! input runs: it is queued and applied once the operation is answered, as the Durable Object's
//! input gate does (R42). A `Call` is not a storage operation: inputs run while one is
//! outstanding, as they do while the steward is being called.

mod batch;
pub mod call;
mod core_slot;
pub mod testkit;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, VecDeque};

#[cfg(test)]
use crate::coordinator::{Coordinator as Core, Effect};
use crate::protocol::{AgentId, ClientMsg, Event, ServerMsg};
use crate::shell::{self, Action, Session, Target, Work};
use batch::{Committed, Held, Pending};
use call::{Call, CallAnswer, CallId, Request};
use core_slot::{Applied, ApplyError, CoreSlot, Prepared, Waits};

/// A client connection, named by the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SocketId(pub u64);

/// One commit of the host's batch of entries, named by the host so `Committed` and `CommitFailed`
/// say which commit finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BatchId(pub u64);

/// Something that happened, with the time it was observed. The host never reads a clock.
#[derive(Debug, Clone)]
pub enum Input {
    /// A socket opened. `verified` is the agent the adapter authenticated: a request without one
    /// is refused before the host sees it (R4), so there is always one.
    Opened {
        now_ms: u64,
        sock: SocketId,
        verified: AgentId,
    },
    /// A text frame arrived on `sock`. Free text: untrusted.
    Frame {
        now_ms: u64,
        sock: SocketId,
        text: String,
    },
    /// `sock` closed, cleanly or not.
    Closed { now_ms: u64, sock: SocketId },
    /// The wake set by `Output::SetWake` is due.
    Wake { now_ms: u64 },
    /// Main may have moved; the host reads the head when it can.
    TrunkPoked { now_ms: u64 },
    /// The answer to `Output::Load`: the stored core state, or `None` for a repository with none.
    Loaded { now_ms: u64, stored: Option<String> },
    /// The answer to `Output::ReadEvents`: a page of the stored log. `done` ends the replay.
    ReplayPage {
        now_ms: u64,
        sock: SocketId,
        events: Vec<Event>,
        done: bool,
    },
    /// The batch is stored.
    Committed { now_ms: u64, batch: BatchId },
    /// The batch is not stored; nothing in it may be sent.
    CommitFailed { now_ms: u64, batch: BatchId },
    /// A call was answered, `Unknown` included.
    Replied {
        now_ms: u64,
        call: CallId,
        answer: CallAnswer,
    },
    /// The adapter lost the call before an answer: counts as `CallAnswer::Unknown`.
    CallLost { now_ms: u64, call: CallId },
}

/// Something the adapter must do.
#[derive(Debug, Clone)]
pub enum Output {
    /// Read the stored core state; the answer is `Input::Loaded`.
    Load,
    /// Store `entries` (key and JSON) in one transaction, then report `Committed` or
    /// `CommitFailed` for `batch`.
    Commit {
        batch: BatchId,
        entries: Vec<(String, String)>,
    },
    /// Held until the batch it follows has committed (protocol invariant 4).
    Send { sock: SocketId, msg: ServerMsg },
    /// Record the session of `sock` (its verified and bound agent, watch position, closed flag).
    SetSession { sock: SocketId, session: Session },
    /// Close `sock` with a WebSocket close code and a reason.
    Close {
        sock: SocketId,
        code: u16,
        reason: String,
    },
    /// Read up to `limit` stored events from `from_seq` for `sock`'s replay; the answer is
    /// `Input::ReplayPage`.
    ReadEvents {
        sock: SocketId,
        from_seq: u64,
        limit: usize,
    },
    /// The next wake, or `None` to clear it.
    SetWake { at_ms: Option<u64> },
    /// Emitted only after the batch that persisted the call has committed.
    Call(Call),
    /// A log line. It names the repo and the operation and never carries a token.
    Log { line: String },
    /// A counter or gauge sample.
    Metric { name: String, value: u64 },
}

/// The repo the host serves and the deployment settings it reads when it builds the core (the
/// Worker's variables today). `run` and `shadow_enabled` matter only for a repo with nothing
/// stored; `reviewers` and `sensitive` are applied on every load.
#[derive(Debug, Clone, Default)]
pub struct HostConfig {
    /// Named in log lines.
    pub repo: String,
    pub run: Option<String>,
    pub shadow_enabled: Option<String>,
    pub reviewers: Option<String>,
    pub sensitive: Option<String>,
}

/// The write that stores the marker of a call before it goes out.
#[cfg(test)]
type MarkerStep = Box<dyn FnOnce(&mut Core) -> Vec<Effect>>;

/// A client message to handle: the expiry step that precedes it runs as its own write first.
struct CallJob {
    now_ms: u64,
    sock: SocketId,
    agent: AgentId,
    msg: ClientMsg,
    session: Session,
}

/// The answer of a call, to be stored once the core is loaded. What an answer does to the core
/// (R21 to R27) moves with the wake (P1-MOVE-WAKE), which will match on the `Request` of the
/// `Call` the table holds and read the `DispatchId` from it. Until then the answer applies
/// nothing: the call leaves the table, the wake is recomputed without it and that is stored.
struct AnswerJob {
    now_ms: u64,
}

/// Starts a call as the wake will: the marker is stored, then the call goes out.
#[cfg(test)]
struct StartJob {
    now_ms: u64,
    call: Call,
    marker: MarkerStep,
}

/// Work that needs the core loaded.
enum Job {
    Call(Box<CallJob>),
    Answer(AnswerJob),
    #[cfg(test)]
    Start(StartJob),
}

/// The input gate: a storage operation outstanding.
enum Gate {
    Open,
    /// `Output::Load` is out; the job waits for the core.
    Loading(Job),
    /// `Output::Commit` is out; its sends wait for `Committed`.
    Committing(Box<Pending>),
}

/// The step machine of the call path. One per repo; it never reads a clock or does I/O.
pub struct Host {
    config: HostConfig,
    slot: CoreSlot,
    sessions: BTreeMap<SocketId, Session>,
    gate: Gate,
    queue: VecDeque<Input>,
    calls: HashMap<CallId, Call>,
    next_batch: u64,
}

/// Whether `input` answers the storage operation that closes the gate, so it is never queued.
fn answers_storage(input: &Input) -> bool {
    match input {
        Input::Loaded { .. } | Input::Committed { .. } | Input::CommitFailed { .. } => true,
        Input::Opened { .. }
        | Input::Frame { .. }
        | Input::Closed { .. }
        | Input::Wake { .. }
        | Input::TrunkPoked { .. }
        | Input::ReplayPage { .. }
        | Input::Replied { .. }
        | Input::CallLost { .. } => false,
    }
}

fn held_of(applied: Applied) -> (Vec<(String, String)>, Held) {
    let held = Held {
        outbound: applied.outbound,
        events: applied.events,
        reply_to: None,
        bind: None,
        next_wake_ms: applied.next_wake_ms,
        calls: Vec::new(),
    };
    (applied.entries, held)
}

impl Host {
    pub fn new(config: HostConfig) -> Self {
        Self {
            config,
            slot: CoreSlot::default(),
            sessions: BTreeMap::new(),
            gate: Gate::Open,
            queue: VecDeque::new(),
            calls: HashMap::new(),
            next_batch: 0,
        }
    }

    /// Take one input and say what the adapter must do. An input that arrives while a storage
    /// operation is outstanding is queued and runs after its answer.
    pub fn step(&mut self, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        if true {
            self.handle(input, &mut out);
        } else {
            self.queue.push_front(input);
        }
        self.drain(&mut out);
        out
    }

    fn gate_is_open(&self) -> bool {
        match self.gate {
            Gate::Open => true,
            Gate::Loading(_) | Gate::Committing(_) => false,
        }
    }

    fn drain(&mut self, out: &mut Vec<Output>) {
        while self.gate_is_open() {
            let Some(input) = self.queue.pop_front() else {
                return;
            };
            self.handle(input, out);
        }
    }

    fn handle(&mut self, input: Input, out: &mut Vec<Output>) {
        match input {
            Input::Opened { sock, verified, .. } => {
                let session = Session {
                    verified: Some(verified),
                    ..Session::default()
                };
                self.sessions.insert(sock, session);
            }
            Input::Frame { now_ms, sock, text } => self.frame(now_ms, sock, &text, out),
            Input::Loaded { stored, .. } => self.loaded(stored.as_deref(), out),
            Input::Committed { now_ms, batch } => self.committed(now_ms, batch, out),
            Input::CommitFailed { batch, .. } => self.commit_failed(batch, out),
            Input::Replied { now_ms, call, .. } | Input::CallLost { now_ms, call } => {
                self.answered(now_ms, call, out);
            }
            Input::Closed { .. } => self.unmoved("a socket closing", out),
            Input::Wake { .. } => self.unmoved("a wake", out),
            Input::TrunkPoked { .. } => self.unmoved("a trunk poke", out),
            Input::ReplayPage { .. } => self.unmoved("a replay page", out),
        }
    }

    fn unmoved(&self, what: &str, out: &mut Vec<Output>) {
        out.push(Output::Log {
            line: format!(
                "coordinator {}: {what} is not served by this host yet",
                self.config.repo
            ),
        });
    }

    fn failure(&self, operation: &str, cause: &str) -> Output {
        Output::Log {
            line: format!(
                "coordinator {}: {operation} failed: {cause}",
                self.config.repo
            ),
        }
    }

    /// What this instance is waiting on, from the calls it has out (R16).
    fn waits(&self) -> Waits {
        let mut waits = Waits {
            merging: false,
            verifying: false,
        };
        for call in self.calls.values() {
            match call.request {
                Request::Prepare { .. } | Request::Test { .. } | Request::Land { .. } => {
                    waits.merging = false;
                }
                Request::Trial { .. } => waits.verifying = true,
                Request::ReadHead { .. } | Request::Reconcile { .. } => {}
            }
        }
        waits
    }

    fn frame(&mut self, now_ms: u64, sock: SocketId, text: &str, out: &mut Vec<Output>) {
        let Ok(msg) = shell::parse_client_msg(text) else {
            self.unmoved("a malformed frame", out);
            return;
        };
        let Some(session) = self.sessions.get(&sock).cloned() else {
            out.push(self.failure("handle frame", "the socket is not open"));
            return;
        };
        match shell::decide(&session, &msg) {
            Action::Call { agent } => {
                let job = CallJob {
                    now_ms,
                    sock,
                    agent,
                    msg,
                    session,
                };
                self.run(Job::Call(Box::new(job)), out);
            }
            Action::Reject(_) | Action::RejectAndClose(_) | Action::Watch { .. } => {
                self.unmoved("a frame the host refuses or replays", out);
            }
        }
    }

    /// Run `job` once the core is loaded: ask for the load first if it is not (R19).
    fn run(&mut self, job: Job, out: &mut Vec<Output>) {
        if !self.slot.is_loaded() {
            out.push(Output::Load);
            self.gate = Gate::Loading(job);
            return;
        }
        match job {
            Job::Call(call) => self.run_call(*call, out),
            Job::Answer(answer) => self.run_answer(answer, out),
            #[cfg(test)]
            Job::Start(start) => self.run_start(start, out),
        }
    }

    fn loaded(&mut self, stored: Option<&str>, out: &mut Vec<Output>) {
        let mut reports = Vec::new();
        let built = core_slot::build_core(&self.config, stored, &mut reports);
        for line in reports {
            out.push(Output::Log { line });
        }
        let core = match built {
            Ok(core) => core,
            Err(e) => {
                match self.gate {
                    Gate::Loading(_) => self.gate = Gate::Open,
                    Gate::Open | Gate::Committing(_) => {}
                }
                out.push(self.failure("load state", &e.to_string()));
                return;
            }
        };
        self.slot.install(core, stored);
        match std::mem::replace(&mut self.gate, Gate::Open) {
            Gate::Loading(job) => self.run(job, out),
            Gate::Committing(pending) => self.gate = Gate::Committing(pending),
            Gate::Open => {}
        }
    }

    fn run_call(&mut self, job: CallJob, out: &mut Vec<Output>) {
        let waits = self.waits();
        if false && self.slot.has_due_expiry(job.now_ms) {
            let now_ms = job.now_ms;
            let prepared = self
                .slot
                .apply(Work::Plain, waits, now_ms, |core| core.expire(now_ms));
            let then = Job::Call(Box::new(job));
            self.commit_unsent("expire leases", prepared, Vec::new(), Some(then), out);
            return;
        }
        let CallJob {
            now_ms,
            sock,
            agent,
            msg,
            session,
        } = job;
        let req = shell::req_of(&msg);
        let work = shell::work_of(&msg);
        let prepared = self
            .slot
            .apply(work, waits, now_ms, |core| core.handle(&agent, msg, now_ms));
        match prepared {
            Ok(Prepared::Ready(applied)) => {
                let bound = Some(Session { agent: Some(agent.clone()), ..session.clone() });
                let (entries, mut held) = held_of(applied);
                held.reply_to = Some(sock);
                held.bind = bound.map(|bound| (sock, bound));
                self.start_commit(entries, held, None, out);
            }
            Ok(Prepared::Refused) => {
                out.push(Output::Log {
                    line: format!(
                        "coordinator {}: {}",
                        self.config.repo,
                        shell::STATE_LIMIT_MESSAGE
                    ),
                });
                let msg = shell::state_limit_reply(req);
                out.push(Output::Send { sock, msg });
            }
            Err(e) => out.push(self.failure(e.operation, &e.cause)),
        }
    }

    fn answered(&mut self, now_ms: u64, call: CallId, out: &mut Vec<Output>) {
        if self.calls.remove(&call).is_none() {
            out.push(self.failure("apply call answer", "the host waits on no such call"));
            return;
        }
        self.run_answer(AnswerJob { now_ms }, out);
    }

    fn run_answer(&mut self, job: AnswerJob, out: &mut Vec<Output>) {
        let waits = self.waits();
        let prepared = self
            .slot
            .apply(Work::Plain, waits, job.now_ms, |_| Vec::new());
        self.commit_unsent("apply call answer", prepared, Vec::new(), None, out);
    }

    #[cfg(test)]
    fn run_start(&mut self, job: StartJob, out: &mut Vec<Output>) {
        let StartJob {
            now_ms,
            call,
            marker,
        } = job;
        let waits = self.waits();
        let prepared = self.slot.apply(Work::Plain, waits, now_ms, marker);
        self.commit_unsent("start call", prepared, vec![call], None, out);
    }

    /// Start a call as the wake will: store `marker`, then hand out `call` once it is stored.
    #[cfg(test)]
    fn begin_call(&mut self, job: StartJob) -> Vec<Output> {
        assert!(self.gate_is_open(), "begin_call runs only on an open gate");
        let mut out = Vec::new();
        self.run(Job::Start(job), &mut out);
        out
    }

    /// Commit the result of a step that has no sender to refuse (an expiry, an answer, a
    /// start). `Work::Plain` is never refused for size, so the `Refused` branch is the
    /// equivalent of R43 and cannot be reached today; if it is, it is an error to log.
    fn commit_unsent(
        &mut self,
        operation: &str,
        prepared: Result<Prepared, ApplyError>,
        calls: Vec<Call>,
        then: Option<Job>,
        out: &mut Vec<Output>,
    ) {
        match prepared {
            Ok(Prepared::Ready(applied)) => {
                let (entries, mut held) = held_of(applied);
                held.calls = calls;
                self.start_commit(entries, held, then, out);
            }
            Ok(Prepared::Refused) => {
                out.push(self.failure(operation, shell::STATE_LIMIT_MESSAGE));
            }
            Err(e) => out.push(self.failure(e.operation, &e.cause)),
        }
    }

    /// Hand the adapter one commit and close the gate until it answers.
    fn start_commit(
        &mut self,
        entries: Vec<(String, String)>,
        held: Held,
        then: Option<Job>,
        out: &mut Vec<Output>,
    ) {
        let batch = BatchId(self.next_batch);
        self.next_batch += 1;
        let pending = Pending::new(batch, entries, held, then);
        out.push(Output::Commit {
            batch,
            entries: pending.entries().to_vec(),
        });
        self.gate = Gate::Committing(Box::new(pending));
    }

    /// The outstanding batch if it is `batch`; the gate opens.
    fn take_pending(&mut self, batch: BatchId) -> Option<Pending> {
        match std::mem::replace(&mut self.gate, Gate::Open) {
            Gate::Committing(pending) if pending.id() == batch => Some(*pending),
            other @ (Gate::Open | Gate::Loading(_) | Gate::Committing(_)) => {
                self.gate = other;
                None
            }
        }
    }

    /// The stored batch, for the one `Input::Committed` that names it (C1).
    fn take_committed(&mut self, batch: BatchId) -> Option<Committed> {
        self.take_pending(batch).map(Pending::commit)
    }

    /// Drop the outstanding batch if it is `batch`. The failure path never holds a `Pending`, so
    /// it cannot build a `Committed`.
    fn discard_pending(&mut self, batch: BatchId) -> bool {
        self.take_pending(batch).is_some()
    }

    fn committed(&mut self, now_ms: u64, batch: BatchId, out: &mut Vec<Output>) {
        let Some(mut committed) = self.take_committed(batch) else {
            out.push(self.failure("commit", "the answer names no outstanding batch"));
            return;
        };
        let then = committed.take_then();
        self.release(now_ms, committed, out);
        if let Some(job) = then {
            self.run(job, out);
        }
    }

    /// The batch is not stored: nothing it held is sent, and the core is ahead of storage, so it
    /// is dropped (R30).
    fn commit_failed(&mut self, batch: BatchId, out: &mut Vec<Output>) {
        if !self.discard_pending(batch) {
            out.push(self.failure("commit", "the failure names no outstanding batch"));
            return;
        }
        out.push(self.failure("persist state and events", "the commit was not stored"));
    }

    /// Bind the socket if the core welcomed it, deliver, reschedule even if delivery failed,
    /// then hand out the calls that were waiting for this batch (R29, R31, R32).
    fn release(&mut self, now_ms: u64, committed: Committed, out: &mut Vec<Output>) {
        let (entries, held) = committed.into_parts();
        let _ = &entries;
        if let Some((sock, session)) = held.bind {
            self.sessions.insert(sock, session.clone());
            out.push(Output::SetSession { sock, session });
        }
        let mut socks = Vec::with_capacity(self.sessions.len());
        let mut sessions = Vec::with_capacity(self.sessions.len());
        for (sock, session) in &self.sessions {
            socks.push(*sock);
            sessions.push(session.clone());
        }
        for (target, msg) in shell::plan_delivery(&held.outbound, &held.events, &sessions) {
            let sock = match target {
                Target::Sender => socks.first().copied(),
                Target::Socket(index) => socks.get(index).copied(),
            };
            match sock {
                Some(sock) => out.push(Output::Send { sock, msg }),
                None => {
                    out.push(self.failure("deliver", "a message has no open socket to go to"));
                    return;
                }
            }
        }
        let at_ms = shell::alarm_at_ms(held.next_wake_ms, now_ms).map(|at| at as u64);
        out.push(Output::SetWake { at_ms });
        for call in held.calls {
            self.calls.insert(call.id, call.clone());
            out.push(Output::Call(call));
        }
    }
}

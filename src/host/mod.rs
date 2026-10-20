//! The sans-IO host: what goes into the step machine and what comes out of it. Adapters do only
//! I/O: they turn what happens (a socket opens, a commit finishes, a call is answered) into an
//! `Input` and carry out each `Output`. `Host::step` is the step machine; it moves the call path
//! and the socket table of `src/runtime.rs` (see `docs/reference/runtime-rules.md`). Frozen design:
//! `docs/proposals/real-use.md`, Architecture.
//!
//! The input gate. While a storage operation is outstanding (a `Load`, a `Commit` or a replay's
//! `ReadEvents`) no other input runs: it is queued and applied once the operation is answered, as
//! the Durable Object's input gate does (R42). A `Call` is not a storage operation: inputs run
//! while one is outstanding, as they do while the steward is being called.
//!
//! The replay. A `Watch` reads the stored log a page at a time and holds the gate for all of it,
//! so the repo stalls while a replay runs. See `replay.rs`.
//!
//! The wake. A wake issues its call and suspends; while a call is outstanding another `Wake`
//! only sets the wake again. See `wake.rs`.

mod batch;
pub mod call;
mod calls;
mod core_slot;
mod replay;
mod sockets;
pub mod testkit;
mod wake;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};

use crate::coordinator::HeadRead;
use crate::protocol::{AgentId, ClientMsg, Event, ServerMsg};
use crate::shell::{self, Action, Session, Target, Work};
use batch::{Committed, Discarded, Held, Pending};
use call::{Call, CallAnswer, CallId, Request};
use calls::AnswerJob;
use core_slot::{Applied, ApplyError, CoreSlot, Prepared, Waits};
use replay::Replay;
use sockets::{Sockets, WithdrawJob, CLOSE_POLICY_VIOLATION, IDENTITY_MISMATCH_REASON};
use wake::WakeStep;

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
    /// A binary frame arrived on `sock`.
    Binary { now_ms: u64, sock: SocketId },
    /// `sock` closed, cleanly or not.
    Closed { now_ms: u64, sock: SocketId },
    /// The adapter could not send a message to `sock`.
    SendFailed { now_ms: u64, sock: SocketId },
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

impl Input {
    /// When the input was observed.
    fn now_ms(&self) -> u64 {
        match self {
            Input::Opened { now_ms, .. }
            | Input::Frame { now_ms, .. }
            | Input::Binary { now_ms, .. }
            | Input::Closed { now_ms, .. }
            | Input::SendFailed { now_ms, .. }
            | Input::Wake { now_ms }
            | Input::TrunkPoked { now_ms }
            | Input::Loaded { now_ms, .. }
            | Input::ReplayPage { now_ms, .. }
            | Input::Committed { now_ms, .. }
            | Input::CommitFailed { now_ms, .. }
            | Input::Replied { now_ms, .. }
            | Input::CallLost { now_ms, .. } => *now_ms,
        }
    }
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
    /// `Input::ReplayPage`. No input reports a failed read, so the adapter must always answer:
    /// with `done: true` (and no events) on a failed read or a closed socket. Without an answer
    /// the gate never reopens, as with `Load` and `Commit`.
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

/// A client message to handle: the expiry step that precedes it runs as its own write first.
struct CallJob {
    now_ms: u64,
    sock: SocketId,
    agent: AgentId,
    msg: ClientMsg,
    session: Session,
}

/// Work that needs the core loaded.
enum Job {
    Call(Box<CallJob>),
    Answer(AnswerJob),
    Withdraw(WithdrawJob),
    Wake(WakeStep),
    Poke,
}

impl Job {
    /// The socket whose message the job handles, and the time it was observed.
    fn origin(&self) -> Option<(SocketId, u64)> {
        match self {
            Job::Call(call) => Some((call.sock, call.now_ms)),
            Job::Answer(_) | Job::Withdraw(_) | Job::Wake(_) | Job::Poke => None,
        }
    }

    /// Whether the job is a step of a wake, which a failure ends (R41).
    fn is_wake(&self) -> bool {
        match self {
            Job::Answer(_) | Job::Wake(_) => true,
            Job::Call(_) | Job::Withdraw(_) | Job::Poke => false,
        }
    }
}

/// The input gate: a storage operation outstanding.
enum Gate {
    Open,
    /// `Output::Load` is out; the job waits for the core.
    Loading(Job),
    /// `Output::Commit` is out; its sends wait for `Committed`.
    Committing(Box<Pending>),
    /// `Output::ReadEvents` is out; the watch waits for its page.
    Replaying(Replay),
}

/// The step machine of the call path. One per repo; it never reads a clock or does I/O.
pub struct Host {
    config: HostConfig,
    slot: CoreSlot,
    sockets: Sockets,
    gate: Gate,
    queue: VecDeque<Input>,
    calls: HashMap<CallId, Call>,
    /// The head read the one outstanding `ReadHead` call was made against.
    reading: Option<HeadRead>,
    /// The socket whose replay a failed send ended while its next read was out: that page is
    /// dropped when it arrives. The adapter answers the reads of a socket in the order it was
    /// asked, so the first page that names the socket is the one the ended replay left out, and
    /// a replay the socket starts afterwards gets its own pages.
    abandoned_read: Option<SocketId>,
    /// A wake is between two of its steps: not suspended at a call and not over.
    waking: bool,
    /// The time of the input being handled.
    now_ms: u64,
    next_batch: u64,
    next_call: u64,
}

/// The wake to ask the adapter for: the core's, never before the next millisecond (R35).
fn wake_at_ms(next_wake_ms: Option<u64>, now_ms: u64) -> Option<u64> {
    shell::alarm_at_ms(next_wake_ms, now_ms).map(|at| at as u64)
}

/// Whether `input` answers the storage operation that closes the gate, so it is never queued.
fn answers_storage(input: &Input) -> bool {
    match input {
        Input::Loaded { .. }
        | Input::ReplayPage { .. }
        | Input::Committed { .. }
        | Input::CommitFailed { .. } => true,
        Input::Opened { .. }
        | Input::Frame { .. }
        | Input::Binary { .. }
        | Input::Closed { .. }
        | Input::SendFailed { .. }
        | Input::Wake { .. }
        | Input::TrunkPoked { .. }
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
            sockets: Sockets::default(),
            gate: Gate::Open,
            queue: VecDeque::new(),
            calls: HashMap::new(),
            reading: None,
            abandoned_read: None,
            waking: false,
            now_ms: 0,
            next_batch: 0,
            next_call: 1,
        }
    }

    /// Take one input and say what the adapter must do. An input that arrives while a storage
    /// operation is outstanding is queued and runs after its answer.
    pub fn step(&mut self, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        if answers_storage(&input) || self.gate_is_open() || self.fails_replay(&input) {
            self.handle(input, &mut out);
        } else {
            self.queue.push_back(input);
        }
        self.drain(&mut out);
        out
    }

    fn gate_is_open(&self) -> bool {
        match self.gate {
            Gate::Open => true,
            Gate::Loading(_) | Gate::Committing(_) | Gate::Replaying(_) => false,
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
        self.now_ms = input.now_ms();
        match input {
            Input::Opened { sock, verified, .. } => out.push(self.sockets.open(sock, verified)),
            Input::Frame { now_ms, sock, text } => self.frame(now_ms, sock, &text, out),
            Input::Binary { sock, .. } => out.push(Output::Send {
                sock,
                msg: shell::binary_rejection(),
            }),
            Input::Loaded { stored, .. } => self.loaded(stored.as_deref(), out),
            Input::Committed { now_ms, batch } => self.committed(now_ms, batch, out),
            Input::CommitFailed { now_ms, batch } => self.commit_failed(now_ms, batch, out),
            Input::Replied { call, answer, .. } => self.answered(call, answer, out),
            Input::CallLost { call, .. } => self.answered(call, CallAnswer::Unknown, out),
            Input::Closed { now_ms, sock } => self.closed(now_ms, sock, out),
            Input::SendFailed { now_ms, sock } => self.send_failed(now_ms, sock, out),
            Input::Wake { .. } => self.woken(out),
            Input::TrunkPoked { .. } => self.run(Job::Poke, out),
            Input::ReplayPage {
                sock, events, done, ..
            } => self.replayed(sock, events, done, out),
        }
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
                    waits.merging = true;
                }
                Request::Trial { .. } => waits.verifying = true,
                Request::ReadHead { .. } | Request::Reconcile { .. } => {}
            }
        }
        waits
    }

    fn frame(&mut self, now_ms: u64, sock: SocketId, text: &str, out: &mut Vec<Output>) {
        let msg = match shell::parse_client_msg(text) {
            Ok(msg) => msg,
            Err(fault) => {
                let msg = fault.reply();
                out.push(Output::Send { sock, msg });
                return;
            }
        };
        let Some(session) = self.sockets.session(sock).cloned() else {
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
            Action::Reject(msg) => out.push(Output::Send { sock, msg }),
            Action::RejectAndClose(msg) => {
                out.push(Output::Send { sock, msg });
                let code = CLOSE_POLICY_VIOLATION;
                self.close_and_withdraw(now_ms, sock, code, IDENTITY_MISMATCH_REASON, out);
            }
            Action::Watch { from_seq } => self.watch(sock, session, from_seq, out),
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
            Job::Withdraw(withdraw) => self.run_withdraw(withdraw, out),
            Job::Wake(step) => self.run_wake(step, out),
            Job::Poke => self.run_poke(out),
        }
    }

    fn loaded(&mut self, stored: Option<&str>, out: &mut Vec<Output>) {
        if self.is_poke_awaiting_load() && Self::is_unknown_repo(stored) {
            self.gate = Gate::Open;
            return;
        }
        let mut reports = Vec::new();
        let built = core_slot::build_core(&self.config, stored, &mut reports);
        for line in reports {
            out.push(Output::Log { line });
        }
        let core = match built {
            Ok(core) => core,
            Err(e) => {
                let abandoned = match std::mem::replace(&mut self.gate, Gate::Open) {
                    Gate::Loading(job) => Some(job),
                    other @ (Gate::Open | Gate::Committing(_) | Gate::Replaying(_)) => {
                        self.gate = other;
                        None
                    }
                };
                out.push(self.failure("load state", &e.to_string()));
                let origin = abandoned.as_ref().and_then(Job::origin);
                self.close_for_error(origin, out);
                if abandoned.as_ref().is_some_and(Job::is_wake) {
                    self.wake_failed(out);
                }
                return;
            }
        };
        self.slot.install(core, stored);
        match std::mem::replace(&mut self.gate, Gate::Open) {
            Gate::Loading(job) => self.run(job, out),
            other @ (Gate::Committing(_) | Gate::Replaying(_)) => self.gate = other,
            Gate::Open => {}
        }
    }

    fn run_call(&mut self, job: CallJob, out: &mut Vec<Output>) {
        let waits = self.waits();
        if self.slot.has_due_expiry(job.now_ms) {
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
                let bound = shell::bind_on_welcome(&session, &agent, &applied.outbound);
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
            Err(e) => {
                out.push(self.failure(e.operation, &e.cause));
                self.close_for_error(Some((sock, now_ms)), out);
            }
        }
    }

    /// Commit the result of a step that has no sender to refuse (a wake step, an answer, an
    /// expiry, a withdrawal, a poke). Every such step is `Work::Plain`, which is never refused
    /// for size (R43 is dropped), so the `Refused` branch is not reached; it is an error to log
    /// if it ever is. A step that fails abandons the client call that waited for it, which
    /// closes its socket (R10), and ends the wake it belongs to (R41).
    fn commit_unsent(
        &mut self,
        operation: &str,
        prepared: Result<Prepared, ApplyError>,
        calls: Vec<Call>,
        then: Option<Job>,
        out: &mut Vec<Output>,
    ) {
        let failure = match prepared {
            Ok(Prepared::Ready(applied)) => {
                let (entries, mut held) = held_of(applied);
                held.calls = calls;
                self.start_commit(entries, held, then, out);
                return;
            }
            Ok(Prepared::Refused) => self.failure(operation, shell::STATE_LIMIT_MESSAGE),
            Err(e) => self.failure(e.operation, &e.cause),
        };
        out.push(failure);
        let origin = then.as_ref().and_then(Job::origin);
        self.close_for_error(origin, out);
        if self.waking {
            self.wake_failed(out);
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
            other @ (Gate::Open | Gate::Loading(_) | Gate::Committing(_) | Gate::Replaying(_)) => {
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
    fn discard_pending(&mut self, batch: BatchId) -> Option<Discarded> {
        self.take_pending(batch).map(Pending::discard)
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
    /// is dropped (R30). The client whose message it stored is told by closing its socket (R10).
    fn commit_failed(&mut self, now_ms: u64, batch: BatchId, out: &mut Vec<Output>) {
        let Some(discarded) = self.discard_pending(batch) else {
            out.push(self.failure("commit", "the failure names no outstanding batch"));
            return;
        };
        self.slot.drop_core();
        out.push(self.failure("persist state and events", "the commit was not stored"));
        let waiting = discarded.then.as_ref().and_then(Job::origin);
        let origin = discarded.reply_to.map(|sock| (sock, now_ms)).or(waiting);
        self.close_for_error(origin, out);
        if self.waking {
            self.wake_failed(out);
        }
    }

    /// Bind the socket if the core welcomed it, deliver, reschedule even if delivery failed,
    /// then hand out the calls that were waiting for this batch (R29, R31, R32).
    fn release(&mut self, now_ms: u64, committed: Committed, out: &mut Vec<Output>) {
        let (entries, held) = committed.into_parts();
        self.slot.written(&entries);
        if let Some((sock, session)) = held.bind {
            out.push(self.sockets.bind(sock, session));
        }
        let (socks, sessions) = self.sockets.listing();
        for (target, msg) in shell::plan_delivery(&held.outbound, &held.events, &sessions) {
            let sock = match target {
                Target::Sender => held.reply_to,
                Target::Socket(index) => socks.get(index).copied(),
            };
            match sock {
                Some(sock) => out.push(Output::Send { sock, msg }),
                None => out.push(self.failure("deliver", "a message has no open socket to go to")),
            }
        }
        let at_ms = wake_at_ms(held.next_wake_ms, now_ms);
        out.push(Output::SetWake { at_ms });
        for call in held.calls {
            self.waking = false;
            self.calls.insert(call.id, call.clone());
            out.push(Output::Call(call));
        }
    }

    fn new_call_id(&mut self) -> CallId {
        let id = CallId(self.next_call);
        self.next_call += 1;
        id
    }

    /// Whether the job waiting for the core is a trunk poke, which a repo with no state drops.
    fn is_poke_awaiting_load(&self) -> bool {
        match &self.gate {
            Gate::Loading(Job::Poke) => true,
            Gate::Loading(_) | Gate::Open | Gate::Committing(_) | Gate::Replaying(_) => false,
        }
    }
}

//! The driver: runs a real `Host` the way an adapter would, against the simulated clock and store,
//! and records what every client received (its transcript). Unlike `host::testkit`, a commit is
//! not stored when it is executed: it is staged until its answer is delivered, so a crash can
//! land between the two, and a commit can fail without one. The adapter's answers wait in a queue
//! until `quiet` delivers them, so a fault can strike between any two outputs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::host::call::{Call, CallAnswer, Request, TrialResult};
use crate::host::{BatchId, Host, HostConfig, Input, Output, SocketId};
use crate::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, ErrorCode, EventKind, Fence, ReleaseReason, RequestId,
    ScopeClaim, ServerMsg,
};
use crate::shell::STATE_KEY;
use crate::sim::clock::Clock;
use crate::sim::store::Store;
use crate::sim::variant::Variant;

/// The most answers and wakes one `quiet` delivers: far more than any workload needs.
const QUIET_LIMIT: usize = 10_000;

/// The head every `Hello` names and every `ReadHead` reads back.
pub const BASE: &str = "abc";

/// What the adapter owes the host.
#[derive(Debug)]
enum Answer {
    Input(Input),
    Commit(BatchId),
}

/// A fault armed for the next outputs or answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crash {
    AfterOutput(u64),
    AfterApply,
}

/// One message a client received, in the order the adapter sent it.
#[derive(Debug, Clone)]
pub struct Delivery {
    pub sock: SocketId,
    pub agent: AgentId,
    pub msg: ServerMsg,
}

/// What a request asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// A message with no request id of its own: a hello or a heartbeat.
    Other,
    Claim,
    Amend,
    Release,
    Submit,
    /// A release that presents a fence the client knows is retired: it must be refused.
    StaleRelease,
}

/// A request a client sent.
#[derive(Debug, Clone, Copy)]
pub struct Sent {
    pub sock: SocketId,
    pub req: RequestId,
    pub kind: RequestKind,
    pub claim: Option<ClaimId>,
    /// For a stale release: how many releases by the agent the stored log held for the claim
    /// when it was sent. A successful release sends the requester nothing, so the log is where
    /// an accepted one shows; `quiet` settles it into `accepted` before anything else is sent.
    pub releases_before: usize,
    /// Set when the host has finished with a stale release and the log holds a release the
    /// probe did not have before it.
    pub accepted: bool,
}

/// What one agent knows from what it received.
#[derive(Debug, Default)]
pub struct AgentView {
    pub sock: Option<SocketId>,
    /// Claims granted and not known to have ended, with the fence each was last granted.
    pub held: BTreeMap<u64, Fence>,
    /// Fences the agent knows are retired: replaced by an amend's grant, or by a lapsed lease.
    pub retired: Vec<(ClaimId, Fence)>,
    /// The scopes the agent asked for under each claim it holds.
    pub scopes: BTreeMap<u64, Vec<ScopeClaim>>,
    /// Claims the agent submitted and the host has not sent back to it.
    pub submitted: BTreeSet<u64>,
}

/// A `Watch` a client sent.
#[derive(Debug, Clone, Copy)]
pub struct Watch {
    pub sock: SocketId,
    pub from_seq: u64,
    /// The client's sends were failed from some point on: it is not owed the rest of the log.
    pub dropped: bool,
}

/// A call the host handed the adapter, with the length of the stored log when it did.
#[derive(Debug, Clone)]
pub struct Dispatch {
    pub request: Request,
    pub events_len: usize,
}

pub struct Sim {
    config: HostConfig,
    host: Host,
    variant: Variant,
    clock: Clock,
    store: Store,
    answers: VecDeque<Answer>,
    wake: Option<u64>,
    sock_agent: BTreeMap<SocketId, String>,
    closed: Vec<SocketId>,
    agents: BTreeMap<String, AgentView>,
    next_sock: u64,
    transcript: Vec<Delivery>,
    sent: Vec<Sent>,
    log: Vec<String>,
    crash: Option<Crash>,
    fail_next_commit: bool,
    executed: u64,
    crashes: u64,
    crashes_after_apply: u64,
    faults: Vec<String>,
    /// Indexes into `sent` of stale releases not yet settled.
    unsettled: Vec<usize>,
    watches: Vec<Watch>,
    dispatches: Vec<Dispatch>,
    /// When each agent sent a heartbeat.
    heartbeats: Vec<(String, u64)>,
    /// Sockets whose sends are to fail, with how many more succeed first.
    send_budget: BTreeMap<SocketId, usize>,
    awaiting_review: BTreeSet<u64>,
    open_races: BTreeSet<u64>,
    race_entrants: BTreeMap<u64, Vec<u64>>,
    /// Main as the simulated git host has it.
    main: String,
    /// Rebased heads the lander made, by the fork commit each came from.
    prepared: BTreeMap<String, String>,
    landed_forks: BTreeSet<String>,
    wakes_set: u64,
    /// The scopes each claim or amend request asked for, by request id; `true` for an amend.
    requested: BTreeMap<u64, (bool, Vec<ScopeClaim>)>,
}

impl Sim {
    pub fn new(config: HostConfig, variant: Variant) -> Self {
        Self {
            host: Host::new(config.clone()),
            config,
            variant,
            clock: Clock::default(),
            store: Store::default(),
            answers: VecDeque::new(),
            wake: Some(crate::sim::clock::START_MS),
            sock_agent: BTreeMap::new(),
            closed: Vec::new(),
            agents: BTreeMap::new(),
            next_sock: 0,
            transcript: Vec::new(),
            sent: Vec::new(),
            log: Vec::new(),
            crash: None,
            fail_next_commit: false,
            executed: 0,
            crashes: 0,
            crashes_after_apply: 0,
            faults: Vec::new(),
            unsettled: Vec::new(),
            watches: Vec::new(),
            dispatches: Vec::new(),
            heartbeats: Vec::new(),
            send_budget: BTreeMap::new(),
            awaiting_review: BTreeSet::new(),
            open_races: BTreeSet::new(),
            race_entrants: BTreeMap::new(),
            main: BASE.to_string(),
            prepared: BTreeMap::new(),
            landed_forks: BTreeSet::new(),
            wakes_set: 0,
            requested: BTreeMap::new(),
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Everything clients received, in order, across crashes.
    pub fn transcript(&self) -> &[Delivery] {
        &self.transcript
    }

    pub fn sent(&self) -> &[Sent] {
        &self.sent
    }

    pub fn watches(&self) -> &[Watch] {
        &self.watches
    }

    pub fn dispatches(&self) -> &[Dispatch] {
        &self.dispatches
    }

    /// When agents sent heartbeats: the only inputs that renew a lease.
    pub fn heartbeats(&self) -> &[(String, u64)] {
        &self.heartbeats
    }

    /// Claims some client was told are held for review and has not seen decided.
    pub fn awaiting_review(&self) -> &BTreeSet<u64> {
        &self.awaiting_review
    }

    /// Races some client was told are open and has not seen decided.
    pub fn open_races(&self) -> &BTreeSet<u64> {
        &self.open_races
    }

    /// The claims that entered `race`.
    pub fn entrants(&self, race: u64) -> &[u64] {
        self.race_entrants.get(&race).map_or(&[], Vec::as_slice)
    }

    /// How many events the stored log holds.
    pub fn stored_events(&self) -> usize {
        self.store.events().0.len()
    }

    /// Whether `sock` is still open: neither side has closed it and no crash has.
    pub fn is_open(&self, sock: SocketId) -> bool {
        self.sock_agent.contains_key(&sock) && !self.closed.contains(&sock)
    }

    /// Mistakes of the harness itself, such as acknowledging a commit that is not staged.
    pub fn faults(&self) -> &[String] {
        &self.faults
    }

    pub fn crashes(&self) -> u64 {
        self.crashes
    }

    /// How many of the crashes struck right after a commit was made durable.
    pub fn crashes_after_apply(&self) -> u64 {
        self.crashes_after_apply
    }

    /// Every input and output in order: two runs of one seed must produce the same lines.
    pub fn log(&self) -> &[String] {
        &self.log
    }

    pub fn view(&self, agent: &str) -> Option<&AgentView> {
        self.agents.get(agent)
    }

    /// The wake the platform holds.
    pub fn wake(&self) -> Option<u64> {
        self.wake
    }

    pub fn advance(&mut self, ms: u64) {
        self.clock.advance(ms);
    }

    /// Jump to the held wake, if any.
    pub fn jump_to_wake(&mut self) {
        if let Some(at_ms) = self.wake {
            self.clock.jump_to(at_ms);
        }
    }

    /// Crash after `outputs` more outputs have been carried out, wherever that falls.
    pub fn crash_after_outputs(&mut self, outputs: u64) {
        self.crash = Some(Crash::AfterOutput(self.executed + outputs.max(1)));
    }

    /// Crash right after the next commit is made durable, before the host hears of it.
    pub fn crash_after_apply(&mut self) {
        self.crash = Some(Crash::AfterApply);
    }

    /// The next commit fails; the process lives.
    pub fn fail_next_commit(&mut self) {
        self.fail_next_commit = true;
    }

    /// Clear faults that never struck.
    pub fn disarm(&mut self) {
        self.crash = None;
        self.fail_next_commit = false;
    }

    fn open_socket(&mut self, agent: &str) -> SocketId {
        let sock = SocketId(self.next_sock);
        self.next_sock += 1;
        self.sock_agent.insert(sock, agent.to_string());
        self.agents.entry(agent.to_string()).or_default().sock = Some(sock);
        let now_ms = self.now_ms();
        self.input(Input::Opened {
            now_ms,
            sock,
            verified: AgentId(agent.to_string()),
        });
        sock
    }

    fn hello(&mut self, sock: SocketId, agent: &str) {
        let hello = ClientMsg::Hello {
            agent: AgentId(agent.to_string()),
            base: CommitId(BASE.to_string()),
            protocol: crate::protocol::PROTOCOL_VERSION,
        };
        self.send(sock, &hello, RequestKind::Other, None);
    }

    /// Open a socket for `agent` and say hello. The agent is connected once `quiet` has run.
    pub fn connect(&mut self, agent: &str) -> SocketId {
        let sock = self.open_socket(agent);
        self.hello(sock, agent);
        sock
    }

    /// Open a socket for the watcher `name` and ask for the log from `from_seq`. With
    /// `hello_mid_replay` the hello follows at once, while the replay is still being read.
    pub fn watch(&mut self, name: &str, from_seq: u64, hello_mid_replay: bool) -> SocketId {
        let sock = self.open_socket(name);
        self.send(
            sock,
            &ClientMsg::Watch { from_seq },
            RequestKind::Other,
            None,
        );
        if hello_mid_replay {
            self.hello(sock, name);
        }
        sock
    }

    /// Every later send to `sock` fails, as to a slow or dropped socket.
    pub fn drop_sends(&mut self, sock: SocketId) {
        self.drop_sends_after(sock, 0);
    }

    /// The next `allowed` sends to `sock` succeed, every later one fails: a socket that dies in
    /// the middle of a replay.
    pub fn drop_sends_after(&mut self, sock: SocketId, allowed: usize) {
        self.send_budget.insert(sock, allowed);
        for watch in self.watches.iter_mut().filter(|w| w.sock == sock) {
            watch.dropped = true;
        }
    }

    /// The agent's socket closed.
    pub fn disconnect(&mut self, sock: SocketId) {
        let now_ms = self.now_ms();
        self.input(Input::Closed { now_ms, sock });
        self.close_socket(sock);
    }

    /// `sock` is closed, by the host or the client: nothing more reaches it, and its agent knows.
    fn close_socket(&mut self, sock: SocketId) {
        if !self.closed.contains(&sock) {
            self.closed.push(sock);
        }
        let Some(agent) = self.sock_agent.get(&sock) else {
            return;
        };
        if let Some(view) = self.agents.get_mut(agent) {
            if view.sock == Some(sock) {
                view.sock = None;
            }
        }
    }

    /// Send `msg` from `sock`. A message that carries a request id is recorded under `kind`.
    pub fn send(
        &mut self,
        sock: SocketId,
        msg: &ClientMsg,
        kind: RequestKind,
        claim: Option<ClaimId>,
    ) {
        if !self.is_open(sock) {
            return;
        }
        if let Some(req) = request_of(msg) {
            let releases_before = claim.map_or(0, |claim| self.agent_releases(claim));
            self.sent.push(Sent {
                sock,
                req,
                kind,
                claim,
                releases_before,
                accepted: false,
            });
            if kind == RequestKind::StaleRelease {
                self.unsettled.push(self.sent.len() - 1);
            }
        }
        let text = match serde_json::to_string(msg) {
            Ok(text) => text,
            Err(e) => {
                self.faults
                    .push(format!("a client message did not serialize: {e}"));
                return;
            }
        };
        let now_ms = self.now_ms();
        self.note_sent(sock, msg, now_ms);
        self.input(Input::Frame { now_ms, sock, text });
        if self.variant == Variant::PhantomHeartbeat {
            let phantom = Input::Frame {
                now_ms,
                sock,
                text: "{\"type\":\"heartbeat\"}".to_string(),
            };
            self.input(phantom);
        }
    }

    /// Remember what a client sent that the checkers and the clients' views need.
    fn note_sent(&mut self, sock: SocketId, msg: &ClientMsg, now_ms: u64) {
        let Some(agent) = self.sock_agent.get(&sock).cloned() else {
            return;
        };
        if let ClientMsg::Claim { req, scopes, .. } = msg {
            self.requested.insert(req.0, (false, scopes.clone()));
        } else if let ClientMsg::Amend { req, add, .. } = msg {
            self.requested.insert(req.0, (true, add.clone()));
        }
        if let ClientMsg::Heartbeat = msg {
            self.heartbeats.push((agent, now_ms));
        } else if let ClientMsg::Watch { from_seq } = msg {
            let from_seq = crate::shell::clamp_watch_from(*from_seq);
            self.watches.push(Watch {
                sock,
                from_seq,
                dropped: false,
            });
        } else if let ClientMsg::Submit { claim, .. } = msg {
            if let Some(view) = self.agents.get_mut(&agent) {
                view.submitted.insert(claim.0);
            }
        }
    }

    /// How many releases by an agent the stored log holds for `claim`.
    fn agent_releases(&self, claim: ClaimId) -> usize {
        let (events, _) = self.store.events();
        events
            .iter()
            .filter(|event| is_agent_release(&event.kind, claim))
            .count()
    }

    fn input(&mut self, input: Input) {
        self.log.push(format!("in {input:?}"));
        let outputs = self.host.step(input);
        self.execute(outputs);
    }

    /// Deliver the adapter's answers and fire due wakes until the host waits for nothing. False
    /// when the host kept asking past `QUIET_LIMIT` rounds.
    pub fn quiet(&mut self) -> bool {
        let finished = self.drain();
        for index in std::mem::take(&mut self.unsettled) {
            let probe = self.sent[index];
            let releases = probe.claim.map_or(0, |claim| self.agent_releases(claim));
            self.sent[index].accepted = releases > probe.releases_before;
        }
        finished
    }

    fn drain(&mut self) -> bool {
        for _ in 0..QUIET_LIMIT {
            if self.deliver_next() {
                continue;
            }
            let now_ms = self.now_ms();
            let Some(at_ms) = self.wake.filter(|at_ms| *at_ms <= now_ms) else {
                return true;
            };
            self.log.push(format!("wake {at_ms}"));
            self.wake = None;
            self.input(Input::Wake { now_ms });
        }
        false
    }

    fn deliver_next(&mut self) -> bool {
        let Some(answer) = self.answers.pop_front() else {
            return false;
        };
        let now_ms = self.now_ms();
        match answer {
            Answer::Input(input) => self.input(input),
            Answer::Commit(batch) => {
                let stored = match self.variant {
                    Variant::WriteBehind => self.store.defer(batch),
                    Variant::Correct
                    | Variant::StaleLoad
                    | Variant::DriftingCodec
                    | Variant::LostWake
                    | Variant::SkipsReplayEvent
                    | Variant::LosesScopesOnLoad
                    | Variant::PhantomHeartbeat => self.store.apply(batch),
                };
                if !stored {
                    let fault = format!("batch {} was acknowledged with nothing staged", batch.0);
                    self.faults.push(fault);
                    return true;
                }
                if self.crash == Some(Crash::AfterApply) {
                    self.crashes_after_apply += 1;
                    self.crash_now();
                    return true;
                }
                self.input(Input::Committed { now_ms, batch });
            }
        }
        true
    }

    /// The process dies and restarts: the host forgets everything, staged writes are lost and
    /// every socket closes. The wake lived in the process, so it is gone; a restarted adapter
    /// wakes once at once, as `tessel-server` does (`wake_on_start`: it holds state, or its port
    /// reads main's head, which the simulated lander port does).
    fn crash_now(&mut self) {
        self.log.push("crash".to_string());
        self.crashes += 1;
        self.crash = None;
        self.host = Host::new(self.config.clone());
        self.answers.clear();
        self.store.crash();
        self.wake = Some(self.clock.now_ms());
        for sock in self.sock_agent.keys() {
            if !self.closed.contains(sock) {
                self.closed.push(*sock);
            }
        }
        for view in self.agents.values_mut() {
            view.sock = None;
        }
    }

    fn execute(&mut self, outputs: Vec<Output>) {
        for output in outputs {
            self.log.push(format!("out {output:?}"));
            self.carry_out(output);
            self.executed += 1;
            if self.crash == Some(Crash::AfterOutput(self.executed)) {
                self.crash_now();
                return;
            }
        }
    }

    fn carry_out(&mut self, output: Output) {
        let now_ms = self.now_ms();
        match output {
            Output::Load => {
                let stored = match self.variant {
                    Variant::StaleLoad => self.store.stale_state(),
                    Variant::Correct
                    | Variant::WriteBehind
                    | Variant::DriftingCodec
                    | Variant::LostWake
                    | Variant::SkipsReplayEvent
                    | Variant::LosesScopesOnLoad
                    | Variant::PhantomHeartbeat => self.store.state(),
                };
                let mut stored = stored.map(str::to_string);
                if self.variant == Variant::LosesScopesOnLoad {
                    stored = stored.map(|json| without_scopes(&json));
                }
                self.answers
                    .push_back(Answer::Input(Input::Loaded { now_ms, stored }));
            }
            Output::Commit { batch, entries } => self.commit(batch, entries),
            Output::Send { sock, msg } => self.deliver_to_client(sock, msg),
            Output::Close { sock, .. } => self.close_socket(sock),
            Output::Call(call) => self.answer_call(&call),
            Output::SetWake { at_ms } => self.set_wake(at_ms),
            Output::ReadEvents {
                sock,
                from_seq,
                limit,
            } => {
                let mut events: Vec<_> = self
                    .store
                    .events()
                    .0
                    .iter()
                    .filter(|event| event.seq >= from_seq)
                    .take(limit)
                    .cloned()
                    .collect();
                if self.variant == Variant::SkipsReplayEvent && events.len() >= 3 {
                    events.remove(1);
                }
                let page = Input::ReplayPage {
                    now_ms,
                    sock,
                    events,
                    done: false,
                };
                self.answers.push_back(Answer::Input(page));
            }
            Output::SetSession { .. } | Output::Log { .. } | Output::Metric { .. } => {}
        }
    }

    fn commit(&mut self, batch: BatchId, mut entries: Vec<(String, String)>) {
        let now_ms = self.now_ms();
        if self.fail_next_commit {
            self.fail_next_commit = false;
            let failed = Input::CommitFailed { now_ms, batch };
            self.answers.push_back(Answer::Input(failed));
            return;
        }
        if self.variant == Variant::DriftingCodec {
            for (key, json) in &mut entries {
                if key == STATE_KEY {
                    *json = with_drift(json);
                }
            }
        }
        self.store.stage(batch, entries);
        self.answers.push_back(Answer::Commit(batch));
    }

    fn set_wake(&mut self, at_ms: Option<u64>) {
        self.wakes_set += 1;
        if self.variant == Variant::LostWake && at_ms.is_some() && self.wakes_set.is_multiple_of(3)
        {
            return;
        }
        self.wake = at_ms;
    }

    /// The lander's answer: every call succeeds. Main moves when a head is prepared, as the core
    /// treats that answer as the landing today.
    fn answer_call(&mut self, call: &Call) {
        let now_ms = self.now_ms();
        let events_len = self.stored_events();
        self.dispatches.push(Dispatch {
            request: call.request.clone(),
            events_len,
        });
        let answer = match &call.request {
            Request::ReadHead { submitted } => CallAnswer::Head {
                main: Some(CommitId(self.main.clone())),
                landed: submitted
                    .iter()
                    .filter(|commit| self.landed_forks.contains(&commit.0))
                    .cloned()
                    .collect(),
            },
            Request::Prepare { fork_commit, .. } => {
                let head = format!("{:040x}", (1_u64 << 40) + call.id.0);
                self.prepared.insert(head.clone(), fork_commit.0.clone());
                let base = CommitId(std::mem::replace(&mut self.main, head.clone()));
                self.landed_forks.insert(fork_commit.0.clone());
                CallAnswer::Prepared {
                    base,
                    head: CommitId(head),
                }
            }
            Request::Test { .. } => CallAnswer::Tested,
            Request::Land { head, .. } => {
                self.main = head.0.clone();
                if let Some(fork) = self.prepared.get(&head.0) {
                    self.landed_forks.insert(fork.clone());
                }
                CallAnswer::Landed { head: head.clone() }
            }
            Request::Trial { .. } => CallAnswer::Trialled {
                before: Some(TrialResult::Clean),
                after: Some(TrialResult::Clean),
            },
            Request::Reconcile { head } => CallAnswer::Reconciled {
                main: CommitId(self.main.clone()),
                landed: head.0 == self.main || self.prepared.contains_key(&head.0),
            },
        };
        let replied = Input::Replied {
            now_ms,
            call: call.id,
            answer,
        };
        self.answers.push_back(Answer::Input(replied));
    }

    fn deliver_to_client(&mut self, sock: SocketId, msg: ServerMsg) {
        if self.closed.contains(&sock) {
            return;
        }
        let Some(agent) = self.sock_agent.get(&sock).cloned() else {
            return;
        };
        let budget = self.send_budget.entry(sock).or_insert(usize::MAX);
        if *budget == 0 {
            let now_ms = self.now_ms();
            let failed = Input::SendFailed { now_ms, sock };
            self.answers.push_back(Answer::Input(failed));
            return;
        }
        *budget = budget.saturating_sub(1);
        self.learn(&agent, &msg);
        let agent = AgentId(agent);
        self.transcript.push(Delivery { sock, agent, msg });
    }

    /// Update what `agent` knows from a message it received.
    fn learn(&mut self, agent: &str, msg: &ServerMsg) {
        self.learn_shared(msg);
        let Some(view) = self.agents.get_mut(agent) else {
            return;
        };
        if let ServerMsg::Granted {
            req,
            claim,
            fence,
            race,
            ..
        } = msg
        {
            if let Some(old) = view.held.insert(claim.0, *fence) {
                view.retired.push((*claim, old));
            }
            if let Some((amend, scopes)) = self.requested.get(&req.0) {
                let known = view.scopes.entry(claim.0).or_default();
                if !*amend {
                    known.clear();
                }
                known.extend(scopes.iter().cloned());
            }
            if let Some(race) = race {
                self.race_entrants.entry(race.0).or_default().push(claim.0);
            }
        }
        if let ServerMsg::LeaseExpired { claim, fence } = msg {
            view.held.remove(&claim.0);
            view.retired.push((*claim, *fence));
        }
        if let ServerMsg::Merged { claim, .. } = msg {
            view.held.remove(&claim.0);
            view.submitted.remove(&claim.0);
        }
        if let ServerMsg::SubmitRejected { claim, .. } = msg {
            view.submitted.remove(&claim.0);
        }
        if let ServerMsg::Uncovered { claim, .. } = msg {
            view.submitted.remove(&claim.0);
        }
        if let ServerMsg::Error {
            req: Some(req),
            code,
            ..
        } = msg
        {
            let ended = *code == ErrorCode::UnknownClaim
                || *code == ErrorCode::StaleFence
                || *code == ErrorCode::NotOwner;
            let claim = self
                .sent
                .iter()
                .find(|sent| sent.req == *req)
                .filter(|sent| sent.kind != RequestKind::StaleRelease)
                .and_then(|sent| sent.claim);
            if let (true, Some(claim)) = (ended, claim) {
                view.held.remove(&claim.0);
                view.submitted.remove(&claim.0);
            }
        }
    }

    /// What every client learns from a message, whoever it went to: which claims await a
    /// reviewer and which races are open. The harness stands in for the dashboard they read.
    fn learn_shared(&mut self, msg: &ServerMsg) {
        if let ServerMsg::ReviewRequired { claim, .. } = msg {
            self.awaiting_review.insert(claim.0);
        }
        if let ServerMsg::Merged { claim, .. } = msg {
            self.awaiting_review.remove(&claim.0);
        }
        if let ServerMsg::SubmitRejected { claim, .. } = msg {
            self.awaiting_review.remove(&claim.0);
        }
        if let ServerMsg::RaceOpened { race, .. } = msg {
            self.open_races.insert(race.0);
        }
        if let ServerMsg::RaceResult { race, .. } = msg {
            self.open_races.remove(&race.0);
        }
    }
}

/// The request id a client message carries, if it carries one.
fn request_of(msg: &ClientMsg) -> Option<RequestId> {
    match msg {
        ClientMsg::Claim { req, .. }
        | ClientMsg::Amend { req, .. }
        | ClientMsg::Submit { req, .. }
        | ClientMsg::OpenRace { req, .. }
        | ClientMsg::JoinRace { req, .. }
        | ClientMsg::PickWinner { req, .. }
        | ClientMsg::Review { req, .. } => Some(*req),
        ClientMsg::Release { req, .. } => *req,
        ClientMsg::Hello { .. } | ClientMsg::Heartbeat | ClientMsg::Watch { .. } => None,
    }
}

/// `json` with a field the core's codec does not know, as a writer from another build leaves.
fn with_drift(json: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(json) else {
        return json.to_string();
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("writer_note".to_string(), serde_json::Value::from(1));
    }
    serde_json::to_string(&value).unwrap_or_else(|_| json.to_string())
}

/// Whether `kind` is an agent's release of `claim`.
pub fn is_agent_release(kind: &EventKind, claim: ClaimId) -> bool {
    if let EventKind::ClaimReleased {
        claim: released,
        reason,
    } = kind
    {
        return *released == claim && *reason == ReleaseReason::Agent;
    }
    false
}

/// `json`, a stored state, with every claim's scopes forgotten.
fn without_scopes(json: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(json) else {
        return json.to_string();
    };
    if let Some(claims) = value.get_mut("claims").and_then(|c| c.as_object_mut()) {
        for claim in claims.values_mut() {
            if let Some(object) = claim.as_object_mut() {
                object.insert("scopes".to_string(), serde_json::Value::Array(Vec::new()));
            }
        }
    }
    serde_json::to_string(&value).unwrap_or_else(|_| json.to_string())
}

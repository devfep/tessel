//! The driver: runs a real `Host` the way an adapter would, against the simulated clock and store,
//! and records what every client received (its transcript). Unlike `host::testkit`, a commit is
//! not stored when it is executed: it is staged until its answer is delivered, so a crash can
//! land between the two, and a commit can fail without one. The adapter's answers wait in a queue
//! until `quiet` delivers them, so a fault can strike between any two outputs.

use std::collections::{BTreeMap, VecDeque};

use crate::host::call::{Call, CallAnswer, Request};
use crate::host::{BatchId, Host, HostConfig, Input, Output, SocketId};
use crate::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, ErrorCode, EventKind, Fence, ReleaseReason, RequestId,
    ServerMsg,
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
    faults: Vec<String>,
    /// Indexes into `sent` of stale releases not yet settled.
    unsettled: Vec<usize>,
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
            wake: None,
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
            faults: Vec::new(),
            unsettled: Vec::new(),
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

    /// Open a socket for `agent` and say hello. The agent is connected once `quiet` has run.
    pub fn connect(&mut self, agent: &str) -> SocketId {
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
        if !self.is_open(sock) {
            return sock;
        }
        let hello = ClientMsg::Hello {
            agent: AgentId(agent.to_string()),
            base: CommitId(BASE.to_string()),
            protocol: crate::protocol::PROTOCOL_VERSION,
        };
        self.send(sock, &hello, RequestKind::Other, None);
        sock
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
        self.input(Input::Frame { now_ms, sock, text });
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
                    Variant::Correct | Variant::StaleLoad | Variant::DriftingCodec => {
                        self.store.apply(batch)
                    }
                };
                if !stored {
                    let fault = format!("batch {} was acknowledged with nothing staged", batch.0);
                    self.faults.push(fault);
                    return true;
                }
                if self.crash == Some(Crash::AfterApply) {
                    self.crash_now();
                    return true;
                }
                self.input(Input::Committed { now_ms, batch });
            }
        }
        true
    }

    /// The process dies: the host forgets everything, staged writes are lost and every socket
    /// closes. The platform keeps the wake and the durable store.
    fn crash_now(&mut self) {
        self.log.push("crash".to_string());
        self.crashes += 1;
        self.crash = None;
        self.host = Host::new(self.config.clone());
        self.answers.clear();
        self.store.crash();
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
                    Variant::Correct | Variant::WriteBehind | Variant::DriftingCodec => {
                        self.store.state()
                    }
                };
                let stored = stored.map(str::to_string);
                self.answers
                    .push_back(Answer::Input(Input::Loaded { now_ms, stored }));
            }
            Output::Commit { batch, entries } => self.commit(batch, entries),
            Output::Send { sock, msg } => self.deliver_to_client(sock, msg),
            Output::Close { sock, .. } => self.close_socket(sock),
            Output::Call(call) => self.answer_call(&call),
            Output::SetWake { at_ms } => self.wake = at_ms,
            Output::ReadEvents { sock, .. } => {
                let page = Input::ReplayPage {
                    now_ms,
                    sock,
                    events: Vec::new(),
                    done: true,
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

    fn answer_call(&mut self, call: &Call) {
        let now_ms = self.now_ms();
        let answer = match &call.request {
            Request::ReadHead { .. } => CallAnswer::Head {
                main: Some(CommitId(BASE.to_string())),
                landed: Vec::new(),
            },
            Request::Prepare { .. }
            | Request::Test { .. }
            | Request::Land { .. }
            | Request::Trial { .. }
            | Request::Reconcile { .. } => CallAnswer::Unknown,
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
        self.learn(&agent, &msg);
        let agent = AgentId(agent);
        self.transcript.push(Delivery { sock, agent, msg });
    }

    /// Update what `agent` knows from a message it received.
    fn learn(&mut self, agent: &str, msg: &ServerMsg) {
        let Some(view) = self.agents.get_mut(agent) else {
            return;
        };
        if let ServerMsg::Granted { claim, fence, .. } = msg {
            if let Some(old) = view.held.insert(claim.0, *fence) {
                view.retired.push((*claim, old));
            }
        }
        if let ServerMsg::LeaseExpired { claim, fence } = msg {
            view.held.remove(&claim.0);
            view.retired.push((*claim, *fence));
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
            }
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

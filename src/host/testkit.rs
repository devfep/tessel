//! Runs a `Host` against an in-memory store and a simulated clock. It executes each `Output` the
//! way an adapter would and records what every client received, so a test reads the transcript.
//!
//! The adapter's answers (`Loaded`, `Committed`, `CommitFailed`) are not delivered at once: they
//! wait in a queue until `deliver_next` or `settle`, so a test can send an input while a storage
//! operation is outstanding.

use std::collections::{BTreeMap, VecDeque};

use crate::host::call::{Call, CallAnswer, CallId};
use crate::host::{Host, HostConfig, Input, Output, SocketId};
use crate::protocol::{AgentId, ClientMsg, Event, ServerMsg};
use crate::shell::{Session, EVENT_PREFIX, STATE_KEY};

/// The most answers `settle` delivers: far more than any test needs.
const SETTLE_LIMIT: usize = 1_000;

/// What one client has been given.
#[derive(Debug, Default)]
pub struct Client {
    pub received: Vec<ServerMsg>,
    pub session: Option<Session>,
    pub closed: Option<(u16, String)>,
    send_fails: bool,
}

pub struct Testkit {
    pub host: Host,
    config: HostConfig,
    now_ms: u64,
    store: BTreeMap<String, String>,
    answers: VecDeque<Input>,
    clients: BTreeMap<SocketId, Client>,
    next_sock: u64,
    wake: Option<u64>,
    calls: Vec<Call>,
    logs: Vec<String>,
    history: Vec<Output>,
    fail_next_commit: bool,
}

impl Testkit {
    pub fn new(config: HostConfig) -> Self {
        Self {
            host: Host::new(config.clone()),
            config,
            now_ms: 1_000,
            store: BTreeMap::new(),
            answers: VecDeque::new(),
            clients: BTreeMap::new(),
            next_sock: 0,
            wake: None,
            calls: Vec::new(),
            logs: Vec::new(),
            history: Vec::new(),
            fail_next_commit: false,
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    pub fn advance(&mut self, ms: u64) {
        self.now_ms += ms;
    }

    /// Put `json` in the store as the adapter would have found it.
    pub fn seed(&mut self, key: &str, json: String) {
        self.store.insert(key.to_string(), json);
    }

    /// The stored JSON of `key`.
    pub fn stored(&self, key: &str) -> Option<&str> {
        self.store.get(key).map(String::as_str)
    }

    /// The stored core state.
    pub fn stored_state(&self) -> Option<&str> {
        self.stored(STATE_KEY)
    }

    /// The next commit is answered `CommitFailed` and stores nothing.
    pub fn fail_next_commit(&mut self) {
        self.fail_next_commit = true;
    }

    /// Every later send to `sock` fails: the adapter answers `Input::SendFailed` and the client
    /// receives nothing.
    pub fn fail_sends_to(&mut self, sock: SocketId) {
        self.client(sock).send_fails = true;
    }

    /// Open a socket for `agent`, as the adapter does for a request it authenticated.
    pub fn open(&mut self, agent: &str) -> SocketId {
        let sock = SocketId(self.next_sock);
        self.next_sock += 1;
        self.clients.insert(sock, Client::default());
        let verified = AgentId(agent.to_string());
        let now_ms = self.now_ms;
        self.input(Input::Opened {
            now_ms,
            sock,
            verified,
        });
        sock
    }

    pub fn send(&mut self, sock: SocketId, msg: &ClientMsg) {
        let text = serde_json::to_string(msg).expect("a client message serializes");
        self.send_text(sock, text);
    }

    pub fn send_text(&mut self, sock: SocketId, text: String) {
        let now_ms = self.now_ms;
        self.input(Input::Frame { now_ms, sock, text });
    }

    pub fn send_binary(&mut self, sock: SocketId) {
        let now_ms = self.now_ms;
        self.input(Input::Binary { now_ms, sock });
    }

    /// The peer closed `sock`, or it failed.
    pub fn close(&mut self, sock: SocketId) {
        let now_ms = self.now_ms;
        self.input(Input::Closed { now_ms, sock });
    }

    /// The wake is due now.
    pub fn wake_now(&mut self) {
        let now_ms = self.now_ms;
        self.input(Input::Wake { now_ms });
    }

    /// The steward poked the trunk.
    pub fn poke(&mut self) {
        let now_ms = self.now_ms;
        self.input(Input::TrunkPoked { now_ms });
    }

    /// The adapter lost `call` before an answer.
    pub fn lose(&mut self, call: CallId) {
        let now_ms = self.now_ms;
        self.input(Input::CallLost { now_ms, call });
    }

    /// The process restarts over the same store: a new host that remembers nothing, and no
    /// answer the old one waited for. The wake the platform holds stays.
    pub fn restart(&mut self) {
        self.host = Host::new(self.config.clone());
        self.answers.clear();
    }

    /// The call `call` was answered.
    pub fn reply(&mut self, call: CallId, answer: CallAnswer) {
        let now_ms = self.now_ms;
        self.input(Input::Replied {
            now_ms,
            call,
            answer,
        });
    }

    /// Hand the host an input at the current time.
    pub fn input(&mut self, input: Input) {
        let outputs = self.host.step(input);
        self.execute(outputs);
    }

    /// Deliver the oldest answer the adapter owes the host. False when there is none.
    pub fn deliver_next(&mut self) -> bool {
        let Some(answer) = self.answers.pop_front() else {
            return false;
        };
        self.input(answer);
        true
    }

    /// Deliver answers until the host is waiting for none. A host that keeps asking (a load that
    /// never sticks, say) fails the test after `SETTLE_LIMIT` answers instead of hanging it.
    pub fn settle(&mut self) {
        for _ in 0..SETTLE_LIMIT {
            if !self.deliver_next() {
                return;
            }
        }
        panic!("the host still waits for the adapter after {SETTLE_LIMIT} answers");
    }

    /// Carry out `outputs` as an adapter would, recording them.
    pub fn execute(&mut self, outputs: Vec<Output>) {
        let now_ms = self.now_ms;
        for output in outputs {
            self.history.push(output.clone());
            match output {
                Output::Load => {
                    let stored = self.store.get(STATE_KEY).cloned();
                    self.answers.push_back(Input::Loaded { now_ms, stored });
                }
                Output::Commit { batch, entries } => {
                    if self.fail_next_commit {
                        self.fail_next_commit = false;
                        self.answers
                            .push_back(Input::CommitFailed { now_ms, batch });
                        continue;
                    }
                    self.store.extend(entries);
                    self.answers.push_back(Input::Committed { now_ms, batch });
                }
                Output::Send { sock, msg } => {
                    let client = self.client(sock);
                    if client.send_fails {
                        self.answers.push_back(Input::SendFailed { now_ms, sock });
                    } else {
                        client.received.push(msg);
                    }
                }
                Output::SetSession { sock, session } => self.client(sock).session = Some(session),
                Output::Close { sock, code, reason } => {
                    self.client(sock).closed = Some((code, reason));
                }
                Output::SetWake { at_ms } => self.wake = at_ms,
                Output::Call(call) => self.calls.push(call),
                Output::Log { line } => self.logs.push(line),
                Output::ReadEvents {
                    sock,
                    from_seq,
                    limit,
                } => {
                    let events = self.read_events(from_seq, limit);
                    self.answers.push_back(Input::ReplayPage {
                        now_ms,
                        sock,
                        events,
                        done: false,
                    });
                }
                Output::Metric { .. } => {}
            }
        }
    }

    /// Up to `limit` stored events with `seq >= from_seq`, in `seq` order, as the adapter reads
    /// them.
    fn read_events(&self, from_seq: u64, limit: usize) -> Vec<Event> {
        let mut events = Vec::new();
        for (key, json) in &self.store {
            if !key.starts_with(EVENT_PREFIX) {
                continue;
            }
            let event: Event = serde_json::from_str(json).expect("a stored event parses");
            if event.seq >= from_seq {
                events.push(event);
            }
        }
        events.sort_by_key(|event| event.seq);
        events.truncate(limit);
        events
    }

    fn client(&mut self, sock: SocketId) -> &mut Client {
        self.clients.entry(sock).or_default()
    }

    pub fn received(&self, sock: SocketId) -> &[ServerMsg] {
        self.clients
            .get(&sock)
            .map_or(&[], |client| client.received.as_slice())
    }

    /// The close code and reason the host gave `sock`, if it closed it.
    pub fn closed(&self, sock: SocketId) -> Option<&(u16, String)> {
        self.clients.get(&sock)?.closed.as_ref()
    }

    pub fn session(&self, sock: SocketId) -> Option<&Session> {
        self.clients.get(&sock)?.session.as_ref()
    }

    pub fn wake(&self) -> Option<u64> {
        self.wake
    }

    /// The calls handed out so far, in order.
    pub fn calls(&self) -> &[Call] {
        &self.calls
    }

    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    /// Every output the host produced, in order.
    pub fn history(&self) -> &[Output] {
        &self.history
    }
}

//! Runs a `Host` against an in-memory store and a simulated clock. It executes each `Output` the
//! way an adapter would and records what every client received, so a test reads the transcript.
//!
//! The adapter's answers (`Loaded`, `Committed`, `CommitFailed`) are not delivered at once: they
//! wait in a queue until `deliver_next` or `settle`, so a test can send an input while a storage
//! operation is outstanding.

use std::collections::{BTreeMap, VecDeque};

use crate::host::call::{Call, CallAnswer, CallId};
use crate::host::{Host, HostConfig, Input, Output, SocketId};
use crate::protocol::{AgentId, ClientMsg, ServerMsg};
use crate::shell::{Session, STATE_KEY};

/// What one client has been given.
#[derive(Debug, Default)]
pub struct Client {
    pub received: Vec<ServerMsg>,
    pub session: Option<Session>,
    pub closed: Option<(u16, String)>,
}

pub struct Testkit {
    pub host: Host,
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
            host: Host::new(config),
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

    /// Deliver answers until the host is waiting for none.
    pub fn settle(&mut self) {
        while self.deliver_next() {}
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
                Output::Send { sock, msg } => self.client(sock).received.push(msg),
                Output::SetSession { sock, session } => self.client(sock).session = Some(session),
                Output::Close { sock, code, reason } => {
                    self.client(sock).closed = Some((code, reason));
                }
                Output::SetWake { at_ms } => self.wake = at_ms,
                Output::Call(call) => self.calls.push(call),
                Output::Log { line } => self.logs.push(line),
                Output::ReadEvents { .. } | Output::Metric { .. } => {}
            }
        }
    }

    fn client(&mut self, sock: SocketId) -> &mut Client {
        self.clients.entry(sock).or_default()
    }

    pub fn received(&self, sock: SocketId) -> &[ServerMsg] {
        self.clients
            .get(&sock)
            .map_or(&[], |client| client.received.as_slice())
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

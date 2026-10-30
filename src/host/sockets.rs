//! Sessions and the socket table (R6 to R13, and the socket halves of R10, R31 and R32).
//!
//! The table holds the session of every socket the adapter has opened and not yet reported
//! closed. A socket the host closes itself stays in it, marked closed, until the adapter reports
//! `Input::Closed`: the runtime may still list a closing socket as open, so a closed session
//! shields no agent from a withdrawal (R12).

use std::collections::BTreeMap;

use crate::host::{Host, Job, Output, SocketId};
use crate::protocol::AgentId;
use crate::shell::{self, Session, Work};

/// WebSocket close code 1008: a `Hello` for an agent the connection's token was not issued for.
pub const CLOSE_POLICY_VIOLATION: u16 = 1008;
/// WebSocket close code 1013: the host cannot serve the socket now; try again later.
pub const CLOSE_TRY_AGAIN_LATER: u16 = 1013;
/// WebSocket close code 1011: the host failed while serving the socket.
pub const CLOSE_INTERNAL_ERROR: u16 = 1011;

pub const IDENTITY_MISMATCH_REASON: &str = "identity mismatch";
const ERROR_REASON: &str = "coordinator error";
const SEND_FAILED_REASON: &str = "send failed";

#[derive(Default)]
pub struct Sockets {
    table: BTreeMap<SocketId, Session>,
}

impl Sockets {
    /// A socket opened: its session holds the verified agent before its first message (R6).
    pub fn open(&mut self, sock: SocketId, verified: AgentId) -> Output {
        let session = Session {
            verified: Some(verified),
            ..Session::default()
        };
        self.bind(sock, session)
    }

    /// Record `session` as the session of `sock`.
    pub fn bind(&mut self, sock: SocketId, session: Session) -> Output {
        self.table.insert(sock, session.clone());
        Output::SetSession { sock, session }
    }

    pub fn session(&self, sock: SocketId) -> Option<&Session> {
        self.table.get(&sock)
    }

    /// The adapter reports `sock` gone.
    pub fn remove(&mut self, sock: SocketId) -> Option<Session> {
        self.table.remove(&sock)
    }

    /// Mark `sock` closed, then close it (R12). Returns the session as it was, or `None` when
    /// the socket is unknown or already closed, so nothing is closed twice.
    pub fn close(
        &mut self,
        sock: SocketId,
        code: u16,
        reason: &str,
        out: &mut Vec<Output>,
    ) -> Option<Session> {
        let session = self.table.get_mut(&sock)?;
        if session.closed {
            return None;
        }
        let before = session.clone();
        session.closed = true;
        out.push(Output::SetSession {
            sock,
            session: session.clone(),
        });
        out.push(Output::Close {
            sock,
            code,
            reason: reason.to_string(),
        });
        Some(before)
    }

    /// Every socket in the table, with its session at the same index.
    pub fn listing(&self) -> (Vec<SocketId>, Vec<Session>) {
        let mut socks = Vec::with_capacity(self.table.len());
        let mut sessions = Vec::with_capacity(self.table.len());
        for (sock, session) in &self.table {
            socks.push(*sock);
            sessions.push(session.clone());
        }
        (socks, sessions)
    }
}

/// A socket went away: its agent's queued request is withdrawn if nobody else can receive the
/// grant. Runs once the core is loaded.
pub struct WithdrawJob {
    pub now_ms: u64,
    pub closing: Session,
}

impl Host {
    /// The adapter reports `sock` closed, cleanly or not (R11).
    pub(super) fn closed(&mut self, now_ms: u64, sock: SocketId, out: &mut Vec<Output>) {
        let Some(closing) = self.sockets.remove(sock) else {
            out.push(self.failure("handle close", "the socket is not open"));
            return;
        };
        self.abandon_replay(sock);
        self.withdraw(now_ms, closing, out);
    }

    /// The adapter could not send to `sock`: it is closed and its agent withdrawn, and delivery
    /// to the other sockets was never interrupted (R31, R32). A replay of `sock` ends here.
    pub(super) fn send_failed(&mut self, now_ms: u64, sock: SocketId, out: &mut Vec<Output>) {
        let Some(session) = self.sockets.session(sock) else {
            out.push(self.failure("handle send failure", "the socket is not open"));
            return;
        };
        let agent = shell::log_agent(shell::session_agent(session));
        out.push(self.failure("send to a socket", &format!("agent={agent}")));
        self.close_and_withdraw(now_ms, sock, CLOSE_INTERNAL_ERROR, SEND_FAILED_REASON, out);
    }

    /// Close `sock` from here, then withdraw its agent's queued request (R9, R10).
    pub(super) fn close_and_withdraw(
        &mut self,
        now_ms: u64,
        sock: SocketId,
        code: u16,
        reason: &str,
        out: &mut Vec<Output>,
    ) {
        self.abandon_replay(sock);
        let Some(closing) = self.sockets.close(sock, code, reason, out) else {
            return;
        };
        self.withdraw(now_ms, closing, out);
    }

    /// An error handling the message of `origin` (a socket and the time of its message) closes
    /// that socket with 1011 (R10).
    pub(super) fn close_for_error(
        &mut self,
        origin: Option<(SocketId, u64)>,
        out: &mut Vec<Output>,
    ) {
        let Some((sock, now_ms)) = origin else {
            return;
        };
        self.close_and_withdraw(now_ms, sock, CLOSE_INTERNAL_ERROR, ERROR_REASON, out);
    }

    fn withdraw(&mut self, now_ms: u64, closing: Session, out: &mut Vec<Output>) {
        if closing.agent.is_none() {
            return;
        }
        self.run(Job::Withdraw(WithdrawJob { now_ms, closing }), out);
    }

    /// The withdrawal is applied, then persisted, then settled, as any other step (R13). An
    /// error is logged: no handler is left to return it to.
    pub(super) fn run_withdraw(&mut self, job: WithdrawJob, out: &mut Vec<Output>) {
        let WithdrawJob { now_ms, closing } = job;
        let (_, open) = self.sockets.listing();
        let slot = &self.slot;
        let queued = |agent: &AgentId| slot.has_queued_request(agent);
        let Some(agent) = shell::agent_to_withdraw(&closing, &open, queued) else {
            return;
        };
        let waits = self.waits();
        let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
            core.disconnect(&agent, now_ms)
        });
        self.commit_unsent("withdraw queued request", prepared, Vec::new(), None, out);
    }
}

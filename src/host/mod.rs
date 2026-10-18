//! The sans-IO host: what goes into the step machine and what comes out of it. Adapters do only
//! I/O: they turn what happens (a socket opens, a commit finishes, a call is answered) into an
//! `Input` and carry out each `Output`. The types only; the step function is a later task.
//! Frozen design: `docs/proposals/real-use.md`, Architecture.

pub mod call;

use crate::protocol::{AgentId, Event, ServerMsg};
use crate::shell::Session;
use call::{Call, CallAnswer, CallId};

/// A client connection, named by the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SocketId(pub u64);

/// One commit of the host's batch of entries, named by the host so `Committed` and `CommitFailed`
/// say which commit finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BatchId(pub u64);

/// Something that happened, with the time it was observed. The host never reads a clock.
#[derive(Debug, Clone)]
pub enum Input {
    /// A socket opened. `verified` is the agent the adapter authenticated, if any.
    Opened {
        now_ms: u64,
        sock: SocketId,
        verified: Option<AgentId>,
    },
    Frame {
        now_ms: u64,
        sock: SocketId,
        text: String,
    },
    Closed {
        now_ms: u64,
        sock: SocketId,
    },
    Wake {
        now_ms: u64,
    },
    TrunkPoked {
        now_ms: u64,
    },
    /// The answer to `Output::Load`: the stored core state, or `None` for a repository with none.
    Loaded {
        now_ms: u64,
        stored: Option<String>,
    },
    /// The answer to `Output::ReadEvents`: a page of the stored log. `done` ends the replay.
    ReplayPage {
        now_ms: u64,
        sock: SocketId,
        events: Vec<Event>,
        done: bool,
    },
    Committed {
        now_ms: u64,
        batch: BatchId,
    },
    CommitFailed {
        now_ms: u64,
        batch: BatchId,
    },
    /// A call was answered, `Unknown` included.
    Replied {
        now_ms: u64,
        call: CallId,
        answer: CallAnswer,
    },
    /// The adapter lost the call before an answer: counts as `CallAnswer::Unknown`.
    CallLost {
        now_ms: u64,
        call: CallId,
    },
}

/// Something the adapter must do.
#[derive(Debug, Clone)]
pub enum Output {
    Load,
    /// Store `entries` (key and JSON) in one transaction, then report `Committed` or
    /// `CommitFailed` for `batch`.
    Commit {
        batch: BatchId,
        entries: Vec<(String, String)>,
    },
    /// Held until the batch it follows has committed (protocol invariant 4).
    Send {
        sock: SocketId,
        msg: ServerMsg,
    },
    SetSession {
        sock: SocketId,
        session: Session,
    },
    Close {
        sock: SocketId,
        code: u16,
        reason: String,
    },
    ReadEvents {
        sock: SocketId,
        from_seq: u64,
        limit: usize,
    },
    /// The next wake, or `None` to clear it.
    SetWake {
        at_ms: Option<u64>,
    },
    /// Emitted only after the batch that persisted the call has committed.
    Call(Call),
    Log {
        line: String,
    },
    Metric {
        name: String,
        value: u64,
    },
}

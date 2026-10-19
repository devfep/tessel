//! The one commit that may be outstanding, and everything held back until it is stored.
//!
//! Protocol invariant 4 (a fence is stored before its grant is sent): the sends, the session
//! change, the wake and the calls of a step are in a `Held` inside a `Pending`. `Committed` is
//! built only by `Pending::commit`, which the host calls only on `Input::Committed`, so nothing
//! reaches an `Output` without a stored batch behind it (C1). A failed commit just drops the
//! `Pending`.

use crate::host::call::Call;
use crate::host::{AnswerStep, BatchId, Job, SocketId};
use crate::protocol::Event;
use crate::shell::{Outbound, Session};

/// A call to hand to the adapter once the batch that persisted it has committed, with what to do
/// with its answer.
pub struct StartedCall {
    pub call: Call,
    pub on_answer: AnswerStep,
}

/// What a step produced besides its stored entries.
pub struct Held {
    pub outbound: Vec<Outbound>,
    pub events: Vec<Event>,
    /// The socket whose message was handled: where a `Reply` goes.
    pub reply_to: Option<SocketId>,
    /// The session to record for a socket the core welcomed, after the persist.
    pub bind: Option<(SocketId, Session)>,
    pub next_wake_ms: Option<u64>,
    pub calls: Vec<StartedCall>,
}

/// A batch handed to the adapter and not yet answered.
pub struct Pending {
    id: BatchId,
    entries: Vec<(String, String)>,
    held: Held,
    then: Option<Job>,
}

/// A batch that is stored. Only `Pending::commit` builds one.
pub struct Committed {
    entries: Vec<(String, String)>,
    held: Held,
    then: Option<Job>,
}

impl Pending {
    pub fn new(id: BatchId, entries: Vec<(String, String)>, held: Held, then: Option<Job>) -> Self {
        Self {
            id,
            entries,
            held,
            then,
        }
    }

    pub fn id(&self) -> BatchId {
        self.id
    }

    pub fn entries(&self) -> &[(String, String)] {
        &self.entries
    }

    /// The batch is stored: release what it held.
    pub fn commit(self) -> Committed {
        Committed {
            entries: self.entries,
            held: self.held,
            then: self.then,
        }
    }
}

impl Committed {
    /// The entries that were stored, what the batch held, and the step of the same task that
    /// waited for it.
    pub fn into_parts(self) -> (Vec<(String, String)>, Held, Option<Job>) {
        (self.entries, self.held, self.then)
    }
}

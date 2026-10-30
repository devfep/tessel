//! The one commit that may be outstanding, and everything held back until it is stored.
//!
//! Protocol invariant 4 (a fence is stored before its grant is sent): the sends, the session
//! change, the wake and the calls of a step that was persisted are in a `Held` inside a
//! `Pending`. `Committed` is built only by `Pending::commit`, and `Host::release` takes a
//! `Committed`, so nothing a batch held reaches an `Output` without that batch stored (C1). A
//! failed commit drops the `Pending`. A reply that persisted nothing, such as the refusal of a
//! call over the soft limit, is sent at once.

use crate::host::call::Call;
use crate::host::{BatchId, Job, SocketId};
use crate::protocol::Event;
use crate::shell::{Outbound, Session};

/// How many times the host hands the adapter one batch before it gives up: the first attempt and
/// two retries.
pub const MAX_COMMIT_ATTEMPTS: u32 = 3;

/// What a step produced besides its stored entries.
pub struct Held {
    pub outbound: Vec<Outbound>,
    pub events: Vec<Event>,
    /// The socket whose message was handled: where a `Reply` goes.
    pub reply_to: Option<SocketId>,
    /// The session to record for a socket the core welcomed, after the persist.
    pub bind: Option<(SocketId, Session)>,
    pub next_wake_ms: Option<u64>,
    /// Calls to hand to the adapter once this batch has committed.
    pub calls: Vec<Call>,
}

/// A batch handed to the adapter and not yet answered.
pub struct Pending {
    id: BatchId,
    entries: Vec<(String, String)>,
    held: Held,
    then: Option<Job>,
}

/// What a batch that was not stored leaves behind. Nothing it held is sent, but whoever waited
/// for it is told by the host (R10).
pub struct Discarded {
    /// The socket whose message the batch stored.
    pub reply_to: Option<SocketId>,
    /// The step that was to run once the batch was stored.
    pub then: Option<Job>,
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

    /// The batch is not stored: drop what it held.
    pub fn discard(self) -> Discarded {
        Discarded {
            reply_to: self.held.reply_to,
            then: self.then,
        }
    }
}

impl Committed {
    /// The step of the same task that waited for this batch, taken once.
    pub fn take_then(&mut self) -> Option<Job> {
        self.then.take()
    }

    /// The entries that were stored and what the batch held.
    pub fn into_parts(self) -> (Vec<(String, String)>, Held) {
        (self.entries, self.held)
    }
}

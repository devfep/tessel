//! The one commit that may be outstanding, and everything held back until it is stored.
//!
//! Protocol invariant 4 (a fence is stored before its grant is sent): the sends, the session
//! change, the wake and the calls of a step that was persisted are in a `Held` inside a
//! `Pending`. `Committed` is built only by `Pending::commit`, and `Host::release` takes a
//! `Committed`, so nothing a batch held reaches an `Output` without that batch stored (C1).
//!
//! A failed commit is retried (C8): the `Pending` stays in memory, its entries are handed to the
//! adapter again, and nothing it held is sent until a commit succeeds. After
//! `MAX_COMMIT_ATTEMPTS` attempts the `Pending` is dropped, which is the cut-off the retry
//! postpones. A reply that persisted nothing, such as the refusal of a call over the soft limit,
//! is sent at once.

use crate::host::call::Call;
use crate::host::{BatchId, Job, SocketId};
use crate::protocol::Event;
use crate::shell::{Outbound, Session};

/// How many times the host hands the adapter one batch before it gives up: the first attempt and
/// two retries, one after the other, with no pause (the host reads no clock). An adapter that
/// wants a pause waits inside its own commit.
///
/// Why no backoff, for each way a real SQLite store fails (`tessel-server/src/store.rs`; its tests
/// pin the first two):
/// - Busy: the store sets `busy_timeout`, so the busy handler sleeps inside the commit for up to
///   that long before it returns `SQLITE_BUSY` (`sqlite3_busy_timeout`). It covers `SQLITE_BUSY`,
///   not `SQLITE_LOCKED`, and only because the commit's first statement is a write: a read first
///   would get `SQLITE_BUSY_SNAPSHOT`, which has no busy handler (the lock test pins the wait).
///   Each attempt therefore already waits; a retry right after one is the pause, repeated.
/// - Disk full (`SQLITE_FULL`) and I/O errors (`SQLITE_IOERR`): SQLite may undo only the failing
///   statement (`sqlite3VdbeHalt`); for the store's single-row upserts it rolled the whole
///   transaction back, and the transaction's rollback on drop covers the rest either way, so a
///   failed attempt stores nothing (the full-database test pins it with two entries) and the
///   retry is harmless. It cannot succeed before space is freed or the device recovers, so a
///   pause of seconds would not change the outcome, and the input gate is closed meanwhile,
///   which stalls every client. The cut-off after the last attempt is the right answer: the core
///   is dropped and the next message loads it from what is stored, so a store that recovers is
///   served again.
/// - A corrupt or unusable database: no retry helps; the cut-off tells the operator at once.
///
/// A backoff would be a wake with the clock as input, kept deterministic in the simulator, for no
/// failure class that a pause fixes.
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
    attempts: u32,
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
            attempts: 1,
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

    /// Count another attempt at storing the batch. False when `MAX_COMMIT_ATTEMPTS` are spent.
    pub fn retry(&mut self) -> bool {
        if self.attempts >= MAX_COMMIT_ATTEMPTS {
            return false;
        }
        self.attempts += 1;
        true
    }

    /// The attempts made so far, the one in flight included.
    pub fn attempts(&self) -> u32 {
        self.attempts
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

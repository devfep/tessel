//! The simulated store. A commit is first staged: it is a write the adapter has accepted and not
//! yet made durable. Only `apply` makes it durable, and a crash loses every staged write. A commit
//! can also fail without a crash, which drops its staged write and leaves the durable store as it
//! was.

use std::collections::BTreeMap;

use crate::host::BatchId;
use crate::protocol::Event;
use crate::shell::{EVENT_PREFIX, STATE_KEY};

#[derive(Debug, Default)]
pub struct Store {
    durable: BTreeMap<String, String>,
    staged: Option<(BatchId, Vec<(String, String)>)>,
    /// Acknowledged by a write-behind adapter and not written yet.
    unflushed: Vec<(String, String)>,
    /// The core state as it was before the last write that changed it.
    previous_state: Option<String>,
}

impl Store {
    /// Accept `entries` for `batch`: not durable until `apply`.
    pub fn stage(&mut self, batch: BatchId, entries: Vec<(String, String)>) {
        self.staged = Some((batch, entries));
    }

    /// Make the staged write of `batch` durable. False when `batch` has nothing staged.
    pub fn apply(&mut self, batch: BatchId) -> bool {
        let Some((staged, entries)) = self.staged.take() else {
            return false;
        };
        if staged != batch {
            self.staged = Some((staged, entries));
            return false;
        }
        self.write(entries);
        true
    }

    /// Acknowledge the staged write of `batch` without writing it: it is written when the next
    /// commit is acknowledged. A write-behind adapter's behaviour.
    pub fn defer(&mut self, batch: BatchId) {
        let Some((staged, entries)) = self.staged.take() else {
            return;
        };
        if staged != batch {
            self.staged = Some((staged, entries));
            return;
        }
        let earlier = std::mem::take(&mut self.unflushed);
        self.write(earlier);
        self.unflushed = entries;
    }

    /// Drop the staged write of `batch`: its commit failed.
    pub fn abandon(&mut self, batch: BatchId) {
        if self
            .staged
            .as_ref()
            .is_some_and(|(staged, _)| *staged == batch)
        {
            self.staged = None;
        }
    }

    /// The process died: every write that is not durable is gone.
    pub fn crash(&mut self) {
        self.staged = None;
        self.unflushed.clear();
    }

    fn write(&mut self, entries: Vec<(String, String)>) {
        for (key, json) in entries {
            if key == STATE_KEY {
                self.previous_state = self.durable.get(STATE_KEY).cloned();
            }
            self.durable.insert(key, json);
        }
    }

    /// Write directly, as a migration or a drifting codec would.
    pub fn put(&mut self, key: &str, json: String) {
        self.durable.insert(key.to_string(), json);
    }

    pub fn state(&self) -> Option<&str> {
        self.durable.get(STATE_KEY).map(String::as_str)
    }

    /// The state a lagging read returns: the one before the last write that changed it.
    pub fn stale_state(&self) -> Option<&str> {
        self.previous_state.as_deref().or_else(|| self.state())
    }

    /// Every durable event, in `seq` order, with the keys that did not parse.
    pub fn events(&self) -> (Vec<Event>, Vec<String>) {
        let mut events = Vec::new();
        let mut unparsed = Vec::new();
        for (key, json) in &self.durable {
            if !key.starts_with(EVENT_PREFIX) {
                continue;
            }
            match serde_json::from_str::<Event>(json) {
                Ok(event) => events.push(event),
                Err(_) => unparsed.push(key.clone()),
            }
        }
        events.sort_by_key(|event| event.seq);
        (events, unparsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, json: &str) -> Vec<(String, String)> {
        vec![(key.to_string(), json.to_string())]
    }

    #[test]
    fn a_crash_loses_a_staged_write_and_keeps_an_applied_one() {
        let mut store = Store::default();
        store.stage(BatchId(1), entry(STATE_KEY, "one"));
        assert!(store.apply(BatchId(1)));
        store.stage(BatchId(2), entry(STATE_KEY, "two"));
        store.crash();
        assert_eq!(store.state(), Some("one"));
        assert!(!store.apply(BatchId(2)));
    }

    #[test]
    fn a_failed_commit_leaves_the_store_as_it_was() {
        let mut store = Store::default();
        store.stage(BatchId(1), entry(STATE_KEY, "one"));
        store.abandon(BatchId(1));
        assert_eq!(store.state(), None);
    }

    #[test]
    fn a_write_for_another_batch_is_not_applied() {
        let mut store = Store::default();
        store.stage(BatchId(1), entry(STATE_KEY, "one"));
        assert!(!store.apply(BatchId(2)));
        assert!(store.apply(BatchId(1)));
    }

    #[test]
    fn a_deferred_write_lands_one_commit_late_and_a_crash_erases_it() {
        let mut store = Store::default();
        store.stage(BatchId(1), entry(STATE_KEY, "one"));
        store.defer(BatchId(1));
        assert_eq!(store.state(), None);
        store.stage(BatchId(2), entry(STATE_KEY, "two"));
        store.defer(BatchId(2));
        assert_eq!(store.state(), Some("one"));
        store.crash();
        assert_eq!(store.state(), Some("one"));
    }

    #[test]
    fn a_stale_read_returns_the_state_before_the_last_change() {
        let mut store = Store::default();
        for (batch, json) in [(1, "one"), (2, "two")] {
            store.stage(BatchId(batch), entry(STATE_KEY, json));
            assert!(store.apply(BatchId(batch)));
        }
        assert_eq!(store.stale_state(), Some("one"));
        assert_eq!(store.state(), Some("two"));
    }
}

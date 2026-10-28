//! The simulated store. A commit is first staged: it is a write the adapter has accepted and not
//! yet made durable. Only `apply` makes it durable, and a crash loses every staged write. A commit
//! that fails without a crash is never staged, so the durable store stays as it was.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::host::BatchId;
use crate::protocol::Event;
use crate::shell::{EVENT_PREFIX, STATE_KEY};

/// The durable events, and the keys that did not parse.
type ParsedEvents = (Rc<Vec<Event>>, Rc<Vec<String>>);

#[derive(Debug, Default, Clone)]
pub struct Store {
    durable: BTreeMap<String, String>,
    staged: Option<(BatchId, Vec<(String, String)>)>,
    /// Acknowledged by a write-behind adapter and not written yet.
    unflushed: Vec<(String, String)>,
    /// The core state as it was before the last write that changed it.
    previous_state: Option<String>,
    /// The events as last parsed, dropped when anything durable changes.
    parsed: RefCell<Option<ParsedEvents>>,
    values: RefCell<Option<Rc<BTreeMap<u64, serde_json::Value>>>>,
}

impl Store {
    /// Accept `entries` for `batch`: not durable until `apply`.
    pub fn stage(&mut self, batch: BatchId, entries: Vec<(String, String)>) {
        self.staged = Some((batch, entries));
    }

    /// The write the adapter has accepted and not yet made durable.
    pub fn staged(&self) -> Option<&[(String, String)]> {
        self.staged.as_ref().map(|(_, entries)| entries.as_slice())
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
    pub fn defer(&mut self, batch: BatchId) -> bool {
        let Some((staged, entries)) = self.staged.take() else {
            return false;
        };
        if staged != batch {
            self.staged = Some((staged, entries));
            return false;
        }
        let earlier = std::mem::take(&mut self.unflushed);
        self.write(earlier);
        self.unflushed = entries;
        true
    }

    /// The process died: every write that is not durable is gone.
    pub fn crash(&mut self) {
        self.staged = None;
        self.unflushed.clear();
    }

    fn write(&mut self, entries: Vec<(String, String)>) {
        *self.parsed.borrow_mut() = None;
        *self.values.borrow_mut() = None;
        for (key, json) in entries {
            if key == STATE_KEY {
                self.previous_state = self.durable.get(STATE_KEY).cloned();
            }
            self.durable.insert(key, json);
        }
    }

    /// Everything durable, key by key.
    pub fn entries(&self) -> &BTreeMap<String, String> {
        &self.durable
    }

    pub fn state(&self) -> Option<&str> {
        self.durable.get(STATE_KEY).map(String::as_str)
    }

    /// The state a lagging read returns: the one before the last write that changed it.
    pub fn stale_state(&self) -> Option<&str> {
        self.previous_state.as_deref().or_else(|| self.state())
    }

    /// The durable events as JSON, by `seq`, for comparing with what clients received.
    pub fn event_values(&self) -> Rc<BTreeMap<u64, serde_json::Value>> {
        if let Some(cached) = self.values.borrow().as_ref() {
            return cached.clone();
        }
        let values: BTreeMap<u64, serde_json::Value> = self
            .events()
            .0
            .iter()
            .filter_map(|event| Some((event.seq, serde_json::to_value(event).ok()?)))
            .collect();
        let values = Rc::new(values);
        *self.values.borrow_mut() = Some(values.clone());
        values
    }

    /// Every durable event, in `seq` order, with the keys that did not parse. Parsed once per
    /// change to the store, then shared.
    pub fn events(&self) -> ParsedEvents {
        if let Some(cached) = self.parsed.borrow().as_ref() {
            return cached.clone();
        }
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
        let parsed = (Rc::new(events), Rc::new(unparsed));
        *self.parsed.borrow_mut() = Some(parsed.clone());
        parsed
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

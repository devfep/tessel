//! Durable Object storage access for the shell. The only place that writes state and events.
//!
//! `Persisted` can be built only by `write`, after the transaction has committed, and it owns the
//! `Applied` that was written. Sending takes a `Persisted` and reads the messages from it, so
//! nothing but what was just stored can be sent (CLAUDE.md rule 6).
//!
//! The pure parts (what a replay reads, how a stored event is decoded) are plain functions with
//! native tests. `write` and `read_events` are glue; they need a real storage to run.

use crate::coordinator::MergeDispatch;
use crate::protocol::Event;
use crate::shell::{self, Outbound};
use worker::{js_sys, Error, ListOptions, Result, Storage};

/// What one call into the core produced, ready to store and then send.
pub struct Applied {
    /// (key, JSON) pairs for one transaction: the core state, then each event.
    pub entries: Vec<(String, String)>,
    pub events: Vec<Event>,
    pub outbound: Vec<Outbound>,
    /// The next alarm: the earliest lease expiry or merge dispatch.
    pub next_alarm_ms: Option<u64>,
    /// The merge to ask the steward for once this is stored. It travels with the stored call so
    /// the steward is never called for a marker that is not persisted.
    pub dispatch: Option<MergeDispatch>,
}

/// An `Applied` that is stored. The field is private: `write` is the only constructor.
pub struct Persisted {
    applied: Applied,
}

impl Persisted {
    pub fn outbound(&self) -> &[Outbound] {
        &self.applied.outbound
    }

    pub fn events(&self) -> &[Event] {
        &self.applied.events
    }

    pub fn next_alarm_ms(&self) -> Option<u64> {
        self.applied.next_alarm_ms
    }

    pub fn dispatch(&self) -> Option<&MergeDispatch> {
        self.applied.dispatch.as_ref()
    }

    /// The (key, JSON) pairs that were stored: the state first, then each event.
    pub fn entries(&self) -> &[(String, String)] {
        &self.applied.entries
    }
}

/// Store `applied.entries` in one transaction: all of them or none.
pub async fn write(storage: &Storage, applied: Applied) -> Result<Persisted> {
    let entries = applied.entries.clone();
    storage
        .transaction(move |txn| async move {
            for (key, json) in entries {
                txn.put(&key, json).await?;
            }
            Ok(())
        })
        .await?;
    Ok(Persisted { applied })
}

/// One page of the replay: up to `limit` events with `seq >= start_seq`.
pub struct ReplayRead {
    first_key: String,
    limit: usize,
}

impl ReplayRead {
    pub fn new(start_seq: u64, limit: usize) -> Self {
        Self {
            first_key: shell::event_key(start_seq),
            limit,
        }
    }

    /// The storage list options: event keys only, from the first key, at most `limit`.
    pub fn options(&self) -> ListOptions<'_> {
        ListOptions::new()
            .prefix(shell::EVENT_PREFIX)
            .start(&self.first_key)
            .limit(self.limit)
    }
}

/// Decode one stored event. The error names the key and position only: stored events hold
/// intents, and serde quotes values.
pub fn decode_event(key: &str, json: &str) -> std::result::Result<Event, String> {
    serde_json::from_str(json).map_err(|e| {
        format!(
            "stored event {key} is corrupt (line {}, column {})",
            e.line(),
            e.column()
        )
    })
}

/// Read up to `limit` stored events with `seq >= start_seq`, in `seq` order.
pub async fn read_events(storage: &Storage, start_seq: u64, limit: usize) -> Result<Vec<Event>> {
    let read = ReplayRead::new(start_seq, limit);
    let stored = storage.list_with_options(read.options()).await?;
    let mut events = Vec::new();
    for entry in stored.entries() {
        let pair = js_sys::Array::from(&entry?);
        let key = pair.get(0).as_string().unwrap_or_default();
        let Some(json) = pair.get(1).as_string() else {
            return Err(Error::RustError(format!(
                "stored event {key} is not a string"
            )));
        };
        events.push(decode_event(&key, &json).map_err(Error::RustError)?);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AgentId, EventKind, RunId};

    fn event_json(seq: u64) -> String {
        let event = Event {
            seq,
            at_ms: 5,
            run: RunId("r".into()),
            kind: EventKind::AgentConnected {
                agent: AgentId("a1".into()),
            },
        };
        serde_json::to_string(&event).unwrap()
    }

    #[test]
    fn replay_reads_event_keys_from_the_start_seq_with_the_page_limit() {
        let options = serde_json::to_value(ReplayRead::new(7, 100).options()).unwrap();
        assert_eq!(
            options,
            serde_json::json!({"start": "ev:00000000000000000007", "prefix": "ev:", "limit": 100})
        );
    }

    #[test]
    fn replay_start_key_follows_the_requested_seq() {
        let at_zero = serde_json::to_value(ReplayRead::new(0, 1).options()).unwrap();
        let at_max = serde_json::to_value(ReplayRead::new(u64::MAX, 1).options()).unwrap();
        assert_eq!(at_zero["start"], "ev:00000000000000000000");
        assert_eq!(at_max["start"], "ev:18446744073709551615");
    }

    #[test]
    fn a_stored_event_decodes_to_the_event_that_was_written() {
        let event = decode_event("ev:x", &event_json(42)).unwrap();
        assert_eq!(event.seq, 42);
        assert_eq!(event.run, RunId("r".into()));
    }

    #[test]
    fn a_corrupt_stored_event_reports_key_and_position_without_quoting_it() {
        let err = decode_event("ev:00000000000000000003", r#"{"seq":"secret-text"}"#).unwrap_err();
        assert!(err.contains("ev:00000000000000000003"), "{err}");
        assert!(err.contains("line 1"), "{err}");
        assert!(!err.contains("secret"), "{err}");
        assert!(decode_event("ev:x", "").is_err());
    }
}

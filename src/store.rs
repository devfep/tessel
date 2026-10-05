//! Durable Object storage access for the shell. The only place that writes state and events.
//!
//! `Persisted` can be built only by `write`, after the transaction has committed. Sending a
//! call's messages takes one, so sending before the write does not type-check (CLAUDE.md rule 6).

use crate::protocol::Event;
use crate::shell;
use worker::{js_sys, Error, ListOptions, Result, Storage};

/// Proof that a call's state and events are stored. The field is private: `write` is the only
/// constructor.
#[derive(Debug)]
pub struct Persisted(());

/// Store `entries` ((key, JSON) pairs) in one transaction: all of them or none.
pub async fn write(storage: &Storage, entries: Vec<(String, String)>) -> Result<Persisted> {
    storage
        .transaction(move |txn| async move {
            for (key, json) in entries {
                txn.put(&key, json).await?;
            }
            Ok(())
        })
        .await?;
    Ok(Persisted(()))
}

/// Read up to `limit` stored events with `seq >= start_seq`, in `seq` order. Errors name the key
/// and position only: stored events hold intents, and serde quotes values.
pub async fn read_events(storage: &Storage, start_seq: u64, limit: usize) -> Result<Vec<Event>> {
    let first_key = shell::event_key(start_seq);
    let options = ListOptions::new()
        .prefix(shell::EVENT_PREFIX)
        .start(&first_key)
        .limit(limit);
    let stored = storage.list_with_options(options).await?;
    let mut events = Vec::new();
    for entry in stored.entries() {
        let pair = js_sys::Array::from(&entry?);
        let key = pair.get(0).as_string().unwrap_or_default();
        let Some(json) = pair.get(1).as_string() else {
            return Err(Error::RustError(format!(
                "stored event {key} is not a string"
            )));
        };
        let event = serde_json::from_str(&json).map_err(|e| {
            Error::RustError(format!(
                "stored event {key} is corrupt (line {}, column {})",
                e.line(),
                e.column()
            ))
        })?;
        events.push(event);
    }
    Ok(events)
}

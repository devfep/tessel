//! Checkers: each proves one invariant of `docs/proposals/phase-1.md` ("Invariants and the
//! simulator checks that prove them") from what clients received and what the simulated store
//! holds, never from what the host meant to send. They run after every step of a seed.
//!
//! Implemented here: I1 (a grant or event a client holds is in the store), I5 (the stored state
//! round-trips through the codec and through every older shape) and I9 (no fence, claim id or seq
//! is seen twice; a retired fence is refused). The invariants of the second half (I2, I4, I8,
//! I10) join `Checker` and `run` with their traffic.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::coordinator::Coordinator;
use crate::host::SocketId;
use crate::protocol::{ClaimId, Event, EventKind, Fence, ServerMsg};
use crate::sim::driver::{Delivery, RequestKind, Sent, Sim};
use crate::sim::store::Store;

/// Which invariant a violation breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Checker {
    I1,
    I5,
    I9,
    /// The harness itself: the host kept asking the adapter past any bound.
    Stuck,
}

impl Checker {
    /// The checkers a run enables by default.
    pub const ALL: [Checker; 3] = [Checker::I1, Checker::I5, Checker::I9];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub checker: Checker,
    pub detail: String,
}

/// Run every enabled checker on `sim` as it stands.
pub fn run(sim: &Sim, enabled: &[Checker]) -> Vec<Violation> {
    let mut found = Vec::new();
    for checker in enabled {
        let details = match checker {
            Checker::I1 => i1(sim.transcript(), sim.store()),
            Checker::I5 => i5(sim.store()),
            Checker::I9 => i9(sim.transcript(), sim.sent(), &|sock| sim.is_open(sock)),
            Checker::Stuck => Vec::new(),
        };
        for detail in details {
            found.push(Violation {
                checker: *checker,
                detail,
            });
        }
    }
    found
}

fn stored_state(store: &Store) -> Result<Option<Value>, String> {
    let Some(json) = store.state() else {
        return Ok(None);
    };
    serde_json::from_str(json)
        .map(Some)
        .map_err(|e| format!("the stored state is not JSON: {e}"))
}

fn counter(state: &Value, field: &str) -> u64 {
    state.get(field).and_then(Value::as_u64).unwrap_or(0)
}

/// I1: each grant and event in a transcript against the state and log in the store.
pub fn i1(transcript: &[Delivery], store: &Store) -> Vec<String> {
    let state = match stored_state(store) {
        Ok(state) => state,
        Err(e) => return vec![e],
    };
    let (events, unparsed) = store.events();
    let mut found: Vec<String> = unparsed
        .iter()
        .map(|key| format!("stored event {key} does not parse"))
        .collect();
    let by_seq: BTreeMap<u64, &Event> = events.iter().map(|event| (event.seq, event)).collect();
    if let Some(state) = &state {
        for (index, event) in events.iter().enumerate() {
            if event.seq != index as u64 {
                found.push(format!("the stored log has a gap before seq {}", event.seq));
                break;
            }
        }
        if counter(state, "next_seq") != events.len() as u64 {
            found.push(format!(
                "state next_seq {} but {} events stored",
                counter(state, "next_seq"),
                events.len()
            ));
        }
    }
    for delivery in transcript {
        found.extend(i1_message(&delivery.msg, state.as_ref(), &events, &by_seq));
    }
    found
}

fn i1_message(
    msg: &ServerMsg,
    state: Option<&Value>,
    events: &[Event],
    by_seq: &BTreeMap<u64, &Event>,
) -> Vec<String> {
    let mut found = Vec::new();
    if let ServerMsg::Granted { claim, fence, .. } = msg {
        let Some(state) = state else {
            return vec![format!("claim {} granted but no state is stored", claim.0)];
        };
        if fence.0 >= counter(state, "next_fence") || claim.0 >= counter(state, "next_claim") {
            found.push(format!(
                "grant of claim {} fence {} is not below the stored counters",
                claim.0, fence.0
            ));
        }
        if !events
            .iter()
            .any(|event| logs_grant(&event.kind, *claim, *fence))
        {
            found.push(format!(
                "grant of claim {} fence {} is not in the stored log",
                claim.0, fence.0
            ));
        }
    }
    if let ServerMsg::LeaseExpired { claim, fence } = msg {
        let current = state
            .and_then(|state| state.get("claims"))
            .and_then(|claims| claims.get(claim.0.to_string()))
            .and_then(|claim| claim.get("fence"))
            .and_then(Value::as_u64);
        if current == Some(fence.0) {
            found.push(format!(
                "claim {} was told its fence {} lapsed but the store still holds it",
                claim.0, fence.0
            ));
        }
    }
    if let ServerMsg::Event { event } = msg {
        let stored = by_seq
            .get(&event.seq)
            .and_then(|e| serde_json::to_value(e).ok());
        if stored != serde_json::to_value(event).ok() {
            found.push(format!("event {} differs from the stored one", event.seq));
        }
    }
    found
}

fn logs_grant(kind: &EventKind, claim: ClaimId, fence: Fence) -> bool {
    if let EventKind::ClaimGranted {
        claim: logged_claim,
        fence: logged_fence,
        ..
    } = kind
    {
        return *logged_claim == claim && *logged_fence == fence;
    }
    if let EventKind::ClaimAmended {
        claim: logged_claim,
        fence: logged_fence,
        ..
    } = kind
    {
        return *logged_claim == claim && *logged_fence == fence;
    }
    false
}

/// An older shape of the stored state: removes the fields that build did not write. False when a
/// field holds something other than the default the old build implied, so the shape does not
/// apply to this state.
type OlderShape = fn(&mut Value) -> bool;

fn remove_if(object: &mut serde_json::Map<String, Value>, key: &str, default: &Value) -> bool {
    match object.get(key) {
        Some(value) if value != default => false,
        Some(_) => {
            object.remove(key);
            true
        }
        None => true,
    }
}

/// Before submissions and shadow claims: no `next_submission`, `config.shadow_enabled`, claim
/// `kind` or `submitted`.
fn before_submit_and_shadow(state: &mut Value) -> bool {
    let Some(object) = state.as_object_mut() else {
        return false;
    };
    let mut applies = remove_if(object, "next_submission", &Value::from(0));
    if let Some(config) = object.get_mut("config").and_then(Value::as_object_mut) {
        applies &= remove_if(config, "shadow_enabled", &Value::from(false));
    }
    for claim in claims_of(object) {
        applies &= remove_if(claim, "kind", &Value::from("real"));
        applies &= remove_if(claim, "submitted", &Value::Null);
    }
    applies
}

/// Before denials were kept: no claim `denial`.
fn before_denials(state: &mut Value) -> bool {
    let Some(object) = state.as_object_mut() else {
        return false;
    };
    let mut applies = true;
    for claim in claims_of(object) {
        applies &= remove_if(claim, "denial", &Value::Null);
    }
    applies
}

fn claims_of(
    state: &mut serde_json::Map<String, Value>,
) -> impl Iterator<Item = &mut serde_json::Map<String, Value>> {
    state
        .get_mut("claims")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flat_map(|claims| claims.values_mut().filter_map(Value::as_object_mut))
}

/// Every older shape the core still loads.
const OLDER_SHAPES: [(&str, OlderShape); 2] = [
    (
        "before submissions and shadow claims",
        before_submit_and_shadow,
    ),
    ("before denials were kept", before_denials),
];

/// I5: the stored state decodes and encodes back to what is stored, and so does each older shape
/// of it that applies.
pub fn i5(store: &Store) -> Vec<String> {
    let state = match stored_state(store) {
        Ok(Some(state)) => state,
        Ok(None) => return Vec::new(),
        Err(e) => return vec![e],
    };
    let mut found = Vec::new();
    if let Some(drift) = round_trip_drift(&state, &state) {
        found.push(format!("the stored state {drift}"));
    }
    for (name, shape) in OLDER_SHAPES {
        let mut older = state.clone();
        if !shape(&mut older) {
            continue;
        }
        if let Some(drift) = round_trip_drift(&older, &state) {
            found.push(format!("the stored state {name} {drift}"));
        }
    }
    found
}

/// How decoding `stored` and encoding it again differs from `expected`, if it does.
fn round_trip_drift(stored: &Value, expected: &Value) -> Option<String> {
    let decoded: Coordinator = match serde_json::from_value(stored.clone()) {
        Ok(decoded) => decoded,
        Err(e) => return Some(format!("does not decode: {e}")),
    };
    let encoded = match serde_json::to_value(&decoded) {
        Ok(encoded) => encoded,
        Err(e) => return Some(format!("does not encode: {e}")),
    };
    (encoded != *expected).then(|| differing_keys(&encoded, expected))
}

fn differing_keys(encoded: &Value, expected: &Value) -> String {
    let (Some(encoded), Some(expected)) = (encoded.as_object(), expected.as_object()) else {
        return "does not round-trip".to_string();
    };
    let mut keys: BTreeSet<&String> = encoded.keys().collect();
    keys.extend(expected.keys());
    let differing: Vec<&str> = keys
        .into_iter()
        .filter(|key| encoded.get(*key) != expected.get(*key))
        .map(String::as_str)
        .collect();
    format!("does not round-trip: {} differ", differing.join(", "))
}

/// I9: nothing a client has seen is issued again, and a retired fence is refused.
pub fn i9(
    transcript: &[Delivery],
    sent: &[Sent],
    is_open: &dyn Fn(SocketId) -> bool,
) -> Vec<String> {
    let mut found = Vec::new();
    let kind_of: BTreeMap<u64, RequestKind> = sent.iter().map(|s| (s.req.0, s.kind)).collect();
    let mut fences: BTreeSet<u64> = BTreeSet::new();
    let mut claims: BTreeSet<u64> = BTreeSet::new();
    let mut latest: BTreeMap<u64, u64> = BTreeMap::new();
    let mut seqs: BTreeMap<u64, Value> = BTreeMap::new();
    let mut seen_by_sock: BTreeSet<(u64, u64)> = BTreeSet::new();
    for delivery in transcript {
        if let ServerMsg::Granted {
            req, claim, fence, ..
        } = &delivery.msg
        {
            if !fences.insert(fence.0) {
                found.push(format!("fence {} was granted twice", fence.0));
            }
            if kind_of.get(&req.0) == Some(&RequestKind::Claim) && !claims.insert(claim.0) {
                found.push(format!("claim {} was created twice", claim.0));
            }
            if latest
                .insert(claim.0, fence.0)
                .is_some_and(|before| before >= fence.0)
            {
                found.push(format!(
                    "claim {} got fence {} after a higher one",
                    claim.0, fence.0
                ));
            }
        }
        if let ServerMsg::Event { event } = &delivery.msg {
            let json = serde_json::to_value(event).unwrap_or(Value::Null);
            if !seen_by_sock.insert((delivery.sock.0, event.seq)) {
                found.push(format!(
                    "socket {} saw seq {} twice",
                    delivery.sock.0, event.seq
                ));
            }
            if seqs
                .insert(event.seq, json.clone())
                .is_some_and(|before| before != json)
            {
                found.push(format!(
                    "seq {} was issued for two different events",
                    event.seq
                ));
            }
        }
    }
    found.extend(unrefused_probes(transcript, sent, is_open));
    found
}

/// A probe is answered with a refusal, unless its socket ended first (a crash, or a failed commit
/// that closes it).
fn unrefused_probes(
    transcript: &[Delivery],
    sent: &[Sent],
    is_open: &dyn Fn(SocketId) -> bool,
) -> Vec<String> {
    let mut found = Vec::new();
    for probe in sent.iter().filter(|s| s.kind == RequestKind::StaleRelease) {
        let mut refused = false;
        for delivery in transcript.iter().filter(|d| d.sock == probe.sock) {
            if let ServerMsg::Error { req: Some(req), .. } = &delivery.msg {
                refused |= *req == probe.req;
            }
            if let ServerMsg::Granted { req, .. } = &delivery.msg {
                if *req == probe.req {
                    found.push(format!("retired fence accepted (request {})", req.0));
                }
            }
        }
        if !refused && is_open(probe.sock) {
            found.push(format!(
                "retired fence not refused (request {})",
                probe.req.0
            ));
        }
    }
    found
}

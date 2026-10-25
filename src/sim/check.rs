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
use crate::protocol::{ClaimId, ErrorCode, Event, EventKind, Fence, ServerMsg};
use crate::sim::driver::{Delivery, RequestKind, Sent, Sim};
use crate::sim::store::Store;

/// Which invariant a violation breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Checker {
    I1,
    I5,
    I9,
    /// The harness itself: the host kept asking the adapter past any bound, or the driver
    /// made a mistake.
    Harness,
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
            Checker::Harness => sim.faults().to_vec(),
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

/// The places in the state where a field can sit: the top level, `config`, and a claim.
fn containers(state: &Value) -> Vec<Vec<String>> {
    let mut found = vec![Vec::new(), vec!["config".to_string()]];
    // Every claim has the same fields: the first is enough, and strips are costly.
    if let Some(id) = state
        .get("claims")
        .and_then(Value::as_object)
        .and_then(|claims| claims.keys().next())
    {
        found.push(vec!["claims".to_string(), id.clone()]);
    }
    found
}

fn at<'a>(state: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(state, |value, key| value.get(key))
}

fn without(state: &Value, path: &[String]) -> Value {
    let mut stripped = state.clone();
    let Some((key, parents)) = path.split_last() else {
        return stripped;
    };
    let mut at = &mut stripped;
    for parent in parents {
        let Some(next) = at.get_mut(parent) else {
            return stripped;
        };
        at = next;
    }
    if let Some(object) = at.as_object_mut() {
        object.remove(key);
    }
    stripped
}

fn encode_decode(state: &Value) -> Result<Value, String> {
    let decoded: Coordinator =
        serde_json::from_value(state.clone()).map_err(|e| format!("does not decode: {e}"))?;
    serde_json::to_value(&decoded).map_err(|e| format!("does not encode: {e}"))
}

/// What stripping each field in turn showed.
struct Stripped {
    /// Fields that can be absent and hold the value the core fills in when they are: every
    /// `#[serde(default)]` field, found with no list to keep.
    defaulted: Vec<Vec<String>>,
    /// Those of them whose absence changes what the state encodes back to.
    drifting: Vec<Vec<String>>,
}

fn strip_each(state: &Value) -> Stripped {
    let mut found = Stripped {
        defaulted: Vec::new(),
        drifting: Vec::new(),
    };
    for container in containers(state) {
        let Some(object) = at(state, &container).and_then(Value::as_object) else {
            continue;
        };
        for key in object.keys() {
            let mut path = container.clone();
            path.push(key.clone());
            let Ok(encoded) = encode_decode(&without(state, &path)) else {
                continue;
            };
            if at(&encoded, &path) != at(state, &path) {
                continue;
            }
            if encoded != *state {
                found.drifting.push(path.clone());
            }
            found.defaulted.push(path);
        }
    }
    found
}

/// The fields an older build did not write, as `strip_each` finds them.
pub fn defaulted_fields(state: &Value) -> Vec<Vec<String>> {
    strip_each(state).defaulted
}

/// I5: the stored state decodes and encodes back to what is stored, and so does the state with
/// each defaulted field absent, and with all of them absent at once.
pub fn i5(store: &Store) -> Vec<String> {
    let state = match stored_state(store) {
        Ok(Some(state)) => state,
        Ok(None) => return Vec::new(),
        Err(e) => return vec![e],
    };
    i5_state(&state)
}

pub fn i5_state(state: &Value) -> Vec<String> {
    let mut found = Vec::new();
    match encode_decode(state) {
        Ok(encoded) if encoded == *state => {}
        Ok(encoded) => found.push(format!(
            "the stored state does not round-trip: {} differ",
            differing_keys(&encoded, state)
        )),
        Err(e) => return vec![format!("the stored state {e}")],
    }
    let stripped = strip_each(state);
    for path in &stripped.drifting {
        found.push(format!(
            "the stored state without {} does not round-trip",
            path.join(".")
        ));
    }
    let mut all_absent = state.clone();
    for path in &stripped.defaulted {
        all_absent = without(&all_absent, path);
    }
    match encode_decode(&all_absent) {
        Ok(encoded) if encoded == *state => {}
        Ok(_) | Err(_) => found.push(
            "the stored state with every defaulted field absent does not round-trip".to_string(),
        ),
    }
    found
}

fn differing_keys(encoded: &Value, expected: &Value) -> String {
    let (Some(encoded), Some(expected)) = (encoded.as_object(), expected.as_object()) else {
        return "the whole state".to_string();
    };
    let mut keys: BTreeSet<&String> = encoded.keys().collect();
    keys.extend(expected.keys());
    let differing: Vec<&str> = keys
        .into_iter()
        .filter(|key| encoded.get(*key) != expected.get(*key))
        .map(String::as_str)
        .collect();
    differing.join(", ")
}

/// The codes that refuse a retired fence: the claim is live under a newer fence, or it has ended.
fn refuses_retired_fence(code: ErrorCode) -> bool {
    code == ErrorCode::StaleFence || code == ErrorCode::UnknownClaim
}

/// I9: nothing a client has seen is issued again, and a retired fence is refused. A release that
/// succeeds sends the requester nothing, so a retired fence that was accepted shows in the stored
/// log as one more release by the agent than there was when the probe was sent; one that was
/// refused shows as a `StaleFence` or `UnknownClaim` error, unless the socket ended first.
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

fn unrefused_probes(
    transcript: &[Delivery],
    sent: &[Sent],
    is_open: &dyn Fn(SocketId) -> bool,
) -> Vec<String> {
    let mut found = Vec::new();
    for probe in sent.iter().filter(|s| s.kind == RequestKind::StaleRelease) {
        if probe.accepted {
            found.push(format!("retired fence accepted (request {})", probe.req.0));
            continue;
        }
        let refused = transcript.iter().any(|delivery| {
            delivery.sock == probe.sock
                && matches!(&delivery.msg, ServerMsg::Error { req: Some(req), code, .. }
                    if *req == probe.req && refuses_retired_fence(*code))
        });
        if !refused && is_open(probe.sock) {
            found.push(format!(
                "retired fence not refused (request {})",
                probe.req.0
            ));
        }
    }
    found
}

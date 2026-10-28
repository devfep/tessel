//! Checkers: each proves one invariant of `docs/proposals/phase-1.md` ("Invariants and the
//! simulator checks that prove them") from what clients received and what the simulated store
//! holds, never from what the host meant to send. They run after every step of a seed.
//!
//! I1: a grant or event a client holds is in the store. I2: a watcher's events are the log from
//! its offset, in order, with no gap and no duplicate. I4: when the core has work due, a wake is
//! pending, later than now. I5: the stored state round-trips through the codec. I8: no two
//! granted claims conflict. I9: no fence, claim id or seq is seen twice, and a retired fence is
//! refused. I10: nothing is dispatched for a submission held for review, nothing uncovered is
//! merged, and no lease is extended without a heartbeat. The checkers that read the merge path
//! (I3, I6, I7, I11, I12 and COORD-HEAD-FROM-AGENT's) are in `merge_check`; `run` calls them.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use serde_json::Value;

use crate::coordinator::Coordinator;
use crate::host::call::Request;
use crate::host::wake::RETRY_MS;
use crate::host::SocketId;
use crate::merge::MERGE_WATCHDOG_MS;
use crate::protocol::{
    ClaimId, ErrorCode, Event, EventKind, Mode, RaceId, Scope, ScopeClaim, ServerMsg,
};
use crate::sim::driver::{Delivery, Dispatch, RequestKind, Sent, Sim, Watch, BASE};
use crate::sim::merge_check;
use crate::sim::store::Store;

/// Which invariant a violation breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Checker {
    I1,
    I2,
    I3,
    I4,
    I5,
    I6,
    I7,
    I8,
    I9,
    I10,
    I11,
    I12,
    /// COORD-HEAD-FROM-AGENT: no dispatch names a head an agent declared.
    AgentBase,
    /// The harness itself: the host kept asking the adapter past any bound, or the driver
    /// made a mistake.
    Harness,
}

impl Checker {
    /// The checkers a run enables by default. `Harness` reports the driver's own mistakes.
    pub const ALL: [Checker; 14] = [
        Checker::I1,
        Checker::I2,
        Checker::I3,
        Checker::I4,
        Checker::I5,
        Checker::I6,
        Checker::I7,
        Checker::I8,
        Checker::I9,
        Checker::I10,
        Checker::I11,
        Checker::I12,
        Checker::AgentBase,
        Checker::Harness,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub checker: Checker,
    pub detail: String,
}

/// Run every enabled checker on `sim` as it stands.
pub fn run(sim: &Sim, enabled: &[Checker]) -> Vec<Violation> {
    let mut found = Vec::new();
    let (events, _) = sim.store().events();
    for checker in enabled {
        let details = match checker {
            Checker::I1 => i1(sim.transcript(), sim.store()),
            Checker::I2 => i2(
                sim.transcript(),
                sim.watches(),
                &sim.store().event_values(),
                &|s| sim.is_open(s),
            ),
            Checker::I4 => i4_waiting(sim.store(), sim.wake(), sim.now_ms(), sim.waits()),
            Checker::I5 => i5(sim.store()),
            Checker::I8 => i8(&events),
            Checker::I10 => i10(
                &events,
                stored_state(sim.store()).ok().flatten().as_ref(),
                sim.dispatches(),
                sim.heartbeats(),
            ),
            Checker::I9 => i9(sim.transcript(), sim.sent(), &|sock| sim.is_open(sock)),
            Checker::I3 => merge_check::i3(&events, sim.transcript(), sim.lander().history()),
            Checker::I6 => merge_check::i6(
                &events,
                &merge_check::MergePath {
                    cutoffs: sim.lander().cutoffs(),
                    poisoned: &sim.lander().poisoned(),
                    strikes: sim.fork_strikes(),
                    history: sim.lander().history(),
                },
            ),
            Checker::I7 => merge_check::i7(sim.stale_answers()),
            Checker::I11 => merge_check::i11(&events, &sim.counters()),
            Checker::I12 => merge_check::i12(sim.slot_notes(), &events),
            Checker::AgentBase => merge_check::agent_base(sim.dispatches(), BASE),
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
    let values = store.event_values();
    let granted: BTreeSet<(u64, u64)> = events
        .iter()
        .filter_map(|event| logged_grant(&event.kind))
        .collect();
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
        found.extend(i1_message(&delivery.msg, state.as_ref(), &granted, &values));
    }
    found
}

fn i1_message(
    msg: &ServerMsg,
    state: Option<&Value>,
    granted: &BTreeSet<(u64, u64)>,
    values: &BTreeMap<u64, Value>,
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
        if !granted.contains(&(claim.0, fence.0)) {
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
        if values.get(&event.seq) != serde_json::to_value(event).ok().as_ref() {
            found.push(format!("event {} differs from the stored one", event.seq));
        }
    }
    found
}

/// The claim and fence a grant or an amend logged.
fn logged_grant(kind: &EventKind) -> Option<(u64, u64)> {
    if let EventKind::ClaimGranted { claim, fence, .. } = kind {
        return Some((claim.0, fence.0));
    }
    if let EventKind::ClaimAmended { claim, fence, .. } = kind {
        return Some((claim.0, fence.0));
    }
    None
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

/// What stripping each field of the state in turn showed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FieldTrial {
    /// The state no longer decodes without the field.
    pub required: Vec<Vec<String>>,
    /// The state decodes without it and the core fills in the value that was stored.
    pub defaulted: Vec<Vec<String>>,
    /// The state decodes without it and the core fills in another value.
    pub differing: Vec<Vec<String>>,
}

/// Strip each field in turn (the top level, `config`, and a claim) and sort the fields by what
/// the core does without them. The `#[serde(default)]` fields are the ones that decode. The trial
/// is costly, so a run does not repeat it: a test pins the three sets, so a field that gains or
/// loses its default, or whose default changes, or a new field, shows as a change to a set.
pub fn field_trial(state: &Value) -> FieldTrial {
    let mut trial = FieldTrial::default();
    for container in containers(state) {
        let Some(object) = at(state, &container).and_then(Value::as_object) else {
            continue;
        };
        for key in object.keys() {
            let mut path = container.clone();
            path.push(key.clone());
            match encode_decode(&without(state, &path)) {
                Err(_) => trial.required.push(path),
                Ok(encoded) if at(&encoded, &path) == at(state, &path) => {
                    trial.defaulted.push(path);
                }
                Ok(_) => trial.differing.push(path),
            }
        }
    }
    trial
}

/// I5: the stored state decodes and encodes back to what is stored.
pub fn i5(store: &Store) -> Vec<String> {
    let state = match stored_state(store) {
        Ok(Some(state)) => state,
        Ok(None) => return Vec::new(),
        Err(e) => return vec![e],
    };
    i5_state(&state)
}

pub fn i5_state(state: &Value) -> Vec<String> {
    match encode_decode(state) {
        Ok(encoded) if encoded == *state => Vec::new(),
        Ok(encoded) => vec![format!(
            "the stored state does not round-trip: {} differ",
            differing_keys(&encoded, state)
        )],
        Err(e) => vec![format!("the stored state {e}")],
    }
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
            if delivery.sock != probe.sock {
                return false;
            }
            if let ServerMsg::Error {
                req: Some(req),
                code,
                ..
            } = &delivery.msg
            {
                return *req == probe.req && refuses_retired_fence(*code);
            }
            false
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

// ---- I2 ----

/// I2: each watcher's events against the stored log from its offset. What it received is always a
/// run of the log in order from the offset, with no gap and no duplicate; unless its socket ended
/// or its sends were failed, it received the whole log from there.
pub fn i2(
    transcript: &[Delivery],
    watches: &[Watch],
    values: &BTreeMap<u64, Value>,
    is_open: &dyn Fn(SocketId) -> bool,
) -> Vec<String> {
    let mut found = Vec::new();
    for watch in watches {
        let received: Vec<&Event> = transcript
            .iter()
            .filter(|delivery| delivery.sock == watch.sock)
            .filter_map(|delivery| {
                if let ServerMsg::Event { event } = &delivery.msg {
                    return Some(event);
                }
                None
            })
            .collect();
        for (index, event) in received.iter().enumerate() {
            let expected = watch.from_seq + index as u64;
            if event.seq != expected {
                found.push(format!(
                    "watcher on socket {} from {} saw seq {} where {} was next",
                    watch.sock.0, watch.from_seq, event.seq, expected
                ));
                break;
            }
            if values.get(&event.seq) != serde_json::to_value(event).ok().as_ref() {
                found.push(format!(
                    "watcher on socket {} saw seq {} differently from the stored event",
                    watch.sock.0, event.seq
                ));
                break;
            }
        }
        let owed = values.range(watch.from_seq..).count();
        if is_open(watch.sock) && !watch.dropped && received.len() < owed {
            found.push(format!(
                "watcher on socket {} from {} received {} of {} events",
                watch.sock.0,
                watch.from_seq,
                received.len(),
                owed
            ));
        }
    }
    found
}

// ---- I4 ----

/// I4: if the core has work due, a wake is pending, later than now and at most `RETRY_MS`
/// after the work (a failed wake retries after that delay; a later wake leaves the work waiting).
///
/// "Exactly one wake" holds by construction: the platform keeps the wake in a single slot that
/// each `SetWake` replaces. The "not later than now" branch cannot fire in a run, because
/// `Sim::quiet` fires every wake at or before now before the checkers look (a host that sets
/// such wakes shows as a stuck harness); only its unit case pins it.
pub fn i4(store: &Store, wake: Option<u64>, now_ms: u64) -> Vec<String> {
    i4_waiting(store, wake, now_ms, Waits::default())
}

/// What the host waits on: a merge call out, a trial out. The core has no work due for what it
/// waits on; the answer resumes it (the wake is suspended meanwhile).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Waits {
    pub merging: bool,
    pub verifying: bool,
    /// A head read or a reconcile is out.
    pub reading: bool,
}

/// I4 for a host that waits on `waits`.
pub fn i4_waiting(store: &Store, wake: Option<u64>, now_ms: u64, waits: Waits) -> Vec<String> {
    let state = match stored_state(store) {
        Ok(Some(state)) => state,
        Ok(None) => return Vec::new(),
        Err(e) => return vec![e],
    };
    let core = match serde_json::from_value::<Coordinator>(state) {
        Ok(core) => core,
        Err(e) => return vec![format!("the stored state does not decode: {e}")],
    };
    let Some(due) = core.next_wake_ms(waits.merging, waits.verifying, now_ms) else {
        return Vec::new();
    };
    if waits.merging || waits.verifying || waits.reading {
        return i4_suspended(wake, now_ms, due);
    }
    match wake {
        None => vec![format!("work is due at {due} and no wake is pending")],
        Some(at) if at <= now_ms => {
            vec![format!(
                "the pending wake {at} is not later than now ({now_ms})"
            )]
        }
        Some(at) if at > due + RETRY_MS => {
            vec![format!(
                "the pending wake {at} is more than {RETRY_MS} ms after the work due at {due}"
            )]
        }
        Some(_) => Vec::new(),
    }
}

/// I4 while a call is out. The wake is suspended at the call: a lease that runs out meanwhile is
/// expired on the next client message or when the answer resumes the wake (R28), not at its time.
/// What the host owes is the watchdog: a wake pending, later than now, at most `MERGE_WATCHDOG_MS`
/// ahead. `P1-ONE-BATCH` changes the suspended wake, and this check with it.
fn i4_suspended(wake: Option<u64>, now_ms: u64, due: u64) -> Vec<String> {
    match wake {
        None => vec![format!(
            "work is due at {due}, a call is out and no wake is pending"
        )],
        Some(at) if at <= now_ms => vec![format!(
            "the pending wake {at} is not later than now ({now_ms})"
        )],
        Some(at) if at > now_ms + MERGE_WATCHDOG_MS => vec![format!(
            "the pending wake {at} is more than the watchdog ({MERGE_WATCHDOG_MS} ms) after now \
             ({now_ms})"
        )],
        Some(_) => Vec::new(),
    }
}

// ---- I8 ----

/// A granted claim, as the log shows it.
struct Live {
    agent: String,
    scopes: Vec<ScopeClaim>,
    race: Option<RaceId>,
}

/// The claims the log shows granted and not ended, by id.
fn live_claims(events: &[Event]) -> BTreeMap<u64, Live> {
    let mut live: BTreeMap<u64, Live> = BTreeMap::new();
    for event in events {
        if let EventKind::ClaimGranted {
            agent,
            claim,
            scopes,
            race,
            ..
        } = &event.kind
        {
            let held = Live {
                agent: agent.0.clone(),
                scopes: scopes.clone(),
                race: *race,
            };
            live.insert(claim.0, held);
        }
        if let EventKind::ClaimAmended { claim, added, .. } = &event.kind {
            if let Some(held) = live.get_mut(&claim.0) {
                held.scopes.extend(added.iter().cloned());
            }
        }
        if let EventKind::ClaimReleased { claim, .. } | EventKind::Merged { claim, .. } =
            &event.kind
        {
            live.remove(&claim.0);
        }
        if let EventKind::RaceOpened { race, scopes, .. } = &event.kind {
            // The race's own locks block everyone else, entrants of the race apart, from the
            // moment it opens. They stand in the table under an id no claim has.
            let held = Live {
                agent: format!("race {}", race.0),
                scopes: scopes.clone(),
                race: Some(*race),
            };
            live.insert(race_lock_id(*race), held);
        }
        if let EventKind::RaceDecided { race, winner, .. } = &event.kind {
            live.remove(&race_lock_id(*race));
            if let Some(held) = winner.and_then(|winner| live.get_mut(&winner.0)) {
                held.race = None;
            }
        }
    }
    live
}

/// The id a race's locks go under: above any claim id.
fn race_lock_id(race: RaceId) -> u64 {
    u64::MAX - race.0
}

/// Whether two claims hold locks that conflict (`Lock::conflicts_with`).
fn locks_conflict(a: &[ScopeClaim], b: &[ScopeClaim]) -> bool {
    for left in a.iter().flat_map(ScopeClaim::locks) {
        for right in b.iter().flat_map(ScopeClaim::locks) {
            if left.0 == right.0 && left.1.conflicts_with(right.1) {
                return true;
            }
        }
    }
    false
}

/// I8: no two granted claims conflict. Entrants of one race share its scopes, and an agent's own
/// claims never block each other.
pub fn i8(events: &[Event]) -> Vec<String> {
    let live = live_claims(events);
    let mut found = Vec::new();
    for (a, left) in &live {
        // An exclusive bound, not `a + 1..`: a claim paired with itself is always skipped below (same
        // race, or its own agent with no race), so that range and `a * 1..` behave alike and the
        // mutant could never be killed.
        for (b, right) in live.range((Bound::Excluded(*a), Bound::Unbounded)) {
            let same_race = left.race.is_some() && left.race == right.race;
            let own = left.agent == right.agent && left.race.is_none() && right.race.is_none();
            if same_race || own {
                continue;
            }
            if locks_conflict(&left.scopes, &right.scopes) {
                found.push(format!(
                    "claims {a} ({}) and {b} ({}) are both granted and conflict",
                    left.agent, right.agent
                ));
            }
        }
    }
    found
}

// ---- I10 ----

/// I10: no dispatch for a submission held for review, no merge of an uncovered submission, no
/// lease extended without a heartbeat.
pub fn i10(
    events: &[Event],
    state: Option<&Value>,
    dispatches: &[Dispatch],
    heartbeats: &[(String, u64)],
) -> Vec<String> {
    let mut found = held_dispatches(events, dispatches);
    found.extend(merged_uncovered(events));
    if let Some(state) = state {
        found.extend(extended_leases(events, state, heartbeats));
    }
    found
}

/// Whether the log up to `events` holds `claim` for review: asked for, and neither approved nor
/// sent back.
fn held_for_review(events: &[Event], claim: ClaimId) -> bool {
    let mut held = false;
    for event in events {
        if let EventKind::ReviewRequested { claim: c, .. } = &event.kind {
            held |= *c == claim;
        }
        if let EventKind::ReviewDecided {
            claim: c, approve, ..
        } = &event.kind
        {
            // A rejection holds the claim until the core sends the work back (`SubmitRejected`).
            if *c == claim && *approve {
                held = false;
            }
        }
        if let EventKind::Submitted { claim: c, .. } | EventKind::SubmitRejected { claim: c, .. } =
            &event.kind
        {
            if *c == claim {
                held = false;
            }
        }
    }
    held
}

fn held_dispatches(events: &[Event], dispatches: &[Dispatch]) -> Vec<String> {
    let mut found = Vec::new();
    for dispatch in dispatches {
        if let Request::Prepare { claim, .. } = &dispatch.request {
            let upto = dispatch.events_len.min(events.len());
            if held_for_review(&events[..upto], *claim) {
                found.push(format!(
                    "claim {} was dispatched while it was held for review",
                    claim.0
                ));
            }
        }
    }
    found
}

/// A `Merged` event for a claim whose latest submission, by the scopes the claim held then, left
/// something uncovered.
fn merged_uncovered(events: &[Event]) -> Vec<String> {
    let mut found = Vec::new();
    let mut claimed: BTreeMap<u64, Vec<ScopeClaim>> = BTreeMap::new();
    let mut uncovered_now: BTreeSet<u64> = BTreeSet::new();
    for event in events {
        if let EventKind::ClaimGranted { claim, scopes, .. } = &event.kind {
            claimed.insert(claim.0, scopes.clone());
        }
        if let EventKind::ClaimAmended { claim, added, .. } = &event.kind {
            claimed
                .entry(claim.0)
                .or_default()
                .extend(added.iter().cloned());
        }
        if let EventKind::Submitted { claim, touched, .. } = &event.kind {
            let held = claimed.get(&claim.0).map_or(&[][..], Vec::as_slice);
            if touched.iter().all(|work| covered(held, work)) {
                uncovered_now.remove(&claim.0);
            } else {
                uncovered_now.insert(claim.0);
            }
        }
        if let EventKind::Merged { claim, .. } = &event.kind {
            if uncovered_now.contains(&claim.0) {
                found.push(format!("claim {} merged with its work uncovered", claim.0));
            }
        }
    }
    found
}

/// A claim's lease may run `lease_ms` past the latest of its grant, its reopening after a
/// rejected submission, and a heartbeat from its agent.
fn extended_leases(events: &[Event], state: &Value, heartbeats: &[(String, u64)]) -> Vec<String> {
    let lease_ms = state
        .pointer("/config/lease_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let Some(claims) = state.get("claims").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (id, claim) in claims {
        if !claim.get("submitted").is_some_and(Value::is_null) {
            continue;
        }
        let (Some(agent), Some(expires)) = (
            claim.get("agent").and_then(Value::as_str),
            claim.get("expires_at_ms").and_then(Value::as_u64),
        ) else {
            continue;
        };
        let renewed = events
            .iter()
            .filter(|event| renews(&event.kind, id))
            .map(|event| event.at_ms)
            .chain(
                heartbeats
                    .iter()
                    .filter(|(who, _)| who == agent)
                    .map(|(_, at)| *at),
            )
            .max()
            .unwrap_or(0);
        if expires > renewed + lease_ms {
            found.push(format!(
                "claim {id} expires at {expires}, more than a lease after its last renewal at {renewed}"
            ));
        }
    }
    found
}

/// Whether `kind` gives claim `id` a fresh lease: its grant, or its reopening after a rejection.
fn renews(kind: &EventKind, id: &str) -> bool {
    if let EventKind::ClaimGranted { claim, .. } | EventKind::SubmitRejected { claim, .. } = kind {
        return claim.0.to_string() == id;
    }
    if let EventKind::RaceDecided {
        winner: Some(claim),
        ..
    } = kind
    {
        return claim.0.to_string() == id;
    }
    false
}

/// The events as JSON by `seq`, as `Store::event_values` has them.
pub fn event_values(events: &[Event]) -> BTreeMap<u64, Value> {
    events
        .iter()
        .filter_map(|event| Some((event.seq, serde_json::to_value(event).ok()?)))
        .collect()
}

/// Whether `work` is covered by some claimed scope that permits its mode (invariant 11), worked
/// out here on paths and a table of its own, so a mistake in `protocol::uncovered` shows as a
/// disagreement (a test compares the two).
pub fn covered(claimed: &[ScopeClaim], work: &ScopeClaim) -> bool {
    claimed
        .iter()
        .any(|held| scope_covers(&held.scope, &work.scope) && mode_permits(held.mode, work.mode))
}

fn path_of(scope: &Scope) -> &str {
    match scope {
        Scope::Dir { path } | Scope::File { path } => path,
        Scope::Symbol(symbol) => &symbol.path,
    }
}

fn scope_covers(held: &Scope, work: &Scope) -> bool {
    if held == work {
        return true;
    }
    let (held_path, work_path) = (path_of(held), path_of(work));
    match held {
        Scope::Dir { .. } => {
            held_path.is_empty() || work_path.starts_with(&format!("{held_path}/"))
        }
        Scope::File { .. } => match work {
            Scope::Symbol(_) => work_path == held_path,
            Scope::Dir { .. } | Scope::File { .. } => false,
        },
        Scope::Symbol(_) => false,
    }
}

fn mode_permits(held: Mode, work: Mode) -> bool {
    if held == work {
        return true;
    }
    match work {
        Mode::Depend => true,
        Mode::EditBody => held == Mode::EditSignature,
        Mode::EditSignature | Mode::Create => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i4_while_a_call_is_out_wants_only_the_watchdog() {
        let now = 1_000;
        let due = 1_500;
        assert!(i4_suspended(Some(now + 1), now, due).is_empty());
        assert!(i4_suspended(Some(now + MERGE_WATCHDOG_MS), now, due).is_empty());
        assert_eq!(i4_suspended(None, now, due).len(), 1);
        assert_eq!(i4_suspended(Some(now), now, due).len(), 1);
        assert_eq!(
            i4_suspended(Some(now + MERGE_WATCHDOG_MS + 1), now, due).len(),
            1
        );
    }
}

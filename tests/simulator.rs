//! The simulator's tests: the real host survives every seed with every checker silent, each seed
//! reproduces, each checker alone sees the bug planted in a host variant for it, and each checker's
//! branches are pinned by cases built by hand from a scripted run.

use serde_json::Value;
use tessel_coordinator::host::call::Request;
use tessel_coordinator::host::{HostConfig, SocketId};
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, Criterion, DecisionRecord, ErrorCode, Event, EventKind,
    Fence, Intent, Mode, OnConflict, RaceId, ReleaseReason, RequestId, RunId, Scope, ScopeClaim,
    ServerMsg,
};
use tessel_coordinator::sim::check::{self, Checker};
use tessel_coordinator::sim::driver::{Delivery, Dispatch, RequestKind, Sent, Sim, Watch};
use tessel_coordinator::sim::store::Store;
use tessel_coordinator::sim::{self, Run, Variant};

/// Seeds searched for a planted bug: far more than the bugs need.
const SEARCH_SEEDS: u64 = 20;

/// The first seed whose run breaks `checker` against `variant` with only `checker` enabled, so
/// no other checker can stop the run first or take the credit.
fn first_caught(variant: Variant, checker: Checker) -> Option<(u64, Run)> {
    (0..SEARCH_SEEDS)
        .map(|seed| (seed, sim::run_seed(seed, variant, &[checker])))
        .find(|(_, run)| run.violations.iter().any(|v| v.checker == checker))
}

/// Seeds that once failed, replayed before every sweep.
const REGRESSIONS: &str = include_str!("simulator-regressions.txt");

#[test]
fn the_regression_list_parses() {
    sim::parse_regressions(REGRESSIONS).unwrap_or_else(|e| panic!("simulator-regressions.txt {e}"));
}

#[test]
fn the_real_host_keeps_every_invariant_on_every_seed_and_each_seed_reproduces() {
    let regressions = sim::parse_regressions(REGRESSIONS)
        .unwrap_or_else(|e| panic!("simulator-regressions.txt {e}"));
    let seeds = sim::seeds().unwrap_or_else(|e| panic!("{e}"));
    let (mut failures, mut crashes) = (Vec::new(), 0);
    for seed in regressions.into_iter().chain(seeds) {
        match sim::check_seed(seed) {
            Ok(run) => crashes += run.crashes,
            Err(failure) => failures.push(failure),
        }
    }
    assert!(
        failures.is_empty(),
        "{} seeds failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        crashes > 0,
        "no seed crashed the host: the faults are not firing"
    );
}

#[test]
fn i1_alone_sees_a_grant_a_crash_erased() {
    assert!(first_caught(Variant::WriteBehind, Checker::I1).is_some());
}

#[test]
fn i9_alone_sees_a_fence_issued_twice_after_a_stale_reload() {
    assert!(first_caught(Variant::StaleLoad, Checker::I9).is_some());
}

#[test]
fn i5_alone_sees_state_the_codec_does_not_round_trip() {
    assert!(first_caught(Variant::DriftingCodec, Checker::I5).is_some());
}

// The order check is pinned by `i2_flags_a_gap_a_duplicate_and_a_wrong_start`; the seed run
// catches this bug through the order check or the tail check, whichever comes first.
#[test]
fn i2_alone_sees_a_watcher_miss_an_event() {
    assert!(first_caught(Variant::SkipsReplayEvent, Checker::I2).is_some());
}

#[test]
fn i4_alone_sees_work_due_with_no_wake_pending() {
    assert!(first_caught(Variant::LostWake, Checker::I4).is_some());
}

#[test]
fn i8_alone_sees_two_granted_claims_that_conflict() {
    assert!(first_caught(Variant::LosesScopesOnLoad, Checker::I8).is_some());
}

#[test]
fn i10_alone_sees_a_lease_extended_without_a_heartbeat() {
    assert!(first_caught(Variant::PhantomHeartbeat, Checker::I10).is_some());
}

// ---- A scripted run to build cases from ----

fn script() -> Sim {
    let config = HostConfig {
        repo: "sim".to_string(),
        run: Some("sim".to_string()),
        ..HostConfig::default()
    };
    let mut sim = Sim::new(config, Variant::Correct);
    let a = sim.connect("agent-a");
    let b = sim.connect("agent-b");
    assert!(sim.quiet());
    let claim = |req: u64, path: &str| ClientMsg::Claim {
        req: RequestId(req),
        intent: Intent {
            summary: "work".to_string(),
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes: vec![file(path, Mode::EditBody)],
        on_conflict: OnConflict::Fail,
    };
    sim.send(a, &claim(1, "src/a.rs"), RequestKind::Claim, None);
    sim.send(b, &claim(2, "src/b.rs"), RequestKind::Claim, None);
    assert!(sim.quiet());
    let amend = ClientMsg::Amend {
        req: RequestId(3),
        claim: ClaimId(1),
        fence: Fence(1),
        add: vec![file("src/c.rs", Mode::Create)],
    };
    sim.send(a, &amend, RequestKind::Amend, Some(ClaimId(1)));
    assert!(sim.quiet());
    sim
}

fn file(path: &str, mode: Mode) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::File { path: path.into() },
        mode,
    }
}

/// Claim 1 is held under fence 3 (fence 1 retired by the amend); claim 2 under fence 2.
fn grants(sim: &Sim) -> Vec<(ClaimId, Fence)> {
    let mut found = Vec::new();
    for delivery in sim.transcript() {
        if let ServerMsg::Granted { claim, fence, .. } = &delivery.msg {
            found.push((*claim, *fence));
        }
    }
    found
}

fn delivery(msg: ServerMsg) -> Delivery {
    Delivery {
        sock: SocketId(0),
        agent: AgentId("agent-a".to_string()),
        msg,
    }
}

fn grant(claim: u64, fence: u64) -> Delivery {
    delivery(ServerMsg::Granted {
        req: RequestId(1),
        claim: ClaimId(claim),
        fence: Fence(fence),
        expires_at_ms: 0,
        race: None,
        at_risk: Vec::new(),
        head: None,
    })
}

fn state_of(sim: &Sim) -> Value {
    serde_json::from_str(sim.store().state().expect("a state is stored")).expect("state is JSON")
}

/// A store holding `entries`, as a crash and reload would find it.
fn store_of(entries: Vec<(String, String)>) -> Store {
    let mut store = Store::default();
    store.stage(tessel_coordinator::host::BatchId(0), entries);
    assert!(store.apply(tessel_coordinator::host::BatchId(0)));
    store
}

fn entries_without(sim: &Sim, key: &str) -> Vec<(String, String)> {
    sim.store()
        .entries()
        .iter()
        .filter(|(k, _)| k.as_str() != key)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn last_event_key(sim: &Sim) -> String {
    sim.store()
        .entries()
        .keys()
        .filter(|k| k.starts_with("ev:"))
        .max()
        .cloned()
        .expect("events are stored")
}

fn open(_: SocketId) -> bool {
    true
}

fn closed(_: SocketId) -> bool {
    false
}

// ---- I1 ----

#[test]
fn i1_is_silent_for_what_the_store_holds() {
    let sim = script();
    assert_eq!(grants(&sim).len(), 3);
    assert_eq!(
        check::i1(sim.transcript(), sim.store()),
        Vec::<String>::new()
    );
}

#[test]
fn i1_flags_a_grant_that_is_not_in_the_stored_log() {
    let sim = script();
    let found = check::i1(&[grant(1, 2)], sim.store());
    assert_eq!(found, ["grant of claim 1 fence 2 is not in the stored log"]);
}

#[test]
fn i1_flags_a_grant_above_the_stored_counters() {
    let sim = script();
    let found = check::i1(&[grant(1, 99)], sim.store());
    assert!(found
        .iter()
        .any(|f| f == "grant of claim 1 fence 99 is not below the stored counters"));
    let found = check::i1(&[grant(99, 1)], sim.store());
    assert!(found
        .iter()
        .any(|f| f.contains("not below the stored counters")));
}

#[test]
fn i1_flags_a_grant_when_nothing_is_stored() {
    let found = check::i1(&[grant(1, 1)], &Store::default());
    assert_eq!(found, ["claim 1 granted but no state is stored"]);
}

#[test]
fn i1_flags_a_lapsed_fence_the_store_still_holds() {
    let sim = script();
    let lapsed = |claim: u64, fence: u64| {
        delivery(ServerMsg::LeaseExpired {
            claim: ClaimId(claim),
            fence: Fence(fence),
        })
    };
    assert!(check::i1(&[lapsed(1, 1)], sim.store()).is_empty());
    assert_eq!(
        check::i1(&[lapsed(1, 3)], sim.store()),
        ["claim 1 was told its fence 3 lapsed but the store still holds it"]
    );
}

#[test]
fn i1_flags_an_event_that_differs_from_the_stored_one() {
    let sim = script();
    let (events, _) = sim.store().events();
    let stored = events[0].clone();
    let same = delivery(ServerMsg::Event {
        event: stored.clone(),
    });
    assert!(check::i1(&[same], sim.store()).is_empty());
    let other = Event {
        kind: EventKind::AgentConnected {
            agent: AgentId("someone-else".to_string()),
        },
        run: RunId("sim".to_string()),
        ..stored
    };
    let found = check::i1(&[delivery(ServerMsg::Event { event: other })], sim.store());
    assert_eq!(found, ["event 0 differs from the stored one"]);
}

#[test]
fn i1_flags_a_gap_in_the_stored_log_and_a_log_shorter_than_the_state() {
    let sim = script();
    let gap = store_of(entries_without(&sim, "ev:00000000000000000001"));
    assert!(check::i1(&[], &gap)
        .iter()
        .any(|f| f == "the stored log has a gap before seq 2"));
    let short = store_of(entries_without(&sim, &last_event_key(&sim)));
    let found = check::i1(&[], &short);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("state next_seq"), "{found:?}");
}

// ---- I5 ----

#[test]
fn i5_is_silent_for_state_the_codec_round_trips() {
    let sim = script();
    assert_eq!(check::i5(sim.store()), Vec::<String>::new());
}

#[test]
fn i5_flags_a_field_the_codec_drops() {
    let sim = script();
    let mut drifted = state_of(&sim);
    drifted["writer_note"] = Value::from(1);
    let found = check::i5_state(&drifted);
    assert!(
        found[0].starts_with("the stored state does not round-trip: writer_note differ"),
        "{found:?}"
    );
}

#[test]
fn i5_flags_state_that_does_not_decode() {
    let sim = script();
    let mut broken = state_of(&sim);
    broken["claims"]["1"]["fence"] = Value::from("one");
    let found = check::i5_state(&broken);
    assert!(found[0].contains("does not decode"), "{found:?}");
}

/// The fields of the state sorted by what the core does without them (the top level, `config`
/// and a claim). Each new field must be placed here, which makes its author decide whether it
/// needs a default; a default that changes moves its field between the last two lists.
const REQUIRED_FIELDS: &[&str] = &[
    "claims",
    "claims.1.agent",
    "claims.1.expires_at_ms",
    "claims.1.fence",
    "claims.1.intent",
    "claims.1.scopes",
    "clock_ms",
    "config",
    "config.lease_ms",
    "config.run",
    "next_claim",
    "next_fence",
    "next_seq",
    "waiting",
];
const DEFAULTED_FIELDS: &[&str] = &[
    "claims.1.denial",
    "claims.1.kind",
    "claims.1.submitted",
    "claims.1.work",
    "config.shadow_enabled",
    "head_pokes",
    "head_start_failures",
    "head_start_retry_ms",
    "head_synced",
    "head_tried",
    "last_dispatch",
    "merge_in_flight",
    "merges_past_verification",
    "next_race",
    "next_submission",
    "next_verification",
    "races",
    "reviewers",
    "sensitive",
    "verification_in_flight",
    "verifications",
];
const DIFFERING_FIELDS: &[&str] = &["head", "head_from_hello"];

fn names(paths: &[Vec<String>]) -> Vec<String> {
    let mut names: Vec<String> = paths.iter().map(|path| path.join(".")).collect();
    names.sort();
    names
}

#[test]
fn i5_pins_which_fields_the_state_requires_defaults_or_changes() {
    let sim = script();
    let trial = check::field_trial(&state_of(&sim));
    assert_eq!(names(&trial.required), REQUIRED_FIELDS, "required");
    assert_eq!(names(&trial.defaulted), DEFAULTED_FIELDS, "defaulted");
    assert_eq!(names(&trial.differing), DIFFERING_FIELDS, "differing");
}

// ---- I9 ----

fn sent(req: u64, kind: RequestKind, claim: Option<u64>) -> Sent {
    Sent {
        sock: SocketId(0),
        req: RequestId(req),
        kind,
        claim: claim.map(ClaimId),
        releases_before: 0,
        accepted: false,
    }
}

fn error(req: u64, code: ErrorCode) -> Delivery {
    delivery(ServerMsg::Error {
        req: Some(RequestId(req)),
        code,
        message: String::new(),
    })
}

#[test]
fn i9_flags_a_fence_granted_twice_and_a_claim_created_twice() {
    let sent = [
        sent(1, RequestKind::Claim, None),
        sent(2, RequestKind::Claim, None),
    ];
    let twice = [grant(1, 1), grant(2, 1)];
    assert_eq!(
        check::i9(&twice, &sent, &open),
        ["fence 1 was granted twice"]
    );
    let reply = |req: u64, claim: u64, fence: u64| {
        let mut d = grant(claim, fence);
        if let ServerMsg::Granted { req: r, .. } = &mut d.msg {
            *r = RequestId(req);
        }
        d
    };
    let created = [reply(1, 1, 1), reply(2, 1, 2)];
    assert_eq!(
        check::i9(&created, &sent, &open),
        ["claim 1 was created twice"]
    );
}

#[test]
fn i9_allows_an_amend_to_regrant_a_claim_with_a_higher_fence_only() {
    let sent = [
        sent(1, RequestKind::Claim, None),
        sent(2, RequestKind::Amend, Some(1)),
    ];
    let reply = |req: u64, fence: u64| {
        let mut d = grant(1, fence);
        if let ServerMsg::Granted { req: r, .. } = &mut d.msg {
            *r = RequestId(req);
        }
        d
    };
    assert!(check::i9(&[reply(1, 1), reply(2, 2)], &sent, &open).is_empty());
    assert_eq!(
        check::i9(&[reply(1, 5), reply(2, 3)], &sent, &open),
        ["claim 1 got fence 3 after a higher one"]
    );
}

fn event(seq: u64, agent: &str) -> ServerMsg {
    ServerMsg::Event {
        event: Event {
            seq,
            at_ms: 0,
            run: RunId("r".to_string()),
            kind: EventKind::AgentConnected {
                agent: AgentId(agent.to_string()),
            },
        },
    }
}

#[test]
fn i9_flags_a_seq_seen_twice_or_issued_for_two_events() {
    let on = |sock: u64, msg: ServerMsg| Delivery {
        sock: SocketId(sock),
        ..delivery(msg)
    };
    let twice = [on(0, event(3, "a")), on(0, event(3, "a"))];
    assert_eq!(check::i9(&twice, &[], &open), ["socket 0 saw seq 3 twice"]);
    let two = [on(0, event(3, "a")), on(1, event(3, "b"))];
    assert_eq!(
        check::i9(&two, &[], &open),
        ["seq 3 was issued for two different events"]
    );
    let shared = [on(0, event(3, "a")), on(1, event(3, "a"))];
    assert!(check::i9(&shared, &[], &open).is_empty());
}

#[test]
fn i9_counts_only_a_stale_fence_or_unknown_claim_error_as_a_refusal() {
    let probe = [sent(7, RequestKind::StaleRelease, Some(1))];
    let missing = ["retired fence not refused (request 7)"];
    for code in [ErrorCode::StaleFence, ErrorCode::UnknownClaim] {
        assert!(check::i9(&[error(7, code)], &probe, &open).is_empty());
    }
    for code in [
        ErrorCode::NoHello,
        ErrorCode::NotOwner,
        ErrorCode::Malformed,
    ] {
        assert_eq!(check::i9(&[error(7, code)], &probe, &open), missing);
    }
    assert_eq!(check::i9(&[], &probe, &open), missing);
    assert!(
        check::i9(&[], &probe, &closed).is_empty(),
        "a socket that ended may have lost the answer"
    );
}

#[test]
fn i9_sees_a_retired_fence_that_the_log_shows_was_accepted() {
    let mut sim = script();
    let a = sim
        .view("agent-a")
        .and_then(|v| v.sock)
        .expect("a is connected");
    // Fence 1 was retired by the amend: the host must refuse it and release nothing.
    let stale = |req: u64, fence: u64| ClientMsg::Release {
        claim: ClaimId(1),
        fence: Fence(fence),
        req: Some(RequestId(req)),
    };
    sim.send(
        a,
        &stale(10, 1),
        RequestKind::StaleRelease,
        Some(ClaimId(1)),
    );
    assert!(sim.quiet());
    let verdict = |sim: &Sim| check::i9(sim.transcript(), sim.sent(), &|s| sim.is_open(s));
    assert_eq!(verdict(&sim), Vec::<String>::new());
    // The current fence stands for a host that ignored the fence: the release goes through.
    sim.send(
        a,
        &stale(11, 3),
        RequestKind::StaleRelease,
        Some(ClaimId(1)),
    );
    assert!(sim.quiet());
    assert_eq!(verdict(&sim), ["retired fence accepted (request 11)"]);
}

// ---- The driver ----

#[test]
fn a_frame_on_a_socket_a_crash_closed_is_dropped() {
    let mut sim = script();
    let a = sim
        .view("agent-a")
        .and_then(|v| v.sock)
        .expect("a is connected");
    sim.crash_after_outputs(1);
    sim.send(a, &ClientMsg::Heartbeat, RequestKind::Other, None);
    assert!(sim.quiet());
    assert!(!sim.is_open(a));
    let before = sim.log().len();
    sim.send(a, &ClientMsg::Heartbeat, RequestKind::Other, None);
    assert_eq!(sim.log().len(), before, "nothing reaches a dead socket");
}

// ---- Events built by hand ----

fn at(seq: u64, at_ms: u64, kind: EventKind) -> Event {
    Event {
        seq,
        at_ms,
        run: RunId("sim".to_string()),
        kind,
    }
}

fn intent() -> Intent {
    Intent {
        summary: "work".to_string(),
        task_ref: None,
        assumptions: Vec::new(),
    }
}

fn granted_event(
    seq: u64,
    agent: &str,
    claim: u64,
    scopes: Vec<ScopeClaim>,
    race: Option<u64>,
) -> Event {
    let kind = EventKind::ClaimGranted {
        agent: AgentId(agent.to_string()),
        claim: ClaimId(claim),
        fence: Fence(claim),
        scopes,
        intent: intent(),
        race: race.map(RaceId),
        at_risk: Vec::new(),
    };
    at(seq, 100, kind)
}

fn released(seq: u64, claim: u64) -> Event {
    let kind = EventKind::ClaimReleased {
        claim: ClaimId(claim),
        reason: ReleaseReason::Agent,
    };
    at(seq, 200, kind)
}

fn connected(seq: u64, agent: &str) -> Event {
    let kind = EventKind::AgentConnected {
        agent: AgentId(agent.to_string()),
    };
    at(seq, 0, kind)
}

// ---- I2 ----

fn sees(sock: u64, events: &[Event]) -> Vec<Delivery> {
    events
        .iter()
        .map(|event| Delivery {
            sock: SocketId(sock),
            agent: AgentId("w".to_string()),
            msg: ServerMsg::Event {
                event: event.clone(),
            },
        })
        .collect()
}

fn watch(sock: u64, from_seq: u64, dropped: bool) -> Watch {
    Watch {
        sock: SocketId(sock),
        from_seq,
        dropped,
    }
}

#[test]
fn i2_is_silent_for_the_log_from_the_offset_and_for_a_watcher_past_its_end() {
    let log: Vec<Event> = (0..4).map(|seq| connected(seq, "a")).collect();
    assert!(check::i2(
        &sees(0, &log),
        &[watch(0, 0, false)],
        &check::event_values(&log),
        &open
    )
    .is_empty());
    assert!(check::i2(
        &sees(0, &log[2..]),
        &[watch(0, 2, false)],
        &check::event_values(&log),
        &open
    )
    .is_empty());
    assert!(check::i2(
        &[],
        &[watch(0, 9, false)],
        &check::event_values(&log),
        &open
    )
    .is_empty());
}

#[test]
fn i2_flags_a_gap_a_duplicate_and_a_wrong_start() {
    let log: Vec<Event> = (0..4).map(|seq| connected(seq, "a")).collect();
    let gap = [log[0].clone(), log[2].clone(), log[3].clone()];
    let found = check::i2(
        &sees(0, &gap),
        &[watch(0, 0, true)],
        &check::event_values(&log),
        &open,
    );
    assert_eq!(
        found,
        ["watcher on socket 0 from 0 saw seq 2 where 1 was next"]
    );
    let twice = [log[0].clone(), log[0].clone()];
    let found = check::i2(
        &sees(0, &twice),
        &[watch(0, 0, true)],
        &check::event_values(&log),
        &open,
    );
    assert_eq!(
        found,
        ["watcher on socket 0 from 0 saw seq 0 where 1 was next"]
    );
    let found = check::i2(
        &sees(0, &log[1..]),
        &[watch(0, 0, true)],
        &check::event_values(&log),
        &open,
    );
    assert_eq!(
        found,
        ["watcher on socket 0 from 0 saw seq 1 where 0 was next"]
    );
}

#[test]
fn i2_flags_an_event_that_differs_from_the_stored_one() {
    let log: Vec<Event> = (0..2).map(|seq| connected(seq, "a")).collect();
    let altered = [log[0].clone(), connected(1, "b")];
    let found = check::i2(
        &sees(0, &altered),
        &[watch(0, 0, false)],
        &check::event_values(&log),
        &open,
    );
    assert_eq!(
        found,
        ["watcher on socket 0 saw seq 1 differently from the stored event"]
    );
}

#[test]
fn i2_flags_a_missing_tail_unless_the_socket_ended_or_was_dropped() {
    let log: Vec<Event> = (0..4).map(|seq| connected(seq, "a")).collect();
    let short = sees(0, &log[..2]);
    let found = check::i2(
        &short,
        &[watch(0, 0, false)],
        &check::event_values(&log),
        &open,
    );
    assert_eq!(found, ["watcher on socket 0 from 0 received 2 of 4 events"]);
    assert!(check::i2(
        &short,
        &[watch(0, 0, false)],
        &check::event_values(&log),
        &closed
    )
    .is_empty());
    assert!(check::i2(
        &short,
        &[watch(0, 0, true)],
        &check::event_values(&log),
        &open
    )
    .is_empty());
}

// ---- I4 ----

#[test]
fn i4_wants_a_wake_later_than_now_and_not_long_after_the_work() {
    let sim = script();
    let now = sim.now_ms();
    assert!(sim.wake().is_some_and(|at| at > now), "the host set a wake");
    assert!(check::i4(sim.store(), sim.wake(), now).is_empty());
    let found = check::i4(sim.store(), None, now);
    assert!(found[0].ends_with("and no wake is pending"), "{found:?}");
    let found = check::i4(sim.store(), Some(now), now);
    assert_eq!(
        found,
        [format!(
            "the pending wake {now} is not later than now ({now})"
        )]
    );
    // The leases fall due at now + 30 000; a failed wake may put the next one 2 000 later.
    let due = now + 30_000;
    assert!(check::i4(sim.store(), Some(due + 2_000), now).is_empty());
    assert!(check::i4(sim.store(), Some(due - 5_000), now).is_empty());
    let late = check::i4(sim.store(), Some(due + 2_001), now);
    assert_eq!(late.len(), 1, "{late:?}");
    assert!(late[0].contains("after the work due at"), "{late:?}");
    let hour = check::i4(sim.store(), Some(due + 3_600_000), now);
    assert_eq!(hour.len(), 1, "{hour:?}");
}

#[test]
fn i4_asks_nothing_when_no_work_is_due() {
    assert!(check::i4(&Store::default(), None, 5).is_empty());
}

// ---- I8 ----

fn claim_on(path: &str, mode: Mode) -> Vec<ScopeClaim> {
    vec![file(path, mode)]
}

#[test]
fn i8_flags_two_granted_claims_of_two_agents_that_conflict() {
    let log = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        granted_event(1, "b", 2, claim_on("src/a.rs", Mode::EditBody), None),
    ];
    assert_eq!(
        check::i8(&log),
        ["claims 1 (a) and 2 (b) are both granted and conflict"]
    );
    let dir = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        granted_event(
            1,
            "b",
            2,
            vec![ScopeClaim {
                scope: Scope::Dir { path: "src".into() },
                mode: Mode::EditSignature,
            }],
            None,
        ),
    ];
    assert_eq!(
        check::i8(&dir).len(),
        1,
        "an ancestor's intention lock conflicts"
    );
}

#[test]
fn i8_allows_what_does_not_conflict() {
    let depend = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::Depend), None),
        granted_event(1, "b", 2, claim_on("src/a.rs", Mode::EditBody), None),
    ];
    assert!(check::i8(&depend).is_empty(), "Depend and EditBody coexist");
    let own = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        granted_event(1, "a", 2, claim_on("src/a.rs", Mode::EditBody), None),
    ];
    assert!(
        check::i8(&own).is_empty(),
        "an agent's own claims do not block each other"
    );
    let apart = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        granted_event(1, "b", 2, claim_on("src/b.rs", Mode::EditBody), None),
    ];
    assert!(check::i8(&apart).is_empty());
    let entrants = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), Some(1)),
        granted_event(1, "b", 2, claim_on("src/a.rs", Mode::EditBody), Some(1)),
    ];
    assert!(
        check::i8(&entrants).is_empty(),
        "entrants of one race share its scopes"
    );
}

#[test]
fn i8_forgets_a_claim_once_it_ended_and_sees_a_conflict_an_amend_makes() {
    let ended = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        released(1, 1),
        granted_event(2, "b", 2, claim_on("src/a.rs", Mode::EditBody), None),
    ];
    assert!(check::i8(&ended).is_empty());
    let merged = at(
        1,
        200,
        EventKind::Merged {
            claim: ClaimId(1),
            head: CommitId("h".into()),
            base: None,
            unverified: Vec::new(),
            untried_shadows: Vec::new(),
        },
    );
    let after_merge = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        merged,
        granted_event(2, "b", 2, claim_on("src/a.rs", Mode::EditBody), None),
    ];
    assert!(check::i8(&after_merge).is_empty());
    let amended = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        granted_event(1, "b", 2, claim_on("src/b.rs", Mode::EditBody), None),
        at(
            2,
            200,
            EventKind::ClaimAmended {
                claim: ClaimId(2),
                fence: Fence(3),
                added: claim_on("src/a.rs", Mode::Create),
            },
        ),
    ];
    assert_eq!(check::i8(&amended).len(), 1);
}

#[test]
fn i8_treats_a_race_winner_as_an_ordinary_claim() {
    let decided = at(
        2,
        200,
        EventKind::RaceDecided {
            race: RaceId(1),
            winner: Some(ClaimId(1)),
            ranking: vec![ClaimId(1)],
        },
    );
    let log = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), Some(1)),
        granted_event(1, "a", 2, claim_on("src/a.rs", Mode::EditBody), None),
        decided,
    ];
    assert!(
        check::i8(&log[..2]).len() == 1,
        "an entrant's race lock blocks even its agent"
    );
    assert!(check::i8(&log).is_empty(), "the winner's own claims do not");
}

// ---- I10 ----

fn prepare(claim: u64, events_len: usize) -> Dispatch {
    Dispatch {
        request: Request::Prepare {
            claim: ClaimId(claim),
            agent: AgentId("a".into()),
            fork_commit: CommitId("f".into()),
            scopes: Vec::new(),
        },
        events_len,
    }
}

fn review_requested(seq: u64, claim: u64) -> Event {
    let kind = EventKind::ReviewRequested {
        claim: ClaimId(claim),
        reasons: Vec::new(),
    };
    at(seq, 150, kind)
}

fn review_decided(seq: u64, claim: u64, approve: bool) -> Event {
    let kind = EventKind::ReviewDecided {
        claim: ClaimId(claim),
        approve,
        note: None,
        reviewer: None,
    };
    at(seq, 160, kind)
}

fn submitted(seq: u64, claim: u64, touched: Vec<ScopeClaim>) -> Event {
    let kind = EventKind::Submitted {
        claim: ClaimId(claim),
        fork_commit: CommitId("f".into()),
        touched,
        decisions: DecisionRecord::default(),
    };
    at(seq, 140, kind)
}

fn merged(seq: u64, claim: u64) -> Event {
    let kind = EventKind::Merged {
        claim: ClaimId(claim),
        head: CommitId("h".into()),
        base: None,
        unverified: Vec::new(),
        untried_shadows: Vec::new(),
    };
    at(seq, 300, kind)
}

#[test]
fn i10_flags_a_dispatch_for_a_submission_held_for_review() {
    let log = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        submitted(1, 1, claim_on("src/a.rs", Mode::EditBody)),
        review_requested(2, 1),
        review_decided(3, 1, true),
    ];
    let hold = |events_len: usize| check::i10(&log, None, &[prepare(1, events_len)], &[]);
    assert_eq!(
        hold(3),
        ["claim 1 was dispatched while it was held for review"]
    );
    assert!(hold(2).is_empty(), "not yet asked for review");
    assert!(hold(4).is_empty(), "approved");
    let other = check::i10(&log, None, &[prepare(2, 3)], &[]);
    assert!(other.is_empty(), "another claim is not held");
}

#[test]
fn i10_flags_a_merge_of_work_the_claim_did_not_cover() {
    let uncovered = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        submitted(1, 1, claim_on("src/b.rs", Mode::EditBody)),
        merged(2, 1),
    ];
    assert_eq!(
        check::i10(&uncovered, None, &[], &[]),
        ["claim 1 merged with its work uncovered"]
    );
    let covered = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        submitted(1, 1, claim_on("src/a.rs", Mode::EditBody)),
        merged(2, 1),
    ];
    assert!(check::i10(&covered, None, &[], &[]).is_empty());
    let resubmitted = [
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        submitted(1, 1, claim_on("src/b.rs", Mode::EditBody)),
        submitted(2, 1, claim_on("src/a.rs", Mode::EditBody)),
        merged(3, 1),
    ];
    assert!(
        check::i10(&resubmitted, None, &[], &[]).is_empty(),
        "the latest submission counts"
    );
}

#[test]
fn i10_flags_a_lease_longer_than_a_lease_past_its_last_renewal() {
    let sim = script();
    let (events, _) = sim.store().events();
    let state = state_of(&sim);
    assert!(check::i10(&events, Some(&state), &[], &[]).is_empty());
    let mut longer = state.clone();
    let expires = longer["claims"]["1"]["expires_at_ms"]
        .as_u64()
        .expect("a lease");
    longer["claims"]["1"]["expires_at_ms"] = Value::from(expires + 1);
    let found = check::i10(&events, Some(&longer), &[], &[]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("claim 1 expires at"), "{found:?}");
    let beat = [("agent-a".to_string(), sim.now_ms() + 1)];
    assert!(
        check::i10(&events, Some(&longer), &[], &beat).is_empty(),
        "a heartbeat renews"
    );
    let other = [("agent-b".to_string(), sim.now_ms() + 1)];
    assert_eq!(
        check::i10(&events, Some(&longer), &[], &other).len(),
        1,
        "another agent's does not"
    );
}

// ---- The driver and the traffic ----

fn seqs_received(sim: &Sim, sock: SocketId) -> Vec<u64> {
    let mut seqs = Vec::new();
    for delivery in sim.transcript().iter().filter(|d| d.sock == sock) {
        if let ServerMsg::Event { event } = &delivery.msg {
            seqs.push(event.seq);
        }
    }
    seqs
}

#[test]
fn a_watcher_gets_the_log_from_its_offset_and_what_follows_it() {
    let mut sim = script();
    let stored = sim.stored_events() as u64;
    let sock = sim.watch("watcher-a", 2, true);
    assert!(sim.quiet());
    let seqs = seqs_received(&sim, sock);
    assert_eq!(seqs.first(), Some(&2));
    assert!(seqs.windows(2).all(|pair| pair[1] == pair[0] + 1));
    assert!(
        seqs.last().is_some_and(|last| *last >= stored),
        "its own hello is in the log too"
    );
    let found = check::i2(
        sim.transcript(),
        sim.watches(),
        &sim.store().event_values(),
        &|s| sim.is_open(s),
    );
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_socket_whose_sends_fail_receives_nothing_and_owes_nothing() {
    let mut sim = script();
    let sock = sim.watch("watcher-b", 0, false);
    sim.drop_sends(sock);
    assert!(sim.quiet());
    assert!(seqs_received(&sim, sock).is_empty());
    assert!(sim.watches().iter().any(|w| w.sock == sock && w.dropped));
}

#[test]
fn the_traffic_reaches_what_the_checkers_need() {
    let wanted = [
        "Merged {",
        "ReviewRequired",
        "Uncovered",
        "RaceResult",
        "SubmitRejected",
        "Accepted",
        "Prepare {",
        "SendFailed",
        "crash",
        "type\\\":\\\"watch",
        "type\\\":\\\"review",
        "type\\\":\\\"join_race",
        "type\\\":\\\"open_race",
        "type\\\":\\\"heartbeat",
        "type\\\":\\\"submit",
    ];
    let mut seen = vec![0_u32; wanted.len()];
    for seed in 0..12 {
        for line in &sim::run_seed(seed, Variant::Correct, &[]).transcript {
            for (index, needle) in wanted.iter().enumerate() {
                seen[index] += u32::from(line.contains(needle));
            }
        }
    }
    for (needle, count) in wanted.iter().zip(&seen) {
        assert!(*count > 0, "twelve seeds never produced {needle}");
    }
}

// ---- Review of the second half ----

#[test]
fn i8_sees_the_locks_of_a_race_that_has_no_entrant_yet() {
    let opened = at(
        0,
        50,
        EventKind::RaceOpened {
            race: RaceId(1),
            scopes: claim_on("src/a.rs", Mode::EditBody),
            criteria: vec![Criterion::FirstSubmitted],
        },
    );
    let outsider = granted_event(1, "b", 2, claim_on("src/a.rs", Mode::EditBody), None);
    let found = check::i8(&[opened.clone(), outsider.clone()]);
    assert_eq!(
        found,
        ["claims 2 (b) and 18446744073709551614 (race 1) are both granted and conflict"]
    );
    let entrant = granted_event(1, "b", 2, claim_on("src/a.rs", Mode::EditBody), Some(1));
    assert!(
        check::i8(&[opened.clone(), entrant]).is_empty(),
        "an entrant shares the race's locks"
    );
    let decided = at(
        2,
        60,
        EventKind::RaceDecided {
            race: RaceId(1),
            winner: None,
            ranking: Vec::new(),
        },
    );
    assert!(
        check::i8(&[opened, decided, outsider]).is_empty(),
        "a decided race holds nothing"
    );
}

#[test]
fn i10_keeps_a_rejected_submission_held_until_it_is_sent_back() {
    let rejected = at(
        4,
        170,
        EventKind::SubmitRejected {
            claim: ClaimId(1),
            reason: "no".into(),
        },
    );
    let mut log = vec![
        granted_event(0, "a", 1, claim_on("src/a.rs", Mode::EditBody), None),
        submitted(1, 1, claim_on("src/a.rs", Mode::EditBody)),
        review_requested(2, 1),
        review_decided(3, 1, false),
    ];
    let found = check::i10(&log, None, &[prepare(1, 4)], &[]);
    assert_eq!(
        found,
        ["claim 1 was dispatched while it was held for review"]
    );
    log.push(rejected);
    assert!(check::i10(&log, None, &[prepare(1, 5)], &[]).is_empty());
}

#[test]
fn i10_decides_coverage_the_way_the_core_does() {
    let paths = ["", "src", "src/a", "src/a.rs", "lib", "lib/d.rs"];
    let modes = [
        Mode::Depend,
        Mode::EditBody,
        Mode::EditSignature,
        Mode::Create,
    ];
    let mut scopes = Vec::new();
    for path in paths {
        let dir = Scope::Dir { path: path.into() };
        scopes.push(dir);
        if !path.is_empty() {
            scopes.push(Scope::File { path: path.into() });
            scopes.push(Scope::Symbol(tessel_coordinator::protocol::SymbolId {
                path: path.into(),
                qualified_name: "f".into(),
            }));
        }
    }
    let mut cases = 0;
    for held in &scopes {
        for held_mode in modes {
            let claimed = [ScopeClaim {
                scope: held.clone(),
                mode: held_mode,
            }];
            for work in &scopes {
                for work_mode in modes {
                    let touched = ScopeClaim {
                        scope: work.clone(),
                        mode: work_mode,
                    };
                    let core = tessel_coordinator::protocol::uncovered(
                        &claimed,
                        std::slice::from_ref(&touched),
                    )
                    .is_empty();
                    assert_eq!(
                        check::covered(&claimed, &touched),
                        core,
                        "{claimed:?} against {touched:?}"
                    );
                    cases += 1;
                }
            }
        }
    }
    assert!(cases > 4_000);
}

#[test]
fn a_socket_cut_off_mid_replay_keeps_the_prefix_it_got() {
    let mut sim = script();
    let sock = sim.watch("watcher-a", 0, false);
    sim.drop_sends_after(sock, 2);
    assert!(sim.quiet());
    assert_eq!(seqs_received(&sim, sock), [0, 1]);
    assert!(sim.watches().iter().any(|w| w.sock == sock && w.dropped));
    let found = check::i2(
        sim.transcript(),
        sim.watches(),
        &sim.store().event_values(),
        &|s| sim.is_open(s),
    );
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn some_default_seeds_cut_a_replay_off_part_way() {
    let cut: usize = (0..sim::DEFAULT_SEEDS)
        .map(|seed| sim::run_seed(seed, Variant::Correct, &[]).cut_off_replays)
        .sum();
    assert!(cut > 0, "no watcher was cut off after receiving events");
}

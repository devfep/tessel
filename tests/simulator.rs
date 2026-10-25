//! The simulator's tests: the real host survives every seed with every checker silent, each seed
//! reproduces, each checker alone sees the bug planted in a host variant for it, and each checker's
//! branches are pinned by cases built by hand from a scripted run.

use serde_json::Value;
use tessel_coordinator::host::{HostConfig, SocketId};
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, ErrorCode, Event, EventKind, Fence, Intent, Mode, OnConflict,
    RequestId, RunId, Scope, ScopeClaim, ServerMsg,
};
use tessel_coordinator::sim::check::{self, Checker};
use tessel_coordinator::sim::driver::{Delivery, RequestKind, Sent, Sim};
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

#[test]
fn the_real_host_keeps_every_invariant_on_every_seed_and_each_seed_reproduces() {
    let seeds = sim::seeds().unwrap_or_else(|e| panic!("{e}"));
    let mut crashes = 0;
    for seed in seeds {
        let first = sim::run_seed(seed, Variant::Correct, &Checker::ALL);
        let second = sim::run_seed(seed, Variant::Correct, &[]);
        assert!(
            first.violations.is_empty(),
            "seed {seed} broke {:?}\n{}",
            first.violations,
            sim::rerun_hint(seed)
        );
        if let Some(difference) = sim::first_difference(&first.transcript, &second.transcript) {
            panic!(
                "seed {seed} is not deterministic: {difference}\n{}",
                sim::rerun_hint(seed)
            );
        }
        assert_eq!(first.steps, sim::STEPS, "seed {seed} stopped early");
        crashes += first.crashes;
    }
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

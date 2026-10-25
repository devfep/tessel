//! The simulator's tests: the real host survives every seed with every checker silent, each seed
//! reproduces, and each checker sees the bug planted in a host variant for it.

use tessel_coordinator::host::SocketId;
use tessel_coordinator::protocol::AgentId;
use tessel_coordinator::protocol::{ClaimId, Event, EventKind, Fence, RequestId, RunId, ServerMsg};
use tessel_coordinator::sim::check::{self, Checker};
use tessel_coordinator::sim::driver::{Delivery, RequestKind, Sent};
use tessel_coordinator::sim::{self, Run, Variant};

/// Seeds searched for a planted bug: far more than the bugs need.
const SEARCH_SEEDS: u64 = 20;

fn granted(sock: u64, req: u64, claim: u64, fence: u64) -> Delivery {
    Delivery {
        sock: SocketId(sock),
        agent: AgentId("a".to_string()),
        msg: ServerMsg::Granted {
            req: RequestId(req),
            claim: ClaimId(claim),
            fence: Fence(fence),
            expires_at_ms: 0,
            race: None,
            at_risk: Vec::new(),
            head: None,
        },
    }
}

fn open(_: SocketId) -> bool {
    true
}

fn closed(_: SocketId) -> bool {
    false
}

fn sent(req: u64, kind: RequestKind) -> Sent {
    Sent {
        sock: SocketId(0),
        req: RequestId(req),
        kind,
        claim: None,
    }
}

/// The first seed whose run breaks `checker` against `variant`.
fn first_caught(variant: Variant, checker: Checker, enabled: &[Checker]) -> Option<(u64, Run)> {
    (0..SEARCH_SEEDS)
        .map(|seed| (seed, sim::run_seed(seed, variant, enabled)))
        .find(|(_, run)| run.violations.iter().any(|v| v.checker == checker))
}

#[test]
fn the_real_host_keeps_every_invariant_on_every_seed_and_each_seed_reproduces() {
    let seeds = sim::seeds().unwrap_or_else(|e| panic!("{e}"));
    let mut crashes = 0;
    for seed in seeds {
        let first = sim::run_seed(seed, Variant::Correct, &Checker::ALL);
        let second = sim::run_seed(seed, Variant::Correct, &Checker::ALL);
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
fn i1_sees_a_grant_a_crash_erased() {
    let caught = first_caught(Variant::WriteBehind, Checker::I1, &Checker::ALL);
    assert!(
        caught.is_some(),
        "no seed in 0..{SEARCH_SEEDS} caught the write-behind commit"
    );
}

#[test]
fn i9_sees_a_fence_issued_twice_after_a_stale_reload() {
    let caught = first_caught(Variant::StaleLoad, Checker::I9, &Checker::ALL);
    assert!(
        caught.is_some(),
        "no seed in 0..{SEARCH_SEEDS} caught the stale load"
    );
}

#[test]
fn i5_sees_state_the_codec_does_not_round_trip() {
    let caught = first_caught(Variant::DriftingCodec, Checker::I5, &Checker::ALL);
    assert!(
        caught.is_some(),
        "no seed in 0..{SEARCH_SEEDS} caught the drifting codec"
    );
}

#[test]
fn each_planted_bug_goes_unseen_without_the_checker_made_for_it() {
    for (variant, checker) in [
        (Variant::WriteBehind, Checker::I1),
        (Variant::StaleLoad, Checker::I9),
        (Variant::DriftingCodec, Checker::I5),
    ] {
        let Some((seed, _)) = first_caught(variant, checker, &Checker::ALL) else {
            panic!("no seed in 0..{SEARCH_SEEDS} caught {variant:?}");
        };
        let others: Vec<Checker> = Checker::ALL.into_iter().filter(|c| *c != checker).collect();
        let run = sim::run_seed(seed, variant, &others);
        let blamed: Vec<_> = run.violations.iter().map(|v| v.checker).collect();
        assert!(
            !blamed.contains(&checker),
            "{variant:?} seed {seed}: {checker:?} was disabled"
        );
    }
}

#[test]
fn i9_flags_a_fence_granted_twice_and_a_claim_created_twice() {
    let transcript = [granted(0, 1, 1, 1), granted(1, 2, 2, 1)];
    let sent = [sent(1, RequestKind::Claim), sent(2, RequestKind::Claim)];
    let found = check::i9(&transcript, &sent, &open);
    assert_eq!(found, ["fence 1 was granted twice"]);

    let transcript = [granted(0, 1, 1, 1), granted(1, 2, 1, 2)];
    let found = check::i9(&transcript, &sent, &open);
    assert_eq!(found, ["claim 1 was created twice"]);
}

#[test]
fn i9_allows_an_amend_to_regrant_a_claim_with_a_higher_fence_only() {
    let sent = [sent(1, RequestKind::Claim), sent(2, RequestKind::Amend)];
    let transcript = [granted(0, 1, 1, 1), granted(0, 2, 1, 2)];
    assert!(check::i9(&transcript, &sent, &open).is_empty());
    let transcript = [granted(0, 1, 1, 5), granted(0, 2, 1, 3)];
    assert_eq!(
        check::i9(&transcript, &sent, &open),
        ["claim 1 got fence 3 after a higher one"]
    );
}

#[test]
fn i9_flags_a_retired_fence_that_is_accepted_or_ignored() {
    let probe = [sent(7, RequestKind::StaleRelease)];
    let accepted = [granted(0, 7, 1, 9)];
    assert_eq!(
        check::i9(&accepted, &probe, &open),
        [
            "retired fence accepted (request 7)",
            "retired fence not refused (request 7)"
        ]
    );
    assert_eq!(
        check::i9(&[], &probe, &open),
        ["retired fence not refused (request 7)"]
    );
    assert!(
        check::i9(&[], &probe, &closed).is_empty(),
        "a closed socket may lose the answer"
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

fn delivery(sock: u64, msg: ServerMsg) -> Delivery {
    Delivery {
        sock: SocketId(sock),
        agent: AgentId("w".to_string()),
        msg,
    }
}

#[test]
fn i9_flags_a_seq_seen_twice_or_issued_for_two_events() {
    let twice = [delivery(0, event(3, "a")), delivery(0, event(3, "a"))];
    assert_eq!(check::i9(&twice, &[], &open), ["socket 0 saw seq 3 twice"]);
    let two_events = [delivery(0, event(3, "a")), delivery(1, event(3, "b"))];
    assert_eq!(
        check::i9(&two_events, &[], &open),
        ["seq 3 was issued for two different events"]
    );
    let shared = [delivery(0, event(3, "a")), delivery(1, event(3, "a"))];
    assert!(
        check::i9(&shared, &[], &open).is_empty(),
        "two watchers may see one seq"
    );
}

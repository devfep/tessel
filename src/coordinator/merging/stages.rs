//! The stages of a merge: what the core does with each answer of `Prepare`, `Test` and `Land`.
//! The queue's own rules (order, review holds, bounds) are in `queue_props`; here each answer of
//! each stage is read on its own, and the journaled head is read back from the stored state.

use crate::coordinator::{Config, Coordinator, Effect, MergeDispatch, MergeStage};
use crate::merge::{infra_backoff_ms, MergeOutcome, RuleKind, StepExit, MAX_INFRA_RETRIES};
use crate::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, DecisionRecord, EventKind, Intent, OnConflict,
    RequestId, RunId, Scope, ScopeClaim, ServerMsg,
};

const NOW: u64 = 1_000;
const OLD: &str = "dddddddddddddddddddddddddddddddddddddddd";
const HEAD: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn commit(sha: &str) -> CommitId {
    CommitId(sha.into())
}

fn prepared() -> MergeOutcome {
    MergeOutcome::Prepared {
        base: commit(OLD),
        head: commit(HEAD),
    }
}

fn merged() -> MergeOutcome {
    MergeOutcome::Merged {
        base: commit(OLD),
        head: commit(HEAD),
    }
}

fn failed_tests() -> MergeOutcome {
    MergeOutcome::TestsFailed {
        result: StepExit {
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
        },
    }
}

/// A core with one submitted claim whose `Prepare` is in flight, and the core's main known.
fn submitted() -> (Coordinator, ClaimId, MergeDispatch) {
    let mut c = Coordinator::new(Config {
        run: RunId("stages".into()),
        lease_ms: 30_000,
        shadow_enabled: true,
    })
    .unwrap();
    let scope = ScopeClaim {
        scope: Scope::File {
            path: "src/a.rs".into(),
        },
        mode: crate::protocol::Mode::EditBody,
    };
    let claim = ClientMsg::Claim {
        req: RequestId(1),
        intent: Intent {
            summary: "s".into(),
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes: vec![scope.clone()],
        on_conflict: OnConflict::Fail,
    };
    let who = AgentId("a".into());
    let effects = c.handle(&who, claim, NOW);
    let granted = effects.iter().find_map(|effect| match effect {
        Effect::Reply(ServerMsg::Granted { claim, fence, .. }) => Some((*claim, *fence)),
        Effect::Reply(_) | Effect::Notify { .. } | Effect::Log(_) => None,
    });
    let (claim, fence) = granted.expect("granted");
    let submit = ClientMsg::Submit {
        req: RequestId(9),
        claim,
        fence,
        fork_commit: commit(&"ab".repeat(20)),
        touched: vec![scope],
        decisions: DecisionRecord {
            evidence: vec!["tests passed".into()],
            ..DecisionRecord::default()
        },
    };
    c.handle(&who, submit, NOW);
    let dispatch = c.begin_merge(NOW).expect("a merge is due");
    (c, claim, dispatch)
}

/// The dispatch in flight after `outcome` was given to `dispatch`.
fn next_after(
    c: &mut Coordinator,
    dispatch: &MergeDispatch,
    outcome: &MergeOutcome,
) -> MergeDispatch {
    let effects = c.merge_outcome(dispatch.id, outcome, NOW + 1);
    assert!(effects.is_empty(), "{effects:?}");
    c.begin_merge(NOW + 1).expect("the merge goes on")
}

fn at_test() -> (Coordinator, ClaimId, MergeDispatch) {
    let (mut c, claim, prepare) = submitted();
    let test = next_after(&mut c, &prepare, &prepared());
    (c, claim, test)
}

fn at_land() -> (Coordinator, ClaimId, MergeDispatch) {
    let (mut c, claim, test) = at_test();
    let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
    (c, claim, land)
}

fn work(c: &Coordinator, claim: ClaimId) -> &crate::coordinator::Submission {
    c.state.claims[&claim.0].work.as_ref().expect("submitted")
}

fn events(effects: &[Effect]) -> Vec<&EventKind> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Log(event) => Some(&event.kind),
            Effect::Reply(_) | Effect::Notify { .. } => None,
        })
        .collect()
}

/// The (base, head) the `Merged` event reports, if the effects hold one.
fn merged_span(effects: &[Effect]) -> Option<(Option<CommitId>, CommitId)> {
    events(effects).into_iter().find_map(|kind| {
        let EventKind::Merged { base, head, .. } = kind else {
            return None;
        };
        Some((base.clone(), head.clone()))
    })
}

fn landed(effects: &[Effect]) -> bool {
    events(effects)
        .iter()
        .any(|kind| matches!(kind, EventKind::Merged { .. }))
}

fn rejected(effects: &[Effect]) -> bool {
    events(effects)
        .iter()
        .any(|kind| matches!(kind, EventKind::SubmitRejected { .. }))
}

// ---- the machine ----

#[test]
fn a_merge_is_three_calls_each_under_its_own_dispatch_id_and_only_the_third_lands() {
    let (mut c, claim, prepare) = submitted();
    assert_eq!(prepare.stage, MergeStage::Prepare);

    let test = next_after(&mut c, &prepare, &prepared());
    assert_eq!(
        test.stage,
        MergeStage::Test {
            base: commit(OLD),
            head: commit(HEAD)
        }
    );
    assert!(test.id > prepare.id);
    assert_eq!((test.claim, test.attempt), (prepare.claim, prepare.attempt));
    assert_eq!(c.state.head, None, "a Prepare lands nothing");

    let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
    assert_eq!(
        land.stage,
        MergeStage::Land {
            base: commit(OLD),
            head: commit(HEAD)
        }
    );
    assert!(land.id > test.id);
    assert_eq!(c.state.head, None, "nor does a Test");

    let effects = c.merge_outcome(land.id, &merged(), NOW + 2);
    assert!(landed(&effects));
    assert_eq!(c.state.head, Some(commit(HEAD)));
    assert!(
        !c.state.claims.contains_key(&claim.0),
        "the claim is released"
    );
    assert!(!c.has_merge_in_flight());
}

#[test]
fn no_call_names_a_head_the_stored_state_does_not_hold() {
    let (mut c, _, prepare) = submitted();
    let stored = serde_json::to_value(&c).unwrap();
    assert!(
        stored["merge_in_flight"]["stage"]["head"].is_null(),
        "a Prepare has no head yet"
    );

    let test = next_after(&mut c, &prepare, &prepared());
    let stored = serde_json::to_string(&c).unwrap();
    let mut restored: Coordinator = serde_json::from_str(&stored).unwrap();
    assert_eq!(
        restored.begin_merge(NOW + 2),
        Some(test.clone()),
        "a core loaded from what was stored offers the same Test, head included"
    );
    let value: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(value["merge_in_flight"]["stage"]["head"], HEAD);
    assert_eq!(value["merge_in_flight"]["stage"]["stage"], "test");

    let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
    let value = serde_json::to_value(&c).unwrap();
    assert_eq!(value["merge_in_flight"]["stage"]["head"], HEAD);
    assert_eq!(value["merge_in_flight"]["dispatch"], land.id);
}

#[test]
fn a_land_is_offered_only_once_a_test_has_passed() {
    let (mut c, _, prepare) = submitted();
    for _ in 0..3 {
        assert_eq!(
            c.begin_merge(NOW).map(|d| d.stage),
            Some(MergeStage::Prepare)
        );
    }
    let test = next_after(&mut c, &prepare, &prepared());
    for _ in 0..3 {
        assert!(
            matches!(
                c.begin_merge(NOW + 2).map(|d| d.stage),
                Some(MergeStage::Test { .. })
            ),
            "until the tests pass, only the Test is offered"
        );
    }
    next_after(&mut c, &test, &MergeOutcome::Tested {});
}

#[test]
fn an_answer_to_an_earlier_stage_after_the_merge_moved_on_changes_nothing() {
    let (mut c, _, prepare) = submitted();
    let test = next_after(&mut c, &prepare, &prepared());
    let before = serde_json::to_string(&c).unwrap();

    let late = c.merge_outcome(prepare.id, &merged(), NOW + 1);

    assert!(late.is_empty());
    assert_eq!(before, serde_json::to_string(&c).unwrap());
    assert_eq!(c.begin_merge(NOW + 1), Some(test));
}

// ---- Prepare ----

#[test]
fn prepare_answered_with_work_main_holds_lands_it_at_main_with_no_test() {
    let (mut c, claim, prepare) = submitted();
    let at_main = MergeOutcome::AlreadyMerged { base: commit(HEAD) };
    let effects = c.merge_outcome(prepare.id, &at_main, NOW + 1);
    assert!(landed(&effects));
    assert!(!c.state.claims.contains_key(&claim.0));
}

#[test]
fn prepare_refused_for_the_work_rejects_it_and_nothing_is_tested_or_landed() {
    for refusal in [
        MergeOutcome::Conflict {
            files: vec!["x".into()],
            total: None,
        },
        MergeOutcome::Uncovered { total: 1 },
        MergeOutcome::GateChanged {},
        MergeOutcome::CommitNotInFork {},
        MergeOutcome::Refused {
            kind: RuleKind::Other,
        },
    ] {
        let (mut c, claim, prepare) = submitted();
        let effects = c.merge_outcome(prepare.id, &refusal, NOW + 1);
        assert!(rejected(&effects), "{refusal:?}");
        assert!(!c.has_merge_in_flight());
        assert!(c.state.claims[&claim.0].work.is_none(), "{refusal:?}");
    }
}

#[test]
fn prepare_that_did_not_finish_is_an_attempt_that_cannot_have_landed() {
    let (mut c, claim, prepare) = submitted();
    let effects = c.merge_outcome(prepare.id, &MergeOutcome::ServiceUnavailable, NOW + 1);
    assert!(effects.is_empty());
    let work = work(&c, claim);
    assert_eq!(work.infra_failures, 1);
    assert!(!work.in_doubt, "no Prepare changes main");
    assert_eq!(work.unanswered_from, None);
    assert_eq!(
        work.retry_at_ms,
        Some(c.state.clock_ms + infra_backoff_ms(1))
    );
}

// ---- Test ----

#[test]
fn a_test_that_passes_moves_the_merge_to_its_land() {
    let (_, _, test) = at_test();
    assert!(matches!(test.stage, MergeStage::Test { .. }));
    let (c, _, land) = at_land();
    assert!(matches!(land.stage, MergeStage::Land { .. }));
    assert_eq!(c.state.head, None);
}

#[test]
fn a_test_that_fails_rejects_the_work_and_lands_nothing() {
    let (mut c, claim, test) = at_test();
    let effects = c.merge_outcome(test.id, &failed_tests(), NOW + 2);
    assert!(rejected(&effects));
    assert!(!landed(&effects));
    assert_eq!(c.state.head, None);
    assert!(c.state.claims[&claim.0].work.is_none());
    assert!(!c.has_merge_in_flight());
}

#[test]
fn a_test_that_did_not_finish_is_retried_from_prepare_and_is_not_in_doubt() {
    let (mut c, claim, test) = at_test();
    c.merge_outcome(test.id, &MergeOutcome::Timeout {}, NOW + 2);
    let work = work(&c, claim);
    assert_eq!((work.infra_failures, work.in_doubt), (1, false));
    assert!(!c.has_merge_in_flight());
    let due = work.retry_at_ms.unwrap();
    let again = c.begin_merge(due).expect("dispatched after the backoff");
    assert_eq!(
        again.stage,
        MergeStage::Prepare,
        "a fresh Prepare, new head by C7"
    );
    assert!(again.id > test.id);
}

// ---- CandidateMissing ----

#[test]
fn a_missing_candidate_at_test_or_land_goes_back_to_prepare_under_a_new_dispatch_id() {
    for stage in ["test", "land"] {
        let (mut c, claim, dispatch) = if stage == "test" {
            at_test()
        } else {
            at_land()
        };
        let effects = c.merge_outcome(dispatch.id, &MergeOutcome::CandidateMissing {}, NOW + 3);
        assert!(effects.is_empty(), "{stage}");
        let work = work(&c, claim);
        assert_eq!(work.infra_failures, 1, "{stage}: counted");
        assert!(!work.in_doubt, "{stage}: nothing landed, so not in doubt");
        assert_eq!(work.unanswered_from, None);
        assert_eq!(work.moved, 0, "{stage}: not contention");
        let due = work.retry_at_ms.expect("a backoff");
        assert_eq!(due, c.state.clock_ms + infra_backoff_ms(1));
        assert_eq!(c.begin_merge(due - 1), None, "{stage}: the backoff is kept");
        let again = c.begin_merge(due).expect("re-dispatched");
        assert_eq!(again.stage, MergeStage::Prepare, "{stage}");
        assert!(again.id > dispatch.id, "{stage}: a new dispatch id");
        assert_eq!(again.attempt, 2);
    }
}

#[test]
fn a_candidate_that_keeps_vanishing_rejects_the_work_at_the_fourth_loss() {
    let (mut c, claim, mut dispatch) = at_land();
    let mut now = NOW;
    for loss in 1..=MAX_INFRA_RETRIES + 1 {
        let effects = c.merge_outcome(dispatch.id, &MergeOutcome::CandidateMissing {}, now);
        if loss <= MAX_INFRA_RETRIES {
            assert!(!rejected(&effects), "loss {loss}");
            now = work(&c, claim).retry_at_ms.unwrap();
            let prepare = c.begin_merge(now).expect("prepared again");
            let test = next_after(&mut c, &prepare, &prepared());
            dispatch = next_after(&mut c, &test, &MergeOutcome::Tested {});
        } else {
            assert!(rejected(&effects), "the fourth in a row rejects");
        }
    }
    assert!(c.state.claims[&claim.0].work.is_none());
}

#[test]
fn a_candidate_missing_answer_to_a_prepare_is_not_an_answer_it_can_get() {
    let (mut c, claim, prepare) = submitted();
    c.merge_outcome(prepare.id, &MergeOutcome::CandidateMissing {}, NOW + 1);
    let work = work(&c, claim);
    assert_eq!((work.infra_failures, work.in_doubt), (1, false));
}

// ---- Land ----

const MOVED: &str = "1111111111111111111111111111111111111111";

/// A core whose `Land` is in flight for a `Prepare` that ran when the core thought main was at
/// `MOVED`, while the lander leased main at `OLD` (main moved outside tessel in between).
fn at_land_after_main_moved() -> (Coordinator, ClaimId, MergeDispatch) {
    let (mut c, claim, prepare) = submitted();
    c.state
        .merge_in_flight
        .as_mut()
        .expect("a Prepare is out")
        .main = Some(commit(MOVED));
    let test = next_after(&mut c, &prepare, &prepared());
    let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
    (c, claim, land)
}

#[test]
fn a_land_that_did_not_finish_leaves_the_claim_in_doubt_from_the_base_it_leased() {
    let (mut c, claim, land) = at_land_after_main_moved();
    c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 4);
    let work = work(&c, claim);
    assert!(work.in_doubt);
    assert_eq!(work.infra_failures, 0, "a Land in doubt is not an attempt");
    assert_eq!(
        work.unanswered_from,
        Some(commit(OLD)),
        "the base the Land leased, not the main the core read at Prepare"
    );
}

#[test]
fn a_cut_off_land_is_in_doubt_from_the_base_it_leased() {
    let (mut c, claim, _land) = at_land_after_main_moved();
    c.recover_merge(NOW + 4);
    let work = work(&c, claim);
    assert!(work.in_doubt);
    assert_eq!(work.unanswered_from, Some(commit(OLD)));
}

#[test]
fn a_landing_found_after_a_lost_land_answer_spans_from_the_leased_base() {
    let (mut c, claim, land) = at_land_after_main_moved();
    c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 4);
    let due = work(&c, claim).retry_at_ms.unwrap();
    let reconcile = c.begin_merge(due).expect("the claim in doubt goes first");
    let effects = c.reconcile_outcome(reconcile.id, &Reconciliation::Landed, due + 1);
    let span = merged_span(&effects);
    assert_eq!(
        span.map(|(base, _)| base),
        Some(Some(commit(OLD))),
        "not the outside commits"
    );
}

#[test]
fn a_cut_off_land_is_in_doubt_and_a_cut_off_prepare_or_test_is_not() {
    let (mut c, claim, land) = at_land();
    let _ = land;
    c.recover_merge(NOW + 4);
    assert!(work(&c, claim).in_doubt);

    for cut_off_at in [submitted(), at_test()] {
        let (mut c, claim, _) = cut_off_at;
        c.recover_merge(NOW + 4);
        let work = work(&c, claim);
        assert_eq!((work.infra_failures, work.in_doubt), (1, false));
    }
}

#[test]
fn a_land_answered_for_another_head_than_the_journaled_one_lands_nothing() {
    let (mut c, claim, land) = at_land();
    let other = MergeOutcome::Merged {
        base: commit(OLD),
        head: commit(OLD),
    };
    let effects = c.merge_outcome(land.id, &other, NOW + 4);
    assert!(!landed(&effects));
    assert_eq!(c.state.head, None);
    assert!(
        work(&c, claim).in_doubt,
        "it is an attempt that did not finish"
    );
}

#[test]
fn a_merged_answer_to_a_prepare_or_a_test_lands_nothing() {
    for (mut c, claim, dispatch) in [submitted(), at_test()] {
        let effects = c.merge_outcome(dispatch.id, &merged(), NOW + 4);
        assert!(!landed(&effects), "{:?}", dispatch.stage);
        assert_eq!(c.state.head, None);
        assert_eq!(work(&c, claim).infra_failures, 1);
        assert!(!work(&c, claim).in_doubt);
    }
}

#[test]
fn a_land_refused_by_a_rule_rejects_the_work() {
    let (mut c, claim, land) = at_land();
    let refused = MergeOutcome::Refused {
        kind: RuleKind::ProtectedRef,
    };
    let effects = c.merge_outcome(land.id, &refused, NOW + 4);
    assert!(rejected(&effects));
    assert!(c.state.claims[&claim.0].work.is_none());
}

// ---- a restart after the head is journaled ----

#[test]
fn a_restart_at_test_sends_the_journaled_head_to_test_again_under_a_new_dispatch_id() {
    let (mut c, claim, test) = at_test();
    c.recover_merge(NOW + 4);
    let work = work(&c, claim);
    assert_eq!((work.infra_failures, work.in_doubt), (1, false));
    assert_eq!(work.unanswered_from, None);
    let again = c.begin_merge(NOW + 4).expect("the Test goes out again");
    assert_eq!(again.stage, test.stage, "the same head, not a new Prepare");
    assert!(again.id > test.id, "a new dispatch id");
    assert_eq!(again.attempt, 2);
    let stale = c.merge_outcome(test.id, &failed_tests(), NOW + 5);
    assert!(
        stale.is_empty(),
        "the cut-off dispatch can no longer be answered"
    );
    assert!(c.has_merge_in_flight());
}

#[test]
fn a_test_sent_again_after_a_restart_goes_on_to_land_and_lands() {
    let (mut c, claim, _) = at_test();
    c.recover_merge(NOW + 4);
    let again = c.begin_merge(NOW + 4).expect("the Test goes out again");
    let land = next_after(&mut c, &again, &MergeOutcome::Tested {});
    assert!(matches!(land.stage, MergeStage::Land { .. }));
    let effects = c.merge_outcome(land.id, &merged(), NOW + 6);
    assert!(landed(&effects));
    assert!(!c.state.claims.contains_key(&claim.0));
}

#[test]
fn a_test_sent_again_whose_candidate_is_gone_goes_back_to_prepare() {
    let (mut c, claim, _) = at_test();
    c.recover_merge(NOW + 4);
    let again = c.begin_merge(NOW + 4).expect("the Test goes out again");
    c.merge_outcome(again.id, &MergeOutcome::CandidateMissing {}, NOW + 5);
    let work = work(&c, claim);
    assert_eq!((work.infra_failures, work.in_doubt), (2, false));
    let due = work.retry_at_ms.expect("a backoff");
    let prepare = c.begin_merge(due).expect("prepared again");
    assert_eq!(prepare.stage, MergeStage::Prepare);
}

#[test]
fn restarts_at_test_are_bounded_like_any_infrastructure_failure() {
    let (mut c, claim, _) = at_test();
    for restart in 1..=MAX_INFRA_RETRIES {
        c.recover_merge(NOW + 4);
        assert_eq!(work(&c, claim).infra_failures, restart);
        c.begin_merge(NOW + 4).expect("the Test goes out again");
    }
    let effects = c.recover_merge(NOW + 5);
    assert!(rejected(&effects), "the fourth in a row rejects");
    assert!(!c.has_merge_in_flight());
    assert!(c.state.claims[&claim.0].work.is_none());
}

#[test]
fn a_restart_at_prepare_dispatches_a_fresh_prepare_after_a_backoff() {
    let (mut c, claim, _) = submitted();
    c.recover_merge(NOW + 4);
    assert!(!c.has_merge_in_flight());
    let due = work(&c, claim).retry_at_ms.expect("a backoff");
    let again = c.begin_merge(due).expect("dispatched after the backoff");
    assert_eq!(again.stage, MergeStage::Prepare);
}

// ---- answers a stage does not ask for ----

#[test]
fn already_merged_answered_to_a_test_lands_nothing_and_is_not_in_doubt() {
    let (mut c, claim, test) = at_test();
    let already = MergeOutcome::AlreadyMerged { base: commit(HEAD) };
    let effects = c.merge_outcome(test.id, &already, NOW + 4);
    assert!(!landed(&effects));
    assert_eq!(c.state.head, None);
    let work = work(&c, claim);
    assert_eq!((work.infra_failures, work.in_doubt), (1, false));
}

#[test]
fn already_merged_answered_to_a_land_lands_nothing_and_leaves_the_claim_in_doubt() {
    let (mut c, claim, land) = at_land();
    let already = MergeOutcome::AlreadyMerged { base: commit(HEAD) };
    let effects = c.merge_outcome(land.id, &already, NOW + 4);
    assert!(!landed(&effects));
    assert_eq!(c.state.head, None);
    let work = work(&c, claim);
    assert_eq!((work.infra_failures, work.in_doubt), (0, true));
}

#[test]
fn main_moved_answered_to_a_test_or_a_land_dispatches_a_fresh_prepare_without_a_failure() {
    for (mut c, claim, dispatch) in [at_test(), at_land()] {
        let effects = c.merge_outcome(dispatch.id, &MergeOutcome::MainMoved {}, NOW + 4);
        assert!(effects.is_empty(), "{:?}", dispatch.stage);
        let work = work(&c, claim);
        assert_eq!(
            (work.moved, work.infra_failures, work.in_doubt),
            (1, 0, false),
            "{:?}",
            dispatch.stage
        );
        let again = c.begin_merge(NOW + 5).expect("dispatched again");
        assert_eq!(again.stage, MergeStage::Prepare);
    }
}

// ---- a Land in doubt is reconciled ----

use crate::coordinator::Reconciliation;

/// A core whose `Land` came back `Unknown` (or was cut off), so the claim is in doubt and the
/// merge waits for its `Reconcile`.
fn in_doubt_after_unknown() -> (Coordinator, ClaimId, MergeDispatch) {
    let (mut c, claim, land) = at_land();
    let effects = c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 4);
    assert!(!rejected(&effects));
    (c, claim, land)
}

/// The dispatch the core offers once the backoff after the last `Unknown` is over.
fn due_reconcile(c: &mut Coordinator, claim: ClaimId) -> MergeDispatch {
    let due = work(c, claim).retry_at_ms.unwrap_or(NOW + 4);
    c.begin_merge(due).expect("the Reconcile is due")
}

fn stage_pair(dispatch: &MergeDispatch) -> (CommitId, CommitId) {
    match &dispatch.stage {
        MergeStage::Test { base, head }
        | MergeStage::Land { base, head }
        | MergeStage::Reconcile { base, head } => (base.clone(), head.clone()),
        MergeStage::Prepare | MergeStage::Unstaged => panic!("no pair in {:?}", dispatch.stage),
    }
}

#[test]
fn unknown_after_land_asks_to_reconcile_the_same_dispatch_and_counts_nothing() {
    let (mut c, claim, land) = in_doubt_after_unknown();
    let work = work(&c, claim);
    assert_eq!((work.infra_failures, work.in_doubt), (0, true));
    assert_eq!(work.unanswered_from, Some(commit(OLD)));
    let reconcile = due_reconcile(&mut c, claim);
    assert_eq!(
        reconcile.stage,
        MergeStage::Reconcile {
            base: commit(OLD),
            head: commit(HEAD)
        }
    );
    assert_eq!(
        reconcile.id, land.id,
        "the reconcile is the Land's dispatch"
    );
}

#[test]
fn unknown_after_land_never_counts_toward_the_infrastructure_bound() {
    let (mut c, claim, land) = in_doubt_after_unknown();
    let mut now = NOW + 4;
    for round in 1..=2 * MAX_INFRA_RETRIES + 2 {
        let due = work(&c, claim).retry_at_ms.expect("a backoff");
        assert!(due > now, "round {round}: reconciles are paced");
        assert_eq!(c.begin_merge(due - 1), None, "round {round}");
        now = due;
        let reconcile = c.begin_merge(now).expect("due");
        assert_eq!(reconcile.id, land.id);
        let effects = c.reconcile_outcome(reconcile.id, &Reconciliation::Unknown, now);
        assert!(!rejected(&effects), "round {round}: Unknown never rejects");
        assert!(!landed(&effects));
        let work = work(&c, claim);
        assert_eq!(work.infra_failures, 0, "round {round}");
        assert!(work.in_doubt);
        let waited = work.retry_at_ms.unwrap() - now;
        let capped = infra_backoff_ms(round.min(MAX_INFRA_RETRIES) + 1);
        assert!(waited <= capped, "round {round}: the backoff is capped");
    }
}

#[test]
fn a_retried_land_reuses_its_dispatch_id_base_head_and_candidate_ref() {
    let (mut c, claim, land) = in_doubt_after_unknown();
    let before = stage_pair(&land);
    let reconcile = due_reconcile(&mut c, claim);
    let at_base = Reconciliation::NotLanded {
        main: Some(commit(OLD)),
    };
    let effects = c.reconcile_outcome(reconcile.id, &at_base, NOW + 10_000);
    assert!(effects.is_empty(), "{effects:?}");
    let again = c
        .begin_merge(NOW + 10_000)
        .expect("the Land goes out again");
    assert_eq!(again.id, land.id, "the same dispatch id");
    assert_eq!(again.attempt, land.attempt, "not a new attempt");
    assert!(matches!(again.stage, MergeStage::Land { .. }));
    assert_eq!(stage_pair(&again), before, "the same base and head");
    let stored = serde_json::to_value(&c).unwrap();
    assert_eq!(stored["merge_in_flight"]["stage"]["head"], HEAD);
    assert_eq!(work(&c, claim).infra_failures, 0);
}

#[test]
fn a_reconcile_that_finds_the_head_on_main_lands_the_work_from_the_leased_base() {
    let (mut c, claim, _) = in_doubt_after_unknown();
    let reconcile = due_reconcile(&mut c, claim);
    let effects = c.reconcile_outcome(reconcile.id, &Reconciliation::Landed, NOW + 10_000);
    assert!(landed(&effects));
    assert_eq!(
        merged_span(&effects),
        Some((Some(commit(OLD)), commit(HEAD)))
    );
    assert!(!c.state.claims.contains_key(&claim.0));
    assert!(!c.has_merge_in_flight());
}

#[test]
fn a_reconcile_that_finds_main_elsewhere_goes_back_to_prepare_and_the_doubt_is_over() {
    let (mut c, claim, land) = in_doubt_after_unknown();
    let reconcile = due_reconcile(&mut c, claim);
    let elsewhere = Reconciliation::NotLanded {
        main: Some(commit(MOVED)),
    };
    let effects = c.reconcile_outcome(reconcile.id, &elsewhere, NOW + 10_000);
    assert!(!landed(&effects) && !rejected(&effects));
    let work = work(&c, claim);
    assert_eq!((work.in_doubt, work.unanswered_from.clone()), (false, None));
    assert_eq!(
        (work.moved, work.infra_failures),
        (1, 0),
        "counted as contention"
    );
    let prepare = c.begin_merge(NOW + 10_000).expect("dispatched at once");
    assert_eq!(prepare.stage, MergeStage::Prepare);
    assert!(prepare.id > land.id, "a new dispatch id");
}

#[test]
fn a_reconcile_that_cannot_say_where_main_is_goes_back_to_prepare_too() {
    let (mut c, claim, _) = in_doubt_after_unknown();
    let reconcile = due_reconcile(&mut c, claim);
    let unsaid = Reconciliation::NotLanded { main: None };
    c.reconcile_outcome(reconcile.id, &unsaid, NOW + 10_000);
    let prepare = c.begin_merge(NOW + 10_000).expect("dispatched");
    assert_eq!(prepare.stage, MergeStage::Prepare);
}

#[test]
fn a_land_that_lost_its_lease_answers_unknown_and_does_not_loop() {
    // `landing/answer.rs` answers `Unknown` for a `Land` that lost its lease. Main moved, so the
    // reconcile finds the head off main and the claim goes back to Prepare instead of looping.
    let (mut c, claim, _) = in_doubt_after_unknown();
    for round in 0..3 {
        let reconcile = due_reconcile(&mut c, claim);
        let elsewhere = Reconciliation::NotLanded {
            main: Some(commit(MOVED)),
        };
        c.reconcile_outcome(reconcile.id, &elsewhere, NOW + 10_000);
        let prepare = c.begin_merge(NOW + 10_000).expect("dispatched again");
        assert_eq!(prepare.stage, MergeStage::Prepare, "round {round}");
        let test = next_after(&mut c, &prepare, &prepared());
        let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
        c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 20_000);
        assert_eq!(
            work(&c, claim).infra_failures,
            0,
            "round {round}: nothing counted"
        );
        assert!(work(&c, claim).moved <= round + 1);
    }
    assert!(
        work(&c, claim).in_doubt,
        "and the last Land is still in doubt"
    );
}

#[test]
fn an_answer_to_the_land_after_the_reconcile_began_lands_only_for_the_journaled_head() {
    let (mut c, claim, land) = in_doubt_after_unknown();
    let late_other = MergeOutcome::Merged {
        base: commit(OLD),
        head: commit(OLD),
    };
    assert!(c.merge_outcome(land.id, &late_other, NOW + 5).is_empty());
    assert!(c
        .merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 5)
        .is_empty());
    assert!(c.has_merge_in_flight(), "the doubt stands");
    assert_eq!(work(&c, claim).infra_failures, 0);
    let effects = c.merge_outcome(land.id, &merged(), NOW + 6);
    assert!(landed(&effects), "the Land did land");
}

#[test]
fn a_restart_at_land_is_reconciled_not_counted_as_a_cut_off() {
    let (mut c, claim, land) = at_land();
    let effects = c.recover_merge(NOW + 4);
    assert!(!rejected(&effects));
    let work = work(&c, claim);
    assert_eq!((work.infra_failures, work.in_doubt), (0, true));
    assert_eq!(work.unanswered_from, Some(commit(OLD)));
    let reconcile = c.begin_merge(NOW + 4).expect("reconciled at once");
    assert_eq!(reconcile.id, land.id);
    assert!(matches!(reconcile.stage, MergeStage::Reconcile { .. }));
    assert!(
        c.recover_merge(NOW + 5).is_empty(),
        "recovery is idempotent"
    );
    assert!(c.has_merge_in_flight());
}

#[test]
fn a_restart_during_a_reconcile_reconciles_again_without_counting() {
    let (mut c, claim, _) = in_doubt_after_unknown();
    let first = due_reconcile(&mut c, claim);
    let stored = serde_json::to_string(&c).unwrap();
    let mut restarted: Coordinator = serde_json::from_str(&stored).unwrap();
    restarted.recover_merge(NOW + 20_000);
    let again = restarted.begin_merge(NOW + 20_000).expect("asked again");
    assert_eq!(again, first);
    assert_eq!(work(&restarted, claim).infra_failures, 0);
}

#[test]
fn only_the_dispatch_in_doubt_can_be_reconciled() {
    let (mut c, claim, land) = in_doubt_after_unknown();
    let before = serde_json::to_string(&c).unwrap();
    let effects = c.reconcile_outcome(land.id + 100, &Reconciliation::Landed, NOW + 4);
    assert!(effects.is_empty());
    assert_eq!(before, serde_json::to_string(&c).unwrap());
    let (mut other, _, test) = at_test();
    let before = serde_json::to_string(&other).unwrap();
    other.reconcile_outcome(test.id, &Reconciliation::Landed, NOW + 1);
    assert_eq!(
        before,
        serde_json::to_string(&other).unwrap(),
        "no Land is in doubt"
    );
    let _ = claim;
}

// ---- a store written before the staged landing ----

/// A core as the build before the staged landing stored it: a merge in flight whose marker has
/// no `stage`, no `doubts` and no `doubt_since_ms`, and whose one call may have pushed.
/// `fixtures/pre_staged_core.json` is a core as 455c889 stored it: the current serializer's output
/// for one submitted claim with a merge in flight, less the three marker fields the staged landing
/// added (`stage`, `doubts`, `doubt_since_ms`), checked in so it cannot drift with the serializer.
fn store_written_before_the_staged_landing() -> String {
    include_str!("fixtures/pre_staged_core.json").to_string()
}

#[test]
fn a_marker_stored_before_the_staged_landing_decodes_as_unstaged_not_as_prepare() {
    let c: Coordinator = serde_json::from_str(&store_written_before_the_staged_landing()).unwrap();
    let flight = c
        .state
        .merge_in_flight
        .as_ref()
        .expect("the marker is read");
    assert_eq!(flight.stage, MergeStage::Unstaged);
}

#[test]
fn a_marker_stored_before_the_staged_landing_leaves_the_claim_in_doubt_from_its_main() {
    let stored = store_written_before_the_staged_landing();
    let mut c: Coordinator = serde_json::from_str(&stored).unwrap();
    let claim = *c.state.claims.keys().next().unwrap();
    c.recover_merge(NOW + 4);
    let work = work(&c, ClaimId(claim));
    assert!(work.in_doubt, "the old call may have pushed");
    assert_eq!(work.unanswered_from, Some(commit(OLD)));
    assert_eq!(
        work.infra_failures, 1,
        "an old call that did not finish is an attempt"
    );
    let due = work.retry_at_ms.expect("a backoff");
    let prepare = c.begin_merge(due).expect("the claim in doubt goes first");
    assert_eq!(prepare.stage, MergeStage::Prepare);
    let already = MergeOutcome::AlreadyMerged { base: commit(HEAD) };
    let effects = c.merge_outcome(prepare.id, &already, due + 1);
    let span = merged_span(&effects);
    assert_eq!(
        span.map(|(base, _)| base),
        Some(Some(commit(OLD))),
        "spans from the old main"
    );
}

// ---- the first baseline of a doubt is kept ----

#[test]
fn a_second_land_lost_on_another_base_keeps_the_first_baseline() {
    let stored = store_written_before_the_staged_landing();
    let mut c: Coordinator = serde_json::from_str(&stored).unwrap();
    let claim = ClaimId(*c.state.claims.keys().next().unwrap());
    c.recover_merge(NOW + 4);
    assert_eq!(work(&c, claim).unanswered_from, Some(commit(OLD)));
    let due = work(&c, claim).retry_at_ms.unwrap();
    let prepare = c.begin_merge(due).unwrap();
    let on_other_base = MergeOutcome::Prepared {
        base: commit(MOVED),
        head: commit(HEAD),
    };
    let test = next_after(&mut c, &prepare, &on_other_base);
    let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
    assert_eq!(stage_pair(&land).0, commit(MOVED));
    c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, due + 5);
    assert_eq!(
        work(&c, claim).unanswered_from,
        Some(commit(OLD)),
        "a landing found later spans from the first main that may have been landed on"
    );
}

// ---- the operator's answer ----

#[test]
fn the_operators_landed_answer_lands_the_work_as_a_reconcile_would() {
    let (mut c, claim, _) = in_doubt_after_unknown();
    let effects = c
        .resolve_doubt(claim, true, NOW + 50)
        .expect("a Land is in doubt");
    assert!(landed(&effects));
    assert!(!c.state.claims.contains_key(&claim.0));
}

#[test]
fn the_operators_not_landed_answer_sends_the_work_back_to_prepare() {
    let (mut c, claim, land) = in_doubt_after_unknown();
    let effects = c
        .resolve_doubt(claim, false, NOW + 50)
        .expect("a Land is in doubt");
    assert!(!landed(&effects) && !rejected(&effects));
    let work = work(&c, claim);
    assert_eq!((work.in_doubt, work.infra_failures), (false, 0));
    let prepare = c.begin_merge(NOW + 50).expect("dispatched");
    assert_eq!(prepare.stage, MergeStage::Prepare);
    assert!(prepare.id > land.id);
}

#[test]
fn the_operator_can_resolve_a_land_the_stopped_server_never_recovered() {
    let (mut c, claim, _) = at_land();
    c.resolve_doubt(claim, true, NOW + 50)
        .expect("a Land marker is in doubt");
    assert!(!c.state.claims.contains_key(&claim.0));
}

#[test]
fn the_operator_cannot_resolve_a_claim_with_no_land_in_doubt() {
    let (mut c, claim, _) = at_test();
    let before = serde_json::to_string(&c).unwrap();
    let refusal = c.resolve_doubt(claim, true, NOW + 2).unwrap_err();
    assert_eq!(
        refusal.to_string(),
        format!("claim {} has no Land in doubt", claim.0)
    );
    assert_eq!(
        before,
        serde_json::to_string(&c).unwrap(),
        "nothing changed"
    );
    assert!(c.resolve_doubt(ClaimId(999), false, NOW + 2).is_err());
    let (mut idle, claim, _) = submitted();
    idle.merge_outcome(
        idle.state.merge_in_flight.as_ref().unwrap().dispatch,
        &MergeOutcome::GateChanged {},
        NOW + 1,
    );
    assert!(
        idle.resolve_doubt(claim, true, NOW + 50).is_err(),
        "nothing in flight"
    );
}

// ---- the submitter is told how long the landing has been in doubt ----

fn doubt_notices(effects: &[Effect]) -> Vec<(ClaimId, Option<u64>)> {
    effects
        .iter()
        .filter_map(|effect| {
            let Effect::Notify {
                msg: ServerMsg::MergeDoubt { claim, since_ms },
                ..
            } = effect
            else {
                return None;
            };
            Some((*claim, *since_ms))
        })
        .collect()
}

#[test]
fn the_submitter_is_told_once_when_the_landing_falls_in_doubt_with_the_time_it_began() {
    let (mut c, claim, land) = at_land();
    let effects = c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 4);
    assert_eq!(doubt_notices(&effects), vec![(claim, Some(NOW + 4))]);
    let reconcile = due_reconcile(&mut c, claim);
    let again = c.reconcile_outcome(reconcile.id, &Reconciliation::Unknown, NOW + 20_000);
    assert!(
        doubt_notices(&again).is_empty(),
        "a longer doubt is not a new one: the age counts from the first"
    );
}

#[test]
fn a_restart_that_finds_a_land_tells_the_submitter_too() {
    let (mut c, claim, _) = at_land();
    let effects = c.recover_merge(NOW + 4);
    assert_eq!(doubt_notices(&effects), vec![(claim, Some(NOW + 4))]);
}

#[test]
fn the_submitter_is_told_when_the_doubt_is_over_and_the_work_goes_back_to_prepare() {
    let (mut c, claim, _) = in_doubt_after_unknown();
    let reconcile = due_reconcile(&mut c, claim);
    let elsewhere = Reconciliation::NotLanded {
        main: Some(commit(MOVED)),
    };
    let effects = c.reconcile_outcome(reconcile.id, &elsewhere, NOW + 10_000);
    assert_eq!(doubt_notices(&effects), vec![(claim, None)]);
}

#[test]
fn a_land_sent_again_that_main_refuses_ends_the_doubt_and_says_so() {
    let (mut c, claim, land) = in_doubt_after_unknown();
    let reconcile = due_reconcile(&mut c, claim);
    let at_base = Reconciliation::NotLanded {
        main: Some(commit(OLD)),
    };
    c.reconcile_outcome(reconcile.id, &at_base, NOW + 10_000);
    let again = c
        .begin_merge(NOW + 10_000)
        .expect("the Land goes out again");
    assert_eq!(again.id, land.id);
    let effects = c.merge_outcome(again.id, &MergeOutcome::MainMoved {}, NOW + 10_001);
    assert_eq!(doubt_notices(&effects), vec![(claim, None)]);
    let work = work(&c, claim);
    assert_eq!(
        (work.in_doubt, work.moved, work.infra_failures),
        (false, 1, 0)
    );
}

#[test]
fn a_land_that_was_never_in_doubt_answered_main_moved_keeps_what_it_had() {
    let (mut c, claim, land) = at_land();
    assert!(c
        .merge_outcome(land.id, &MergeOutcome::MainMoved {}, NOW + 4)
        .is_empty());
    assert_eq!(work(&c, claim).moved, 1);
}

// ---- the end of a doubt ----

#[test]
fn the_operators_not_landed_answer_does_not_count_main_moved_and_cannot_reject() {
    let (mut c, claim, _) = in_doubt_after_unknown();
    c.state
        .claims
        .get_mut(&claim.0)
        .and_then(|held| held.work.as_mut())
        .expect("submitted")
        .moved = crate::merge::MAX_MAIN_MOVED - 1;
    let effects = c.resolve_doubt(claim, false, NOW + 50).expect("in doubt");
    assert!(!rejected(&effects), "nobody said main moved");
    assert_eq!(work(&c, claim).moved, crate::merge::MAX_MAIN_MOVED - 1);
    let prepare = c.begin_merge(NOW + 50).expect("dispatched at once");
    assert_eq!(prepare.stage, MergeStage::Prepare);
}

/// The `Land` sent again after a reconcile found main at its base, in flight.
fn resent_land() -> (Coordinator, ClaimId, MergeDispatch) {
    let (mut c, claim, _) = in_doubt_after_unknown();
    let reconcile = due_reconcile(&mut c, claim);
    let at_base = Reconciliation::NotLanded {
        main: Some(commit(OLD)),
    };
    c.reconcile_outcome(reconcile.id, &at_base, NOW + 10_000);
    let again = c
        .begin_merge(NOW + 10_000)
        .expect("the Land goes out again");
    (c, claim, again)
}

#[test]
fn a_resent_land_refused_ends_the_doubt_and_says_so() {
    let (mut c, claim, again) = resent_land();
    let refused = MergeOutcome::Refused {
        kind: RuleKind::ProtectedRef,
    };
    let effects = c.merge_outcome(again.id, &refused, NOW + 10_001);
    assert!(rejected(&effects));
    assert_eq!(doubt_notices(&effects), vec![(claim, None)]);
}

#[test]
fn a_resent_land_whose_candidate_is_gone_ends_the_doubt_and_says_so() {
    let (mut c, claim, again) = resent_land();
    let effects = c.merge_outcome(again.id, &MergeOutcome::CandidateMissing {}, NOW + 10_001);
    assert_eq!(doubt_notices(&effects), vec![(claim, None)]);
    let work = work(&c, claim);
    assert_eq!((work.in_doubt, work.unanswered_from.clone()), (false, None));
    assert_eq!(work.infra_failures, 1, "counted, as a missing candidate is");
}

// ---- Welcome lists the claims in doubt ----

fn welcome_doubts(c: &mut Coordinator, who: &str, now: u64) -> Vec<crate::protocol::ClaimInDoubt> {
    let hello = ClientMsg::Hello {
        agent: AgentId(who.into()),
        base: commit(OLD),
        protocol: crate::protocol::PROTOCOL_VERSION,
    };
    let effects = c.handle(&AgentId(who.into()), hello, now);
    effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Reply(ServerMsg::Welcome { merge_doubts, .. }) => Some(merge_doubts),
            Effect::Reply(_) | Effect::Notify { .. } | Effect::Log(_) => None,
        })
        .expect("a Welcome")
}

#[test]
fn a_welcome_lists_the_claims_of_that_agent_whose_landing_is_in_doubt_with_when_it_began() {
    let (mut c, claim, _) = in_doubt_after_unknown();
    let listed = welcome_doubts(&mut c, "a", NOW + 100);
    let expected = crate::protocol::ClaimInDoubt {
        claim,
        since_ms: NOW + 4,
    };
    assert_eq!(listed, vec![expected]);
    assert!(welcome_doubts(&mut c, "someone-else", NOW + 101).is_empty());
}

#[test]
fn a_restart_that_found_a_land_lists_it_at_the_next_welcome() {
    let (mut c, claim, _) = at_land();
    c.recover_merge(NOW + 4);
    let listed = welcome_doubts(&mut c, "a", NOW + 5);
    assert_eq!(
        listed.iter().map(|d| d.claim).collect::<Vec<_>>(),
        vec![claim]
    );
}

#[test]
fn a_welcome_lists_nothing_once_the_doubt_is_over_however_it_ended() {
    // The reconcile found main elsewhere.
    let (mut c, claim, _) = in_doubt_after_unknown();
    let reconcile = due_reconcile(&mut c, claim);
    let elsewhere = Reconciliation::NotLanded {
        main: Some(commit(MOVED)),
    };
    c.reconcile_outcome(reconcile.id, &elsewhere, NOW + 10_000);
    assert!(welcome_doubts(&mut c, "a", NOW + 10_001).is_empty());
    // The operator answered, each way: what `admin resolve` stores and a restart loads.
    for landed in [true, false] {
        let (mut c, claim, _) = in_doubt_after_unknown();
        c.resolve_doubt(claim, landed, NOW + 50).expect("in doubt");
        assert!(
            welcome_doubts(&mut c, "a", NOW + 51).is_empty(),
            "landed {landed}"
        );
    }
    // A resent Land was refused, so the work went back to its agent.
    let (mut c, _, again) = resent_land();
    let refused = MergeOutcome::Refused {
        kind: RuleKind::ProtectedRef,
    };
    c.merge_outcome(again.id, &refused, NOW + 10_001);
    assert!(welcome_doubts(&mut c, "a", NOW + 10_002).is_empty());
}

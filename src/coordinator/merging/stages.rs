//! The stages of a merge: what the core does with each answer of `Prepare`, `Test` and `Land`.
//! The queue's own rules (order, review holds, bounds) are in `queue_props`; here each answer of
//! each stage is read on its own, and the journaled head is read back from the stored state.

use crate::coordinator::{Config, Coordinator, Effect, MergeDispatch, MergeStage};
use crate::merge::{
    infra_backoff_ms, MergeOutcome, RuleKind, StepExit, TrialOutcome, TrialReport,
    MAX_INFRA_RETRIES,
};
use crate::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, Criterion, DecisionRecord, EventKind, Intent,
    OnConflict, RequestId, RunId, Scope, ScopeClaim, ServerMsg,
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
        let doubts = c.state.merge_in_flight.as_ref().map(|flight| flight.doubts);
        assert_eq!(
            doubts,
            Some(round + 1),
            "round {round}: the Land's doubt and each ask count"
        );
        let capped = infra_backoff_ms((round + 1).min(MAX_INFRA_RETRIES));
        assert_eq!(
            waited, capped,
            "round {round}: the backoff grows to its cap"
        );
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
    assert_eq!(
        work(&c, claim).retry_at_ms,
        None,
        "the resend clears the stale backoff (the Land stage never reads it, so this pins state)"
    );
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
    assert_eq!(work.doubted_land, None);
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
    assert_eq!(work.doubted_land, None);
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

/// A core whose head is only adopted from a `Hello`, with claim `a`'s `Land` in doubt and a
/// one-entrant race on another file that closed meanwhile, so it waits for the head.
fn in_doubt_with_a_race_waiting_for_the_head() -> (Coordinator, ClaimId, AgentId) {
    let file = |path: &str| ScopeClaim {
        scope: Scope::File { path: path.into() },
        mode: crate::protocol::Mode::EditBody,
    };
    let intent = || Intent {
        summary: "s".into(),
        task_ref: None,
        assumptions: Vec::new(),
    };
    let decisions = || DecisionRecord {
        evidence: vec!["tests passed".into()],
        ..DecisionRecord::default()
    };
    let granted = |effects: &[Effect]| {
        effects.iter().find_map(|effect| match effect {
            Effect::Reply(ServerMsg::Granted { claim, fence, .. }) => Some((*claim, *fence)),
            Effect::Reply(_) | Effect::Notify { .. } | Effect::Log(_) => None,
        })
    };
    let mut c = Coordinator::new(Config {
        run: RunId("stages".into()),
        lease_ms: 30_000,
        shadow_enabled: true,
    })
    .unwrap();
    let (felix, a, r1) = (
        AgentId("felix".into()),
        AgentId("a".into()),
        AgentId("r1".into()),
    );
    c.set_reviewers(vec![felix.clone()]);
    c.begin_head_read().expect("no head: a read is due");
    let hello = ClientMsg::Hello {
        agent: felix.clone(),
        base: commit(OLD),
        protocol: crate::protocol::PROTOCOL_VERSION,
    };
    c.handle(&felix, hello, NOW);

    let claim = ClientMsg::Claim {
        req: RequestId(1),
        intent: intent(),
        scopes: vec![file("src/a.rs")],
        on_conflict: OnConflict::Fail,
    };
    let (claim, fence) = granted(&c.handle(&a, claim, NOW)).expect("granted");
    let submit = ClientMsg::Submit {
        req: RequestId(2),
        claim,
        fence,
        fork_commit: commit(&"ab".repeat(20)),
        touched: vec![file("src/a.rs")],
        decisions: decisions(),
    };
    c.handle(&a, submit, NOW);

    let open = ClientMsg::OpenRace {
        req: RequestId(3),
        intent: intent(),
        scopes: vec![file("src/r.rs")],
        max_entrants: 1,
        deadline_ms: NOW + 20_000,
        criteria: vec![Criterion::FirstSubmitted],
    };
    let opened = c.handle(&felix, open, NOW);
    let race = opened.iter().find_map(|effect| match effect {
        Effect::Reply(ServerMsg::RaceOpened { race, .. }) => Some(*race),
        Effect::Reply(_) | Effect::Notify { .. } | Effect::Log(_) => None,
    });
    let join = ClientMsg::JoinRace {
        req: RequestId(4),
        race: race.expect("RaceOpened"),
    };
    let (entry, fence) = granted(&c.handle(&r1, join, NOW)).expect("joined");
    let enter = ClientMsg::Submit {
        req: RequestId(5),
        claim: entry,
        fence,
        fork_commit: commit(&"cd".repeat(20)),
        touched: vec![file("src/r.rs")],
        decisions: decisions(),
    };
    c.handle(&r1, enter, NOW + 1);
    assert!(c.races_await_head(), "the race closed on an adopted head");

    let prepare = c.begin_merge(NOW + 2).expect("a's merge");
    let test = next_after(&mut c, &prepare, &prepared());
    let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
    c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 4);
    (c, claim, r1)
}

#[test]
fn an_offline_landed_answer_releases_the_races_waiting_on_the_head_and_the_trials_tell_the_entrants(
) {
    let (mut c, claim, r1) = in_doubt_with_a_race_waiting_for_the_head();
    let effects = c.resolve_doubt(claim, true, NOW + 50).expect("in doubt");
    assert!(landed(&effects));
    assert!(
        !c.races_await_head(),
        "the landing is a head the core has read"
    );
    let notified = |effects: &[Effect]| {
        effects
            .iter()
            .filter(|effect| {
                matches!(
                    effect,
                    Effect::Notify {
                        msg: ServerMsg::RaceResult { .. },
                        ..
                    }
                )
            })
            .count()
    };
    assert_eq!(
        notified(&effects),
        0,
        "the trial is queued, not judged: nothing for `admin resolve` to drop"
    );

    let stored = serde_json::to_string(&c).unwrap();
    let mut restarted: Coordinator = serde_json::from_str(&stored).unwrap();
    let trial = restarted
        .begin_verification(NOW + 60)
        .expect("the queued trial");
    assert_eq!(
        (trial.agent.clone(), trial.main.clone()),
        (r1.clone(), commit(HEAD))
    );
    let clean = TrialReport {
        before: Some(TrialOutcome::Clean {}),
        after: Some(TrialOutcome::Clean {}),
    };
    let decided = restarted.verification_outcome(trial.id, &clean, NOW + 61);
    assert_eq!(
        notified(&decided),
        2,
        "the opener and the entrant hear the result"
    );
    assert!(
        events(&decided)
            .iter()
            .any(|kind| matches!(kind, EventKind::RaceDecided { .. })),
        "and the log records it; an entrant offline now is not told"
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
    assert_eq!(work.doubted_land, None);
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
fn a_resent_land_refused_stays_in_doubt_and_rejects_nothing() {
    let (mut c, claim, again) = resent_land();
    let refused = MergeOutcome::Refused {
        kind: RuleKind::ProtectedRef,
    };
    let effects = c.merge_outcome(again.id, &refused, NOW + 10_001);
    assert!(!rejected(&effects));
    assert_eq!(doubt_notices(&effects), vec![]);
    let work = work(&c, claim);
    assert_eq!((work.in_doubt, work.infra_failures), (true, 0));
    assert_eq!(work.unanswered_from, Some(commit(OLD)));
}

/// The `Land` sent again was answered `CandidateMissing`, and the core's `Prepare` is due.
fn doubted_prepare() -> (Coordinator, ClaimId, MergeDispatch, MergeDispatch) {
    let (mut c, claim, again) = resent_land();
    let effects = c.merge_outcome(again.id, &MergeOutcome::CandidateMissing {}, NOW + 10_001);
    assert!(effects.is_empty(), "{effects:?}");
    let due = work(&c, claim).retry_at_ms.expect("a backoff");
    let prepare = c.begin_merge(due).expect("the Prepare is due");
    assert_eq!(prepare.stage, MergeStage::Prepare);
    (c, claim, again, prepare)
}

/// A `Prepare` answer for the doubted base and another head.
fn prepared_other_head() -> MergeOutcome {
    MergeOutcome::Prepared {
        base: commit(OLD),
        head: commit(MOVED),
    }
}

fn retry_at(c: &Coordinator, claim: ClaimId) -> u64 {
    work(c, claim).retry_at_ms.expect("a backoff")
}

#[test]
fn a_resent_land_whose_candidate_is_gone_stays_in_doubt_and_counts_nothing() {
    let (mut c, claim, again) = resent_land();
    let effects = c.merge_outcome(again.id, &MergeOutcome::CandidateMissing {}, NOW + 10_001);
    assert!(!rejected(&effects));
    assert_eq!(doubt_notices(&effects), vec![], "the doubt is not over");
    let work = work(&c, claim);
    assert!(work.in_doubt);
    assert_eq!(work.unanswered_from, Some(commit(OLD)));
    assert_eq!((work.infra_failures, work.moved), (0, 0));
    let due = work.retry_at_ms.expect("the doubts' backoff");
    assert_eq!(due, c.state.clock_ms + infra_backoff_ms(2));
    assert_eq!(c.begin_merge(due - 1), None, "the backoff is kept");
    let prepare = c.begin_merge(due).expect("a Prepare re-makes the ref");
    assert_eq!(prepare.stage, MergeStage::Prepare);
    assert!(prepare.id > again.id);
    assert!(c.merge_in_doubt().is_some_and(|doubt| doubt.claim == claim));
}

#[test]
fn a_prepare_of_the_doubted_pair_goes_straight_to_land_and_landed_merges_the_work() {
    let (mut c, claim, _, prepare) = doubted_prepare();
    let land = next_after(&mut c, &prepare, &prepared());
    let pair = MergeStage::Land {
        base: commit(OLD),
        head: commit(HEAD),
    };
    assert_eq!(land.stage, pair, "that head was tested at the doubted Land");
    assert!(c.merge_in_doubt().is_some_and(|doubt| doubt.claim == claim));
    let effects = c.merge_outcome(land.id, &merged(), NOW + 20_000);
    let span = (Some(commit(OLD)), commit(HEAD));
    assert_eq!(merged_span(&effects), Some(span));
}

#[test]
fn a_prepare_of_the_same_base_and_another_head_is_tested_and_keeps_the_doubt() {
    let (mut c, claim, _, prepare) = doubted_prepare();
    let test = next_after(&mut c, &prepare, &prepared_other_head());
    let pair = MergeStage::Test {
        base: commit(OLD),
        head: commit(MOVED),
    };
    assert_eq!(test.stage, pair);
    let effects = c.merge_outcome(test.id, &failed_tests(), NOW + 20_000);
    assert!(!rejected(&effects), "a flaky test of the doubted work");
    assert!(work(&c, claim).in_doubt);
}

#[test]
fn nothing_rejects_the_work_in_doubt_however_often_the_merge_path_fails() {
    let (mut c, claim, _, mut prepare) = doubted_prepare();
    let mut now = NOW + 20_000;
    for failure in 1..=2 * MAX_INFRA_RETRIES + 2 {
        let outcome = if failure % 2 == 0 {
            MergeOutcome::Timeout {}
        } else {
            MergeOutcome::CandidateMissing {}
        };
        let effects = c.merge_outcome(prepare.id, &outcome, now);
        assert!(!rejected(&effects), "failure {failure}");
        let work = work(&c, claim);
        assert_eq!((work.in_doubt, work.infra_failures), (true, 0), "{failure}");
        let due = retry_at(&c, claim);
        let wait = due - c.state.clock_ms;
        assert!(
            wait <= infra_backoff_ms(MAX_INFRA_RETRIES),
            "{failure}: capped"
        );
        now = due;
        prepare = c.begin_merge(now).expect("a Prepare is due again");
    }
}

#[test]
fn a_restart_at_test_in_doubt_waits_the_doubts_backoff_counts_nothing_and_never_rejects() {
    let (mut c, claim, _, prepare) = doubted_prepare();
    next_after(&mut c, &prepare, &prepared_other_head());
    let mut now = NOW + 20_000;
    for restart in 1..=MAX_INFRA_RETRIES + 3 {
        let effects = c.recover_merge(now);
        assert!(!rejected(&effects), "restart {restart}");
        assert_eq!(
            c.begin_merge(now),
            None,
            "restart {restart}: it waits first"
        );
        let due = retry_at(&c, claim);
        assert!(due > now, "restart {restart}");
        assert!(
            due - c.state.clock_ms <= infra_backoff_ms(MAX_INFRA_RETRIES),
            "{restart}: capped"
        );
        assert_eq!(
            work(&c, claim).infra_failures,
            0,
            "restart {restart}: counts none"
        );
        now = due;
        let prepare = c.begin_merge(now).expect("a Prepare is due");
        assert_eq!(prepare.stage, MergeStage::Prepare, "restart {restart}");
        next_after(&mut c, &prepare, &prepared_other_head());
    }
    assert!(work(&c, claim).in_doubt);
}

#[test]
fn a_marker_stored_before_the_staged_landing_is_counted_and_rejected_at_the_bound() {
    let stored = store_written_before_the_staged_landing();
    let mut c: Coordinator = serde_json::from_str(&stored).unwrap();
    let claim = ClaimId(*c.state.claims.keys().next().unwrap());
    let mut now = NOW + 4;
    for restart in 1..=MAX_INFRA_RETRIES {
        let effects = c.recover_merge(now);
        assert!(!rejected(&effects), "restart {restart}");
        let wait = retry_at(&c, claim) - c.state.clock_ms;
        assert_eq!(wait, infra_backoff_ms(restart), "restart {restart}");
        assert!(work(&c, claim).in_doubt);
        now = retry_at(&c, claim);
        let prepare = c.begin_merge(now).expect("the claim in doubt goes first");
        assert_eq!(prepare.stage, MergeStage::Prepare);
    }
    let effects = c.recover_merge(now);
    assert!(rejected(&effects), "no late Land can put it on main");
    assert!(!c.has_merge_in_flight());
}

#[test]
fn a_rule_refusal_at_test_or_land_in_doubt_rejects_nothing() {
    let refused = MergeOutcome::Refused {
        kind: RuleKind::RequiredCheck,
    };
    let (mut c, claim, _, prepare) = doubted_prepare();
    let land = next_after(&mut c, &prepare, &prepared());
    let effects = c.merge_outcome(land.id, &refused, NOW + 20_000);
    assert!(!rejected(&effects), "at Land");
    assert!(work(&c, claim).in_doubt);
    let due = retry_at(&c, claim);
    let prepare = c.begin_merge(due).expect("a Prepare is due");
    let test = next_after(&mut c, &prepare, &prepared_other_head());
    let effects = c.merge_outcome(test.id, &refused, due + 1);
    assert!(!rejected(&effects), "at Test");
    assert!(work(&c, claim).in_doubt);
}

#[test]
fn a_prepare_on_another_base_ends_the_doubt_and_the_bound_applies_again() {
    let (mut c, claim, _, prepare) = doubted_prepare();
    let elsewhere = MergeOutcome::Prepared {
        base: commit(MOVED),
        head: commit(HEAD),
    };
    let effects = c.merge_outcome(prepare.id, &elsewhere, NOW + 20_000);
    assert_eq!(doubt_notices(&effects), vec![(claim, None)]);
    let work = work(&c, claim);
    assert_eq!((work.in_doubt, work.unanswered_from.clone()), (false, None));
    assert_eq!(work.doubted_land, None);
    assert_eq!(c.merge_in_doubt(), None);
    let mut test = c.begin_merge(NOW + 20_000).expect("tested again");
    assert!(matches!(test.stage, MergeStage::Test { .. }));
    let mut now = NOW + 20_001;
    for failure in 1..=MAX_INFRA_RETRIES + 1 {
        let effects = c.merge_outcome(test.id, &MergeOutcome::Timeout {}, now);
        if failure > MAX_INFRA_RETRIES {
            assert!(rejected(&effects), "the fourth rejects again");
            break;
        }
        assert!(!rejected(&effects), "failure {failure}");
        now = retry_at(&c, claim);
        let prepare = c.begin_merge(now).expect("a Prepare");
        test = next_after(&mut c, &prepare, &elsewhere);
    }
}

#[test]
fn already_merged_at_a_prepare_in_doubt_lands_the_work_from_the_leased_base() {
    let (mut c, _, _, prepare) = doubted_prepare();
    let already = MergeOutcome::AlreadyMerged { base: commit(HEAD) };
    let effects = c.merge_outcome(prepare.id, &already, NOW + 20_000);
    let span = (Some(commit(OLD)), commit(HEAD));
    assert_eq!(merged_span(&effects), Some(span));
}

#[test]
fn the_operator_can_resolve_a_doubt_at_prepare_or_test() {
    for landed_there in [true, false] {
        for at_test in [false, true] {
            let (mut c, claim, _, prepare) = doubted_prepare();
            if at_test {
                next_after(&mut c, &prepare, &prepared_other_head());
            }
            let journaled = Some((commit(OLD), commit(HEAD)));
            assert_eq!(c.journaled_land(claim), journaled, "test {at_test}");
            let effects = c
                .resolve_doubt(claim, landed_there, NOW + 30_000)
                .expect("a doubt at any stage");
            assert_eq!(landed(&effects), landed_there, "test {at_test}");
            if !landed_there {
                assert!(!work(&c, claim).in_doubt);
                assert_eq!(work(&c, claim).doubted_land, None);
                assert_eq!(c.merge_in_doubt(), None);
            }
        }
    }
}

#[test]
fn a_doubted_pair_survives_the_store() {
    let (c, claim, _, _) = doubted_prepare();
    let stored = serde_json::to_string(&c).unwrap();
    let back: Coordinator = serde_json::from_str(&stored).unwrap();
    let journaled = Some((commit(OLD), commit(HEAD)));
    assert_eq!(back.journaled_land(claim), journaled);
}

// ---- a doubt carries the lease of its own Land ----

/// A core whose claim is in doubt without a `Land` behind it: the marker was stored before the
/// staged landing, a restart found it, and its `Prepare` is due.
fn unstaged_doubt() -> (Coordinator, ClaimId, MergeDispatch) {
    let stored = store_written_before_the_staged_landing();
    let mut c: Coordinator = serde_json::from_str(&stored).unwrap();
    let claim = ClaimId(*c.state.claims.keys().next().unwrap());
    c.recover_merge(NOW + 4);
    let prepare = c
        .begin_merge(retry_at(&c, claim))
        .expect("the Prepare is due");
    assert_eq!(prepare.stage, MergeStage::Prepare);
    (c, claim, prepare)
}

/// A core as the build before the staged landing left it between two dispatches of a claim in
/// doubt: the flag set by an infrastructure failure, no merge in flight.
fn old_store_doubt() -> (Coordinator, ClaimId, MergeDispatch) {
    let stored = store_written_before_the_staged_landing();
    let mut c: Coordinator = serde_json::from_str(&stored).unwrap();
    let claim = ClaimId(*c.state.claims.keys().next().unwrap());
    c.state.merge_in_flight = None;
    let work = c
        .state
        .claims
        .get_mut(&claim.0)
        .and_then(|held| held.work.as_mut());
    let work = work.expect("submitted");
    work.in_doubt = true;
    work.unanswered_from = Some(commit(OLD));
    work.infra_failures = 1;
    let prepare = c
        .begin_merge(NOW + 4)
        .expect("the claim in doubt goes first");
    assert_eq!(prepare.stage, MergeStage::Prepare);
    (c, claim, prepare)
}

fn prepared_on_moved_main() -> MergeOutcome {
    MergeOutcome::Prepared {
        base: commit(MOVED),
        head: commit(HEAD),
    }
}

#[test]
fn a_doubt_ends_against_the_base_its_own_land_leased_not_the_first_baseline() {
    let (mut c, claim, prepare) = unstaged_doubt();
    let test = next_after(&mut c, &prepare, &prepared_on_moved_main());
    let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
    assert_eq!(stage_pair(&land), (commit(MOVED), commit(HEAD)));
    c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 20_000);
    let reconcile = due_reconcile(&mut c, claim);
    let at_lease = Reconciliation::NotLanded {
        main: Some(commit(MOVED)),
    };
    c.reconcile_outcome(reconcile.id, &at_lease, NOW + 40_000);
    let again = c
        .begin_merge(NOW + 40_000)
        .expect("the Land goes out again");
    let missing = MergeOutcome::CandidateMissing {};
    c.merge_outcome(again.id, &missing, NOW + 40_001);
    let prepare = c
        .begin_merge(retry_at(&c, claim))
        .expect("a Prepare is due");
    let effects = c.merge_outcome(prepare.id, &prepared_on_moved_main(), NOW + 90_000);
    assert_eq!(
        doubt_notices(&effects),
        vec![],
        "main is still at that Land's lease"
    );
    assert!(work(&c, claim).in_doubt);
    let land = c
        .begin_merge(NOW + 90_000)
        .expect("the doubted pair goes on");
    let pair = MergeStage::Land {
        base: commit(MOVED),
        head: commit(HEAD),
    };
    assert_eq!(land.stage, pair, "straight to Land: no test can reject it");
}

#[test]
fn a_later_land_in_the_same_doubt_replaces_the_pair_the_next_prepare_is_compared_with() {
    let (mut c, claim, _, prepare) = doubted_prepare();
    let test = next_after(&mut c, &prepare, &prepared_other_head());
    let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
    assert_eq!(stage_pair(&land), (commit(OLD), commit(MOVED)));
    c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 20_000);
    let replaced = Some((commit(OLD), commit(MOVED)));
    assert_eq!(
        work(&c, claim).doubted_land,
        replaced,
        "at the doubt itself"
    );
    let reconcile = due_reconcile(&mut c, claim);
    let at_base = Reconciliation::NotLanded {
        main: Some(commit(OLD)),
    };
    c.reconcile_outcome(reconcile.id, &at_base, NOW + 40_000);
    let again = c
        .begin_merge(NOW + 40_000)
        .expect("the Land goes out again");
    c.merge_outcome(again.id, &MergeOutcome::CandidateMissing {}, NOW + 40_001);
    let journaled = Some((commit(OLD), commit(MOVED)));
    assert_eq!(c.journaled_land(claim), journaled, "the latest Land's pair");
    let prepare = c
        .begin_merge(retry_at(&c, claim))
        .expect("a Prepare is due");
    let land = next_after(&mut c, &prepare, &prepared_other_head());
    let pair = MergeStage::Land {
        base: commit(OLD),
        head: commit(MOVED),
    };
    assert_eq!(
        land.stage, pair,
        "that head passed its Test at the later Land"
    );
}

#[test]
fn a_doubt_with_no_land_behind_it_rejects_work_whose_tests_fail() {
    for (mut c, claim, prepare) in [unstaged_doubt(), old_store_doubt()] {
        let test = next_after(&mut c, &prepare, &prepared());
        let effects = c.merge_outcome(test.id, &failed_tests(), NOW + 20_000);
        assert!(
            rejected(&effects),
            "claim {}: no late Land to fence",
            claim.0
        );
        assert!(!c.has_merge_in_flight());
    }
}

#[test]
fn a_doubt_with_no_land_behind_it_rejects_a_land_a_rule_refuses() {
    let refused = MergeOutcome::Refused {
        kind: RuleKind::RequiredCheck,
    };
    for (mut c, _, prepare) in [unstaged_doubt(), old_store_doubt()] {
        let test = next_after(&mut c, &prepare, &prepared());
        let land = next_after(&mut c, &test, &MergeOutcome::Tested {});
        let effects = c.merge_outcome(land.id, &refused, NOW + 20_000);
        assert!(rejected(&effects));
    }
}

#[test]
fn a_doubt_with_no_land_behind_it_rejects_work_whose_test_keeps_hanging() {
    for (mut c, claim, mut prepare) in [unstaged_doubt(), old_store_doubt()] {
        let mut rejected_at = None;
        for attempt in 1..=MAX_INFRA_RETRIES + 1 {
            let test = next_after(&mut c, &prepare, &prepared());
            let effects = c.merge_outcome(test.id, &MergeOutcome::Timeout {}, NOW + 20_000);
            if rejected(&effects) {
                rejected_at = Some(attempt);
                break;
            }
            let due = retry_at(&c, claim);
            prepare = c.begin_merge(due).expect("a Prepare is due");
        }
        assert!(rejected_at.is_some(), "claim {}: it must not loop", claim.0);
        assert!(!c.has_merge_in_flight());
    }
}

#[test]
fn a_doubt_with_no_land_behind_it_cut_off_at_test_is_counted_and_rejected_at_the_bound() {
    let (mut c, claim, prepare) = unstaged_doubt();
    next_after(&mut c, &prepare, &prepared());
    let mut now = NOW + 20_000;
    let mut rejected_at = None;
    for restart in 1..=MAX_INFRA_RETRIES + 1 {
        let effects = c.recover_merge(now);
        if rejected(&effects) {
            rejected_at = Some(restart);
            break;
        }
        let test = c.begin_merge(now).expect("the Test goes out again");
        assert!(
            matches!(test.stage, MergeStage::Test { .. }),
            "restart {restart}"
        );
        now += 1;
    }
    assert!(rejected_at.is_some(), "claim {}: it must not loop", claim.0);
}

#[test]
fn a_doubt_with_no_land_behind_it_is_not_the_operators_to_resolve_and_says_so() {
    let (mut c, claim, _) = unstaged_doubt();
    for landed in [true, false] {
        let refusal = c.resolve_doubt(claim, landed, NOW + 50).unwrap_err();
        assert_eq!(
            refusal.to_string(),
            format!("claim {} has no Land in doubt", claim.0)
        );
    }
    assert_eq!(c.journaled_land(claim), None);
}

#[test]
fn a_doubt_stored_without_a_pair_gets_one_when_its_land_is_sent_again() {
    let stored = include_str!("fixtures/doubt_without_pair.json");
    let mut c: Coordinator = serde_json::from_str(stored).unwrap();
    let claim = ClaimId(*c.state.claims.keys().next().unwrap());
    assert!(c.recover_merge(NOW + 10).is_empty());
    let reconcile = due_reconcile(&mut c, claim);
    let at_base = Reconciliation::NotLanded {
        main: Some(commit(OLD)),
    };
    c.reconcile_outcome(reconcile.id, &at_base, NOW + 11_000);
    let again = c
        .begin_merge(NOW + 11_000)
        .expect("the Land goes out again");
    let refused = MergeOutcome::Refused {
        kind: RuleKind::RequiredCheck,
    };
    let effects = c.merge_outcome(again.id, &refused, NOW + 11_001);
    assert!(
        !rejected(&effects),
        "a late copy of that Land may still land it"
    );
    let journaled = Some((commit(OLD), commit(HEAD)));
    assert_eq!(
        c.journaled_land(claim),
        journaled,
        "the pair is read at Prepare too"
    );
    let effects = c
        .resolve_doubt(claim, false, NOW + 12_000)
        .expect("the operator can");
    assert!(!landed(&effects) && !rejected(&effects));
    assert!(!work(&c, claim).in_doubt);
}

fn conflict() -> MergeOutcome {
    MergeOutcome::Conflict {
        files: vec!["a.rs".to_string()],
        total: None,
    }
}

#[test]
fn a_refused_prepare_in_doubt_asks_a_reconcile_paced_by_the_backoff_and_does_not_reject() {
    let (mut c, claim, _, prepare) = doubted_prepare();
    let effects = c.merge_outcome(prepare.id, &MergeOutcome::CommitNotInFork {}, NOW + 20_000);
    assert!(!rejected(&effects), "the doubted ref can still be landed");
    assert_eq!(doubt_notices(&effects), vec![]);
    let work = work(&c, claim);
    assert_eq!(
        (work.in_doubt, work.moved, work.infra_failures),
        (true, 0, 0)
    );
    assert_eq!(c.begin_merge(NOW + 20_000), None, "paced, not at once");
    let due = retry_at(&c, claim);
    assert!(due > NOW + 20_000);
    assert!(due - c.state.clock_ms <= infra_backoff_ms(MAX_INFRA_RETRIES));
    let reconcile = c.begin_merge(due).expect("a Reconcile is due");
    assert!(matches!(reconcile.stage, MergeStage::Reconcile { .. }));
    assert_eq!(stage_pair(&reconcile), (commit(OLD), commit(HEAD)));
}

#[test]
fn a_refused_prepare_in_doubt_with_main_at_the_base_sends_the_land_again_for_ever() {
    let (mut c, claim, _, mut prepare) = doubted_prepare();
    let at_base = Reconciliation::NotLanded {
        main: Some(commit(OLD)),
    };
    let mut now = NOW + 20_000;
    for pass in 1..=40 {
        let effects = c.merge_outcome(prepare.id, &MergeOutcome::CommitNotInFork {}, now);
        assert!(!rejected(&effects), "pass {pass}");
        let due = retry_at(&c, claim);
        let reconcile = c.begin_merge(due).expect("a Reconcile is due");
        assert!(matches!(reconcile.stage, MergeStage::Reconcile { .. }));
        c.reconcile_outcome(reconcile.id, &at_base, due);
        let land = c.begin_merge(due).expect("the Land goes out again");
        assert!(matches!(land.stage, MergeStage::Land { .. }), "pass {pass}");
        c.merge_outcome(land.id, &MergeOutcome::CandidateMissing {}, due + 1);
        now = retry_at(&c, claim);
        prepare = c.begin_merge(now).expect("a Prepare is due");
        assert_eq!(prepare.stage, MergeStage::Prepare, "pass {pass}");
        let work = work(&c, claim);
        assert_eq!(
            (work.in_doubt, work.moved, work.infra_failures),
            (true, 0, 0)
        );
    }
}

#[test]
fn a_refused_prepare_in_doubt_with_main_elsewhere_ends_the_doubt_and_the_next_prepare_rejects() {
    let (mut c, claim, _, prepare) = doubted_prepare();
    let effects = c.merge_outcome(prepare.id, &conflict(), NOW + 20_000);
    assert!(!rejected(&effects));
    let reconcile = c.begin_merge(retry_at(&c, claim)).expect("a Reconcile");
    let elsewhere = Reconciliation::NotLanded {
        main: Some(commit(MOVED)),
    };
    let effects = c.reconcile_outcome(reconcile.id, &elsewhere, NOW + 40_000);
    assert_eq!(doubt_notices(&effects), vec![(claim, None)]);
    let work = work(&c, claim);
    assert_eq!((work.in_doubt, work.moved), (false, 1));
    assert_eq!(work.doubted_land, None);
    let again = c.begin_merge(NOW + 40_000).expect("a fresh Prepare");
    assert_eq!(again.stage, MergeStage::Prepare);
    let effects = c.merge_outcome(again.id, &conflict(), NOW + 40_001);
    assert!(rejected(&effects), "no late Land can put it on main now");
}

#[test]
fn a_refused_prepare_in_doubt_whose_head_a_late_land_put_on_main_lands_the_work() {
    let (mut c, claim, _, prepare) = doubted_prepare();
    c.merge_outcome(prepare.id, &conflict(), NOW + 20_000);
    let reconcile = c.begin_merge(retry_at(&c, claim)).expect("a Reconcile");
    let effects = c.reconcile_outcome(reconcile.id, &Reconciliation::Landed, NOW + 40_000);
    assert!(landed(&effects));
}

#[test]
fn main_moved_at_prepare_or_test_in_doubt_asks_a_reconcile_before_the_doubt_ends() {
    for at_test in [false, true] {
        for landed_there in [true, false] {
            let (mut c, claim, _, prepare) = doubted_prepare();
            let flight = if at_test {
                next_after(&mut c, &prepare, &prepared_other_head())
            } else {
                prepare
            };
            let effects = c.merge_outcome(flight.id, &MergeOutcome::MainMoved {}, NOW + 20_000);
            let case = format!("test {at_test}, landed {landed_there}");
            assert_eq!(doubt_notices(&effects), vec![], "{case}: not ended yet");
            assert_eq!(work(&c, claim).moved, 0, "{case}");
            let reconcile = c.begin_merge(NOW + 20_000).expect("a Reconcile at once");
            assert_eq!(
                stage_pair(&reconcile),
                (commit(OLD), commit(HEAD)),
                "{case}"
            );
            assert!(
                matches!(reconcile.stage, MergeStage::Reconcile { .. }),
                "{case}"
            );
            assert!(reconcile.id > flight.id, "{case}: a new dispatch id");
            if landed_there {
                let effects =
                    c.reconcile_outcome(reconcile.id, &Reconciliation::Landed, NOW + 30_000);
                assert!(landed(&effects), "{case}: the late copy landed it");
            } else {
                let elsewhere = Reconciliation::NotLanded {
                    main: Some(commit(MOVED)),
                };
                let effects = c.reconcile_outcome(reconcile.id, &elsewhere, NOW + 30_000);
                assert_eq!(doubt_notices(&effects), vec![(claim, None)], "{case}");
                assert_eq!(
                    (work(&c, claim).in_doubt, work(&c, claim).moved),
                    (false, 1),
                    "{case}"
                );
                assert_eq!(work(&c, claim).doubted_land, None, "{case}");
            }
        }
    }
}

#[test]
fn main_moved_at_prepare_to_a_doubt_with_no_pair_keeps_the_doubt_and_counts_the_move() {
    let (mut c, claim, prepare) = unstaged_doubt();
    let effects = c.merge_outcome(prepare.id, &MergeOutcome::MainMoved {}, NOW + 20_000);
    assert_eq!(doubt_notices(&effects), vec![]);
    assert_eq!((work(&c, claim).in_doubt, work(&c, claim).moved), (true, 1));
}

#[test]
fn a_wake_before_a_doubted_prepare_is_due_does_not_recover_it_as_cut_off() {
    let (mut c, claim, again) = resent_land();
    c.merge_outcome(again.id, &MergeOutcome::CandidateMissing {}, NOW + 10_001);
    let due = retry_at(&c, claim);
    for wake in [due - 1, due, due + 50_000] {
        assert!(c.recover_merge(wake).is_empty(), "wake {wake}");
        assert_eq!(
            retry_at(&c, claim),
            due,
            "wake {wake}: the wait is not renewed"
        );
    }
    let prepare = c
        .begin_merge(due)
        .expect("the Prepare goes out when it is due");
    assert_eq!(prepare.stage, MergeStage::Prepare);
    assert!(
        c.recover_merge(due + 1).is_empty(),
        "a restart after the dispatch"
    );
    assert!(retry_at(&c, claim) > due, "cut off: it waits again");
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
    // A resent Land found main elsewhere, so the work went back to Prepare.
    let (mut c, _, again) = resent_land();
    c.merge_outcome(again.id, &MergeOutcome::MainMoved {}, NOW + 10_001);
    assert!(welcome_doubts(&mut c, "a", NOW + 10_002).is_empty());
}

// ---- the head the operator checks, and who may be resolved ----

/// A second agent's claim on `src/b.rs`, submitted and queued behind the merge in flight.
fn submit_another(c: &mut Coordinator) -> ClaimId {
    let scope = ScopeClaim {
        scope: Scope::File {
            path: "src/b.rs".into(),
        },
        mode: crate::protocol::Mode::EditBody,
    };
    let claim = ClientMsg::Claim {
        req: RequestId(1),
        intent: Intent {
            summary: "other".into(),
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes: vec![scope.clone()],
        on_conflict: OnConflict::Fail,
    };
    let who = AgentId("b".into());
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
        fork_commit: commit(&"cd".repeat(20)),
        touched: vec![scope],
        decisions: DecisionRecord {
            evidence: vec!["tests passed".into()],
            ..DecisionRecord::default()
        },
    };
    c.handle(&who, submit, NOW);
    claim
}

#[test]
fn the_journaled_land_is_the_base_and_head_of_the_claim_whose_land_is_in_flight() {
    let (c, claim, _) = at_test();
    assert_eq!(c.journaled_land(claim), None, "a Test has no Land yet");
    let (mut c, claim, _) = at_land();
    let journaled = Some((commit(OLD), commit(HEAD)));
    assert_eq!(c.journaled_land(claim), journaled);
    assert_eq!(
        c.journaled_land(ClaimId(claim.0 + 1)),
        None,
        "another claim"
    );
    c.merge_outcome(
        c.merge_flight_id(claim),
        &MergeOutcome::ServiceUnavailable,
        NOW + 4,
    );
    assert_eq!(c.journaled_land(claim), journaled, "and at its Reconcile");
    let (idle, claim, _) = submitted();
    assert_eq!(idle.journaled_land(claim), None, "a Prepare has none");
}

#[test]
fn the_operator_cannot_resolve_another_claim_than_the_one_in_doubt() {
    let (mut c, claim, _) = in_doubt_after_unknown();
    let other = submit_another(&mut c);
    assert_ne!(other, claim);
    let before = serde_json::to_string(&c).unwrap();
    for landed in [true, false] {
        let refusal = c.resolve_doubt(other, landed, NOW + 50).unwrap_err();
        assert_eq!(refusal.claim, other, "landed {landed}");
    }
    assert_eq!(
        before,
        serde_json::to_string(&c).unwrap(),
        "nothing changed"
    );
    assert!(c.merge_in_doubt().is_some_and(|doubt| doubt.claim == claim));
}

#[test]
fn a_claim_without_its_submission_or_its_work_is_not_resolved_or_reconciled() {
    for submitted_gone in [true, false] {
        let (mut c, claim, _) = in_doubt_after_unknown();
        let held = c.state.claims.get_mut(&claim.0).unwrap();
        if submitted_gone {
            held.submitted = None;
        } else {
            held.work = None;
        }
        let before = serde_json::to_string(&c).unwrap();
        assert!(c.resolve_doubt(claim, true, NOW + 50).is_err());
        assert_eq!(before, serde_json::to_string(&c).unwrap());
        let dispatch = c.merge_flight_id(claim);
        let effects = c.reconcile_outcome(dispatch, &Reconciliation::Landed, NOW + 10_000);
        assert!(
            effects.is_empty(),
            "submitted gone {submitted_gone}: {effects:?}"
        );
    }
}

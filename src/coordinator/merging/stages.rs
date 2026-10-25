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

#[test]
fn a_land_that_did_not_finish_leaves_the_claim_in_doubt_from_main_as_dispatched() {
    let (mut c, claim, land) = at_land();
    c.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, NOW + 4);
    let work = work(&c, claim);
    assert!(work.in_doubt);
    assert_eq!(work.infra_failures, 1);
    assert_eq!(
        work.unanswered_from, c.state.head,
        "main as the Land was dispatched from"
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

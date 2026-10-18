//! Calls: durable jobs the host hands to the lander, the test runner or the git host, finished by
//! reconciliation, not by a request held open (`docs/proposals/real-use.md`, "Calls").
//!
//! `Call` and `CallAnswer` are persisted, so both round-trip through serde. Free text in an
//! answer (file names) is untrusted data: it is carried, never acted on.

use serde::{Deserialize, Serialize};

use crate::protocol::{ClaimId, CommitId, ScopeClaim};

/// Names a call. The adapter echoes it in `Replied` and `CallLost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub u64);

/// Unique to one dispatch, never reused. A retry of the same landing keeps it (invariant I7: an
/// answer for any other dispatch id changes nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DispatchId(pub u64);

/// An outstanding call: persisted in the batch that creates it, and emitted as `Output::Call`
/// only after that batch commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub id: CallId,
    pub dispatch_id: DispatchId,
    pub request: Request,
    /// Persisted per call. An expired deadline before `Land` is one infrastructure failure; after
    /// `Land` it starts `Reconcile`.
    pub deadline_ms: u64,
}

/// What the call asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Fetch the fork commit, verify it, rebase it deterministically, check coverage against the
    /// claim's scopes and the gate paths, and return the head. No repo code runs.
    Prepare {
        claim: ClaimId,
        fork_commit: CommitId,
        scopes: Vec<ScopeClaim>,
    },
    /// Run the tests on the prepared head.
    Test { base: CommitId, head: CommitId },
    /// Compare-and-swap main from `base` to `head`.
    Land { base: CommitId, head: CommitId },
    /// Verify a challenged assumption: `commit` (or the fork's head, if `None`) against `before`
    /// and then against `main`.
    Trial {
        commit: Option<CommitId>,
        before: CommitId,
        main: CommitId,
    },
    /// Read main's head and, for each of `submitted`, whether it is an ancestor of main.
    ReadHead { submitted: Vec<CommitId> },
    /// For the dispatch id of the call: is `head` an ancestor of main, and where is main.
    Reconcile { head: CommitId },
}

/// How a call ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallAnswer {
    Prepared {
        base: CommitId,
        head: CommitId,
    },
    Tested,
    Landed {
        head: CommitId,
    },
    Trialled {
        before: Option<TrialResult>,
        after: Option<TrialResult>,
    },
    Head {
        main: Option<CommitId>,
        landed: Vec<CommitId>,
    },
    Reconciled {
        main: CommitId,
        landed: bool,
    },
    /// A fact about the work, never retried.
    Refused {
        refusal: Refusal,
    },
    /// The call may or may not have taken effect: a stale `beforeOid`, a reply lost after a
    /// push, a lost call. The core decides what follows.
    Unknown,
}

/// Why the work was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Refusal {
    /// Replaying the commit onto main stopped with these files unmerged.
    Conflict {
        files: Vec<String>,
    },
    /// The rebased commit changes `total` files the claim does not cover (invariant 11).
    Uncovered {
        total: u64,
    },
    /// The rebased commit changes the gate (`tessel.toml` or CI configuration).
    GateChanged,
    TestsFailed,
    CommitNotInFork,
}

/// One run of a trial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialResult {
    Clean,
    Conflict,
    TestsFailed,
    NothingToTest,
    CommitNotInFork,
    MainUnreachable,
    /// The attempt did not finish: counts for or against nothing.
    Infrastructure,
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::protocol::{Mode, Scope};

    fn commit() -> impl Strategy<Value = CommitId> {
        "[0-9a-f]{1,40}".prop_map(CommitId)
    }

    fn scope() -> impl Strategy<Value = ScopeClaim> {
        ("[a-z/._]{0,12}", any::<bool>()).prop_map(|(path, dir)| ScopeClaim {
            scope: if dir {
                Scope::Dir { path }
            } else {
                Scope::File { path }
            },
            mode: Mode::EditBody,
        })
    }

    fn request() -> impl Strategy<Value = Request> {
        prop_oneof![
            (any::<u64>(), commit(), prop::collection::vec(scope(), 0..3)).prop_map(
                |(claim, fork_commit, scopes)| Request::Prepare {
                    claim: ClaimId(claim),
                    fork_commit,
                    scopes,
                }
            ),
            (commit(), commit()).prop_map(|(base, head)| Request::Test { base, head }),
            (commit(), commit()).prop_map(|(base, head)| Request::Land { base, head }),
            (prop::option::of(commit()), commit(), commit()).prop_map(|(commit, before, main)| {
                Request::Trial {
                    commit,
                    before,
                    main,
                }
            }),
            prop::collection::vec(commit(), 0..4)
                .prop_map(|submitted| Request::ReadHead { submitted }),
            commit().prop_map(|head| Request::Reconcile { head }),
        ]
    }

    fn call() -> impl Strategy<Value = Call> {
        (any::<u64>(), any::<u64>(), request(), any::<u64>()).prop_map(
            |(id, dispatch_id, request, deadline_ms)| Call {
                id: CallId(id),
                dispatch_id: DispatchId(dispatch_id),
                request,
                deadline_ms,
            },
        )
    }

    fn trial_result() -> impl Strategy<Value = TrialResult> {
        prop_oneof![
            Just(TrialResult::Clean),
            Just(TrialResult::Conflict),
            Just(TrialResult::TestsFailed),
            Just(TrialResult::NothingToTest),
            Just(TrialResult::CommitNotInFork),
            Just(TrialResult::MainUnreachable),
            Just(TrialResult::Infrastructure),
        ]
    }

    fn refusal() -> impl Strategy<Value = Refusal> {
        prop_oneof![
            prop::collection::vec(".{0,12}", 0..3).prop_map(|files| Refusal::Conflict { files }),
            any::<u64>().prop_map(|total| Refusal::Uncovered { total }),
            Just(Refusal::GateChanged),
            Just(Refusal::TestsFailed),
            Just(Refusal::CommitNotInFork),
        ]
    }

    fn answer() -> impl Strategy<Value = CallAnswer> {
        prop_oneof![
            (commit(), commit()).prop_map(|(base, head)| CallAnswer::Prepared { base, head }),
            Just(CallAnswer::Tested),
            commit().prop_map(|head| CallAnswer::Landed { head }),
            (
                prop::option::of(trial_result()),
                prop::option::of(trial_result())
            )
                .prop_map(|(before, after)| CallAnswer::Trialled { before, after }),
            (
                prop::option::of(commit()),
                prop::collection::vec(commit(), 0..4)
            )
                .prop_map(|(main, landed)| CallAnswer::Head { main, landed }),
            (commit(), any::<bool>())
                .prop_map(|(main, landed)| CallAnswer::Reconciled { main, landed }),
            refusal().prop_map(|refusal| CallAnswer::Refused { refusal }),
            Just(CallAnswer::Unknown),
        ]
    }

    proptest! {
        #[test]
        fn an_outstanding_call_round_trips_through_serde(call in call()) {
            let json = serde_json::to_string(&call).unwrap();
            prop_assert_eq!(serde_json::from_str::<Call>(&json).unwrap(), call);
        }

        #[test]
        fn every_call_answer_round_trips_through_serde(answer in answer()) {
            let json = serde_json::to_string(&answer).unwrap();
            prop_assert_eq!(serde_json::from_str::<CallAnswer>(&json).unwrap(), answer);
        }
    }

    #[test]
    fn unknown_has_its_own_stored_form() {
        assert_eq!(
            serde_json::to_string(&CallAnswer::Unknown).unwrap(),
            r#"{"kind":"unknown"}"#
        );
    }
}

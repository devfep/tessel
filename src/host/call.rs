//! Calls: durable jobs the host hands to the lander, the test runner or the git host, finished by
//! reconciliation, not by a request held open (`docs/proposals/real-use.md`, "Calls").
//!
//! `Call` and `CallAnswer` round-trip through serde, so an adapter may carry them or keep them
//! (the stored state holds the merge marker, not the call; see `calls.rs`). Free text in an answer
//! (file names) is untrusted data: it is carried, never acted on.

use serde::{Deserialize, Serialize};

use crate::merge::RuleKind;
use crate::protocol::{AgentId, ClaimId, CommitId, ScopeClaim};

/// Names a call. The adapter echoes it in `Replied` and `CallLost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub u64);

/// Unique to one dispatch, never reused. A retry of the same landing keeps it (invariant I7: an
/// answer for any other dispatch id changes nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DispatchId(pub u64);

/// An outstanding call: held in memory by the host, and emitted as `Output::Call` only after the
/// batch that starts it has committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub id: CallId,
    pub dispatch_id: DispatchId,
    pub request: Request,
    /// Held per call. An expired deadline before `Land` is one infrastructure failure; after
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
        /// Whose fork `fork_commit` is on.
        agent: AgentId,
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
        /// Whose fork `commit` is on, and whose head is tried when `commit` is `None`.
        agent: AgentId,
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
    /// push, a lost call. A late `Land` that finds main moved also arrives as `Unknown`; the core
    /// then reconciles. The core decides what follows. A stored or received answer of a kind this
    /// build does not know also reads as `Unknown`, the one answer that never decides an outcome.
    #[serde(other)]
    Unknown,
}

/// Why the work was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Refusal {
    /// Replaying the commit onto main stopped with these files unmerged.
    Conflict {
        files: Vec<String>,
        /// How many files conflicted, when `files` was cut. Absent: `files` is all of them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
    },
    /// The rebased commit changes `total` files the claim does not cover (invariant 11).
    Uncovered {
        total: u64,
    },
    /// The rebased commit changes the gate (`tessel.toml` or CI configuration).
    GateChanged,
    /// The repo's tests ran and did not pass.
    TestsFailed {
        failure: TestFailure,
    },
    CommitNotInFork,
    /// The git host refused the update by a rule of its own (a protected ref, a missing required
    /// check). GitHub types these (`UNPROCESSABLE`, `Branch 'refs/heads/main' is protected: ...`,
    /// `docs/proposals/github-spike.md` section 2), unlike a stale `beforeOid`, which is `Unknown`.
    Rule {
        /// Which rule, as a fixed word: the agent reads one fixed reason per kind. Named `rule`
        /// because `kind` is this enum's tag. An answer stored without one reads as `Other`.
        #[serde(default)]
        rule: RuleKind,
        /// The host's message, untrusted. The lander must cut it to
        /// `REASON_OUTPUT_MAX_CHARS` characters; the host logs it, bounded and quoted again, and
        /// never sends it to an agent.
        message: String,
    },
}

/// The failed test step: its exit code and the tails of its output, as `merge::StepExit` carries
/// them. The output is untrusted text from the repo's tests. A rejection reason built from it must
/// bound it (`REASON_OUTPUT_MAX_LINES`, `REASON_OUTPUT_LINE_MAX_CHARS`,
/// `REASON_OUTPUT_MAX_CHARS`) and quote it, as `merge::tests_reason` does for `StepExit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFailure {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
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
    use crate::protocol::{Mode, Scope, SymbolId};

    fn commit() -> impl Strategy<Value = CommitId> {
        "[0-9a-f]{1,40}".prop_map(CommitId)
    }

    fn agent() -> impl Strategy<Value = AgentId> {
        "[a-z0-9-]{1,12}".prop_map(AgentId)
    }

    fn mode() -> impl Strategy<Value = Mode> {
        prop_oneof![
            Just(Mode::Depend),
            Just(Mode::EditBody),
            Just(Mode::EditSignature),
            Just(Mode::Create),
        ]
    }

    fn scope() -> impl Strategy<Value = ScopeClaim> {
        let path = "[a-z/._]{0,12}";
        let scope = prop_oneof![
            path.prop_map(|path| Scope::Dir { path }),
            path.prop_map(|path| Scope::File { path }),
            (path, "[a-z:]{1,12}").prop_map(|(path, qualified_name)| Scope::Symbol(SymbolId {
                path,
                qualified_name
            })),
        ];
        (scope, mode()).prop_map(|(scope, mode)| ScopeClaim { scope, mode })
    }

    fn request() -> impl Strategy<Value = Request> {
        prop_oneof![
            (
                any::<u64>(),
                agent(),
                commit(),
                prop::collection::vec(scope(), 0..3)
            )
                .prop_map(|(claim, agent, fork_commit, scopes)| Request::Prepare {
                    claim: ClaimId(claim),
                    agent,
                    fork_commit,
                    scopes,
                }),
            (commit(), commit()).prop_map(|(base, head)| Request::Test { base, head }),
            (commit(), commit()).prop_map(|(base, head)| Request::Land { base, head }),
            (agent(), prop::option::of(commit()), commit(), commit()).prop_map(
                |(agent, commit, before, main)| {
                    Request::Trial {
                        agent,
                        commit,
                        before,
                        main,
                    }
                }
            ),
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

    fn rule_kind() -> impl Strategy<Value = RuleKind> {
        prop_oneof![
            Just(RuleKind::NoLander),
            Just(RuleKind::ProtectedRef),
            Just(RuleKind::RequiredCheck),
            Just(RuleKind::ForkMissing),
            Just(RuleKind::GateInvalid),
            Just(RuleKind::Other),
        ]
    }

    fn refusal() -> impl Strategy<Value = Refusal> {
        prop_oneof![
            (
                prop::collection::vec(".{0,12}", 0..3),
                prop::option::of(any::<u64>())
            )
                .prop_map(|(files, total)| Refusal::Conflict { files, total }),
            any::<u64>().prop_map(|total| Refusal::Uncovered { total }),
            Just(Refusal::GateChanged),
            (any::<i64>(), ".{0,40}", ".{0,40}").prop_map(|(exit_code, stdout, stderr)| {
                Refusal::TestsFailed {
                    failure: TestFailure {
                        exit_code,
                        stdout,
                        stderr,
                    },
                }
            }),
            Just(Refusal::CommitNotInFork),
            (rule_kind(), ".{0,40}").prop_map(|(rule, message)| Refusal::Rule { rule, message }),
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
    fn an_answer_of_an_unknown_kind_reads_as_unknown() {
        let answer: CallAnswer = serde_json::from_str(r#"{"kind":"from_a_newer_build"}"#).unwrap();
        assert_eq!(answer, CallAnswer::Unknown);
    }

    #[test]
    fn a_rule_refusal_stored_without_a_rule_reads_as_other() {
        let old: Refusal =
            serde_json::from_str(r#"{"kind":"rule","message":"protected"}"#).unwrap();
        assert_eq!(
            old,
            Refusal::Rule {
                rule: RuleKind::Other,
                message: "protected".to_string()
            }
        );
        let typed = Refusal::Rule {
            rule: RuleKind::NoLander,
            message: "m".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&typed).unwrap(),
            r#"{"kind":"rule","rule":"no_lander","message":"m"}"#
        );
    }

    #[test]
    fn unknown_has_its_own_stored_form() {
        assert_eq!(
            serde_json::to_string(&CallAnswer::Unknown).unwrap(),
            r#"{"kind":"unknown"}"#
        );
    }
}

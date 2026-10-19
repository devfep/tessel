//! The calls the wake issues, and what their answers mean to the core (R21 to R23, R26, R38).
//!
//! Until the core stages its landing (P1-CORE-STAGED-LANDING), a merge is one call: `Prepare`,
//! which stands for the steward's one-call `/merge`. It is answered `Prepared { base, head }`
//! once main is at `head`, or `Refused`. The staged `Test` and `Land` calls are not issued yet,
//! though an answer to either counts as a merge answer. `Call` carries no evidence record, so the
//! `MergeRecord` the steward receives today is not sent.
//!
//! An answer that is not of the kind the call asks for, `Unknown` included, is the service being
//! unavailable: infrastructure, retried with backoff (R26). A head answer that is not a head is
//! `None` (R23).

use crate::coordinator::{MergeDispatch, VerifyDispatch};
use crate::host::call::{
    Call, CallAnswer, CallId, DispatchId, Refusal, Request, TestFailure, TrialResult,
};
use crate::host::wake::WakeStep;
use crate::host::{Host, Job, Output};
use crate::merge::{MergeOutcome, StepExit, TrialOutcome, TrialReport, STEWARD_CALL_TIMEOUT_MS};
use crate::protocol::CommitId;
use crate::shell::Work;

/// How long a head read waits: one ref, not a test run (R38).
pub const HEAD_CALL_TIMEOUT_MS: u64 = 30_000;

/// A merge, as the `Prepare` call that stands for it.
pub fn merge_call(id: CallId, dispatch: &MergeDispatch, now_ms: u64) -> Call {
    Call {
        id,
        dispatch_id: DispatchId(dispatch.id),
        request: Request::Prepare {
            claim: dispatch.claim,
            agent: dispatch.agent.clone(),
            fork_commit: dispatch.fork_commit.clone(),
            scopes: dispatch.scopes.clone(),
        },
        deadline_ms: now_ms.saturating_add(STEWARD_CALL_TIMEOUT_MS),
    }
}

pub fn trial_call(id: CallId, dispatch: &VerifyDispatch, now_ms: u64) -> Call {
    Call {
        id,
        dispatch_id: DispatchId(dispatch.id),
        request: Request::Trial {
            agent: dispatch.agent.clone(),
            commit: dispatch.commit.clone(),
            before: dispatch.before.clone(),
            main: dispatch.main.clone(),
        },
        deadline_ms: now_ms.saturating_add(STEWARD_CALL_TIMEOUT_MS),
    }
}

/// A head read changes nothing stored, so it has no core dispatch: its dispatch id is its call id.
pub fn head_call(id: CallId, now_ms: u64) -> Call {
    Call {
        id,
        dispatch_id: DispatchId(id.0),
        request: Request::ReadHead {
            submitted: Vec::new(),
        },
        deadline_ms: now_ms.saturating_add(HEAD_CALL_TIMEOUT_MS),
    }
}

/// The wake step that follows the step that issued `request`.
pub fn resume_after(request: &Request) -> WakeStep {
    match request {
        Request::Prepare { .. } | Request::Test { .. } | Request::Land { .. } => {
            WakeStep::Verification
        }
        Request::Trial { .. } => WakeStep::HeadRead,
        Request::ReadHead { .. } | Request::Reconcile { .. } => WakeStep::Finish,
    }
}

/// What the answer to a merge call means to the core.
pub fn merge_outcome_of(answer: &CallAnswer) -> MergeOutcome {
    match answer {
        CallAnswer::Prepared { base, head } => MergeOutcome::Merged {
            base: base.clone(),
            head: head.clone(),
        },
        CallAnswer::Refused { refusal } => refusal_outcome(refusal),
        CallAnswer::Tested
        | CallAnswer::Landed { .. }
        | CallAnswer::Trialled { .. }
        | CallAnswer::Head { .. }
        | CallAnswer::Reconciled { .. }
        | CallAnswer::Unknown => MergeOutcome::ServiceUnavailable,
    }
}

fn refusal_outcome(refusal: &Refusal) -> MergeOutcome {
    match refusal {
        Refusal::Conflict { files } => MergeOutcome::Conflict {
            files: files.clone(),
        },
        Refusal::Uncovered { total } => MergeOutcome::Uncovered { total: *total },
        Refusal::GateChanged => MergeOutcome::GateChanged {},
        Refusal::TestsFailed { failure } => MergeOutcome::TestsFailed {
            result: step_exit(failure),
        },
        Refusal::CommitNotInFork => MergeOutcome::CommitNotInFork {},
        Refusal::Rule { .. } => MergeOutcome::Refused,
    }
}

fn step_exit(failure: &TestFailure) -> StepExit {
    StepExit {
        exit_code: failure.exit_code,
        stdout: failure.stdout.clone(),
        stderr: failure.stderr.clone(),
    }
}

/// What the answer to a trial call means to the core.
pub fn trial_report_of(answer: &CallAnswer) -> TrialReport {
    match answer {
        CallAnswer::Trialled { before, after } => TrialReport {
            before: before.map(trial_outcome),
            after: after.map(trial_outcome),
        },
        CallAnswer::Refused { .. } => TrialReport::stopped(TrialOutcome::Refused),
        CallAnswer::Prepared { .. }
        | CallAnswer::Tested
        | CallAnswer::Landed { .. }
        | CallAnswer::Head { .. }
        | CallAnswer::Reconciled { .. }
        | CallAnswer::Unknown => TrialReport::stopped(TrialOutcome::ServiceUnavailable),
    }
}

fn trial_outcome(result: TrialResult) -> TrialOutcome {
    match result {
        TrialResult::Clean => TrialOutcome::Clean {},
        TrialResult::Conflict => TrialOutcome::Conflict {},
        TrialResult::TestsFailed => TrialOutcome::TestsFailed {},
        TrialResult::NothingToTest => TrialOutcome::NothingToTest {},
        TrialResult::CommitNotInFork => TrialOutcome::CommitNotInFork {},
        TrialResult::MainUnreachable => TrialOutcome::MainUnreachable {},
        TrialResult::Infrastructure => TrialOutcome::ServiceUnavailable,
    }
}

/// The head a head answer carries, if it carries one.
pub fn head_of(answer: &CallAnswer) -> Option<CommitId> {
    match answer {
        CallAnswer::Head { main, .. } => main.clone(),
        CallAnswer::Prepared { .. }
        | CallAnswer::Tested
        | CallAnswer::Landed { .. }
        | CallAnswer::Trialled { .. }
        | CallAnswer::Reconciled { .. }
        | CallAnswer::Refused { .. }
        | CallAnswer::Unknown => None,
    }
}

/// The answer of a call, to be applied once the core is loaded.
pub struct AnswerJob {
    pub call: Call,
    pub answer: CallAnswer,
}

impl Host {
    /// A call was answered, or lost, which reads as `Unknown`. The wake that issued it resumes
    /// once the answer is stored.
    pub(super) fn answered(&mut self, id: CallId, answer: CallAnswer, out: &mut Vec<Output>) {
        let Some(call) = self.calls.remove(&id) else {
            out.push(self.failure("apply call answer", "the host waits on no such call"));
            return;
        };
        self.waking = true;
        self.run(Job::Answer(AnswerJob { call, answer }), out);
    }

    /// Apply an answer to the core (R19: the core was reloaded first if it was dropped), store
    /// it, then resume the wake. An answer for any dispatch but the one in flight changes
    /// nothing (I7); the core decides.
    pub(super) fn run_answer(&mut self, job: AnswerJob, out: &mut Vec<Output>) {
        let AnswerJob { call, answer } = job;
        let now_ms = self.now_ms;
        let waits = self.waits();
        let dispatch = call.dispatch_id.0;
        let resume = resume_after(&call.request);
        let (operation, prepared) = match call.request {
            Request::Prepare { .. } | Request::Test { .. } | Request::Land { .. } => {
                let outcome = merge_outcome_of(&answer);
                if outcome == MergeOutcome::ServiceUnavailable {
                    out.push(self.failure("steward merge call", "no usable answer"));
                }
                let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
                    core.merge_outcome(dispatch, &outcome, now_ms)
                });
                ("apply merge outcome", prepared)
            }
            Request::Trial { .. } => {
                let report = trial_report_of(&answer);
                if report == TrialReport::stopped(TrialOutcome::ServiceUnavailable) {
                    out.push(self.failure("steward trial call", "no usable answer"));
                }
                let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
                    core.verification_outcome(dispatch, &report, now_ms)
                });
                ("apply verification outcome", prepared)
            }
            Request::ReadHead { .. } => {
                let Some(read) = self.reading.take() else {
                    out.push(self.failure("apply trunk head", "no head read was started"));
                    self.wake_failed(out);
                    return;
                };
                let head = head_of(&answer);
                if head.is_none() {
                    out.push(self.failure("steward head call", "no head in the answer"));
                }
                let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
                    core.trunk_read(&read, head, now_ms)
                });
                ("apply trunk head", prepared)
            }
            Request::Reconcile { .. } => {
                out.push(self.failure("apply call answer", "reconcile is not issued yet"));
                self.wake_failed(out);
                return;
            }
        };
        self.commit_unsent(
            operation,
            prepared,
            Vec::new(),
            Some(Job::Wake(resume)),
            out,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::MergeRecord;
    use crate::protocol::{AgentId, ClaimId, Intent, Mode, Scope, ScopeClaim};

    fn commit(text: &str) -> CommitId {
        CommitId(text.to_string())
    }

    fn merge_dispatch() -> MergeDispatch {
        MergeDispatch {
            id: 7,
            claim: ClaimId(3),
            agent: AgentId("a1".to_string()),
            fork_commit: commit("fork"),
            scopes: vec![ScopeClaim {
                scope: Scope::File {
                    path: "x.rs".to_string(),
                },
                mode: Mode::EditBody,
            }],
            attempt: 1,
            record: Box::new(MergeRecord {
                claim: ClaimId(3),
                intent: Intent {
                    summary: "s".to_string(),
                    task_ref: None,
                    assumptions: Vec::new(),
                },
                decisions: None,
                review: None,
            }),
        }
    }

    #[test]
    fn a_merge_is_a_prepare_call_with_the_dispatch_of_the_core() {
        let call = merge_call(CallId(4), &merge_dispatch(), 1_000);
        assert_eq!(call.id, CallId(4));
        assert_eq!(call.dispatch_id, DispatchId(7));
        assert_eq!(call.deadline_ms, 1_000 + 780_000);
        let Request::Prepare {
            claim,
            agent,
            fork_commit,
            scopes,
        } = call.request
        else {
            panic!("a merge is a Prepare");
        };
        assert_eq!(claim, ClaimId(3));
        assert_eq!(agent, AgentId("a1".to_string()));
        assert_eq!(fork_commit, commit("fork"));
        assert_eq!(scopes, merge_dispatch().scopes);
    }

    #[test]
    fn a_trial_call_carries_the_dispatch_of_the_core() {
        let dispatch = VerifyDispatch {
            id: 9,
            agent: AgentId("a2".to_string()),
            commit: Some(commit("c")),
            before: commit("old"),
            main: commit("new"),
            attempt: 1,
        };
        let call = trial_call(CallId(5), &dispatch, 2_000);
        assert_eq!(call.id, CallId(5));
        assert_eq!(call.dispatch_id, DispatchId(9));
        assert_eq!(call.deadline_ms, 2_000 + 780_000);
        assert_eq!(
            call.request,
            Request::Trial {
                agent: AgentId("a2".to_string()),
                commit: Some(commit("c")),
                before: commit("old"),
                main: commit("new"),
            }
        );
    }

    #[test]
    fn a_head_call_has_its_own_id_for_a_dispatch_id_and_waits_30_seconds() {
        let call = head_call(CallId(6), 3_000);
        assert_eq!(call.id, CallId(6));
        assert_eq!(call.dispatch_id, DispatchId(6));
        assert_eq!(call.deadline_ms, 3_000 + 30_000);
        assert_eq!(
            call.request,
            Request::ReadHead {
                submitted: Vec::new()
            }
        );
    }

    #[test]
    fn a_deadline_does_not_wrap() {
        assert_eq!(head_call(CallId(1), u64::MAX).deadline_ms, u64::MAX);
    }

    #[test]
    fn the_wake_resumes_at_the_step_after_the_call_it_issued() {
        let claim_scopes = Vec::new();
        let prepare = Request::Prepare {
            claim: ClaimId(1),
            agent: AgentId("a".to_string()),
            fork_commit: commit("f"),
            scopes: claim_scopes,
        };
        let (base, head) = (commit("b"), commit("h"));
        let test = Request::Test {
            base: base.clone(),
            head: head.clone(),
        };
        let land = Request::Land { base, head };
        let trial = Request::Trial {
            agent: AgentId("a".to_string()),
            commit: None,
            before: commit("o"),
            main: commit("n"),
        };
        let read = Request::ReadHead {
            submitted: Vec::new(),
        };
        let reconcile = Request::Reconcile { head: commit("h") };
        assert_eq!(resume_after(&prepare), WakeStep::Verification);
        assert_eq!(resume_after(&test), WakeStep::Verification);
        assert_eq!(resume_after(&land), WakeStep::Verification);
        assert_eq!(resume_after(&trial), WakeStep::HeadRead);
        assert_eq!(resume_after(&read), WakeStep::Finish);
        assert_eq!(resume_after(&reconcile), WakeStep::Finish);
    }

    fn refused(refusal: Refusal) -> CallAnswer {
        CallAnswer::Refused { refusal }
    }

    #[test]
    fn a_merge_answer_means_the_outcome_the_core_knows() {
        let prepared = CallAnswer::Prepared {
            base: commit("b"),
            head: commit("h"),
        };
        assert_eq!(
            merge_outcome_of(&prepared),
            MergeOutcome::Merged {
                base: commit("b"),
                head: commit("h")
            }
        );
        let conflict = refused(Refusal::Conflict {
            files: vec!["x.rs".to_string()],
        });
        assert_eq!(
            merge_outcome_of(&conflict),
            MergeOutcome::Conflict {
                files: vec!["x.rs".to_string()]
            }
        );
        assert_eq!(
            merge_outcome_of(&refused(Refusal::Uncovered { total: 4 })),
            MergeOutcome::Uncovered { total: 4 }
        );
        assert_eq!(
            merge_outcome_of(&refused(Refusal::GateChanged)),
            MergeOutcome::GateChanged {}
        );
        assert_eq!(
            merge_outcome_of(&refused(Refusal::CommitNotInFork)),
            MergeOutcome::CommitNotInFork {}
        );
        let rule = refused(Refusal::Rule {
            message: "protected".to_string(),
        });
        assert_eq!(merge_outcome_of(&rule), MergeOutcome::Refused);
        let failed = refused(Refusal::TestsFailed {
            failure: TestFailure {
                exit_code: 2,
                stdout: "out".to_string(),
                stderr: "err".to_string(),
            },
        });
        assert_eq!(
            merge_outcome_of(&failed),
            MergeOutcome::TestsFailed {
                result: StepExit {
                    exit_code: 2,
                    stdout: "out".to_string(),
                    stderr: "err".to_string()
                }
            }
        );
    }

    #[test]
    fn a_merge_answer_that_is_not_a_merge_answer_is_the_service_unavailable() {
        let unusable = [
            CallAnswer::Tested,
            CallAnswer::Landed { head: commit("h") },
            CallAnswer::Trialled {
                before: None,
                after: None,
            },
            CallAnswer::Head {
                main: None,
                landed: Vec::new(),
            },
            CallAnswer::Reconciled {
                main: commit("m"),
                landed: true,
            },
            CallAnswer::Unknown,
        ];
        for answer in unusable {
            assert_eq!(
                merge_outcome_of(&answer),
                MergeOutcome::ServiceUnavailable,
                "{answer:?}"
            );
        }
    }

    #[test]
    fn a_trial_answer_means_the_report_the_core_knows() {
        let results = [
            (TrialResult::Clean, TrialOutcome::Clean {}),
            (TrialResult::Conflict, TrialOutcome::Conflict {}),
            (TrialResult::TestsFailed, TrialOutcome::TestsFailed {}),
            (TrialResult::NothingToTest, TrialOutcome::NothingToTest {}),
            (
                TrialResult::CommitNotInFork,
                TrialOutcome::CommitNotInFork {},
            ),
            (
                TrialResult::MainUnreachable,
                TrialOutcome::MainUnreachable {},
            ),
            (
                TrialResult::Infrastructure,
                TrialOutcome::ServiceUnavailable,
            ),
        ];
        for (result, outcome) in results {
            let answer = CallAnswer::Trialled {
                before: Some(result),
                after: None,
            };
            let report = TrialReport {
                before: Some(outcome),
                after: None,
            };
            assert_eq!(trial_report_of(&answer), report);
            let answer = CallAnswer::Trialled {
                before: None,
                after: Some(result),
            };
            assert_eq!(trial_report_of(&answer).before, None);
            assert!(trial_report_of(&answer).after.is_some());
        }
        let rule = refused(Refusal::CommitNotInFork);
        assert_eq!(
            trial_report_of(&rule),
            TrialReport::stopped(TrialOutcome::Refused)
        );
        for answer in [CallAnswer::Unknown, CallAnswer::Tested] {
            assert_eq!(
                trial_report_of(&answer),
                TrialReport::stopped(TrialOutcome::ServiceUnavailable),
                "{answer:?}"
            );
        }
    }

    #[test]
    fn only_a_head_answer_with_a_main_is_a_head() {
        let moved = CallAnswer::Head {
            main: Some(commit("m")),
            landed: Vec::new(),
        };
        assert_eq!(head_of(&moved), Some(commit("m")));
        let headless = CallAnswer::Head {
            main: None,
            landed: Vec::new(),
        };
        assert_eq!(head_of(&headless), None);
        assert_eq!(head_of(&CallAnswer::Unknown), None);
        assert_eq!(head_of(&CallAnswer::Landed { head: commit("h") }), None);
    }
}

//! The calls the wake issues, and what their answers mean to the core (R21 to R23, R26, R38).
//!
//! A merge is three calls, each with the dispatch id of the core's stage (`MergeStage`): `Prepare`
//! (answered `Prepared { base, head }` once the head is on the candidate ref, main unmoved),
//! `Test` (`Tested`) and `Land` (`Landed { head }` once main holds the head). What an answer
//! means depends on the call it answers: `merge_outcome_of` reads it with its request, so a
//! `Landed` is a landing only as the answer to a `Land`. `Call` carries no evidence record, so no
//! `MergeRecord` reaches the lander.
//!
//! An answer that is not of the kind the call asks for, `Unknown` included, is the service being
//! unavailable: infrastructure, retried with backoff (R26). A head answer that is not a head is
//! `None` (R23). After a `Land` the same answer is not a failure but a doubt: the core asks
//! `Reconcile` (answered `Reconciled { main, landed }`) with the `Land`'s dispatch id, and an
//! answer to a `Reconcile` that is not a `Reconciled` is `Unknown` again, never an outcome.
//!
//! The host does not time out a call itself (HOST-CALL-DEADLINE-BACKSTOP). The adapter's deadline
//! is enough, for two reasons. The adapter owns the call's lifetime: it holds the call's
//! `deadline_ms` and answers `Unknown` when the deadline passes, whatever the work is doing, and
//! a panic in the answer reaches the host as `CallLost`. And the one failure a deadline cannot
//! cover, the adapter process dying mid-call, takes the host with it. Calls are held in memory
//! only; what is stored is the merge marker. A restarted host finds a merge in flight that no
//! call waits for and recovers it on the first wake (R24, `recover_merge_step`): a `Prepare`
//! marker is retried as an attempt, a `Test` is sent again, and a `Land` is reconciled, so the
//! claim is not left waiting for an answer nobody will send. What stays uncovered is an
//! adapter that neither answers, panics nor dies: a bug in the adapter, not a state of the call.

use crate::coordinator::{MergeDispatch, MergeStage, Reconciliation, VerifyDispatch};
use crate::host::call::{
    Call, CallAnswer, CallId, DispatchId, Refusal, Request, TestFailure, TrialResult,
};
use crate::host::wake::WakeStep;
use crate::host::{Host, Job, Output};
use crate::merge::{
    MergeOutcome, StepExit, TrialOutcome, TrialReport, LAND_CALL_TIMEOUT_MS,
    PREPARE_CALL_TIMEOUT_MS, REASON_OUTPUT_MAX_CHARS, RECONCILE_CALL_TIMEOUT_MS,
    STEWARD_CALL_TIMEOUT_MS, TEST_CALL_TIMEOUT_MS,
};
use crate::protocol::{ClaimInDoubt, CommitId};
use crate::shell::Work;

/// How long a head read waits: one ref, not a test run (R38).
pub const HEAD_CALL_TIMEOUT_MS: u64 = 30_000;

/// The call of one stage of a merge, with that stage's share of the time budget.
pub fn merge_call(id: CallId, dispatch: &MergeDispatch, now_ms: u64) -> Call {
    let (request, timeout_ms) = match &dispatch.stage {
        MergeStage::Prepare => (
            Request::Prepare {
                claim: dispatch.claim,
                agent: dispatch.agent.clone(),
                fork_commit: dispatch.fork_commit.clone(),
                scopes: dispatch.scopes.clone(),
            },
            PREPARE_CALL_TIMEOUT_MS,
        ),
        MergeStage::Test { base, head } => (
            Request::Test {
                base: base.clone(),
                head: head.clone(),
            },
            TEST_CALL_TIMEOUT_MS,
        ),
        MergeStage::Land { base, head } => (
            Request::Land {
                base: base.clone(),
                head: head.clone(),
            },
            LAND_CALL_TIMEOUT_MS,
        ),
        MergeStage::Reconcile { head, .. } => (
            Request::Reconcile { head: head.clone() },
            RECONCILE_CALL_TIMEOUT_MS,
        ),
        MergeStage::Unstaged => (
            Request::Prepare {
                claim: dispatch.claim,
                agent: dispatch.agent.clone(),
                fork_commit: dispatch.fork_commit.clone(),
                scopes: dispatch.scopes.clone(),
            },
            PREPARE_CALL_TIMEOUT_MS,
        ),
    };
    Call {
        id,
        dispatch_id: DispatchId(dispatch.id),
        request,
        deadline_ms: now_ms.saturating_add(timeout_ms),
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

/// The wake step that follows the step that issued `request`. A `Prepare` or a `Test` that the
/// core answered by moving the merge to its next stage (`merge_continues`) goes straight back to
/// the merge step, which issues that stage's call: the marker with the journaled head was stored
/// with the answer. Every other merge answer goes on to the verification.
pub fn resume_after(request: &Request, merge_continues: bool) -> WakeStep {
    match request {
        Request::Prepare { .. } | Request::Test { .. } if merge_continues => WakeStep::Merge,
        Request::Reconcile { .. } => WakeStep::Merge,
        Request::Prepare { .. } | Request::Test { .. } | Request::Land { .. } => {
            WakeStep::Verification
        }
        Request::Trial { .. } => WakeStep::HeadRead,
        Request::ReadHead { .. } => WakeStep::Finish,
    }
}

/// What the answer to the merge call `request` means to the core. An answer that is not of the
/// kind that call asks for is the service being unavailable.
pub fn merge_outcome_of(request: &Request, answer: &CallAnswer) -> MergeOutcome {
    if let CallAnswer::Refused { refusal } = answer {
        return refusal_outcome(refusal);
    }
    match (request, answer) {
        // A head that is the base is a rebase that left nothing to add: main already holds the
        // work, as after a lost answer. The core lands that outcome at main, with the main the
        // attempt was dispatched from as its `before` (`unanswered_from`).
        (Request::Prepare { .. }, CallAnswer::Prepared { base, head }) if base == head => {
            MergeOutcome::AlreadyMerged { base: base.clone() }
        }
        (Request::Prepare { .. }, CallAnswer::Prepared { base, head }) => MergeOutcome::Prepared {
            base: base.clone(),
            head: head.clone(),
        },
        (Request::Test { .. }, CallAnswer::Tested) => MergeOutcome::Tested {},
        // The base of the landing is the one the call was made with.
        (Request::Land { base, .. }, CallAnswer::Landed { head }) => MergeOutcome::Merged {
            base: base.clone(),
            head: head.clone(),
        },
        (Request::Test { .. } | Request::Land { .. }, CallAnswer::CandidateMissing) => {
            MergeOutcome::CandidateMissing {}
        }
        (
            Request::Prepare { .. },
            CallAnswer::Tested
            | CallAnswer::Landed { .. }
            | CallAnswer::Trialled { .. }
            | CallAnswer::Head { .. }
            | CallAnswer::Reconciled { .. }
            | CallAnswer::CandidateMissing
            | CallAnswer::Refused { .. }
            | CallAnswer::Unknown,
        )
        | (
            Request::Test { .. },
            CallAnswer::Prepared { .. }
            | CallAnswer::Landed { .. }
            | CallAnswer::Trialled { .. }
            | CallAnswer::Head { .. }
            | CallAnswer::Reconciled { .. }
            | CallAnswer::Refused { .. }
            | CallAnswer::Unknown,
        )
        | (
            Request::Land { .. },
            CallAnswer::Prepared { .. }
            | CallAnswer::Tested
            | CallAnswer::Trialled { .. }
            | CallAnswer::Head { .. }
            | CallAnswer::Reconciled { .. }
            | CallAnswer::Refused { .. }
            | CallAnswer::Unknown,
        )
        | (
            Request::Trial { .. } | Request::ReadHead { .. } | Request::Reconcile { .. },
            CallAnswer::Prepared { .. }
            | CallAnswer::Tested
            | CallAnswer::Landed { .. }
            | CallAnswer::Trialled { .. }
            | CallAnswer::Head { .. }
            | CallAnswer::Reconciled { .. }
            | CallAnswer::CandidateMissing
            | CallAnswer::Refused { .. }
            | CallAnswer::Unknown,
        ) => MergeOutcome::ServiceUnavailable,
    }
}

fn refusal_outcome(refusal: &Refusal) -> MergeOutcome {
    match refusal {
        Refusal::Conflict { files, total } => MergeOutcome::Conflict {
            files: files.clone(),
            total: *total,
        },
        Refusal::Uncovered { total } => MergeOutcome::Uncovered { total: *total },
        Refusal::GateChanged => MergeOutcome::GateChanged {},
        Refusal::TestsFailed { failure } => MergeOutcome::TestsFailed {
            result: step_exit(failure),
        },
        Refusal::CommitNotInFork => MergeOutcome::CommitNotInFork {},
        Refusal::Rule { rule, .. } => MergeOutcome::Refused { kind: *rule },
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
        | CallAnswer::CandidateMissing
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

/// What the answer to a `Reconcile` means to the core. Anything but a `Reconciled` is `Unknown`:
/// the reconcile is asked again after its backoff and decides nothing.
pub fn reconciliation_of(answer: &CallAnswer) -> Reconciliation {
    match answer {
        CallAnswer::Reconciled { landed: true, .. } => Reconciliation::Landed,
        CallAnswer::Reconciled {
            main,
            landed: false,
        } => Reconciliation::NotLanded {
            main: Some(main.clone()),
        },
        CallAnswer::Prepared { .. }
        | CallAnswer::Tested
        | CallAnswer::Landed { .. }
        | CallAnswer::Trialled { .. }
        | CallAnswer::Head { .. }
        | CallAnswer::CandidateMissing
        | CallAnswer::Refused { .. }
        | CallAnswer::Unknown => Reconciliation::Unknown,
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
        | CallAnswer::CandidateMissing
        | CallAnswer::Refused { .. }
        | CallAnswer::Unknown => None,
    }
}

/// The log line for a refusal by a rule, which carries the rule's own message: the one place the
/// message goes, since the agent reads a fixed reason for the kind. The message is untrusted, so it
/// is cut to `REASON_OUTPUT_MAX_CHARS` characters again here and written as a JSON string.
fn rule_refusal_log(repo: &str, answer: &CallAnswer) -> Option<Output> {
    let CallAnswer::Refused {
        refusal: Refusal::Rule { rule, message },
    } = answer
    else {
        return None;
    };
    let cut: String = message.chars().take(REASON_OUTPUT_MAX_CHARS).collect();
    let more = if cut.len() < message.len() { "..." } else { "" };
    let quoted = serde_json::Value::String(cut);
    Some(Output::Log {
        line: format!("coordinator {repo}: a rule ({rule:?}) refused the merge: {quoted}{more}"),
    })
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
        let mut resume = resume_after(&call.request, false);
        let (operation, prepared) = match &call.request {
            Request::Prepare { .. } | Request::Test { .. } | Request::Land { .. } => {
                let outcome = merge_outcome_of(&call.request, &answer);
                out.extend(rule_refusal_log(&self.config.repo, &answer));
                if outcome == MergeOutcome::ServiceUnavailable {
                    out.push(self.failure("lander merge call", "no usable answer"));
                }
                let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
                    core.merge_outcome(dispatch, &outcome, now_ms)
                });
                resume = resume_after(&call.request, self.merge_continues());
                out.extend(self.doubt_log());
                ("apply merge outcome", prepared)
            }
            Request::Trial { .. } => {
                let report = trial_report_of(&answer);
                out.extend(rule_refusal_log(&self.config.repo, &answer));
                if report == TrialReport::stopped(TrialOutcome::ServiceUnavailable) {
                    out.push(self.failure("lander trial call", "no usable answer"));
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
                    out.push(self.failure("head call", "no head in the answer"));
                }
                self.log_waiting_races(head.is_some(), out);
                let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
                    core.trunk_read(&read, head, now_ms)
                });
                ("apply trunk head", prepared)
            }
            Request::Reconcile { .. } => {
                let found = reconciliation_of(&answer);
                if found == Reconciliation::Unknown {
                    out.push(self.failure("lander reconcile call", "no usable answer"));
                }
                let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
                    core.reconcile_outcome(dispatch, &found, now_ms)
                });
                resume = resume_after(&call.request, self.merge_continues());
                out.extend(self.doubt_log());
                ("apply reconcile outcome", prepared)
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

    /// Say once that closed races wait for a head read that gave no head, until a read answers.
    fn log_waiting_races(&mut self, answered: bool, out: &mut Vec<Output>) {
        let waiting = !answered && self.slot.races_await_head();
        if waiting && !self.race_wait_logged {
            out.push(self.failure(
                "head call",
                "races wait for the head read; if it keeps failing they are judged, tests unknown",
            ));
        }
        self.race_wait_logged = waiting;
    }

    /// Whether the merge goes on after this answer: one is still in flight and no merge call of
    /// this instance is out.
    fn merge_continues(&self) -> bool {
        self.slot.has_merge_in_flight() && !self.waits().merging
    }

    /// The log line that names the claim whose landing is in doubt, for the operator who looks
    /// for it without the submitter.
    fn doubt_log(&self) -> Option<Output> {
        let doubt = self.slot.merge_in_doubt()?;
        let (base, head) = self.slot.journaled_land(doubt.claim)?;
        Some(Output::Log {
            line: doubt_line(&self.config.repo, &doubt, (&base, &head)),
        })
    }
}

/// The text of the doubt log line: the claim, the main its `Land` leased and the head it
/// journaled, and the command that settles it. `admin resolve` needs the claim number and, for
/// `--landed`, the head, which is the rebased commit and not the submitter's fork commit. The time
/// is the server's clock, milliseconds since the epoch, not a duration.
fn doubt_line(repo: &str, doubt: &ClaimInDoubt, (base, head): (&CommitId, &CommitId)) -> String {
    let (claim, since) = (doubt.claim.0, doubt.since_ms);
    let (base, head) = (&base.0, &head.0);
    format!(
        "coordinator {repo}: the landing of claim {claim} is in doubt since {since} (epoch ms); \
         its Land leased base {base} and journaled head {head}. With the server stopped, settle it \
         by hand with: tessel-server admin resolve {claim} --landed {head} --config <the server's \
         configuration file> if `git merge-base --is-ancestor {head} origin/main` succeeds in a \
         fetched checkout of the trunk, and with --not-landed (no head) otherwise or when unsure"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::{MergeRecord, RuleKind};
    use crate::protocol::{AgentId, ClaimId, Intent, Mode, Scope, ScopeClaim};

    fn commit(text: &str) -> CommitId {
        CommitId(text.to_string())
    }

    #[test]
    fn the_doubt_line_names_the_claim_base_head_and_the_whole_command() {
        let doubt = ClaimInDoubt {
            claim: ClaimId(7),
            since_ms: 1_234,
        };
        let line = doubt_line("acme/demo", &doubt, (&commit("base1"), &commit("head1")));
        for part in [
            "acme/demo",
            "claim 7 is in doubt since 1234 (epoch ms)",
            "leased base base1",
            "journaled head head1",
            "admin resolve 7 --landed head1 --tenant acme --repo demo --config",
            "git merge-base --is-ancestor head1 origin/main",
            "--not-landed",
        ] {
            assert!(line.contains(part), "{part:?} missing from {line}");
        }
    }

    fn merge_dispatch(stage: MergeStage) -> MergeDispatch {
        MergeDispatch {
            stage,
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

    fn prepare_request() -> Request {
        let dispatch = merge_dispatch(MergeStage::Prepare);
        merge_call(CallId(1), &dispatch, 0).request
    }

    fn test_request() -> Request {
        Request::Test {
            base: commit("b"),
            head: commit("h"),
        }
    }

    fn land_request() -> Request {
        Request::Land {
            base: commit("b"),
            head: commit("h"),
        }
    }

    #[test]
    fn a_prepare_stage_is_a_prepare_call_with_the_dispatch_of_the_core() {
        let call = merge_call(CallId(4), &merge_dispatch(MergeStage::Prepare), 1_000);
        assert_eq!(call.id, CallId(4));
        assert_eq!(call.dispatch_id, DispatchId(7));
        assert_eq!(call.deadline_ms, 1_000 + PREPARE_CALL_TIMEOUT_MS);
        let Request::Prepare {
            claim,
            agent,
            fork_commit,
            scopes,
        } = call.request
        else {
            panic!("the first stage is a Prepare");
        };
        assert_eq!(claim, ClaimId(3));
        assert_eq!(agent, AgentId("a1".to_string()));
        assert_eq!(fork_commit, commit("fork"));
        assert_eq!(scopes, merge_dispatch(MergeStage::Prepare).scopes);
    }

    #[test]
    fn the_test_and_land_stages_are_calls_of_the_journaled_base_and_head_with_their_own_budgets() {
        let pair = (commit("b"), commit("h"));
        let stage = |land: bool| {
            let (base, head) = pair.clone();
            if land {
                MergeStage::Land { base, head }
            } else {
                MergeStage::Test { base, head }
            }
        };
        let test = merge_call(CallId(5), &merge_dispatch(stage(false)), 2_000);
        assert_eq!(test.dispatch_id, DispatchId(7));
        assert_eq!(test.deadline_ms, 2_000 + TEST_CALL_TIMEOUT_MS);
        assert_eq!(test.request, test_request());
        let land = merge_call(CallId(6), &merge_dispatch(stage(true)), 3_000);
        assert_eq!(land.deadline_ms, 3_000 + LAND_CALL_TIMEOUT_MS);
        assert_eq!(land.request, land_request());
        const {
            assert!(TEST_CALL_TIMEOUT_MS > PREPARE_CALL_TIMEOUT_MS);
            assert!(TEST_CALL_TIMEOUT_MS > LAND_CALL_TIMEOUT_MS);
        }
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
        for continues in [false, true] {
            assert_eq!(resume_after(&trial, continues), WakeStep::HeadRead);
            assert_eq!(resume_after(&read, continues), WakeStep::Finish);
            assert_eq!(
                resume_after(&reconcile, continues),
                WakeStep::Merge,
                "a reconcile that sends the Land again or the claim back to Prepare goes on to it"
            );
            assert_eq!(
                resume_after(&land, continues),
                WakeStep::Verification,
                "a landing is the last stage"
            );
        }
        for request in [&prepare, &test] {
            assert_eq!(resume_after(request, false), WakeStep::Verification);
            assert_eq!(
                resume_after(request, true),
                WakeStep::Merge,
                "a stage that moved the merge on goes straight to the next call"
            );
        }
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
            merge_outcome_of(&prepare_request(), &prepared),
            MergeOutcome::Prepared {
                base: commit("b"),
                head: commit("h")
            }
        );
        let conflict = refused(Refusal::Conflict {
            files: vec!["x.rs".to_string()],
            total: Some(70),
        });
        assert_eq!(
            merge_outcome_of(&prepare_request(), &conflict),
            MergeOutcome::Conflict {
                files: vec!["x.rs".to_string()],
                total: Some(70)
            }
        );
        assert_eq!(
            merge_outcome_of(
                &prepare_request(),
                &refused(Refusal::Uncovered { total: 4 })
            ),
            MergeOutcome::Uncovered { total: 4 }
        );
        assert_eq!(
            merge_outcome_of(&prepare_request(), &refused(Refusal::GateChanged)),
            MergeOutcome::GateChanged {}
        );
        assert_eq!(
            merge_outcome_of(&prepare_request(), &refused(Refusal::CommitNotInFork)),
            MergeOutcome::CommitNotInFork {}
        );
        for kind in [
            RuleKind::NoLander,
            RuleKind::ProtectedRef,
            RuleKind::RequiredCheck,
            RuleKind::ForkMissing,
            RuleKind::Other,
        ] {
            let rule = refused(Refusal::Rule {
                rule: kind,
                message: "protected".to_string(),
            });
            assert_eq!(
                merge_outcome_of(&prepare_request(), &rule),
                MergeOutcome::Refused { kind },
                "{kind:?}"
            );
        }
        let failed = refused(Refusal::TestsFailed {
            failure: TestFailure {
                exit_code: 2,
                stdout: "out".to_string(),
                stderr: "err".to_string(),
            },
        });
        assert_eq!(
            merge_outcome_of(&test_request(), &failed),
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
    fn a_stage_is_answered_by_its_own_kind_of_answer_only() {
        let landed = CallAnswer::Landed { head: commit("h") };
        let prepared = CallAnswer::Prepared {
            base: commit("b"),
            head: commit("h"),
        };
        assert_eq!(
            merge_outcome_of(&test_request(), &CallAnswer::Tested),
            MergeOutcome::Tested {}
        );
        assert_eq!(
            merge_outcome_of(&land_request(), &landed),
            MergeOutcome::Merged {
                base: commit("b"),
                head: commit("h")
            },
            "the base of a landing is the one the call was made with"
        );
        for request in [test_request(), land_request()] {
            assert_eq!(
                merge_outcome_of(&request, &CallAnswer::CandidateMissing),
                MergeOutcome::CandidateMissing {}
            );
        }
        let unusable = [
            (prepare_request(), CallAnswer::Tested),
            (prepare_request(), CallAnswer::CandidateMissing),
            (prepare_request(), landed.clone()),
            (test_request(), prepared.clone()),
            (test_request(), landed.clone()),
            (land_request(), prepared.clone()),
            (land_request(), CallAnswer::Tested),
        ];
        for (request, answer) in unusable {
            assert_eq!(
                merge_outcome_of(&request, &answer),
                MergeOutcome::ServiceUnavailable,
                "{request:?} answered {answer:?}"
            );
        }
    }

    #[test]
    fn a_merge_answer_that_is_not_a_merge_answer_is_the_service_unavailable() {
        let unusable = [
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
        for request in [prepare_request(), test_request(), land_request()] {
            for answer in &unusable {
                assert_eq!(
                    merge_outcome_of(&request, answer),
                    MergeOutcome::ServiceUnavailable,
                    "{request:?} answered {answer:?}"
                );
            }
        }
    }

    #[test]
    fn a_reconcile_answer_means_landed_not_landed_with_main_or_unknown() {
        assert_eq!(
            reconciliation_of(&CallAnswer::Reconciled {
                main: commit("m"),
                landed: true
            }),
            Reconciliation::Landed
        );
        assert_eq!(
            reconciliation_of(&CallAnswer::Reconciled {
                main: commit("m"),
                landed: false
            }),
            Reconciliation::NotLanded {
                main: Some(commit("m"))
            }
        );
        for answer in [
            CallAnswer::Unknown,
            CallAnswer::Tested,
            CallAnswer::Landed { head: commit("h") },
            CallAnswer::CandidateMissing,
            refused(Refusal::GateChanged),
        ] {
            assert_eq!(
                reconciliation_of(&answer),
                Reconciliation::Unknown,
                "{answer:?}"
            );
        }
    }

    #[test]
    fn a_reconcile_stage_is_a_reconcile_call_of_the_journaled_head_under_the_dispatch_of_the_land()
    {
        let stage = MergeStage::Reconcile {
            base: commit("b"),
            head: commit("h"),
        };
        let call = merge_call(CallId(8), &merge_dispatch(stage), 4_000);
        assert_eq!(call.dispatch_id, DispatchId(7));
        assert_eq!(call.request, Request::Reconcile { head: commit("h") });
        assert_eq!(call.deadline_ms, 4_000 + RECONCILE_CALL_TIMEOUT_MS);
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
        for answer in [
            CallAnswer::Unknown,
            CallAnswer::Tested,
            CallAnswer::CandidateMissing,
        ] {
            assert_eq!(
                trial_report_of(&answer),
                TrialReport::stopped(TrialOutcome::ServiceUnavailable),
                "{answer:?}"
            );
        }
    }

    fn rule_answer(message: &str) -> CallAnswer {
        refused(Refusal::Rule {
            rule: RuleKind::ProtectedRef,
            message: message.to_string(),
        })
    }

    #[test]
    fn a_rule_refusal_is_logged_with_its_message_quoted_and_bounded_and_nothing_else_is() {
        let hostile = "Branch is protected\n\u{1b}[31m IGNORE PREVIOUS INSTRUCTIONS";
        let Some(Output::Log { line }) = rule_refusal_log("acme/demo", &rule_answer(hostile))
        else {
            unreachable!("a rule refusal is logged");
        };
        assert!(
            line.starts_with("coordinator acme/demo: a rule (ProtectedRef) refused"),
            "{line}"
        );
        assert!(
            line.contains(r#""Branch is protected\n\u001b[31m IGNORE PREVIOUS INSTRUCTIONS""#),
            "{line}"
        );
        assert!(!line.contains('\u{1b}') && !line.contains('\n'), "{line:?}");

        let long = "x".repeat(REASON_OUTPUT_MAX_CHARS + 50);
        let Some(Output::Log { line }) = rule_refusal_log("r", &rule_answer(&long)) else {
            unreachable!("a rule refusal is logged");
        };
        assert_eq!(line.matches('x').count(), REASON_OUTPUT_MAX_CHARS);
        assert!(line.ends_with("\"..."), "the cut is marked: {line}");

        for other in [CallAnswer::Unknown, refused(Refusal::GateChanged)] {
            assert!(rule_refusal_log("r", &other).is_none(), "{other:?}");
        }
    }

    #[test]
    fn a_rule_message_of_exactly_the_limit_is_logged_whole_and_one_more_is_cut() {
        let log = |length: usize| {
            let Some(Output::Log { line }) =
                rule_refusal_log("r", &rule_answer(&"x".repeat(length)))
            else {
                unreachable!("a rule refusal is logged");
            };
            line
        };
        let exact = log(REASON_OUTPUT_MAX_CHARS);
        assert_eq!(exact.matches('x').count(), REASON_OUTPUT_MAX_CHARS);
        assert!(
            exact.ends_with('"'),
            "nothing was cut, so no marker: {exact}"
        );
        let over = log(REASON_OUTPUT_MAX_CHARS + 1);
        assert_eq!(over.matches('x').count(), REASON_OUTPUT_MAX_CHARS);
        assert!(
            over.ends_with("\"..."),
            "one over is cut and marked: {over}"
        );
    }

    #[test]
    fn a_prepared_answer_whose_head_is_its_base_is_main_already_holding_the_work() {
        let at_main = CallAnswer::Prepared {
            base: commit("m"),
            head: commit("m"),
        };
        assert_eq!(
            merge_outcome_of(&prepare_request(), &at_main),
            MergeOutcome::AlreadyMerged { base: commit("m") }
        );
        assert_eq!(
            merge_outcome_of(&test_request(), &at_main),
            MergeOutcome::ServiceUnavailable,
            "only a Prepare is answered Prepared"
        );
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

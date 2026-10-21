//! What the lander's outcomes mean to the host (rule 6: nothing is reported landed that did not
//! land), and what the server log keeps of them.
//!
//! A merge outcome that is a fact about the work becomes a refusal, which the core never retries.
//! One that is not (main moved, or any infrastructure failure) becomes `Unknown`, which the core
//! retries with backoff. Only `Merged` is `Prepared`, and the lander returns `Merged` only after
//! it read main back and found the head on it.

use tessel_coordinator::host::call::{CallAnswer, Refusal, TestFailure, TrialResult};
use tessel_coordinator::protocol::CommitId;
use tessel_lander::outcome::{MergeOutcome, StepOutcome, TrialOutcome, TrialReport};
use tessel_lander::run::GitOutput;
use tessel_lander::types::Sha;

use super::names::{cap_names, quote, MAX_LOGGED_CHARS};

pub fn commit_id(sha: &Sha) -> CommitId {
    CommitId(sha.to_string())
}

fn refused(refusal: Refusal) -> CallAnswer {
    CallAnswer::Refused { refusal }
}

fn git_note(output: &GitOutput) -> String {
    format!(
        "exit {:?}, stderr {}",
        output.exit_code,
        quote(&output.stderr, MAX_LOGGED_CHARS)
    )
}

fn step_note(step: &StepOutcome) -> String {
    format!(
        "{:?} exit {}, timed out {}",
        step.step, step.exit_code, step.timed_out
    )
}

/// The answer to `Prepare`, which stands for the whole merge. `first_failure` is the runner's
/// first failing test, already bounded and quoted; it is logged and never sent to an agent.
pub fn merge_answer(repo: &str, outcome: MergeOutcome, first_failure: Option<&str>) -> CallAnswer {
    match outcome {
        MergeOutcome::Merged { base, head, .. } => {
            tracing::info!(repo, %base, %head, "landed: main was read back at the head");
            CallAnswer::Prepared {
                base: commit_id(&base),
                head: commit_id(&head),
            }
        }
        // The rebase left nothing to add: main already holds the work, as after a landing whose
        // answer was lost. A head equal to the base is how the host reads this as `AlreadyMerged`,
        // so the core lands the claim at main with the main it dispatched from as its `before`.
        MergeOutcome::AlreadyMerged { base } => {
            tracing::info!(repo, %base, "main already holds the work");
            CallAnswer::Prepared {
                base: commit_id(&base),
                head: commit_id(&base),
            }
        }
        MergeOutcome::Conflict { files, .. } => {
            let capped = cap_names(&files);
            tracing::info!(
                repo,
                total = capped.total,
                names = %capped.names.join(", "),
                "conflict with main"
            );
            refused(Refusal::Conflict {
                files: capped.names,
                total: u64::try_from(capped.total).ok(),
            })
        }
        MergeOutcome::Uncovered { files, total, .. } => {
            let capped = cap_names(&files);
            tracing::info!(
                repo,
                total,
                names = %capped.names.join(", "),
                "the change is not covered by the claim"
            );
            refused(Refusal::Uncovered {
                total: u64::try_from(total).unwrap_or(u64::MAX),
            })
        }
        // Only an admin merge reaches `GateInvalid`; the server lands submissions only.
        MergeOutcome::GateChanged { .. } | MergeOutcome::GateInvalid { .. } => {
            tracing::info!(repo, "the change touches the gate");
            refused(Refusal::GateChanged)
        }
        MergeOutcome::CommitNotInFork => refused(Refusal::CommitNotInFork),
        MergeOutcome::TestsFailed { result, .. } => {
            tracing::info!(
                repo,
                exit_code = result.exit_code,
                first_failure = %first_failure.unwrap_or("none"),
                "the tests failed"
            );
            refused(Refusal::TestsFailed {
                failure: TestFailure {
                    exit_code: i64::from(result.exit_code),
                    stdout: String::new(),
                    stderr: String::new(),
                },
            })
        }
        MergeOutcome::MainMoved { expected, actual } => {
            tracing::info!(
                repo,
                %expected,
                %actual,
                "main moved under the merge; the core retries"
            );
            CallAnswer::Unknown
        }
        MergeOutcome::Clone { result } => infrastructure(repo, "clone", &git_note(&result)),
        MergeOutcome::GitFailed { result } => infrastructure(repo, "git", &git_note(&result)),
        MergeOutcome::Install { result, .. } => {
            infrastructure(repo, "install", &step_note(&result))
        }
        MergeOutcome::Timeout { result, .. } => {
            infrastructure(repo, "timeout", &step_note(&result))
        }
        MergeOutcome::PushFailed { result, .. } => infrastructure(repo, "push", &git_note(&result)),
    }
}

fn infrastructure(repo: &str, what: &str, note: &str) -> CallAnswer {
    tracing::warn!(
        repo,
        what,
        note = %note,
        "the merge did not finish; the core retries"
    );
    CallAnswer::Unknown
}

fn trial_result(outcome: &TrialOutcome) -> TrialResult {
    match outcome {
        TrialOutcome::Clean { .. } => TrialResult::Clean,
        TrialOutcome::Conflict { .. } => TrialResult::Conflict,
        TrialOutcome::TestsFailed { .. } => TrialResult::TestsFailed,
        TrialOutcome::NothingToTest { .. } => TrialResult::NothingToTest,
        TrialOutcome::CommitNotInFork => TrialResult::CommitNotInFork,
        TrialOutcome::MainUnreachable { .. } => TrialResult::MainUnreachable,
        TrialOutcome::Clone { .. }
        | TrialOutcome::GitFailed { .. }
        | TrialOutcome::Install { .. }
        | TrialOutcome::Timeout { .. } => TrialResult::Infrastructure,
    }
}

/// The answer to `Trial`. The file names of a trial's conflict are not sent anywhere.
pub fn trial_answer(report: &TrialReport) -> CallAnswer {
    CallAnswer::Trialled {
        before: Some(trial_result(&report.before)),
        after: report.after.as_ref().map(trial_result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(digit: char) -> Sha {
        Sha::parse(&digit.to_string().repeat(40)).unwrap()
    }

    fn step(exit_code: i32, timed_out: bool) -> StepOutcome {
        StepOutcome {
            timed_out,
            ..StepOutcome::finished(
                tessel_lander::outcome::PackageStep::Test,
                exit_code,
                "o",
                "e",
            )
        }
    }

    fn git_output() -> GitOutput {
        GitOutput {
            exit_code: Some(128),
            stdout: String::new(),
            stderr: "fatal".to_string(),
        }
    }

    #[test]
    fn only_a_merged_outcome_is_prepared() {
        let merged = MergeOutcome::Merged {
            base: sha('a'),
            head: sha('b'),
            evidence: None,
        };
        assert_eq!(
            merge_answer("r", merged, None),
            CallAnswer::Prepared {
                base: commit_id(&sha('a')),
                head: commit_id(&sha('b'))
            }
        );
        let outcomes = [
            MergeOutcome::MainMoved {
                expected: sha('a'),
                actual: sha('c'),
            },
            MergeOutcome::Clone {
                result: git_output(),
            },
            MergeOutcome::GitFailed {
                result: git_output(),
            },
            MergeOutcome::Install {
                base: sha('a'),
                head: sha('b'),
                result: step(1, false),
            },
            MergeOutcome::Timeout {
                base: sha('a'),
                head: sha('b'),
                result: step(137, true),
            },
            MergeOutcome::PushFailed {
                base: sha('a'),
                head: sha('b'),
                result: git_output(),
            },
        ];
        for outcome in outcomes {
            let shown = format!("{outcome:?}");
            assert_eq!(
                merge_answer("r", outcome, None),
                CallAnswer::Unknown,
                "{shown}"
            );
        }
    }

    #[test]
    fn facts_about_the_work_are_refusals() {
        assert_eq!(
            merge_answer("r", MergeOutcome::CommitNotInFork, None),
            refused(Refusal::CommitNotInFork)
        );
        let gate = MergeOutcome::GateChanged {
            base: sha('a'),
            head: sha('b'),
        };
        assert_eq!(merge_answer("r", gate, None), refused(Refusal::GateChanged));
        let uncovered = MergeOutcome::Uncovered {
            base: sha('a'),
            head: sha('b'),
            files: vec!["x".to_string()],
            total: 7,
        };
        assert_eq!(
            merge_answer("r", uncovered, None),
            refused(Refusal::Uncovered { total: 7 })
        );
    }

    #[test]
    fn a_failed_test_run_gives_the_agent_the_exit_code_and_no_output() {
        let failed = MergeOutcome::TestsFailed {
            base: sha('a'),
            head: sha('b'),
            result: step(101, false),
        };
        assert_eq!(
            merge_answer("r", failed, Some("\"FAILED\"")),
            refused(Refusal::TestsFailed {
                failure: TestFailure {
                    exit_code: 101,
                    stdout: String::new(),
                    stderr: String::new(),
                }
            })
        );
    }

    #[test]
    fn work_main_already_holds_is_prepared_at_main() {
        let answer = merge_answer("r", MergeOutcome::AlreadyMerged { base: sha('d') }, None);
        assert_eq!(
            answer,
            CallAnswer::Prepared {
                base: commit_id(&sha('d')),
                head: commit_id(&sha('d'))
            }
        );
    }

    #[test]
    fn a_conflict_carries_at_most_fifty_quoted_names() {
        let files: Vec<String> = (0..51).map(|n| format!("f\u{1b}{n}")).collect();
        let answer = merge_answer(
            "r",
            MergeOutcome::Conflict {
                base: sha('a'),
                files,
            },
            None,
        );
        let CallAnswer::Refused {
            refusal: Refusal::Conflict { files, total },
        } = answer
        else {
            unreachable!("a conflict is a refusal: {answer:?}");
        };
        assert_eq!(files.len(), 50);
        assert_eq!(total, Some(51), "the real total travels with the cut list");
        assert_eq!(files[0], r#""f\u{1b}0""#);
    }

    #[test]
    fn a_trial_reports_each_side_and_keeps_infrastructure_apart() {
        let report = TrialReport {
            before: TrialOutcome::Clean {
                base: sha('a'),
                head: sha('b'),
                commit: sha('c'),
            },
            after: Some(TrialOutcome::Timeout {
                base: sha('a'),
                head: sha('b'),
                commit: sha('c'),
                result: step(137, true),
            }),
        };
        assert_eq!(
            trial_answer(&report),
            CallAnswer::Trialled {
                before: Some(TrialResult::Clean),
                after: Some(TrialResult::Infrastructure)
            }
        );
    }
}

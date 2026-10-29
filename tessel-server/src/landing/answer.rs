//! What the lander's outcomes mean to the host (rule 6: nothing is reported landed that did not
//! land), and what the server log keeps of them.
//!
//! A merge outcome that is a fact about the work becomes a refusal, which the core never retries.
//! One that is not (main moved, or any infrastructure failure) becomes `Unknown`, which the core
//! retries with backoff. Only `Merged` is `Prepared`, and the lander returns `Merged` only after
//! it read main back and found the head on it.

use tessel_coordinator::host::call::{CallAnswer, Refusal, TrialResult};
use tessel_coordinator::protocol::CommitId;
use tessel_lander::outcome::{MergeOutcome, TrialOutcome, TrialReport};
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

/// The answer to `Prepare`, which rebases the commit and checks it without running repo code.
/// `Prepared` with a head different from its base means the head is on the candidate ref; the
/// caller makes that answer after the push, so no outcome of the lander is it.
pub fn prepare_answer(repo: &str, outcome: MergeOutcome) -> CallAnswer {
    match outcome {
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
        MergeOutcome::PushFailed { result, .. } => infrastructure(repo, "push", &git_note(&result)),
        // The tests, the landing and the candidate ref belong to `Test` and `Land`.
        MergeOutcome::Merged { .. }
        | MergeOutcome::TestsFailed { .. }
        | MergeOutcome::Install { .. }
        | MergeOutcome::Timeout { .. }
        | MergeOutcome::CandidateMissing
        | MergeOutcome::HeadNotOnBase { .. } => {
            infrastructure(repo, "prepare", "an outcome of another call")
        }
    }
}

/// The answer to `Land`. Only `Merged` is landed, and the lander returns it only after it found
/// the head on main. A head on neither the candidate ref nor main is
/// [`CallAnswer::CandidateMissing`]. Main moved under the push, or any infrastructure failure, is
/// `Unknown`: the core reconciles.
pub fn land_answer(repo: &str, outcome: MergeOutcome) -> CallAnswer {
    match outcome {
        MergeOutcome::Merged { base, head, .. } => {
            tracing::info!(repo, %base, %head, "landed: main holds the head");
            CallAnswer::Landed {
                head: commit_id(&head),
            }
        }
        MergeOutcome::CandidateMissing => CallAnswer::CandidateMissing,
        // The candidate cannot be landed from this base: as good as missing. The core goes back
        // to `Prepare`, which makes the head again on the current main, and counts it.
        MergeOutcome::HeadNotOnBase { base, head } => {
            tracing::warn!(repo, %base, %head, "the candidate does not descend from its base");
            CallAnswer::CandidateMissing
        }
        MergeOutcome::MainMoved { expected, actual } => {
            tracing::info!(repo, %expected, %actual, "main moved under the push; core reconciles");
            CallAnswer::Unknown
        }
        MergeOutcome::Clone { result } => infrastructure(repo, "clone", &git_note(&result)),
        MergeOutcome::GitFailed { result } => infrastructure(repo, "git", &git_note(&result)),
        MergeOutcome::PushFailed { result, .. } => infrastructure(repo, "push", &git_note(&result)),
        // The rebase, the coverage check, the gate and the tests belong to `Prepare` and `Test`.
        MergeOutcome::AlreadyMerged { .. }
        | MergeOutcome::Conflict { .. }
        | MergeOutcome::TestsFailed { .. }
        | MergeOutcome::Uncovered { .. }
        | MergeOutcome::GateChanged { .. }
        | MergeOutcome::GateInvalid { .. }
        | MergeOutcome::CommitNotInFork
        | MergeOutcome::Install { .. }
        | MergeOutcome::Timeout { .. } => {
            infrastructure(repo, "land", "an outcome of another step")
        }
    }
}

fn infrastructure(repo: &str, what: &str, note: &str) -> CallAnswer {
    tracing::warn!(
        repo,
        what,
        note = %note,
        "the call did not finish; the core retries a Prepare and reconciles a Land"
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
    use tessel_lander::outcome::StepOutcome;

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
    fn nothing_the_lander_decides_in_prepare_is_a_landing() {
        let merged = MergeOutcome::Merged {
            base: sha('a'),
            head: sha('b'),
            evidence: None,
        };
        let outcomes = [
            merged,
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
            MergeOutcome::CandidateMissing,
        ];
        for outcome in outcomes {
            let shown = format!("{outcome:?}");
            assert_eq!(prepare_answer("r", outcome), CallAnswer::Unknown, "{shown}");
        }
    }

    #[test]
    fn only_a_merged_outcome_is_landed_and_a_head_off_its_base_is_a_missing_candidate() {
        let merged = MergeOutcome::Merged {
            base: sha('a'),
            head: sha('b'),
            evidence: None,
        };
        assert_eq!(
            land_answer("r", merged),
            CallAnswer::Landed {
                head: commit_id(&sha('b'))
            }
        );
        assert_eq!(
            land_answer("r", MergeOutcome::CandidateMissing),
            CallAnswer::CandidateMissing
        );
        let off_base = MergeOutcome::HeadNotOnBase {
            base: sha('a'),
            head: sha('b'),
        };
        assert_eq!(
            land_answer("r", off_base),
            CallAnswer::CandidateMissing,
            "a pair that cannot land goes back to Prepare, counted, and does not loop"
        );
        let doubt = [
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
            MergeOutcome::PushFailed {
                base: sha('a'),
                head: sha('b'),
                result: git_output(),
            },
        ];
        for outcome in doubt {
            let shown = format!("{outcome:?}");
            assert_eq!(land_answer("r", outcome), CallAnswer::Unknown, "{shown}");
        }
    }

    #[test]
    fn facts_about_the_work_are_refusals() {
        assert_eq!(
            prepare_answer("r", MergeOutcome::CommitNotInFork),
            refused(Refusal::CommitNotInFork)
        );
        let gate = MergeOutcome::GateChanged {
            base: sha('a'),
            head: sha('b'),
        };
        assert_eq!(prepare_answer("r", gate), refused(Refusal::GateChanged));
        let uncovered = MergeOutcome::Uncovered {
            base: sha('a'),
            head: sha('b'),
            files: vec!["x".to_string()],
            total: 7,
        };
        assert_eq!(
            prepare_answer("r", uncovered),
            refused(Refusal::Uncovered { total: 7 })
        );
    }

    #[test]
    fn work_main_already_holds_is_prepared_at_main() {
        let answer = prepare_answer("r", MergeOutcome::AlreadyMerged { base: sha('d') });
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
        let answer = prepare_answer(
            "r",
            MergeOutcome::Conflict {
                base: sha('a'),
                files,
            },
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

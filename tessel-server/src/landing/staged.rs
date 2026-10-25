//! `Test` and `Land` of the staged landing (`docs/proposals/candidate-ref.md`).
//!
//! `Test` makes a tree from the candidate ref of the head, in a repository git makes for it with
//! no remote, and runs the trunk's tests on it. No credential is live then: the only use of the
//! write credential is `with_candidate_access` and the push of main, and neither is called here.
//! `Land` reads the ref first, then pushes main with a lease and reads it back by ancestry; a head
//! that is on neither the ref nor main is `CandidateMissing`.

use tessel_coordinator::host::call::{CallAnswer, Refusal, TestFailure};
use tessel_coordinator::merge::RuleKind;
use tessel_coordinator::protocol::CommitId;
use tessel_lander::candidate::{land_candidate, read_candidate};
use tessel_lander::git::MergeSources;
use tessel_lander::merge::{MergeContext, MergePorts, PushUpdate};
use tessel_lander::outcome::PackageStep;
use tessel_lander::run::SystemRunner;
use tessel_lander::types::Sha;
use tessel_lander::workspace::{fetch_candidate_tree, CandidateTreeOutcome, CloneError};

use super::answer::land_answer;
use super::names::{quote, MAX_LOGGED_CHARS};
use super::{Inner, Work};

impl Inner {
    /// Runs the tests on the candidate ref of `head`.
    pub(super) fn test(&self, work: &Work, head: &CommitId) -> CallAnswer {
        let repo = self.repo.to_string();
        let Ok(head) = Sha::parse(&head.0) else {
            tracing::warn!(repo, "a test names a head that is not a commit id");
            return CallAnswer::Unknown;
        };
        match read_candidate(&self.git, &SystemRunner, &self.remote, &head) {
            Ok(Some(tip)) if tip == head => {}
            Ok(Some(_) | None) => return missing(&repo, &head),
            Err(error) => return unfinished(&repo, "read the candidate ref", &error),
        }
        let destination = work.tree_dir("test");
        let tree = match fetch_candidate_tree(
            &self.git,
            &SystemRunner,
            &self.remote,
            &head,
            &destination,
        ) {
            Ok(CandidateTreeOutcome::Fetched(tree)) => tree,
            Ok(CandidateTreeOutcome::Failed(output)) => {
                let note = quote(&output.stderr, MAX_LOGGED_CHARS);
                tracing::warn!(repo, exit = ?output.exit_code, note, "the candidate was not fetched");
                return CallAnswer::Unknown;
            }
            Err(CloneError::TestTreeNotAtHead { .. }) => return missing(&repo, &head),
            Err(error) => return unfinished(&repo, "make the test tree", &error),
        };
        let ports = self.ports(work);
        match MergePorts::run_package_step(&ports, PackageStep::Test, &tree) {
            Ok(step) if step.timed_out => {
                tracing::warn!(repo, "the tests timed out; the core retries");
                CallAnswer::Unknown
            }
            Ok(step) if step.passed => {
                tracing::info!(repo, %head, "the tests passed on the candidate");
                CallAnswer::Tested
            }
            Ok(step) => {
                tracing::info!(
                    repo,
                    exit_code = step.exit_code,
                    first_failure = %ports.take_first_failure().as_deref().unwrap_or("none"),
                    "the tests failed"
                );
                CallAnswer::Refused {
                    refusal: Refusal::TestsFailed {
                        failure: TestFailure {
                            exit_code: i64::from(step.exit_code),
                            stdout: String::new(),
                            stderr: String::new(),
                        },
                    },
                }
            }
            Err(error) => match ports.take_gate_refusal() {
                Some(message) => CallAnswer::Refused {
                    refusal: Refusal::Rule {
                        rule: RuleKind::GateInvalid,
                        message,
                    },
                },
                None => unfinished(&repo, "run the tests", &error),
            },
        }
    }

    /// Lands `head` on main from `base`.
    pub(super) fn land(&self, work: &Work, base: &CommitId, head: &CommitId) -> CallAnswer {
        let repo = self.repo.to_string();
        let (Ok(base), Ok(head)) = (Sha::parse(&base.0), Sha::parse(&head.0)) else {
            tracing::warn!(repo, "a land names a commit that is not a commit id");
            return CallAnswer::Unknown;
        };
        let sources = MergeSources {
            main_remote: self.remote.clone(),
            fork_remote: String::new(),
        };
        let (clone_dir, test_dir) = (work.dir.child("main"), work.tree_dir("unused"));
        let ctx = MergeContext {
            git: &self.git,
            runner: &SystemRunner,
            sources: &sources,
            clone_dir: &clone_dir,
            test_dir: &test_dir,
            pinned_main: &base,
        };
        let ports = self.ports(work);
        let update = PushUpdate {
            base: base.clone(),
            head,
        };
        match land_candidate(&ctx, &ports, &update) {
            Ok(outcome) => land_answer(&repo, outcome),
            Err(error) => unfinished(&repo, "land", &error),
        }
    }
}

fn missing(repo: &str, head: &Sha) -> CallAnswer {
    tracing::warn!(repo, %head, "the candidate ref is missing or does not hold the head");
    CallAnswer::CandidateMissing
}

fn unfinished(repo: &str, what: &str, error: &dyn std::fmt::Display) -> CallAnswer {
    let note = quote(&error.to_string(), MAX_LOGGED_CHARS);
    tracing::warn!(repo, what, error = %note, "the call did not finish; the core retries");
    CallAnswer::Unknown
}

//! `Trial`: a commit of an agent's fork tried on main as it was and as it is now. A trial with no
//! commit is not submitted work: there is nothing to try, so it is inconclusive.

use std::sync::atomic::{AtomicU64, Ordering};

use tessel_coordinator::host::call::{CallAnswer, TrialResult};
use tessel_coordinator::protocol::{AgentId, CommitId};
use tessel_lander::git::MergeSources;
use tessel_lander::merge::MergeError;
use tessel_lander::outcome::TrialOutcome;
use tessel_lander::run::SystemRunner;
use tessel_lander::trial::{report_trial, run_trial, TrialContext, TrialPlan};
use tessel_lander::types::Sha;

use super::answer::trial_answer;
use super::names::{quote, MAX_LOGGED_CHARS};
use super::{Inner, Work};

impl Inner {
    /// Tries `commit` on main at `before` and at `main`. `None` is not submitted: nothing to try.
    pub(super) fn trial(
        &self,
        work: &Work,
        agent: &AgentId,
        commit: Option<&CommitId>,
        (before, main): (&CommitId, &CommitId),
    ) -> CallAnswer {
        let repo = self.repo.to_string();
        let fork_remote = match self.fork_of(agent) {
            Ok(fork) => fork,
            Err(refused) => return refused,
        };
        let (Ok(before), Ok(main)) = (Sha::parse(&before.0), Sha::parse(&main.0)) else {
            tracing::warn!(repo, "a trial names a main that is not a commit id");
            return CallAnswer::Unknown;
        };
        let Some(commit) = commit.and_then(|commit| Sha::parse(&commit.0).ok()) else {
            return CallAnswer::Trialled {
                before: Some(TrialResult::CommitNotInFork),
                after: None,
            };
        };
        let sources = MergeSources {
            main_remote: self.remote.clone(),
            fork_remote,
        };
        let ports = self.ports(work);
        let runs = AtomicU64::new(0);
        let run = |on: &Sha, commit: &Sha| -> Result<TrialOutcome, MergeError> {
            let clone_dir =
                work.tree_dir(&format!("trial-{}", runs.fetch_add(1, Ordering::SeqCst)));
            let ctx = TrialContext {
                git: &self.git,
                runner: &SystemRunner,
                sources: &sources,
                clone_dir: &clone_dir,
            };
            run_trial(&ctx, &ports, on, commit)
        };
        let plan = TrialPlan {
            before,
            main,
            commit,
        };
        match report_trial(&plan, &run) {
            Ok(report) => trial_answer(&report),
            Err(error) => {
                let note = quote(&error.to_string(), MAX_LOGGED_CHARS);
                tracing::warn!(repo, error = %note, "the trial did not finish");
                CallAnswer::Unknown
            }
        }
    }
}

//! A trial and the two-sided report, ported from the steward's `runTrial` in `merge-steps.ts` and
//! `reportTrial` in `trial-report.ts`.
//!
//! [`run_trial`] tries a fork's commit on main as it was at a given sha: the steps of a merge up
//! to and including the tests, and nothing after them. It does not push, does not touch the write
//! credential ([`TrialPorts`] has none), and does not check coverage: it verifies, it does not
//! merge. [`report_trial`] makes two, for a baseline and for the new main.
//!
//! The order, which the tests pin:
//! 1. Clone main and fetch the fork with read credentials, then revoke them before anything else
//!    runs (if the revocation fails, nothing runs and the trial is an error).
//! 2. Check that `main` is on main's history, else `MainUnreachable`.
//! 3. Verify that some fork branch contains the commit.
//! 4. Rebase `merge-base..commit` onto `main` with the fixed committer, as a merge does.
//! 5. Run the dependency check and the tests. No credential is live.
//!
//! A trial runs repo code, so it never runs where a landing will push from. It makes its own
//! [`Workspace`] with [`clone_main`](crate::workspace::clone_main), which refuses a directory that
//! is in use, and drops it when it returns: no function hands a trial's workspace to
//! [`land`](crate::merge::land), and a merge's workspace cannot be reused for a trial.

use std::panic;
use std::path::Path;
use std::thread;

use crate::git::{Git, MergeSources};
use crate::merge::{
    fetch_then_revoke, install_then_test, rebase_onto_main, verify_commit, BoundaryError, GitEnv,
    Halt, MergeError, Rebase,
};
use crate::outcome::{PackageStep, StepOutcome, TrialOutcome, TrialReport};
use crate::run::GitRunner;
use crate::types::Sha;
use crate::workspace::{CloneOutcome, TestTree, Workspace};

/// What a trial needs from the platform: the read credentials and the sandbox that runs the
/// repo's tests. It has no way to push and no way to mint a write credential, so [`run_trial`]
/// cannot do either.
///
/// ```compile_fail,E0599
/// use tessel_lander::merge::PushUpdate;
/// use tessel_lander::trial::TrialPorts;
///
/// fn push(ports: &impl TrialPorts, update: &PushUpdate) {
///     let _ = ports.with_push_access(update, || ());
/// }
/// ```
///
/// ```compile_fail,E0599
/// use tessel_lander::trial::TrialPorts;
///
/// fn read_main_back(ports: &impl TrialPorts) {
///     let _ = ports.current_main();
/// }
/// ```
pub trait TrialPorts {
    /// Revokes the read credentials of main and of the fork. Must not panic; `false` on failure.
    fn revoke_read_access(&self) -> bool;

    /// Runs the install step or the test step in `tree`, the rebased tree of the trial's clone.
    /// No credential is live. The clone is the trial's own: nothing runs git in it or pushes from
    /// it afterwards. The lander does not remove `tree`.
    fn run_package_step(
        &self,
        step: PackageStep,
        tree: &TestTree,
    ) -> Result<StepOutcome, BoundaryError>;
}

/// Where a trial runs.
pub struct TrialContext<'a> {
    pub git: &'a Git,
    pub runner: &'a dyn GitRunner,
    pub sources: &'a MergeSources,
    /// Where main is cloned for this trial alone: missing or empty.
    pub clone_dir: &'a Path,
}

/// Tries `commit` on main at `main`.
///
/// # Errors
/// A git command that could not run or ran too long, read credentials that could not be revoked,
/// or a port that failed.
pub fn run_trial(
    ctx: &TrialContext<'_>,
    ports: &impl TrialPorts,
    main: &Sha,
    commit: &Sha,
) -> Result<TrialOutcome, MergeError> {
    let env = GitEnv {
        git: ctx.git,
        runner: ctx.runner,
        sources: ctx.sources,
    };
    let workspace = match fetch_then_revoke(&env, ctx.clone_dir, || ports.revoke_read_access())? {
        CloneOutcome::Cloned(workspace) => workspace,
        CloneOutcome::Failed(result) => return Ok(TrialOutcome::Clone { result }),
    };
    let on_main = env.run(&env.git.on_main_command(&workspace, main))?;
    if !on_main.success() {
        return Ok(TrialOutcome::MainUnreachable { main: main.clone() });
    }
    if let Some(halt) = verify_commit(&env, &workspace, commit)? {
        return Ok(stopped(halt, main, commit));
    }
    match rebase_onto_main(&env, &workspace, main, commit)? {
        Rebase::Stopped(halt) => Ok(stopped(halt, main, commit)),
        Rebase::Rebased(head) => test_rebased(ports, workspace, main, head, commit),
    }
}

fn stopped(halt: Halt, base: &Sha, commit: &Sha) -> TrialOutcome {
    match halt {
        Halt::CommitNotInFork => TrialOutcome::CommitNotInFork,
        Halt::Conflict { files } => TrialOutcome::Conflict {
            base: base.clone(),
            commit: commit.clone(),
            files,
        },
        Halt::GitFailed(result) => TrialOutcome::GitFailed { result },
    }
}

fn test_rebased(
    ports: &impl TrialPorts,
    workspace: Workspace,
    base: &Sha,
    head: Sha,
    commit: &Sha,
) -> Result<TrialOutcome, MergeError> {
    let (base, commit) = (base.clone(), commit.clone());
    if head == base {
        return Ok(TrialOutcome::NothingToTest { base, commit });
    }
    let tree = TestTree::from_workspace(workspace);
    let (tested, _steps) = install_then_test(&|step| ports.run_package_step(step, &tree))?;
    Ok(if tested.timed_out {
        TrialOutcome::Timeout {
            base,
            head,
            commit,
            result: tested,
        }
    } else if tested.step == PackageStep::Install {
        TrialOutcome::Install {
            base,
            head,
            commit,
            result: tested,
        }
    } else if !tested.passed {
        TrialOutcome::TestsFailed {
            base,
            head,
            commit,
            result: tested,
        }
    } else {
        TrialOutcome::Clean { base, head, commit }
    })
}

/// What [`report_trial`] tries: `commit` on main at `before`, the baseline, and at `main`.
#[derive(Debug, Clone)]
pub struct TrialPlan {
    pub before: Sha,
    pub main: Sha,
    pub commit: Sha,
}

/// Tries the commit on main at `before` and on main at `main`, both at once. Each is its own call
/// to `runner`, which must give each its own clone, its own credentials and its own sandbox: what
/// one run leaves behind (files in a temp directory or the home directory, a process still
/// listening) cannot then make the other fail. Both runs try the same commit, which the caller
/// resolved once, so a fork head that moves meanwhile cannot make them differ. They run at once
/// because one side's worst case already fills the call timeout.
///
/// The result of `main` is kept only if the baseline was `Clean`: otherwise the work was already
/// failing and the run at `main` says nothing about the merge that moved main, so it is discarded.
/// When `before` and `main` are the same sha there is one run, and it is both sides.
///
/// # Errors
/// The error of the baseline's run, else of main's run when the baseline was clean, after both
/// runs have finished.
pub fn report_trial<R>(plan: &TrialPlan, runner: &R) -> Result<TrialReport, MergeError>
where
    R: Fn(&Sha, &Sha) -> Result<TrialOutcome, MergeError> + Sync,
{
    if plan.before == plan.main {
        let only = runner(&plan.main, &plan.commit)?;
        let after = matches!(only, TrialOutcome::Clean { .. }).then(|| only.clone());
        return Ok(TrialReport {
            before: only,
            after,
        });
    }
    let (before, main) = thread::scope(|scope| {
        let main = scope.spawn(|| runner(&plan.main, &plan.commit));
        let before = runner(&plan.before, &plan.commit);
        let main = main
            .join()
            .unwrap_or_else(|payload| panic::resume_unwind(payload));
        (before, main)
    });
    let before = before?;
    if !matches!(before, TrialOutcome::Clean { .. }) {
        return Ok(TrialReport {
            before,
            after: None,
        });
    }
    Ok(TrialReport {
        before,
        after: Some(main?),
    })
}

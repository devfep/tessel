//! A merge in two calls (C6), ported from the steward's `run_merge` in `merge-steps.ts`.
//!
//! [`prepare`] fetches, verifies the commit, pins main, rebases deterministically and checks
//! coverage and the gate file. It runs no repo code and returns the head. [`land`] pushes main
//! from `base` to that head with a compare-and-swap and reads main back. [`run_merge`] is the two
//! with the repo's own tests between them.
//!
//! The order, which the tests pin:
//! 1. Clone main and fetch the fork with read credentials, then revoke them before anything else
//!    runs (if the revocation fails, nothing runs and the merge is an error).
//! 2. Check the cloned main is the commit whose gate was read (else `MainMoved`), then verify the
//!    commit is contained in a fork branch.
//! 3. Rebase `merge-base..commit` onto main with a fixed committer.
//! 4. Check that the claim covers every file the rebased range changes (invariant 11) and what
//!    the range does to `tessel.toml`. This is file level; see [`crate::coverage`].
//! 5. Clone the rebased head into a test tree of its own, then run the dependency check and the
//!    tests there. No credential is live, and repo code never sees the workspace that step 6
//!    pushes from.
//! 6. Only if the tests passed: mint the write credential, push with a lease on the cloned main,
//!    revoke it.
//! 7. Decide the outcome from a read of main, never from the push's exit code: repo code ran and
//!    can fake it. Main at `head` is `Merged`; at `base` it is `PushFailed`; anywhere else it is
//!    `MainMoved`; unreadable is `PushFailed`. Equality only: landing by ancestry is a later task.
//! 8. Only if main was read at `head`: write the evidence record. The outcome stays `Merged`
//!    whatever happens there.
//!
//! What depends on the platform (credentials, the sandbox that runs the repo's tests, reading
//! main, the record repository) is behind [`MergePorts`], which the caller implements. Git runs
//! through a [`GitRunner`], in a [`Workspace`] that only the clone step makes.
//!
//! A [`MergeOutcome`] is a verdict about the commit. A [`MergeError`] is an attempt that did not
//! finish: a git command that could not run or ran too long, a credential that stayed live, a
//! port that failed. It is retried, never read as evidence.

use std::path::Path;

use tessel_coordinator::protocol::ScopeClaim;

use crate::coverage::{parse_name_status, uncovered_paths, Requirement, MAX_REPORTED_FILES};
use crate::gate::{parse_gate_config, TESSEL_TOML_PATH};
use crate::git::{parse_nul_separated, CommandError, Git, MergeSources};
use crate::outcome::{
    is_timed_out, EvidenceResult, GateStep, LandedMerge, MergeOutcome, PackageStep, StepOutcome,
};
use crate::run::{GitOutput, GitRunner, RunError};
use crate::types::Sha;
use crate::workspace::{
    clone_main, clone_test_tree, CloneError, CloneOutcome, TestTree, TestTreeOutcome, Workspace,
};

/// The one update the write credential may be used for: main from `base` to `head`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushUpdate {
    pub base: Sha,
    pub head: Sha,
}

/// A port failed. `operation` names the port call, `message` says what went wrong.
#[derive(Debug, thiserror::Error)]
#[error("{operation} failed: {message}")]
pub struct BoundaryError {
    pub operation: &'static str,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Run(#[from] RunError),
    #[error(transparent)]
    Clone(#[from] CloneError),
    #[error(
        "the read credentials could not be revoked, so no repo code was run; \
         revoke them before retrying"
    )]
    ReadAccessNotRevoked,
    #[error(transparent)]
    Port(#[from] BoundaryError),
}

/// Who asks for the merge. Only an admin merge may change the gate; the caller decides this from
/// how the request arrived and never from its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergePolicy {
    Submission,
    Admin,
}

/// What a merge needs from the platform it runs on.
pub trait MergePorts {
    /// Revokes the read credentials of main and of the fork. Must not panic; `false` on failure.
    fn revoke_read_access(&self) -> bool;

    /// Runs the install step or the test step of the gate in `tree`, and nowhere else. No write
    /// credential is live. `tree` is a clone of the rebased head that [`run_merge`] made for this
    /// merge; [`land`] pushes from a different clone. This API never hands the step the
    /// [`Workspace`], and nothing the step writes inside `tree` changes what [`land`] does. It
    /// does not stop code running as the same OS user from writing the workspace's files by
    /// path (the tree's `origin` and reflog name it): that boundary is the sandbox's. The lander
    /// does not remove `tree`.
    fn run_package_step(
        &self,
        step: PackageStep,
        tree: &TestTree,
    ) -> Result<StepOutcome, BoundaryError>;

    /// Mints the write credential for main, lets `use_access` push exactly `update`, and revokes
    /// the credential when `use_access` ends, whether it returned an error or not. Called only
    /// after the tests passed.
    fn with_push_access<T>(
        &self,
        update: &PushUpdate,
        use_access: impl FnOnce() -> T,
    ) -> Result<T, BoundaryError>;

    /// The sha main points at now, or `None` if it could not be read.
    fn current_main(&self) -> Option<Sha>;

    /// Writes the evidence record of a merge that landed. Called only after [`current_main`]
    /// read the merged head, so it mints its own credentials after main's is revoked. Never
    /// fails the merge.
    ///
    /// [`current_main`]: MergePorts::current_main
    fn record_merge(&self, landed: &LandedMerge) -> EvidenceResult;
}

/// Where a merge runs and what it is pinned to.
pub struct MergeContext<'a> {
    pub git: &'a Git,
    pub runner: &'a dyn GitRunner,
    pub sources: &'a MergeSources,
    /// Where main is cloned: missing or empty.
    pub clone_dir: &'a Path,
    /// Where the repo's tests run: a second clone, of the rebased head. Missing or empty, and not
    /// inside `clone_dir`. The caller removes it; the lander does not.
    pub test_dir: &'a Path,
    /// The commit of main whose gate was read. The merge must be based on exactly this commit.
    pub pinned_main: &'a Sha,
}

impl MergeContext<'_> {
    fn run(&self, command: &crate::git::GitCommand) -> Result<GitOutput, MergeError> {
        Ok(self.runner.run(command)?)
    }

    fn env(&self) -> GitEnv<'_> {
        GitEnv {
            git: self.git,
            runner: self.runner,
            sources: self.sources,
        }
    }
}

/// What the steps a merge and a trial share need to run git: the part of [`MergeContext`] that
/// does not name a landing.
pub(crate) struct GitEnv<'a> {
    pub(crate) git: &'a Git,
    pub(crate) runner: &'a dyn GitRunner,
    pub(crate) sources: &'a MergeSources,
}

impl GitEnv<'_> {
    pub(crate) fn run(&self, command: &crate::git::GitCommand) -> Result<GitOutput, MergeError> {
        Ok(self.runner.run(command)?)
    }
}

/// Why verifying the commit or replaying it onto main stopped, before any repo code ran.
pub(crate) enum Halt {
    CommitNotInFork,
    Conflict { files: Vec<String> },
    GitFailed(GitOutput),
}

impl Halt {
    fn into_merge_outcome(self, base: &Sha) -> MergeOutcome {
        match self {
            Halt::CommitNotInFork => MergeOutcome::CommitNotInFork,
            Halt::Conflict { files } => MergeOutcome::Conflict {
                base: base.clone(),
                files,
            },
            Halt::GitFailed(result) => MergeOutcome::GitFailed { result },
        }
    }
}

/// The submitted commit and the claim it is judged against.
pub struct MergeRequest<'a> {
    pub commit: &'a Sha,
    /// The claim's scopes. Required: the coverage check cannot be skipped.
    pub scopes: &'a [ScopeClaim],
    pub policy: MergePolicy,
}

/// What the tests do for this change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateReview {
    /// The trunk's gate decides: run the install step and the tests.
    TrunkTests,
    /// An admin merge whose diff is only `tessel.toml`, and the head's file is a valid gate: the
    /// file is the whole review, because the trunk's own may be the broken one.
    HeadFileOnly,
}

/// A rebased change that passed every check that runs no repo code.
#[derive(Debug)]
pub struct Ready {
    pub workspace: Workspace,
    pub base: Sha,
    pub head: Sha,
    pub gate: GateReview,
}

#[derive(Debug)]
pub enum Prepared {
    Ready(Ready),
    /// The attempt ended before the tests with this outcome.
    Done(MergeOutcome),
}

pub(crate) enum Rebase {
    Rebased(Sha),
    Stopped(Halt),
}

enum Coverage {
    Refused(MergeOutcome),
    Covered(GateReview),
}

fn sha_of(output: &GitOutput) -> Option<Sha> {
    if output.success() {
        Sha::parse(output.stdout.trim()).ok()
    } else {
        None
    }
}

fn exited_with(output: &GitOutput, code: i32) -> bool {
    output.exit_code == Some(code)
}

fn clone_and_fetch(env: &GitEnv<'_>, clone_dir: &Path) -> Result<CloneOutcome, MergeError> {
    let workspace = match clone_main(env.git, env.runner, env.sources, clone_dir)? {
        CloneOutcome::Cloned(workspace) => workspace,
        failed @ CloneOutcome::Failed(_) => return Ok(failed),
    };
    let fetch = env.run(&env.git.fetch_fork_command(&workspace, env.sources))?;
    Ok(if fetch.success() {
        CloneOutcome::Cloned(workspace)
    } else {
        CloneOutcome::Failed(fetch)
    })
}

/// Runs the clone and fetch, then revokes the read credentials whether they returned or failed,
/// so a credential is never live once repo code could run.
pub(crate) fn fetch_then_revoke(
    env: &GitEnv<'_>,
    clone_dir: &Path,
    revoke_read_access: impl FnOnce() -> bool,
) -> Result<CloneOutcome, MergeError> {
    let fetched = clone_and_fetch(env, clone_dir);
    let revoked = revoke_read_access();
    let fetched = fetched?;
    if revoked {
        Ok(fetched)
    } else {
        Err(MergeError::ReadAccessNotRevoked)
    }
}

/// Checks that the commit exists in the fetched fork and that some fork branch contains it.
pub(crate) fn verify_commit(
    env: &GitEnv<'_>,
    workspace: &Workspace,
    commit: &Sha,
) -> Result<Option<Halt>, MergeError> {
    let exists = env.run(&env.git.commit_exists_command(workspace, commit))?;
    if exited_with(&exists, 1) {
        return Ok(Some(Halt::CommitNotInFork));
    }
    if !exists.success() {
        return Ok(Some(Halt::GitFailed(exists)));
    }
    let reachable = env.run(&env.git.reachable_command(workspace, commit))?;
    if !reachable.success() {
        return Ok(Some(Halt::GitFailed(reachable)));
    }
    Ok(reachable
        .stdout
        .trim()
        .is_empty()
        .then_some(Halt::CommitNotInFork))
}

/// Replays `merge-base..commit` onto `base` and reads the new head. A stopped rebase with
/// unmerged paths is a conflict; any other failure is not.
pub(crate) fn rebase_onto_main(
    env: &GitEnv<'_>,
    workspace: &Workspace,
    base: &Sha,
    commit: &Sha,
) -> Result<Rebase, MergeError> {
    let merge_base_output = env.run(&env.git.merge_base_command(workspace, base, commit))?;
    let Some(merge_base) = sha_of(&merge_base_output) else {
        return Ok(Rebase::Stopped(Halt::GitFailed(merge_base_output)));
    };
    let rebase = env.run(&env.git.rebase_command(workspace, base, &merge_base, commit))?;
    if !rebase.success() {
        let conflicts = env.run(&env.git.conflicts_command(workspace))?;
        let files = if conflicts.success() {
            parse_nul_separated(&conflicts.stdout)
        } else {
            Vec::new()
        };
        return Ok(Rebase::Stopped(if files.is_empty() {
            Halt::GitFailed(rebase)
        } else {
            Halt::Conflict { files }
        }));
    }
    let head_output = env.run(&env.git.head_command(workspace))?;
    Ok(match sha_of(&head_output) {
        Some(head) => Rebase::Rebased(head),
        None => Rebase::Stopped(Halt::GitFailed(head_output)),
    })
}

/// What a change to `tessel.toml` means. A submission may not touch the gate (`GateChanged`). An
/// admin merge may, if the head's file is a valid gate (else `GateInvalid`); when the diff
/// touches nothing else the head's file is the whole gate ([`GateReview::HeadFileOnly`]).
fn review_gate(
    ctx: &MergeContext<'_>,
    workspace: &Workspace,
    required: &[Requirement],
    range: (&Sha, &Sha),
    policy: MergePolicy,
) -> Result<Coverage, MergeError> {
    let (base, head) = range;
    if !required.iter().any(|r| r.path == TESSEL_TOML_PATH) {
        return Ok(Coverage::Covered(GateReview::TrunkTests));
    }
    let (base, head) = (base.clone(), head.clone());
    match policy {
        MergePolicy::Submission => {
            return Ok(Coverage::Refused(MergeOutcome::GateChanged { base, head }))
        }
        MergePolicy::Admin => {}
    }
    let show = ctx.runner.run(
        &ctx.git
            .show_file_command(workspace, &head, TESSEL_TOML_PATH),
    );
    let file = match show {
        Ok(file) => file,
        Err(RunError::OutputTooLarge { .. }) => {
            return Ok(Coverage::Refused(MergeOutcome::GateInvalid { base, head }));
        }
        Err(error) => return Err(error.into()),
    };
    if !file.success() || parse_gate_config(&file.stdout).is_err() {
        return Ok(Coverage::Refused(MergeOutcome::GateInvalid { base, head }));
    }
    let only_gate = required.iter().all(|r| r.path == TESSEL_TOML_PATH);
    Ok(Coverage::Covered(if only_gate {
        GateReview::HeadFileOnly
    } else {
        GateReview::TrunkTests
    }))
}

/// Invariant 11 on the change the lander sees: every file the rebased range `base..head` changes
/// must be covered by the claim, and none may change the gate unless an admin asks. Runs before
/// any repo code. A diff that fails, is too large to read whole, or has a record this code does
/// not know is `GitFailed`: it is never read as covered.
fn check_coverage(
    ctx: &MergeContext<'_>,
    workspace: &Workspace,
    range: (&Sha, &Sha),
    request: &MergeRequest<'_>,
) -> Result<Coverage, MergeError> {
    let (base, head) = range;
    let diff = match ctx
        .runner
        .run(&ctx.git.changed_files_command(workspace, base, head))
    {
        Ok(diff) => diff,
        Err(error @ RunError::OutputTooLarge { .. }) => GitOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: error.to_string(),
        },
        Err(error) => return Err(error.into()),
    };
    let required = if diff.success() {
        parse_name_status(&diff.stdout).ok()
    } else {
        None
    };
    let Some(required) = required else {
        return Ok(Coverage::Refused(MergeOutcome::GitFailed { result: diff }));
    };
    let review = match review_gate(ctx, workspace, &required, range, request.policy)? {
        refused @ Coverage::Refused(_) => return Ok(refused),
        Coverage::Covered(review) => review,
    };
    let uncovered = uncovered_paths(&required, request.scopes);
    if uncovered.is_empty() {
        return Ok(Coverage::Covered(review));
    }
    Ok(Coverage::Refused(MergeOutcome::Uncovered {
        base: base.clone(),
        head: head.clone(),
        total: uncovered.len(),
        files: uncovered.into_iter().take(MAX_REPORTED_FILES).collect(),
    }))
}

/// Everything up to the tests, running no repo code.
///
/// # Errors
/// A git command that could not run or ran too long, or read credentials that could not be
/// revoked.
pub fn prepare(
    ctx: &MergeContext<'_>,
    ports: &impl MergePorts,
    request: &MergeRequest<'_>,
) -> Result<Prepared, MergeError> {
    let env = ctx.env();
    let workspace = match fetch_then_revoke(&env, ctx.clone_dir, || ports.revoke_read_access())? {
        CloneOutcome::Cloned(workspace) => workspace,
        CloneOutcome::Failed(result) => {
            return Ok(Prepared::Done(MergeOutcome::Clone { result }));
        }
    };
    let base_output = ctx.run(&ctx.git.base_command(&workspace))?;
    let Some(base) = sha_of(&base_output) else {
        return Ok(Prepared::Done(MergeOutcome::GitFailed {
            result: base_output,
        }));
    };
    if base != *ctx.pinned_main {
        return Ok(Prepared::Done(MergeOutcome::MainMoved {
            expected: ctx.pinned_main.clone(),
            actual: base,
        }));
    }
    if let Some(halt) = verify_commit(&env, &workspace, request.commit)? {
        return Ok(Prepared::Done(halt.into_merge_outcome(&base)));
    }
    let head = match rebase_onto_main(&env, &workspace, &base, request.commit)? {
        Rebase::Rebased(head) => head,
        Rebase::Stopped(halt) => return Ok(Prepared::Done(halt.into_merge_outcome(&base))),
    };
    if head == base {
        return Ok(Prepared::Done(MergeOutcome::AlreadyMerged { base }));
    }
    match check_coverage(ctx, &workspace, (&base, &head), request)? {
        Coverage::Refused(outcome) => Ok(Prepared::Done(outcome)),
        Coverage::Covered(gate) => Ok(Prepared::Ready(Ready {
            workspace,
            base,
            head,
            gate,
        })),
    }
}

/// Pushes `update.head` to main only if main is still `update.base`, then decides the outcome
/// from a read of main. The write credential exists only inside the push.
///
/// # Errors
/// The push could not run (the credential is revoked first), or a port failed.
pub fn land(
    ctx: &MergeContext<'_>,
    ports: &impl MergePorts,
    workspace: &Workspace,
    update: &PushUpdate,
) -> Result<MergeOutcome, MergeError> {
    let PushUpdate { base, head } = update;
    let push = ctx.git.push_command(workspace, ctx.sources, base, head);
    let result = ports.with_push_access(update, || ctx.runner.run(&push))??;
    let outcome = match ports.current_main() {
        Some(actual) if actual == *head => MergeOutcome::Merged {
            base: base.clone(),
            head: head.clone(),
            evidence: None,
        },
        Some(actual) if actual != *base => MergeOutcome::MainMoved {
            expected: base.clone(),
            actual,
        },
        Some(_) | None => MergeOutcome::PushFailed {
            base: base.clone(),
            head: head.clone(),
            result,
        },
    };
    Ok(outcome)
}

pub(crate) type StepRunner<'a> = &'a dyn Fn(PackageStep) -> Result<StepOutcome, BoundaryError>;

fn run_step(
    run_package_step: StepRunner<'_>,
    step: PackageStep,
    gate: &mut Vec<GateStep>,
) -> Result<StepOutcome, MergeError> {
    let outcome = run_package_step(step)?;
    gate.push(GateStep {
        step: outcome.step,
        exit_code: outcome.exit_code,
        wall_ms: outcome.wall_ms,
        timed_out: is_timed_out(outcome.exit_code),
    });
    Ok(outcome)
}

/// The install step, then the test step. Install must exit 0 or the tests do not run: tests that
/// cannot have their dependencies are not test evidence. A step that ran out of time or was
/// killed is returned with `timed_out` and keeps its own `step`. Returns the outcome that ended
/// the run, and the steps as they ran.
pub(crate) fn install_then_test(
    run_package_step: StepRunner<'_>,
) -> Result<(StepOutcome, Vec<GateStep>), MergeError> {
    let mut gate = Vec::new();
    let install = run_step(run_package_step, PackageStep::Install, &mut gate)?;
    if install.exit_code != 0 {
        let timed_out = is_timed_out(install.exit_code);
        return Ok((
            StepOutcome {
                timed_out,
                ..install
            },
            gate,
        ));
    }
    let test = run_step(run_package_step, PackageStep::Test, &mut gate)?;
    let timed_out = is_timed_out(test.exit_code);
    Ok((
        StepOutcome {
            passed: test.passed && !timed_out,
            timed_out,
            ..test
        },
        gate,
    ))
}

fn record_if_merged(
    ports: &impl MergePorts,
    outcome: MergeOutcome,
    gate: Option<Vec<GateStep>>,
) -> MergeOutcome {
    let MergeOutcome::Merged {
        ref base, ref head, ..
    } = outcome
    else {
        return outcome;
    };
    let evidence = ports.record_merge(&LandedMerge {
        base: base.clone(),
        head: head.clone(),
        gate,
    });
    MergeOutcome::Merged {
        base: base.clone(),
        head: head.clone(),
        evidence: Some(evidence),
    }
}

/// [`prepare`], the repo's tests, [`land`], then the record. See the module docs for the order.
///
/// # Errors
/// Any [`MergeError`]; a boundary that fails never turns into an outcome.
pub fn run_merge(
    ctx: &MergeContext<'_>,
    ports: &impl MergePorts,
    request: &MergeRequest<'_>,
) -> Result<MergeOutcome, MergeError> {
    let ready = match prepare(ctx, ports, request)? {
        Prepared::Done(outcome) => return Ok(outcome),
        Prepared::Ready(ready) => ready,
    };
    let Ready {
        workspace,
        base,
        head,
        gate,
    } = ready;
    let update = PushUpdate {
        base: base.clone(),
        head: head.clone(),
    };
    match gate {
        GateReview::HeadFileOnly => {
            let landed = land(ctx, ports, &workspace, &update)?;
            return Ok(record_if_merged(ports, landed, None));
        }
        GateReview::TrunkTests => {}
    }
    let tree = match clone_test_tree(ctx.git, ctx.runner, &workspace, &head, ctx.test_dir)? {
        TestTreeOutcome::Cloned(tree) => tree,
        TestTreeOutcome::Failed(result) => return Ok(MergeOutcome::GitFailed { result }),
    };
    let (tested, steps) = install_then_test(&|step| ports.run_package_step(step, &tree))?;
    if tested.timed_out {
        return Ok(MergeOutcome::Timeout {
            base,
            head,
            result: tested,
        });
    }
    if tested.step == PackageStep::Install {
        return Ok(MergeOutcome::Install {
            base,
            head,
            result: tested,
        });
    }
    if !tested.passed {
        return Ok(MergeOutcome::TestsFailed {
            base,
            head,
            result: tested,
        });
    }
    let landed = land(ctx, ports, &workspace, &update)?;
    Ok(record_if_merged(ports, landed, Some(steps)))
}

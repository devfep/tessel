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
//!    commit is reachable from the fork's default branch.
//! 3. Rebase `merge-base..commit` onto main with a fixed committer.
//! 4. Check that the claim covers every file the rebased range changes (invariant 11) and what
//!    the range does to `tessel.toml`. This is file level; see [`crate::coverage`].
//! 5. Run the dependency check and the tests. No credential is live.
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
use crate::workspace::{clone_main, CloneError, CloneOutcome, Workspace};

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

    /// Runs the install step or the test step of the gate. No write credential is live. It must
    /// not run in a [`Workspace`] directory: [`land`] pushes from there afterwards, and repo code
    /// could have rewritten that clone's config.
    fn run_package_step(&self, step: PackageStep) -> Result<StepOutcome, BoundaryError>;

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
    /// The commit of main whose gate was read. The merge must be based on exactly this commit.
    pub pinned_main: &'a Sha,
}

impl MergeContext<'_> {
    fn run(&self, command: &crate::git::GitCommand) -> Result<GitOutput, MergeError> {
        Ok(self.runner.run(command)?)
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

enum Rebase {
    Rebased(Sha),
    Stopped(MergeOutcome),
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

fn clone_and_fetch(ctx: &MergeContext<'_>) -> Result<CloneOutcome, MergeError> {
    let workspace = match clone_main(ctx.git, ctx.runner, ctx.sources, ctx.clone_dir)? {
        CloneOutcome::Cloned(workspace) => workspace,
        failed @ CloneOutcome::Failed(_) => return Ok(failed),
    };
    let fetch = ctx.run(&ctx.git.fetch_fork_command(&workspace, ctx.sources)?)?;
    Ok(if fetch.success() {
        CloneOutcome::Cloned(workspace)
    } else {
        CloneOutcome::Failed(fetch)
    })
}

/// Runs the clone and fetch, then revokes the read credentials whether they returned or failed,
/// so a credential is never live once repo code could run.
fn fetch_then_revoke(
    ctx: &MergeContext<'_>,
    ports: &impl MergePorts,
) -> Result<CloneOutcome, MergeError> {
    let fetched = clone_and_fetch(ctx);
    let revoked = ports.revoke_read_access();
    let fetched = fetched?;
    if revoked {
        Ok(fetched)
    } else {
        Err(MergeError::ReadAccessNotRevoked)
    }
}

/// Checks that the commit exists in the fetched fork and is reachable from its default branch.
fn verify_commit(
    ctx: &MergeContext<'_>,
    workspace: &Workspace,
    commit: &Sha,
) -> Result<Option<MergeOutcome>, MergeError> {
    let exists = ctx.run(&ctx.git.commit_exists_command(workspace, commit))?;
    if exited_with(&exists, 1) {
        return Ok(Some(MergeOutcome::CommitNotInFork));
    }
    if !exists.success() {
        return Ok(Some(MergeOutcome::GitFailed { result: exists }));
    }
    let reachable = ctx.run(&ctx.git.reachable_command(workspace, ctx.sources, commit)?)?;
    if exited_with(&reachable, 1) {
        return Ok(Some(MergeOutcome::CommitNotInFork));
    }
    Ok(if reachable.success() {
        None
    } else {
        Some(MergeOutcome::GitFailed { result: reachable })
    })
}

fn rebase_onto_main(
    ctx: &MergeContext<'_>,
    workspace: &Workspace,
    base: &Sha,
    commit: &Sha,
) -> Result<Rebase, MergeError> {
    let merge_base_output = ctx.run(&ctx.git.merge_base_command(workspace, base, commit))?;
    let Some(merge_base) = sha_of(&merge_base_output) else {
        return Ok(Rebase::Stopped(MergeOutcome::GitFailed {
            result: merge_base_output,
        }));
    };
    let rebase = ctx.run(&ctx.git.rebase_command(workspace, base, &merge_base, commit))?;
    if !rebase.success() {
        let conflicts = ctx.run(&ctx.git.conflicts_command(workspace))?;
        let files = if conflicts.success() {
            parse_nul_separated(&conflicts.stdout)
        } else {
            Vec::new()
        };
        return Ok(Rebase::Stopped(if files.is_empty() {
            MergeOutcome::GitFailed { result: rebase }
        } else {
            MergeOutcome::Conflict {
                base: base.clone(),
                files,
            }
        }));
    }
    let head_output = ctx.run(&ctx.git.head_command(workspace))?;
    Ok(match sha_of(&head_output) {
        Some(head) => Rebase::Rebased(head),
        None => Rebase::Stopped(MergeOutcome::GitFailed {
            result: head_output,
        }),
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
    let file = ctx.run(
        &ctx.git
            .show_file_command(workspace, &head, TESSEL_TOML_PATH),
    )?;
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
    let workspace = match fetch_then_revoke(ctx, ports)? {
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
    if let Some(unverified) = verify_commit(ctx, &workspace, request.commit)? {
        return Ok(Prepared::Done(unverified));
    }
    let head = match rebase_onto_main(ctx, &workspace, &base, request.commit)? {
        Rebase::Rebased(head) => head,
        Rebase::Stopped(outcome) => return Ok(Prepared::Done(outcome)),
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

fn run_step(
    ports: &impl MergePorts,
    step: PackageStep,
    gate: &mut Vec<GateStep>,
) -> Result<StepOutcome, MergeError> {
    let outcome = ports.run_package_step(step)?;
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
fn install_then_test(ports: &impl MergePorts) -> Result<(StepOutcome, Vec<GateStep>), MergeError> {
    let mut gate = Vec::new();
    let install = run_step(ports, PackageStep::Install, &mut gate)?;
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
    let test = run_step(ports, PackageStep::Test, &mut gate)?;
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
    let (tested, steps) = install_then_test(ports)?;
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

//! What a merge attempt returns, ported from the steward's `merge-types.ts`. Exactly one variant;
//! the fields of a variant are exactly what is known when it is returned.
//!
//! Evidence (CLAUDE.md rule 6). Only these are verified facts about the code:
//! - `Merged`: main was updated to `head`. The lander read main after the push and got `head`;
//!   the push's exit code is not trusted. `evidence` says whether the record of the merge was
//!   written; it is not merge evidence itself.
//! - `Conflict`: replaying the commit onto the real main at `base` stopped with these files
//!   unmerged. This is the only outcome that shows a conflict was real.
//! - `TestsFailed`: the repo's gate ran on the rebased `head` and did not exit 0. A step that
//!   timed out or was killed is never this: it is `Timeout`.
//! - `GateChanged`: the commit rebased onto main changes `tessel.toml`, the gate it would be
//!   judged by, and the request is a submission. Only an admin merge may change the gate. Read
//!   from the diff before any repo code ran; nothing was tested or pushed.
//! - `GateInvalid`: an admin merge changes `tessel.toml` to a file that is not a valid gate (or
//!   deletes it). Nothing was tested or pushed.
//! - `Uncovered`: the rebased commit changes files that the claim's scopes do not cover in a
//!   permitting mode (invariant 11), read before any repo code ran. `files` holds at most
//!   [`MAX_REPORTED_FILES`](crate::coverage::MAX_REPORTED_FILES) paths (untrusted data); `total`
//!   counts all of them.
//!
//! Not conflict or test evidence, but true statements about this attempt: `AlreadyMerged`,
//! `MainMoved` (retry the merge) and `CommitNotInFork`.
//!
//! Infrastructure (never counts for or against anything; the attempt did not finish): `Clone`,
//! `GitFailed`, `Install`, `Timeout` and `PushFailed`.
//!
//! Every `stdout` and `stderr` in a result comes from the repo or from git and is untrusted data.

use crate::run::GitOutput;
use crate::types::Sha;

/// A step that runs repo code, in the run's gate plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageStep {
    Install,
    Test,
}

/// Exit codes of `timeout` for a command it terminated (124) or killed (137).
const TIMED_OUT_EXIT_CODES: [i32; 2] = [124, 137];

/// Whether an exit code means the step ran out of time or was killed, not that it failed.
pub fn is_timed_out(exit_code: i32) -> bool {
    TIMED_OUT_EXIT_CODES.contains(&exit_code)
}

/// Why a step stopped the run before it reached a verdict, when the port knows and the repo does
/// not. A fixed value, never repo text. A step that ran out of time is `timed_out`, not a reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepReason {
    /// The repo declares dependencies and has no `tessel.toml` on main.
    Dependencies,
    /// The repo declares dependencies and its `tessel.toml` on main is not acceptable.
    Config,
    /// The dependency check did not complete.
    Unknown,
    /// An install command of a `tessel.toml` exited non-zero (a changed lockfile, or a
    /// dependency missing from the image).
    InstallFailed,
}

/// What a package step reported. Its output is untrusted data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    pub step: PackageStep,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// True only for the test step with exit code 0: installing is never test evidence.
    pub passed: bool,
    /// Set by the port when it knows why the step stopped the run; the merge carries it.
    pub reason: Option<StepReason>,
    /// Set by the merge when the step ran out of time or was killed.
    pub timed_out: bool,
    /// The lander's own clock around the step, when measured.
    pub wall_ms: Option<u64>,
}

impl StepOutcome {
    /// The outcome of a step that ran to an exit code.
    pub fn finished(step: PackageStep, exit_code: i32, stdout: &str, stderr: &str) -> StepOutcome {
        StepOutcome {
            step,
            exit_code,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            passed: step == PackageStep::Test && exit_code == 0,
            reason: None,
            timed_out: false,
            wall_ms: None,
        }
    }
}

/// One step of the gate as it ran, for the merge's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateStep {
    pub step: PackageStep,
    pub exit_code: i32,
    pub wall_ms: Option<u64>,
    pub timed_out: bool,
}

/// What became of a merge's evidence record. `Written` only when the record was read back at
/// `head` with the bytes written. It never changes the merge's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceResult {
    Written {
        repo: String,
        path: String,
        head: Sha,
    },
    NotWritten {
        reason: String,
    },
}

/// A merge the lander read on main: what its record is written from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandedMerge {
    pub base: Sha,
    pub head: Sha,
    /// The gate's steps as they ran, or `None` when no gate ran (an admin merge of `tessel.toml`).
    pub gate: Option<Vec<GateStep>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged {
        base: Sha,
        head: Sha,
        evidence: Option<EvidenceResult>,
    },
    AlreadyMerged {
        base: Sha,
    },
    Conflict {
        base: Sha,
        files: Vec<String>,
    },
    TestsFailed {
        base: Sha,
        head: Sha,
        result: StepOutcome,
    },
    Uncovered {
        base: Sha,
        head: Sha,
        files: Vec<String>,
        total: usize,
    },
    GateChanged {
        base: Sha,
        head: Sha,
    },
    GateInvalid {
        base: Sha,
        head: Sha,
    },
    MainMoved {
        expected: Sha,
        actual: Sha,
    },
    CommitNotInFork,
    /// Cloning main or fetching the fork failed.
    Clone {
        result: GitOutput,
    },
    /// A local git step failed in a way that is not a conflict.
    GitFailed {
        result: GitOutput,
    },
    Install {
        base: Sha,
        head: Sha,
        result: StepOutcome,
    },
    Timeout {
        base: Sha,
        head: Sha,
        result: StepOutcome,
    },
    /// The push did not move main and main did not move either.
    PushFailed {
        base: Sha,
        head: Sha,
        result: GitOutput,
    },
}

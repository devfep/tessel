//! What a run reports, and its conversion to the lander's step outcome.

use serde::{Deserialize, Serialize};
use tessel_lander::outcome::{PackageStep, StepOutcome};

/// The one step the runner runs. A fixed word on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultStep {
    Test,
}

/// The result of one run. `stdout`, `stderr` and `first_failure` come from the repository and are
/// untrusted data.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the wire shape is fixed: passed, timed_out and one truncated flag per stream"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunResult {
    pub step: ResultStep,
    /// Of the last command that ran; 137 when the run was killed at its timeout, and 128 plus the
    /// signal number when a signal ended the command.
    pub exit_code: i32,
    /// True only when every command exited 0 and the run did not time out.
    pub passed: bool,
    pub timed_out: bool,
    /// The last bytes of standard output, at most 256 KiB, of all the commands that ran.
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// The first failing test's block, at most 1,500 bytes. Informational: never a verdict.
    pub first_failure: Option<String>,
    pub wall_ms: u64,
}

impl RunResult {
    /// The lander's view of this run. `passed` only for exit 0 without a timeout.
    pub fn into_step_outcome(self) -> StepOutcome {
        StepOutcome {
            step: PackageStep::Test,
            exit_code: self.exit_code,
            stdout: self.stdout,
            stderr: self.stderr,
            passed: self.exit_code == 0 && !self.timed_out,
            reason: None,
            timed_out: self.timed_out,
            wall_ms: Some(self.wall_ms),
        }
    }
}

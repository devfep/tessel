//! The local test runner: runs a repository's test commands in a tree and reports pass or fail.
//!
//! It is for one trusted user on their own machine. The lander runs git and never repo code
//! (CLAUDE.md rule 4); this is the separate process that runs repo code, and it refuses to start
//! unless its configuration file names the user who started it. It runs no git and opens no network
//! connection. Started as its own account through the sudo rule, with a configuration that names
//! a `work_dir` and a `handoff_dir`, it copies the tree into its own directory, empties what the
//! account left behind, limits its processes and takes its environment from the file alone
//! (docs/proposals/own-users.md). Without them it is the in-process runner of one trusted user.

mod config;
mod error;
mod failure;
mod limits;
mod request;
mod result;
mod sandbox;
mod supervise;
mod sweep;
mod toolchain;

pub use config::{
    effective_login, effective_uid, RunnerConfig, Sandbox, CONFIG_FILE_NAME, DEFAULT_MAX_PROCESSES,
    DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS,
};
pub use error::{ConfigReason, RequestRefusal, RunnerError};
pub use failure::MAX_FIRST_FAILURE_BYTES;
pub use limits::{apply_limits, make_undumpable};
pub use request::{Plan, PlannedCommand, RequestTest, RunRequest, MAX_REQUEST_BYTES};
pub use result::{ResultStep, RunResult};
pub use supervise::MAX_TAIL_BYTES;

/// Checks `request` and, if it passes every check, runs its commands.
///
/// A run that completed returns `Ok` whether the tests passed or not; the verdict is in the
/// result. `Err` means nothing ran ([`RunnerError::Config`], [`RunnerError::Request`]) or the run
/// could not be supervised ([`RunnerError::Unsupervised`]).
pub fn run(config: &RunnerConfig, request: &RunRequest) -> Result<RunResult, RunnerError> {
    if let Some(sandbox) = config.sandbox() {
        return sandbox::run(config, sandbox, request);
    }
    let plan = request.plan(config)?;
    supervise::run_plan(config.env(), &plan)
}

//! The local test runner: runs a repository's test commands in a tree and reports pass or fail.
//!
//! It is for one trusted user on their own machine. The lander runs git and never repo code
//! (CLAUDE.md rule 4); this is the separate process that runs repo code, and it refuses to start
//! unless its configuration file names the user who started it. It runs no git, opens no network
//! connection and has no sandbox: the sandbox is M5.

mod config;
mod error;
mod failure;
mod request;
mod result;
mod supervise;

pub use config::{
    effective_login, effective_uid, RunnerConfig, CONFIG_FILE_NAME, DEFAULT_TIMEOUT_SECS,
};
pub use error::{ConfigReason, RequestRefusal, RunnerError};
pub use failure::MAX_FIRST_FAILURE_BYTES;
pub use request::{Plan, PlannedCommand, RequestTest, RunRequest, MAX_REQUEST_BYTES};
pub use result::{ResultStep, RunResult};
pub use supervise::MAX_TAIL_BYTES;

/// Checks `request` and, if it passes every check, runs its commands.
///
/// A run that completed returns `Ok` whether the tests passed or not; the verdict is in the
/// result. `Err` means nothing ran ([`RunnerError::Config`], [`RunnerError::Request`]) or the run
/// could not be supervised ([`RunnerError::Unsupervised`]).
pub fn run(config: &RunnerConfig, request: &RunRequest) -> Result<RunResult, RunnerError> {
    let plan = request.plan(config)?;
    supervise::run_plan(&config.env, &plan)
}

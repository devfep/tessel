//! The local test runner: runs a repository's test commands in a tree and reports pass or fail.
//!
//! It is for one trusted user on their own machine. The lander runs git and never repo code
//! (CLAUDE.md rule 4); this is the separate process that runs repo code, and it refuses to start
//! unless its configuration file names the user who started it. It runs no git, opens no network
//! connection and has no sandbox: the sandbox is M5.

mod config;
mod error;
mod request;

pub use config::{
    effective_login, effective_uid, RunnerConfig, CONFIG_FILE_NAME, DEFAULT_TIMEOUT_SECS,
};
pub use error::{ConfigReason, RequestRefusal, RunnerError};
pub use request::{Plan, PlannedCommand, RequestTest, RunRequest, MAX_REQUEST_BYTES};

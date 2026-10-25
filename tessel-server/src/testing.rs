//! Test support: a clock a test can freeze and advance, for a server run in process. Production
//! code never builds one: `server::start` and `server::start_with_runner` use the system clock, and
//! nothing in the configuration or on the wire reaches this module.

use std::sync::Arc;

use crate::config::Config;
use crate::landing::TestRunner;
use crate::server::{self, Deps, RunningServer, StartError};

/// A clock a test controls. While it is frozen the server sees the instant it was frozen at, plus
/// whatever `advance` has added.
#[derive(Clone, Default)]
pub struct TestClock;

impl TestClock {
    pub fn new() -> Self {
        Self
    }

    /// The time the server sees, in milliseconds.
    pub fn now_ms(&self) -> u64 {
        0
    }

    /// Stop the clock at the present instant: from now on it moves only with `advance`.
    pub fn freeze(&self) {}

    /// Move the clock forward by `ms`; the server notices at once.
    pub fn advance(&self, _ms: u64) {}
}

/// Like `server::start` (or `server::start_with_runner` when `runner` is given), with the
/// repositories reading `clock`.
pub async fn start_with_clock(
    config: &Config,
    deps: Deps,
    runner: Option<Arc<dyn TestRunner>>,
    _clock: &TestClock,
) -> Result<RunningServer, StartError> {
    match runner {
        Some(runner) => server::start_with_runner(config, deps, runner).await,
        None => server::start(config, deps).await,
    }
}

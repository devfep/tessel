//! Test support: a clock a test can freeze and advance, for a server run in process. Production
//! code never builds one: `server::start` and `server::start_with_runner` use the system clock, and
//! nothing in the configuration or on the wire reaches this module.

use std::sync::Arc;

use crate::config::Config;
use crate::landing::TestRunner;
use crate::repo::{Clock, Shared};
use crate::server::{self, Deps, RunningServer, StartError};

/// A clock a test controls. While it is frozen the server sees the instant it was frozen at, plus
/// whatever `advance` has added. Clones share one clock; build one per server.
#[derive(Clone)]
pub struct TestClock(Arc<Shared>);

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl TestClock {
    pub fn new() -> Self {
        Self(Arc::new(Shared::new()))
    }

    /// The time the server sees, in milliseconds.
    pub fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }

    /// Stop the clock at the present instant: from now on it moves only with `advance`.
    pub fn freeze(&self) {
        self.0.freeze();
    }

    /// Move the clock forward by `ms`; the server notices at once.
    pub fn advance(&self, ms: u64) {
        self.0.advance(ms);
    }
}

/// Like `server::start` (or `server::start_with_runner` when `runner` is given), with the
/// repositories reading `clock`.
pub async fn start_with_clock(
    config: &Config,
    deps: Deps,
    runner: Option<Arc<dyn TestRunner>>,
    clock: &TestClock,
) -> Result<RunningServer, StartError> {
    server::start_with(config, deps, runner, Clock::shared(Arc::clone(&clock.0))).await
}

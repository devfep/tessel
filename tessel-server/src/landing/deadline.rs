//! The time a call may take, from its own `deadline_ms`, and the signal that a call was given up.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::repo::now_ms;

/// How long before the deadline the runner is told to stop, so that it kills its tests and
/// returns before the adapter gives the call up.
const RUNNER_MARGIN_SECS: u64 = 5;

/// Whether the work of a call should go on: the call's deadline has not passed and the adapter has
/// not given the call up. Work that is past either must not push main and must not start tests.
#[derive(Debug, Clone)]
pub struct Stop {
    deadline_ms: u64,
    abandoned: Arc<AtomicBool>,
}

impl Stop {
    pub fn new(deadline_ms: u64) -> Self {
        Self {
            deadline_ms,
            abandoned: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn remaining(&self) -> Duration {
        Duration::from_millis(self.deadline_ms.saturating_sub(now_ms()))
    }

    /// The adapter answered the call without waiting for this work: it must do nothing more.
    pub fn abandon(&self) {
        self.abandoned.store(true, Ordering::SeqCst);
    }

    pub fn requested(&self) -> bool {
        self.abandoned.load(Ordering::SeqCst) || self.remaining().is_zero()
    }

    /// The seconds a test run may take: what is left of the call, less a margin, and at least one.
    pub fn run_seconds(&self) -> u64 {
        self.remaining()
            .as_secs()
            .saturating_sub(RUNNER_MARGIN_SECS)
            .max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_in_the_past_asks_the_work_to_stop() {
        assert!(Stop::new(now_ms().saturating_sub(1)).requested());
        assert!(Stop::new(0).requested());
    }

    #[test]
    fn a_deadline_ahead_lets_the_work_go_on_until_the_call_is_abandoned() {
        let stop = Stop::new(now_ms() + 60_000);
        assert!(!stop.requested());
        stop.clone().abandon();
        assert!(stop.requested(), "a clone shares the abandonment");
    }

    #[test]
    fn a_test_run_gets_the_time_left_less_the_margin_and_at_least_one_second() {
        let long = Stop::new(now_ms() + 100_000).run_seconds();
        assert!((90..=95).contains(&long), "{long}");
        assert_eq!(Stop::new(now_ms() + 2_000).run_seconds(), 1);
        assert_eq!(Stop::new(0).run_seconds(), 1);
    }
}

//! The simulated clock: the only time the host ever sees. It starts at a fixed instant and moves
//! only when the simulator moves it, in small steps or in jumps past a lease or a wake.

/// Where every run starts, in milliseconds.
pub const START_MS: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    now_ms: u64,
}

impl Default for Clock {
    fn default() -> Self {
        Self { now_ms: START_MS }
    }
}

impl Clock {
    pub fn now_ms(self) -> u64 {
        self.now_ms
    }

    /// Move forward by `ms`.
    pub fn advance(&mut self, ms: u64) {
        self.now_ms = self.now_ms.saturating_add(ms);
    }

    /// Jump to `at_ms`, or stay where it is if that has passed: time never runs backwards.
    pub fn jump_to(&mut self, at_ms: u64) {
        self.now_ms = self.now_ms.max(at_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jumps_forward_and_never_back() {
        let mut clock = Clock::default();
        clock.advance(500);
        assert_eq!(clock.now_ms(), START_MS + 500);
        clock.jump_to(START_MS + 90_000);
        assert_eq!(clock.now_ms(), START_MS + 90_000);
        clock.jump_to(START_MS);
        assert_eq!(clock.now_ms(), START_MS + 90_000);
    }
}

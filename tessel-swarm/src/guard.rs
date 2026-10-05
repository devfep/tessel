//! The target guard. The harness creates, writes and floods repositories, so it may only ever
//! touch scratch repositories it names itself: `swarm-` plus a suffix. Every function that
//! reaches a coordinator or a steward takes a `ScratchRepo`, which only `parse` can build, so no
//! code path can address another repository.

use std::fmt;

use anyhow::{bail, Result};

/// Repositories that must never be a target, whatever their name looks like.
const PROTECTED: [&str; 2] = ["tessel-dogfood", "demo"];
const PREFIX: &str = "swarm-";
const MAX_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchRepo(String);

impl ScratchRepo {
    /// Accepts `swarm-<suffix>` where the suffix is `A-Za-z0-9._` plus single hyphens. A double
    /// hyphen is refused because it separates a repo from an agent in fork names.
    pub fn parse(name: &str) -> Result<Self> {
        if PROTECTED.contains(&name) {
            bail!("{name:?} is a protected repository; the swarm only targets {PREFIX}* scratch repos");
        }
        let Some(suffix) = name.strip_prefix(PREFIX) else {
            bail!("{name:?} is not a scratch repository: the name must start with {PREFIX:?}");
        };
        if suffix.is_empty() || name.len() > MAX_LEN {
            bail!("{name:?} needs a suffix after {PREFIX:?} and at most {MAX_LEN} characters");
        }
        let plain = suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !plain || suffix.contains("--") || suffix.ends_with('-') {
            bail!("{name:?} may use only A-Z a-z 0-9 . _ and single hyphens after {PREFIX:?}");
        }
        Ok(Self(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScratchRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_names_pass() {
        for ok in ["swarm-1", "swarm-seed1-k3x9", "swarm-a.b_c"] {
            assert_eq!(
                ScratchRepo::parse(ok)
                    .map(|r| r.to_string())
                    .ok()
                    .as_deref(),
                Some(ok)
            );
        }
    }

    #[test]
    fn protected_and_foreign_names_are_refused() {
        for bad in [
            "tessel-dogfood",
            "demo",
            "swarm",
            "swarm-",
            "Swarm-1",
            "my-swarm-1",
            "swarm-a--b",
            "swarm-a-",
            "swarm-a/b",
            "swarm-a b",
            "swarm-../demo",
            "",
        ] {
            assert!(ScratchRepo::parse(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(ScratchRepo::parse(&format!("swarm-{}", "x".repeat(80))).is_err());
    }

    #[test]
    fn refusal_names_the_rule() {
        let message = ScratchRepo::parse("tessel-dogfood")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(message.contains("protected"), "{message}");
    }
}

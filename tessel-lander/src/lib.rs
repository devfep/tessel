//! The lander's git layer: builds and runs `git` invocations with a fixed environment, and checks
//! a rebased change against a claim. Ported from the steward's `merge-commands.ts` and
//! `merge-coverage.ts`, which stay the reference until the steward is removed.
//!
//! The lander runs git but never repo code. Every command here runs with a cleared environment
//! (see [`git::Git`]), so no user or system config, hook, editor, prompt or ambient `GIT_*`
//! variable can reach it.

pub mod coverage;
pub mod git;
pub mod run;
pub mod types;

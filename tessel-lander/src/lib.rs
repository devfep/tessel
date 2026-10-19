//! The lander's git layer: builds and runs `git` invocations with a fixed environment, checks a
//! rebased change against a claim, and merges a commit in two calls (`prepare`, then `land`).
//! Ported from the steward's `merge-commands.ts`, `merge-coverage.ts` and `merge-steps.ts`, which
//! stay the reference until the steward is removed.
//!
//! The lander runs git but never repo code. Every command here runs with a cleared environment
//! (see [`git::Git`]), so no user or system config, hook, editor, prompt or ambient `GIT_*`
//! variable can reach it, and git commands that read a repository's own config run only in a
//! [`workspace::Workspace`], which only the clone step makes.

pub mod coverage;
pub mod gate;
pub mod git;
pub mod merge;
pub mod outcome;
pub mod run;
pub mod trial;
pub mod types;
pub mod workspace;

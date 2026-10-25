//! Tests of the runner's refusals and, with a stand-in `cargo`, of its runs.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a failed expect or panic is a failed test; the helpers are not #[test] functions"
)]

mod binary;
mod common;
mod refusals;
mod running;
mod sandbox;

//! Tessel coordinator crate: the wire protocol, the pure coordinator core, the shell helpers, the
//! merge request and answer types, the portable host and the simulator that tests it. The CLI and
//! the lander depend on it so every side speaks the same types.

// `clippy.toml` bans the wall clocks for this crate, and the crate allows the ban everywhere but
// the host and the simulator, which take time as input. The ban reaches this crate's `tests/` too
// (they do not set the allow). The other crates of the workspace have their own `clippy.toml` and
// keep their clocks.
#![allow(clippy::disallowed_methods)]

pub mod coordinator;
#[deny(clippy::disallowed_methods)]
pub mod host;
pub mod merge;
pub mod protocol;
pub mod shell;
#[deny(clippy::disallowed_methods)]
pub mod sim;

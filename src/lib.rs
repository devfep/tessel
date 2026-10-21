//! Tessel coordinator crate: the wire protocol, the pure coordinator core, the shell helpers, the
//! merge request and answer types, and the portable host. The CLI and the lander depend on it so
//! every side speaks the same types.

pub mod coordinator;
pub mod host;
pub mod merge;
pub mod protocol;
pub mod shell;

//! Tessel coordinator crate. The pure parts (`protocol`, `coordinator`, `shell`) build natively
//! and are shared with the CLI, so both sides speak the same types. The Worker and Durable
//! Object glue (`runtime`, `store`, `identity`) is behind the `runtime` feature, which is on by
//! default: the wasm build uses it, and the CLI depends on this crate without it so a native
//! build never needs the `worker` crate or the signing primitives.

pub mod coordinator;
pub mod merge;
// `protocol` is frozen and carries `expect(dead_code)` on items that are dead only while the
// module is private. Public, those expectations are unfulfilled, which is not an error here.
#[allow(unfulfilled_lint_expectations)]
pub mod protocol;
pub mod shell;

#[cfg(feature = "runtime")]
mod identity;
#[cfg(feature = "runtime")]
mod runtime;
#[cfg(feature = "runtime")]
mod store;

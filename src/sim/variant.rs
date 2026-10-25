//! Host variants: the real host with one planted bug in how it is run. Each variant breaks one
//! invariant, and the test of that invariant's checker must see it. `Correct` plants nothing and
//! is the only variant a seed may pass with no violation by luck.

/// Which bug, if any, the run carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// No bug: every checker must stay silent.
    Correct,
    /// The adapter acknowledges a commit before it is durable and writes it one commit later, so
    /// a grant can reach a client that a crash then erases (breaks I1).
    WriteBehind,
    /// A load returns the state from one commit before the last (a lagging read), so after a
    /// reload the host issues a fence a client already holds (breaks I9).
    StaleLoad,
    /// The stored state carries a field the codec drops on its way back (breaks I5).
    DriftingCodec,
}

impl Variant {
    pub const PLANTED: [Variant; 3] = [
        Variant::WriteBehind,
        Variant::StaleLoad,
        Variant::DriftingCodec,
    ];
}

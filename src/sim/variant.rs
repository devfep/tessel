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
    /// The platform loses some of the wakes the host sets, so a lease or a merge due later has no
    /// wake pending (breaks I4).
    LostWake,
    /// A replay page the adapter reads leaves out an event (breaks I2).
    SkipsReplayEvent,
    /// A load returns the claims without their scopes, so the host places no locks for them and
    /// grants what conflicts (breaks I8).
    LosesScopesOnLoad,
    /// The adapter sends a heartbeat of its own after every frame, so an agent that never
    /// heartbeats keeps its lease (breaks I10).
    PhantomHeartbeat,
    /// The adapter loses `doubt_since_ms` of the merge in flight on its way to the store (it writes
    /// null), so a claim whose landing is in doubt has a merge in flight nobody was told about
    /// (breaks the fenced-claim rule, `Checker::FencedRule`).
    UntoldDoubt,
    /// The adapter loses `doubt_since_ms` of a merge in flight that moves from a `Prepare` to a
    /// `Land` under a new dispatch id (it writes null), as a core would that cleared it when the
    /// answer to a doubted `Prepare` sent the doubted pair on to `Land`: the claim is still
    /// fenced, the merge is in flight, and nobody was told (breaks the fenced-claim rule,
    /// `Checker::FencedRule`).
    DoubtClearedAtLand,
    /// After the host takes the answer to a `Prepare` in doubt, the adapter writes the merge in
    /// flight and its claim's work from before the answer back over the state the host staged
    /// (and the claim itself, if the answer removed it), as a core would whose answer left no
    /// trace in what it stores: the host's memory moves on and the stored state does not (breaks
    /// the liveness rule, `Checker::DoubtLiveness`).
    DoubtedPrepareAnswerDropped,
    /// The same for the answer to a `Reconcile` in doubt (breaks `Checker::DoubtLiveness`).
    DoubtedReconcileAnswerDropped,
}

impl Variant {
    pub const PLANTED: [Variant; 11] = [
        Variant::WriteBehind,
        Variant::StaleLoad,
        Variant::DriftingCodec,
        Variant::LostWake,
        Variant::SkipsReplayEvent,
        Variant::LosesScopesOnLoad,
        Variant::PhantomHeartbeat,
        Variant::UntoldDoubt,
        Variant::DoubtClearedAtLand,
        Variant::DoubtedPrepareAnswerDropped,
        Variant::DoubtedReconcileAnswerDropped,
    ];
}

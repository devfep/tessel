//! Tessel claim protocol between the agent CLI and the per-repo coordinator (Durable Object).
//! Transport: JSON over WebSocket. Shared crate, compiled natively (CLI) and to wasm (Worker).
//!
//! Coordinator invariants:
//! 1. Claims are atomic: every scope granted, or none.
//! 2. `OnConflict::Wait` is only allowed if the agent holds no other claims.
//!    No hold-and-wait means no deadlock.
//! 3. Every claim has a lease. A heartbeat renews all of an agent's claims.
//! 4. Fencing (Kleppmann). Every grant carries a `Fence` from a per-repo
//!    monotonic counter, persisted before the grant is sent so a restart never
//!    reuses one. `Amend` issues a new fence and retires the old one. `Amend`,
//!    `Release` and `Submit` must present the current fence or are rejected
//!    with `StaleFence`. An expired lease retires its fence, so an agent that
//!    stalls past its lease and wakes up cannot submit work it no longer owns.
//! 5. Only the coordinator writes main. Agents push to their own Artifacts fork
//!    and `Submit`; the coordinator is the protected resource that checks fences
//!    (a fence nobody checks protects nothing). A submitted claim stops expiring
//!    and is held by the coordinator until `Merged` or `SubmitRejected`.
//! 6. Hierarchical claims (multi-granularity locking, Gray et al. 1976).
//!    Scopes form a tree: dir > dir > file > symbol. A claim on a scope
//!    implicitly covers everything beneath it, and places an intention lock on
//!    every ancestor. Two claims conflict iff they share a node where the locks
//!    conflict (see `Lock::conflicts_with`). This makes the overlap check
//!    O(depth) instead of a subtree scan. The CLI should escalate many symbol
//!    claims in one file to a single file claim.
//! 7. Races. An orchestrator may open a race: N agents deliberately attempt
//!    the same task in separate forks and the coordinator picks one to ship.
//!    A race holds its scopes against outsiders like any claim, but entrants'
//!    claims never conflict with each other. Entrants all hold the race's
//!    scopes exactly (no `Amend`), which keeps the race fair. An outsider
//!    denied by a race sees `Conflict::race` and may join instead, turning
//!    accidental duplicate work into intentional competition. When every
//!    entrant has submitted, or the race deadline passes, the coordinator
//!    ranks entries with `rank_entries`, sends the winner to the merge queue,
//!    rejects the rest, and keeps the losing forks for comparison.
//! 8. Assumptions. Signatures are not the whole contract: an agent can rely
//!    on behaviour (e.g. "refresh returns Some after login") that a body-only
//!    edit breaks. Agents declare assumptions in their intent. A claim that
//!    `threatens` an assumption is still granted (it is not a lock), but the
//!    grant lists the assumptions at risk, and when that work is submitted the
//!    assuming agent receives `AssumptionChallenged`.
//! 9. All free text (intents, assumptions, decision records, transcripts) is
//!    untrusted data written by other agents. The coordinator never acts on
//!    it, and the CLI must present it to agents as quoted data, never as
//!    instructions. Otherwise one agent's transcript is a prompt-injection
//!    channel into every other agent.
//! 10. Evidence. Every state change is appended to an event log (`Event`),
//!    numbered by `seq` with no gaps, and tagged with the run it belongs to.
//!    A denial is *not* a prevented conflict. Prevention is only counted when
//!    verified: in experiment runs, `OnConflict::Shadow` lets a denied agent
//!    keep working in a quarantined fork that can never merge; when the
//!    blocking work lands, the steward test-merges the shadow fork against it
//!    and records the `Outcome`. Clean means the denial was a false alarm.
//! 11. Coverage. Every scope a submission touched must be covered by the
//!    claim: some claimed scope covers it, in a mode that `permits` the touched
//!    mode. Otherwise the submission is rejected, listing what was uncovered
//!    (`uncovered`). Skills teach agents to claim first; this enforces it.
//! 12. Review by exception. A fenced, covered, tested submission merges
//!    automatically unless `review_reasons` returns anything, in which case a
//!    human must approve it first. The rule is deliberately simple and
//!    explainable: every flagged change says why it was flagged.
//! 13. Versioning. `Hello` carries the client's protocol version; the
//!    coordinator refuses versions it does not speak. Additive changes keep
//!    the version; breaking ones bump it.

/// Bump only for breaking changes. Additive fields use `#[serde(default)]`.
pub const PROTOCOL_VERSION: u16 = 1;

fn v1() -> u16 {
    1
}

use serde::{Deserialize, Serialize};

// ---------- Identity ----------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(pub String);

/// Identity of a claim. Assigned by the coordinator, monotonic per repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClaimId(pub u64);

/// Authority to act on a claim. Monotonic per repo; a newer fence always
/// supersedes an older one for the same claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fence(pub u64);

/// Identity of a race. Assigned by the coordinator, monotonic per repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RaceId(pub u64);

/// Assigned by the client to match responses to requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

/// Git commit SHA.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommitId(pub String);

/// File path + fully qualified name, e.g.
/// ("src/auth/session.rs", "auth::session::Session::refresh").
/// A rename is `EditSignature` on the old id plus `Create` on the new one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolId {
    pub path: String,
    pub qualified_name: String,
}

// ---------- Scopes (the lock hierarchy) ----------

/// A node in the lock tree. Paths are repo-relative, '/'-separated, with no
/// leading or trailing slash; the repo root is `Dir { path: "" }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    Dir { path: String },
    File { path: String },
    Symbol(SymbolId),
}

impl Scope {
    pub fn root() -> Scope {
        Scope::Dir { path: String::new() }
    }

    /// Strict ancestors, nearest first, ending at the repo root.
    pub fn ancestors(&self) -> Vec<Scope> {
        let mut out = Vec::new();
        let mut dir = match self {
            Scope::Symbol(s) => {
                out.push(Scope::File { path: s.path.clone() });
                Some(parent_dir(&s.path))
            }
            Scope::File { path } => Some(parent_dir(path)),
            Scope::Dir { path } if path.is_empty() => None,
            Scope::Dir { path } => Some(parent_dir(path)),
        };
        while let Some(d) = dir {
            out.push(Scope::Dir { path: d.to_string() });
            dir = if d.is_empty() { None } else { Some(parent_dir(d)) };
        }
        out
    }

    /// True if `self` is `other` or one of its ancestors.
    pub fn covers(&self, other: &Scope) -> bool {
        self == other || other.ancestors().contains(self)
    }
}

fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

// ---------- Claim modes ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Relies on the signatures in scope (calls, implements, imports).
    Depend,
    /// Changes bodies only. Callers are unaffected.
    EditBody,
    /// Changes signatures, renames, or deletes. Breaks dependents.
    EditSignature,
    /// Introduces new symbols in scope.
    Create,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Depend, Mode::EditBody, Mode::EditSignature, Mode::Create];

    /// Whether two claims on overlapping scopes conflict.
    /// Deliberately exhaustive with no wildcard: adding a variant forces
    /// a decision for every pairing.
    /// Does holding a claim in `self` authorise work done in `needed`?
    /// Invariant: if `self` permits `needed`, everything that conflicts with
    /// `needed` also conflicts with `self`, so the claim really protected the
    /// work (checked in tests).
    pub fn permits(self, needed: Mode) -> bool {
        use Mode::*;
        match (self, needed) {
            (a, b) if a == b => true,
            (EditSignature, EditBody) => true,
            (EditSignature | EditBody | Create, Depend) => true,
            _ => false,
        }
    }

    pub fn conflicts_with(self, other: Mode) -> bool {
        use Mode::*;
        match (self, other) {
            (Depend, Depend) => false,
            (Depend, EditBody) | (EditBody, Depend) => false,
            (Depend, Create) | (Create, Depend) => false,
            // The semantic conflict git misses: a caller vs. a signature change.
            (Depend, EditSignature) | (EditSignature, Depend) => true,
            (EditBody, EditBody) => true,
            (EditBody, EditSignature) | (EditSignature, EditBody) => true,
            (EditSignature, EditSignature) => true,
            // Two agents adding the same thing: duplicate work.
            (Create, Create) => true,
            (Create, EditBody) | (EditBody, Create) => true,
            (Create, EditSignature) | (EditSignature, Create) => true,
        }
    }
}

/// What one claim holds on one node of the scope tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "lock", content = "mode", rename_all = "snake_case")]
pub enum Lock {
    /// The claim is on this node, and implicitly on everything beneath it.
    Explicit(Mode),
    /// The claim is on some descendant of this node, in this mode.
    Intention(Mode),
}

impl Lock {
    /// Gray's multi-granularity rule generalised to our modes: intentions never
    /// conflict with each other; any pairing with an explicit lock conflicts
    /// iff the underlying modes do. Restricted to {Depend, EditSignature} this
    /// reproduces Gray's IS/IX/S/X matrix exactly (see tests).
    pub fn conflicts_with(self, other: Lock) -> bool {
        match (self, other) {
            (Lock::Intention(_), Lock::Intention(_)) => false,
            (Lock::Explicit(a) | Lock::Intention(a), Lock::Explicit(b) | Lock::Intention(b)) => {
                a.conflicts_with(b)
            }
        }
    }
}

// ---------- Payloads ----------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopeClaim {
    pub scope: Scope,
    pub mode: Mode,
}

/// Touched scopes not covered by any claimed scope in a permitting mode
/// (invariant 11). Empty means the submission is fully covered.
pub fn uncovered(claimed: &[ScopeClaim], touched: &[ScopeClaim]) -> Vec<ScopeClaim> {
    touched
        .iter()
        .filter(|t| {
            !claimed
                .iter()
                .any(|c| c.scope.covers(&t.scope) && c.mode.permits(t.mode))
        })
        .cloned()
        .collect()
}

impl ScopeClaim {
    /// Every (node, lock) pair this claim places in the lock table:
    /// an explicit lock on its scope and an intention lock on each ancestor.
    pub fn locks(&self) -> Vec<(Scope, Lock)> {
        let mut out = vec![(self.scope.clone(), Lock::Explicit(self.mode))];
        out.extend(
            self.scope
                .ancestors()
                .into_iter()
                .map(|a| (a, Lock::Intention(self.mode))),
        );
        out
    }
}

/// Why the agent is doing this. Shown to other agents on conflict,
/// and written to git-notes when the work merges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub summary: String,
    pub task_ref: Option<String>,
    /// Behaviour this work relies on but does not own.
    #[serde(default)]
    pub assumptions: Vec<Assumption>,
}

/// A behavioural expectation about code the agent does not intend to change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Assumption {
    pub scope: Scope,
    /// e.g. "returns Some after a successful login". Untrusted text.
    pub statement: String,
}

impl Assumption {
    /// True if `claim` could break this assumption: it edits bodies or
    /// signatures in a scope that overlaps the assumed one, in either direction.
    pub fn threatened_by(&self, claim: &ScopeClaim) -> bool {
        matches!(claim.mode, Mode::EditBody | Mode::EditSignature)
            && (claim.scope.covers(&self.scope) || self.scope.covers(&claim.scope))
    }
}

/// An assumption held by another agent, reported to someone about to break it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldAssumption {
    pub agent: AgentId,
    pub claim: ClaimId,
    pub assumption: Assumption,
}

/// Distilled at submit time and stored in git-notes beside the commit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Approaches tried and abandoned, with why. Saves the next agent the retry.
    #[serde(default)]
    pub rejected: Vec<RejectedApproach>,
    /// Evidence, e.g. "cargo test auth:: passed (42 tests)".
    #[serde(default)]
    pub evidence: Vec<String>,
    pub transcript: Option<TranscriptRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedApproach {
    pub approach: String,
    pub reason: String,
}

/// Pointer to a full, redacted session transcript stored outside the code
/// repo (its own Artifacts repo), so code history stays small and secret-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptRef {
    /// Artifacts repo holding transcripts, e.g. "transcripts/agent-17".
    pub repo: String,
    pub path: String,
    /// Hex SHA-256 of the stored (already redacted) transcript.
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnConflict {
    /// Deny immediately; the agent picks other work or coordinates.
    Fail,
    /// Queue until the conflicting claims are released.
    Wait,
    /// Experiment runs only (invariant 10). Record the denial, then let the
    /// agent continue in a quarantined fork that places no locks and can never
    /// merge, so the steward can later verify whether the conflict was real.
    Shadow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    /// What you asked for.
    pub requested: ScopeClaim,
    /// What blocks it. May be an ancestor or descendant of your scope.
    pub held: ScopeClaim,
    pub held_by: AgentId,
    pub their_intent: Intent,
    /// Set when the blocking claim belongs to an open race you could join.
    pub race: Option<RaceId>,
}

// ---------- Races ----------

/// How a race picks its winner, applied in order. `TestsPass` filters;
/// the others rank, with earlier criteria taking precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Criterion {
    /// Drop entries whose tests did not pass.
    TestsPass,
    /// Lower risk score first.
    LowestRisk,
    /// Fewer changed lines first.
    SmallestDiff,
    /// Earlier submission first.
    FirstSubmitted,
    /// Rank as a recommendation, then wait for `PickWinner` from a human.
    HumanPick,
}

/// One entrant's result, as judged by the coordinator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaceEntry {
    pub claim: ClaimId,
    pub agent: AgentId,
    pub fork_commit: CommitId,
    pub submitted_at_ms: u64,
    /// `None` until tests have run.
    pub tests_passed: Option<bool>,
    /// Risk in basis points, 0..=10_000. Integer so ranking is deterministic.
    pub risk_bp: Option<u16>,
    pub diff_lines: Option<u32>,
}

/// Deterministic ranking: best first. Entries missing a measured value sort
/// after entries that have one; the final tie-break is claim id, so the same
/// inputs always produce the same winner.
pub fn rank_entries(criteria: &[Criterion], entries: &[RaceEntry]) -> Vec<ClaimId> {
    let mut pool: Vec<&RaceEntry> = entries
        .iter()
        .filter(|e| !criteria.contains(&Criterion::TestsPass) || e.tests_passed == Some(true))
        .collect();
    pool.sort_by(|a, b| {
        for c in criteria {
            let ord = match c {
                Criterion::LowestRisk => none_last(a.risk_bp, b.risk_bp),
                Criterion::SmallestDiff => none_last(a.diff_lines, b.diff_lines),
                Criterion::FirstSubmitted => a.submitted_at_ms.cmp(&b.submitted_at_ms),
                Criterion::TestsPass | Criterion::HumanPick => std::cmp::Ordering::Equal,
            };
            if ord.is_ne() {
                return ord;
            }
        }
        a.claim.0.cmp(&b.claim.0)
    });
    pool.into_iter().map(|e| e.claim).collect()
}

fn none_last<T: Ord>(a: Option<T>, b: Option<T>) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Less,
        (None, Some(_)) => Greater,
        (None, None) => Equal,
    }
}

// ---------- Messages ----------

/// CLI -> coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Must be the first message on a connection.
    Hello {
        agent: AgentId,
        base: CommitId,
        #[serde(default = "v1")]
        protocol: u16,
    },
    Claim {
        req: RequestId,
        intent: Intent,
        scopes: Vec<ScopeClaim>,
        on_conflict: OnConflict,
    },
    /// Add scopes discovered mid-task. Atomic; never waits. On success the
    /// coordinator issues a new fence and retires `fence`.
    Amend { req: RequestId, claim: ClaimId, fence: Fence, add: Vec<ScopeClaim> },
    Heartbeat,
    Release { claim: ClaimId, fence: Fence },
    /// Work is pushed to the agent's fork and ready to merge. `touched` is what
    /// actually changed, which may differ from what was claimed.
    Submit {
        req: RequestId,
        claim: ClaimId,
        fence: Fence,
        fork_commit: CommitId,
        touched: Vec<ScopeClaim>,
        #[serde(default)]
        decisions: DecisionRecord,
    },
    /// Orchestrator: open a race for one task.
    OpenRace {
        req: RequestId,
        intent: Intent,
        scopes: Vec<ScopeClaim>,
        max_entrants: u32,
        deadline_ms: u64,
        criteria: Vec<Criterion>,
    },
    /// Agent: enter an open race. Answered with `Granted` (with `race` set).
    JoinRace { req: RequestId, race: RaceId },
    /// Human or orchestrator: choose the winner of a `HumanPick` race.
    PickWinner { req: RequestId, race: RaceId, claim: ClaimId },
    /// Human reviewer: approve or reject a flagged submission.
    Review { req: RequestId, claim: ClaimId, approve: bool, note: Option<String> },
    /// Dashboard or harness: stream events with `seq >= from_seq`
    /// (0 replays the whole log), then follow live.
    Watch { from_seq: u64 },
}

/// Coordinator -> CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome { head: CommitId, lease_ms: u64, protocol: u16 },
    Granted {
        req: RequestId,
        claim: ClaimId,
        fence: Fence,
        expires_at_ms: u64,
        /// Set when this claim is a race entry.
        race: Option<RaceId>,
        /// Other agents' assumptions this claim could break (invariant 8).
        #[serde(default)]
        at_risk: Vec<HeldAssumption>,
    },
    Denied { req: RequestId, conflicts: Vec<Conflict> },
    /// `OnConflict::Shadow`: denied for real, but you may continue in a
    /// quarantined fork. Your submission is used only for verification.
    Shadowed {
        req: RequestId,
        claim: ClaimId,
        fence: Fence,
        expires_at_ms: u64,
        conflicts: Vec<Conflict>,
    },
    Queued { req: RequestId, position: u32 },
    /// Submission passed the fence check and is in the merge queue.
    Accepted { req: RequestId, claim: ClaimId, queue_position: u32 },
    Merged { claim: ClaimId, head: CommitId },
    SubmitRejected { claim: ClaimId, reason: String },
    /// Invariant 11: these touched scopes were not covered by your claim.
    Uncovered { claim: ClaimId, scopes: Vec<ScopeClaim> },
    /// Invariant 12: waiting for a human. Sent to the submitter and watchers.
    ReviewRequired { claim: ClaimId, reasons: Vec<ReviewReason> },
    /// Main moved under you, touching scopes you have claimed.
    BaseMoved { head: CommitId, by: AgentId, affected: Vec<Scope> },
    /// Submitted work touches something your claim assumes. Re-check it.
    AssumptionChallenged {
        claim: ClaimId,
        assumption: Assumption,
        by: AgentId,
        their_commit: CommitId,
    },
    /// Your lease lapsed; `fence` is now retired.
    LeaseExpired { claim: ClaimId, fence: Fence },
    RaceOpened { req: RequestId, race: RaceId, deadline_ms: u64 },
    /// Sent to every entrant. `winner` is `None` while awaiting `PickWinner`,
    /// or if no entry survived the filters. `ranking` is best first.
    RaceResult {
        race: RaceId,
        winner: Option<ClaimId>,
        ranking: Vec<ClaimId>,
        entries: Vec<RaceEntry>,
    },
    /// One entry of the event log, for `Watch` subscribers.
    Event { event: Event },
    Error { req: Option<RequestId>, code: ErrorCode, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NoHello,
    UnknownClaim,
    NotOwner,
    /// Invariant 2: Wait requested while holding other claims.
    WaitWhileHolding,
    /// Invariant 4: the fence presented is not the claim's current fence.
    StaleFence,
    AlreadySubmitted,
    UnknownRace,
    RaceClosed,
    RaceFull,
    /// Invariant 7: race entries cannot amend their scopes.
    RaceScopeFixed,
    /// `PickWinner` named a claim that is not an eligible entry.
    NotAnEntrant,
    /// `OnConflict::Shadow` used outside an experiment run.
    ShadowDisabled,
    /// Invariant 13: the client's protocol version is not supported.
    UnsupportedProtocol,
    /// `Review` named a claim that is not awaiting review.
    NotAwaitingReview,
    Malformed,
}

// ---------- Review by exception (invariant 12) ----------

/// Why a submission needs a human. Shown verbatim on the review screen.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ReviewReason {
    /// Changes a signature others may depend on.
    SignatureChange { scope: Scope },
    /// Could break assumptions other agents declared.
    ThreatensAssumptions { count: u32 },
    /// Touches a path the repo marks sensitive (e.g. "src/auth/", "migrations/").
    SensitivePath { scope: Scope, pattern: String },
    /// No test evidence attached.
    NoTestEvidence,
}

/// The whole review policy. Empty result = merge automatically.
/// `sensitive` entries are path prefixes, matched against each touched scope.
pub fn review_reasons(
    touched: &[ScopeClaim],
    threatened_assumptions: u32,
    has_test_evidence: bool,
    sensitive: &[String],
) -> Vec<ReviewReason> {
    let mut out = Vec::new();
    for t in touched {
        if t.mode == Mode::EditSignature {
            out.push(ReviewReason::SignatureChange { scope: t.scope.clone() });
        }
        let path = match &t.scope {
            Scope::Dir { path } | Scope::File { path } => path,
            Scope::Symbol(s) => &s.path,
        };
        if let Some(p) = sensitive.iter().find(|p| path.starts_with(p.as_str())) {
            out.push(ReviewReason::SensitivePath { scope: t.scope.clone(), pattern: p.clone() });
        }
    }
    if threatened_assumptions > 0 {
        out.push(ReviewReason::ThreatensAssumptions { count: threatened_assumptions });
    }
    if !has_test_evidence {
        out.push(ReviewReason::NoTestEvidence);
    }
    out
}

// ---------- Evidence (invariant 10) ----------

/// Which run an event belongs to, e.g. "dogfood", "ab-17-coordinated",
/// "ab-17-uncoordinated". Keeps real use and experiments separable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub String);

/// Result of test-merging one change against another, or against main.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Clean,
    TextualConflict,
    BuildFailed,
    TestsFailed,
    /// Could not be judged (e.g. sandbox error). Excluded from precision.
    Inconclusive,
}

impl Outcome {
    /// Did this outcome show a real conflict?
    pub fn is_conflict(self) -> Option<bool> {
        match self {
            Outcome::Clean => Some(false),
            Outcome::TextualConflict | Outcome::BuildFailed | Outcome::TestsFailed => Some(true),
            Outcome::Inconclusive => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    Agent,
    LeaseExpired,
    LostRace,
    Merged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Position in the log: starts at 0, increments by 1, never reused.
    pub seq: u64,
    pub at_ms: u64,
    pub run: RunId,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    AgentConnected { agent: AgentId },
    ClaimGranted {
        agent: AgentId,
        claim: ClaimId,
        fence: Fence,
        scopes: Vec<ScopeClaim>,
        intent: Intent,
        race: Option<RaceId>,
        at_risk: Vec<HeldAssumption>,
    },
    ClaimDenied { agent: AgentId, scopes: Vec<ScopeClaim>, intent: Intent, conflicts: Vec<Conflict> },
    ClaimShadowed { agent: AgentId, claim: ClaimId, scopes: Vec<ScopeClaim>, conflicts: Vec<Conflict> },
    ClaimAmended { claim: ClaimId, fence: Fence, added: Vec<ScopeClaim> },
    ClaimReleased { claim: ClaimId, reason: ReleaseReason },
    Submitted { claim: ClaimId, fork_commit: CommitId, touched: Vec<ScopeClaim>, decisions: DecisionRecord },
    Merged { claim: ClaimId, head: CommitId },
    SubmitRejected { claim: ClaimId, reason: String },
    ReviewRequested { claim: ClaimId, reasons: Vec<ReviewReason> },
    ReviewDecided { claim: ClaimId, approve: bool, note: Option<String> },
    BaseMoved { head: CommitId, by: AgentId, notified: Vec<AgentId> },
    AssumptionChallenged { claim: ClaimId, assumption: Assumption, by: AgentId, their_commit: CommitId },
    RaceOpened { race: RaceId, scopes: Vec<ScopeClaim>, criteria: Vec<Criterion> },
    RaceDecided { race: RaceId, winner: Option<ClaimId>, ranking: Vec<ClaimId> },
    /// Shadow verification: the denied work, test-merged against what blocked it.
    DenialVerified { shadow_claim: ClaimId, blocking_claim: ClaimId, outcome: Outcome },
    /// After a challenged change merged, the assuming agent's tests were re-run.
    AssumptionVerified { claim: ClaimId, assumption: Assumption, outcome: Outcome },
    /// Uncoordinated A/B runs: plain-git merge of one agent's work, in order.
    ReplayMerged { agent: AgentId, fork_commit: CommitId, outcome: Outcome },
}

/// Counters for the dashboard and the A/B table, computed one way everywhere.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub claims_granted: u64,
    /// Denials issued. NOT the same as conflicts prevented.
    pub denials: u64,
    /// Denials proven real by shadow verification. The honest headline.
    pub conflicts_prevented_verified: u64,
    pub false_alarms: u64,
    /// verified / (verified + false alarms); `None` until anything is verified.
    pub precision: Option<f64>,
    pub assumptions_challenged: u64,
    pub assumptions_confirmed_broken: u64,
    pub merges: u64,
    /// Merges that needed a human. merges - this = merged without review.
    pub reviews_requested: u64,
    pub base_moved_notices: u64,
    pub races_decided: u64,
    /// Uncoordinated replay merges: total, and how many hit a conflict.
    pub replay_merges: u64,
    pub replay_conflicts: u64,
}

impl Summary {
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a Event>) -> Summary {
        let mut s = Summary::default();
        for e in events {
            match &e.kind {
                EventKind::ClaimGranted { .. } => s.claims_granted += 1,
                EventKind::ClaimDenied { .. } | EventKind::ClaimShadowed { .. } => s.denials += 1,
                EventKind::DenialVerified { outcome, .. } => match outcome.is_conflict() {
                    Some(true) => s.conflicts_prevented_verified += 1,
                    Some(false) => s.false_alarms += 1,
                    None => {}
                },
                EventKind::AssumptionChallenged { .. } => s.assumptions_challenged += 1,
                EventKind::AssumptionVerified { outcome, .. } => {
                    if outcome.is_conflict() == Some(true) {
                        s.assumptions_confirmed_broken += 1;
                    }
                }
                EventKind::Merged { .. } => s.merges += 1,
                EventKind::ReviewRequested { .. } => s.reviews_requested += 1,
                EventKind::BaseMoved { notified, .. } => s.base_moved_notices += notified.len() as u64,
                EventKind::RaceDecided { .. } => s.races_decided += 1,
                EventKind::ReplayMerged { outcome, .. } => {
                    s.replay_merges += 1;
                    if outcome.is_conflict() == Some(true) {
                        s.replay_conflicts += 1;
                    }
                }
                EventKind::AgentConnected { .. }
                | EventKind::ClaimAmended { .. }
                | EventKind::ClaimReleased { .. }
                | EventKind::Submitted { .. }
                | EventKind::SubmitRejected { .. }
                | EventKind::ReviewDecided { .. }
                | EventKind::RaceOpened { .. } => {}
            }
        }
        let judged = s.conflicts_prevented_verified + s.false_alarms;
        if judged > 0 {
            s.precision = Some(s.conflicts_prevented_verified as f64 / judged as f64);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(path: &str, name: &str) -> Scope {
        Scope::Symbol(SymbolId { path: path.into(), qualified_name: name.into() })
    }
    fn file(path: &str) -> Scope {
        Scope::File { path: path.into() }
    }
    fn dir(path: &str) -> Scope {
        Scope::Dir { path: path.into() }
    }

    #[test]
    fn conflict_matrix_is_symmetric() {
        for a in Mode::ALL {
            for b in Mode::ALL {
                assert_eq!(a.conflicts_with(b), b.conflicts_with(a), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn signature_change_breaks_callers() {
        assert!(Mode::Depend.conflicts_with(Mode::EditSignature));
        assert!(!Mode::Depend.conflicts_with(Mode::EditBody));
    }

    #[test]
    fn ancestors_walk_to_root() {
        assert_eq!(
            sym("src/auth/session.rs", "refresh").ancestors(),
            vec![file("src/auth/session.rs"), dir("src/auth"), dir("src"), dir("")]
        );
        assert_eq!(file("main.rs").ancestors(), vec![dir("")]);
        assert!(Scope::root().ancestors().is_empty());
    }

    /// With S = Depend and X = EditSignature, the generalised rule must
    /// reproduce Gray's compatibility matrix for IS, IX, S, X.
    #[test]
    fn reproduces_grays_matrix() {
        let is = Lock::Intention(Mode::Depend);
        let ix = Lock::Intention(Mode::EditSignature);
        let s = Lock::Explicit(Mode::Depend);
        let x = Lock::Explicit(Mode::EditSignature);
        let order = [is, ix, s, x];
        // true = compatible. Rows/cols: IS, IX, S, X.
        let gray = [
            [true, true, true, false],
            [true, true, false, false],
            [true, false, true, false],
            [false, false, false, false],
        ];
        for (i, a) in order.iter().enumerate() {
            for (j, b) in order.iter().enumerate() {
                assert_eq!(!a.conflicts_with(*b), gray[i][j], "{a:?} vs {b:?}");
            }
        }
    }

    /// The lock-table check (shared nodes only) must agree with the definition:
    /// two claims conflict iff one scope covers the other and the modes conflict.
    #[test]
    fn lock_table_matches_definition() {
        let scopes = [
            Scope::root(),
            dir("src"),
            dir("src/auth"),
            dir("src/billing"),
            file("src/auth/session.rs"),
            file("src/auth/token.rs"),
            sym("src/auth/session.rs", "refresh"),
            sym("src/auth/session.rs", "login"),
            sym("src/auth/token.rs", "sign"),
        ];
        for a_scope in &scopes {
            for b_scope in &scopes {
                for a_mode in Mode::ALL {
                    for b_mode in Mode::ALL {
                        let a = ScopeClaim { scope: a_scope.clone(), mode: a_mode };
                        let b = ScopeClaim { scope: b_scope.clone(), mode: b_mode };
                        let expected = (a_scope.covers(b_scope) || b_scope.covers(a_scope))
                            && a_mode.conflicts_with(b_mode);
                        let by_table = a.locks().iter().any(|(na, la)| {
                            b.locks().iter().any(|(nb, lb)| na == nb && la.conflicts_with(*lb))
                        });
                        assert_eq!(by_table, expected, "{a:?} vs {b:?}");
                    }
                }
            }
        }
    }

    fn entry(id: u64, passed: Option<bool>, risk: Option<u16>, diff: u32, at: u64) -> RaceEntry {
        RaceEntry {
            claim: ClaimId(id),
            agent: AgentId(format!("agent-{id}")),
            fork_commit: CommitId(format!("c{id}")),
            submitted_at_ms: at,
            tests_passed: passed,
            risk_bp: risk,
            diff_lines: Some(diff),
        }
    }

    #[test]
    fn race_filters_failing_tests_then_ranks() {
        let entries = [
            entry(1, Some(false), Some(100), 10, 1), // lowest risk, but tests fail
            entry(2, Some(true), Some(900), 10, 2),
            entry(3, Some(true), Some(300), 50, 3),
            entry(4, None, Some(200), 10, 4), // tests not run yet
        ];
        let crit = [Criterion::TestsPass, Criterion::LowestRisk, Criterion::SmallestDiff];
        assert_eq!(rank_entries(&crit, &entries), vec![ClaimId(3), ClaimId(2)]);
    }

    #[test]
    fn race_missing_values_sort_last_and_ties_are_deterministic() {
        let entries = [
            entry(7, Some(true), None, 5, 1),
            entry(5, Some(true), Some(400), 20, 9),
            entry(6, Some(true), Some(400), 20, 9),
        ];
        let crit = [Criterion::LowestRisk, Criterion::SmallestDiff, Criterion::FirstSubmitted];
        assert_eq!(rank_entries(&crit, &entries), vec![ClaimId(5), ClaimId(6), ClaimId(7)]);
        // Input order must not change the result.
        let mut reversed = entries.clone();
        reversed.reverse();
        assert_eq!(rank_entries(&crit, &reversed), rank_entries(&crit, &entries));
    }

    #[test]
    fn body_edits_threaten_assumptions_in_overlapping_scopes() {
        let a = Assumption {
            scope: sym("src/auth/session.rs", "refresh"),
            statement: "returns Some after login".into(),
        };
        let claim = |scope, mode| ScopeClaim { scope, mode };
        // Same symbol, body edit: the gap the conflict matrix cannot see.
        assert!(a.threatened_by(&claim(sym("src/auth/session.rs", "refresh"), Mode::EditBody)));
        // Whole-file or whole-dir edits cover it.
        assert!(a.threatened_by(&claim(file("src/auth/session.rs"), Mode::EditSignature)));
        assert!(a.threatened_by(&claim(dir("src/auth"), Mode::EditBody)));
        // Readers and creators do not threaten it; neither do siblings.
        assert!(!a.threatened_by(&claim(sym("src/auth/session.rs", "refresh"), Mode::Depend)));
        assert!(!a.threatened_by(&claim(sym("src/auth/session.rs", "login"), Mode::EditBody)));
    }

    #[test]
    fn old_clients_without_new_fields_still_parse() {
        let json = r#"{"type":"submit","req":1,"claim":2,"fence":3,"fork_commit":"abc","touched":[]}"#;
        assert!(matches!(serde_json::from_str::<ClientMsg>(json).unwrap(), ClientMsg::Submit { .. }));
        let intent: Intent = serde_json::from_str(r#"{"summary":"x","task_ref":null}"#).unwrap();
        assert!(intent.assumptions.is_empty());
    }

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event { seq, at_ms: seq * 10, run: RunId("test".into()), kind }
    }

    #[test]
    fn denials_are_not_counted_as_prevented_until_verified() {
        let denied = || EventKind::ClaimDenied {
            agent: AgentId("a".into()),
            scopes: vec![],
            intent: Intent { summary: "x".into(), task_ref: None, assumptions: vec![] },
            conflicts: vec![],
        };
        let verified = |o| EventKind::DenialVerified {
            shadow_claim: ClaimId(9),
            blocking_claim: ClaimId(1),
            outcome: o,
        };
        let log = vec![
            ev(0, denied()),
            ev(1, denied()),
            ev(2, denied()),
            ev(3, verified(Outcome::TextualConflict)),
            ev(4, verified(Outcome::TestsFailed)),
            ev(5, verified(Outcome::Clean)),
            ev(6, verified(Outcome::Inconclusive)),
        ];
        let s = Summary::from_events(&log);
        assert_eq!(s.denials, 3);
        assert_eq!(s.conflicts_prevented_verified, 2);
        assert_eq!(s.false_alarms, 1);
        assert_eq!(s.precision, Some(2.0 / 3.0)); // inconclusive excluded
    }

    #[test]
    fn replay_counts_conflicts_and_precision_starts_empty() {
        let replay = |o| EventKind::ReplayMerged {
            agent: AgentId("a".into()),
            fork_commit: CommitId("c".into()),
            outcome: o,
        };
        let log = vec![ev(0, replay(Outcome::Clean)), ev(1, replay(Outcome::BuildFailed))];
        let s = Summary::from_events(&log);
        assert_eq!((s.replay_merges, s.replay_conflicts), (2, 1));
        assert_eq!(s.precision, None);
    }

    #[test]
    fn events_round_trip_through_json() {
        let e = ev(42, EventKind::Merged { claim: ClaimId(3), head: CommitId("abc".into()) });
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""event":"merged""#) && json.contains(r#""seq":42"#), "{json}");
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back.seq, 42);
        assert!(matches!(back.kind, EventKind::Merged { .. }));
    }

    #[test]
    fn permits_never_weakens_protection() {
        for held in Mode::ALL {
            for needed in Mode::ALL {
                if held.permits(needed) {
                    for other in Mode::ALL {
                        if needed.conflicts_with(other) {
                            assert!(held.conflicts_with(other), "{held:?} permits {needed:?} but misses {other:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn uncovered_finds_work_outside_the_claim() {
        let c = |scope, mode| ScopeClaim { scope, mode };
        let claimed = [c(file("src/auth/session.rs"), Mode::EditBody)];
        let touched = [
            c(sym("src/auth/session.rs", "refresh"), Mode::EditBody), // covered
            c(sym("src/auth/session.rs", "login"), Mode::EditSignature), // mode too strong
            c(sym("src/auth/token.rs", "sign"), Mode::EditBody), // outside scope
        ];
        let missing = uncovered(&claimed, &touched);
        assert_eq!(missing, touched[1..].to_vec());
    }

    #[test]
    fn review_gate_flags_with_reasons_and_passes_routine_work() {
        let c = |scope, mode| ScopeClaim { scope, mode };
        let sensitive = vec!["src/auth/".to_string()];
        // Routine: body edit outside sensitive paths, tested, threatens nobody.
        let routine = [c(sym("src/ui/menu.rs", "render"), Mode::EditBody)];
        assert!(review_reasons(&routine, 0, true, &sensitive).is_empty());
        // Risky: signature change in a sensitive path, threatens 2, untested.
        let risky = [c(sym("src/auth/session.rs", "refresh"), Mode::EditSignature)];
        let reasons = review_reasons(&risky, 2, false, &sensitive);
        assert_eq!(reasons.len(), 4, "{reasons:?}");
    }

    #[test]
    fn hello_without_version_means_v1() {
        let m: ClientMsg = serde_json::from_str(r#"{"type":"hello","agent":"a","base":"b"}"#).unwrap();
        assert!(matches!(m, ClientMsg::Hello { protocol: 1, .. }));
    }

    #[test]
    fn json_shape_is_stable() {
        let msg = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent { summary: "fix refresh".into(), task_ref: None, assumptions: vec![] },
            scopes: vec![ScopeClaim { scope: sym("src/a.rs", "f"), mode: Mode::EditBody }],
            on_conflict: OnConflict::Fail,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ClientMsg::Claim { .. }));
        assert!(json.contains(r#""kind":"symbol""#), "{json}");
    }
}

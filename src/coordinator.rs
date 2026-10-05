//! Pure coordinator core: a deterministic, in-memory state machine for claims.
//!
//! No clock, no randomness, no `worker` dependency: the caller passes `now_ms` and delivers the
//! returned `Effect`s, so the whole thing is tested natively. The Durable Object wraps it.
//!
//! Decisions made here, beyond the protocol invariants:
//! - An agent's own claims never conflict with each other.
//! - Claim ids and fences start at 1 and are never reused. Event `seq` starts at 0.
//! - `Hello` does not have to precede other messages; connection state belongs to the caller.
//! - The lock table is derived from the claims. It is not serialized; deserializing rebuilds it.
//! - A submitted claim stops expiring and keeps its locks until a later task reports its merge
//!   outcome. Submissions are numbered, which fixes their order in the merge queue.
//! - A shadow claim (invariant 10) is a real claim with an id, a fence and a lease that places no
//!   lock, so it blocks nobody. Submitting one records it for verification and never queues it.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into the Durable Object in COORD-4")
)]

use std::collections::{hash_map, BTreeMap, HashMap};

use serde::{Deserialize, Serialize, Serializer};

use crate::protocol::{
    uncovered, AgentId, ClaimId, ClientMsg, CommitId, Conflict, DecisionRecord, ErrorCode, Event,
    EventKind, Fence, HeldAssumption, Intent, Lock, OnConflict, ReleaseReason, RequestId, RunId,
    Scope, ScopeClaim, ServerMsg, PROTOCOL_VERSION,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub run: RunId,
    pub lease_ms: u64,
    /// Experiment runs only (invariant 10): allow `OnConflict::Shadow`.
    #[serde(default)]
    pub shadow_enabled: bool,
}

/// `Config` the coordinator cannot run with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidConfig {
    reason: &'static str,
}

impl std::fmt::Display for InvalidConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid coordinator config: {}", self.reason)
    }
}

impl std::error::Error for InvalidConfig {}

/// Something the caller must do after `Coordinator::handle`.
///
/// The caller must durably persist the coordinator state and every `Log` event BEFORE delivering
/// any `Reply` or `Notify` from the same call. A fence is persisted before its grant is sent
/// (invariant 4, CLAUDE.md rule 6), so a restart can never hand out the same fence twice.
#[derive(Debug, Clone)]
pub enum Effect {
    /// Send to the agent whose message was handled.
    Reply(ServerMsg),
    /// Send to another agent.
    Notify { agent: AgentId, msg: ServerMsg },
    /// Append to the event log. `seq` is already assigned.
    Log(Event),
}

/// One claim as the coordinator holds it. Free text in `intent` is untrusted and only relayed.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActiveClaim {
    agent: AgentId,
    fence: Fence,
    intent: Intent,
    scopes: Vec<ScopeClaim>,
    /// The lease runs out at this instant: the claim is expired when `now_ms >= expires_at_ms`.
    expires_at_ms: u64,
    /// A shadow claim places no lock and blocks nobody (invariant 10).
    #[serde(default)]
    shadow: bool,
    /// The submission's ordinal, set once the claim is submitted. A submitted claim never expires
    /// (invariant 5); the ordinal fixes its place in the merge queue.
    #[serde(default)]
    submitted: Option<u64>,
}

/// A `Submit` message minus the sender.
struct SubmitRequest {
    req: RequestId,
    claim: ClaimId,
    fence: Fence,
    fork_commit: CommitId,
    touched: Vec<ScopeClaim>,
    decisions: DecisionRecord,
}

/// One lock on one node. Carries everything a `Conflict` needs, so reporting one cannot fail.
#[derive(Debug, Clone)]
struct Holder {
    claim: ClaimId,
    agent: AgentId,
    intent: Intent,
    /// The claimed scope that placed this lock, and its index within the claim.
    held: ScopeClaim,
    slot: usize,
    lock: Lock,
}

type LockTable = HashMap<Scope, Vec<Holder>>;

/// A `Claim` message minus the conflict policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClaimRequest {
    req: RequestId,
    intent: Intent,
    scopes: Vec<ScopeClaim>,
}

/// An `Amend` message minus the sender.
struct AmendRequest {
    req: RequestId,
    claim: ClaimId,
    fence: Fence,
    add: Vec<ScopeClaim>,
}

/// A `Claim` with `OnConflict::Wait` that is queued behind the claims blocking it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Waiting {
    agent: AgentId,
    request: ClaimRequest,
}

/// One blocker, with the key that orders it: (index of the requested scope in the request,
/// blocking claim id, index of the held scope within that claim).
struct Blocked {
    index: usize,
    claim: ClaimId,
    slot: usize,
    conflict: Conflict,
}

impl Blocked {
    fn key(&self) -> (usize, u64, usize) {
        (self.index, self.claim.0, self.slot)
    }
}

/// What is persisted. Claims are keyed by claim id so iteration (and JSON) is in id order.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CoordinatorState {
    config: Config,
    /// Main's head as the coordinator knows it. The steward will own this later.
    head: Option<CommitId>,
    next_claim: u64,
    next_fence: u64,
    next_seq: u64,
    /// Ordinal of the next submission; starts at 0 and is never reused.
    #[serde(default)]
    next_submission: u64,
    /// The latest `now_ms` the core has seen. Time never runs backwards inside the core.
    /// Every call advances it, including calls that return an error: "errors change nothing"
    /// means no claim, queue, counter or event changes, not the clock (time is not a decision).
    clock_ms: u64,
    claims: BTreeMap<u64, ActiveClaim>,
    /// The Wait queue, oldest first (invariant 2).
    ///
    /// Whenever claims go away (release or expiry) the queue is walked once, in order. A waiter
    /// is granted iff its scopes conflict neither with any active claim of another agent nor with
    /// any earlier request that is still waiting after being considered in this walk, so a later
    /// waiter never overtakes an earlier one it conflicts with. A grant only adds locks, so it
    /// cannot unblock a later waiter: one pass in order is enough.
    ///
    /// Incoming claims are checked against active claims only, never against waiters, so a steady
    /// stream of compatible new claims can keep delaying a waiter. That is a known limit.
    /// An agent with a queued request holds nothing and may not make any other claim.
    waiting: Vec<Waiting>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(from = "CoordinatorState")]
pub struct Coordinator {
    state: CoordinatorState,
    /// Derived from `state.claims`; rebuilt on deserialize, never persisted.
    locks: LockTable,
}

impl Serialize for Coordinator {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.state.serialize(serializer)
    }
}

impl From<CoordinatorState> for Coordinator {
    fn from(state: CoordinatorState) -> Self {
        let mut locks = LockTable::new();
        for (id, claim) in &state.claims {
            place_locks(&mut locks, ClaimId(*id), claim);
        }
        Self { state, locks }
    }
}

impl Coordinator {
    /// Fails if `config.lease_ms` is zero: every grant would already be due, so the shell's
    /// expiry alarm would fire forever.
    pub fn new(config: Config) -> Result<Self, InvalidConfig> {
        if config.lease_ms == 0 {
            return Err(InvalidConfig {
                reason: "lease_ms must be greater than zero",
            });
        }
        Ok(Self::from(CoordinatorState {
            config,
            head: None,
            next_claim: 1,
            next_fence: 1,
            next_seq: 0,
            next_submission: 0,
            clock_ms: 0,
            claims: BTreeMap::new(),
            waiting: Vec::new(),
        }))
    }

    /// Returns `now_ms`, or the latest time already seen if that is later, and remembers it. The
    /// caller's clocks (an alarm and a WebSocket message) may disagree slightly; this keeps
    /// leases from shortening and `Event::at_ms` from decreasing.
    fn advance_clock(&mut self, now_ms: u64) -> u64 {
        self.state.clock_ms = self.state.clock_ms.max(now_ms);
        self.state.clock_ms
    }

    /// Apply one client message from `agent`.
    ///
    /// `agent` is the only identity the core trusts and logs. The caller binds it to the
    /// connection and passes it on every call, including `Hello`, whose own `agent` field must
    /// match it.
    ///
    /// Leases that ran out by `now_ms` are expired first, so a late alarm never lets an expired
    /// claim block or act. Their effects precede the effects of `msg`.
    ///
    /// `now_ms` is clamped to the latest time the core has seen, so an earlier value than a
    /// previous call's is treated as that previous time.
    pub fn handle(&mut self, agent: &AgentId, msg: ClientMsg, now_ms: u64) -> Vec<Effect> {
        let now_ms = self.advance_clock(now_ms);
        let mut effects = self.expire(now_ms);
        effects.extend(self.route(agent, msg, now_ms));
        effects
    }

    /// Expire every claim whose lease ran out (`expires_at_ms <= now_ms`), in claim id order.
    ///
    /// Each expiry removes the claim and its locks, which retires its fence, logs
    /// `ClaimReleased { LeaseExpired }` and notifies the owner. Waiters that the freed scopes
    /// unblock are then granted. The Durable Object calls this from its alarm.
    ///
    /// `now_ms` is clamped to the latest time the core has seen, as in `handle`.
    pub fn expire(&mut self, now_ms: u64) -> Vec<Effect> {
        let now_ms = self.advance_clock(now_ms);
        let mut due = Vec::new();
        self.state.claims.retain(|id, claim| {
            if claim.submitted.is_some() || claim.expires_at_ms > now_ms {
                return true;
            }
            due.push((*id, claim.clone()));
            false
        });
        let mut effects = Vec::new();
        let any_expired = !due.is_empty();
        for (id, expired) in due {
            let claim = ClaimId(id);
            remove_locks(&mut self.locks, claim, &expired);
            let reason = ReleaseReason::LeaseExpired;
            effects.push(self.event(now_ms, EventKind::ClaimReleased { claim, reason }));
            effects.push(Effect::Notify {
                agent: expired.agent,
                msg: ServerMsg::LeaseExpired {
                    claim,
                    fence: expired.fence,
                },
            });
        }
        if any_expired {
            effects.extend(self.grant_unblocked_waiters(now_ms));
        }
        effects
    }

    /// The earliest lease expiry of any unsubmitted claim, for the shell's next alarm.
    pub fn next_expiry_ms(&self) -> Option<u64> {
        self.state
            .claims
            .values()
            .filter(|claim| claim.submitted.is_none())
            .map(|claim| claim.expires_at_ms)
            .min()
    }

    fn route(&mut self, agent: &AgentId, msg: ClientMsg, now_ms: u64) -> Vec<Effect> {
        match msg {
            ClientMsg::Hello { .. }
            | ClientMsg::Claim { .. }
            | ClientMsg::Amend { .. }
            | ClientMsg::Heartbeat
            | ClientMsg::Release { .. }
            | ClientMsg::Submit { .. } => self.handle_lifecycle(agent, msg, now_ms),
            ClientMsg::OpenRace { .. }
            | ClientMsg::JoinRace { .. }
            | ClientMsg::PickWinner { .. }
            | ClientMsg::Review { .. }
            | ClientMsg::Watch { .. } => Self::handle_collective(msg),
        }
    }

    /// The claim lifecycle: connect, claim, amend, heartbeat, release, submit.
    fn handle_lifecycle(&mut self, agent: &AgentId, msg: ClientMsg, now_ms: u64) -> Vec<Effect> {
        match msg {
            ClientMsg::Hello {
                agent: declared,
                base,
                protocol,
            } => self.hello(agent, &declared, base, protocol, now_ms),
            ClientMsg::Claim {
                req,
                intent,
                scopes,
                on_conflict,
            } => {
                let request = ClaimRequest {
                    req,
                    intent,
                    scopes,
                };
                self.claim(agent, on_conflict, request, now_ms)
            }
            ClientMsg::Release { claim, fence } => self.release(agent, claim, fence, now_ms),
            ClientMsg::Amend {
                req,
                claim,
                fence,
                add,
            } => {
                let request = AmendRequest {
                    req,
                    claim,
                    fence,
                    add,
                };
                self.amend(agent, request, now_ms)
            }
            ClientMsg::Heartbeat => self.heartbeat(agent, now_ms),
            ClientMsg::Submit {
                req,
                claim,
                fence,
                fork_commit,
                touched,
                decisions,
            } => {
                let request = SubmitRequest {
                    req,
                    claim,
                    fence,
                    fork_commit,
                    touched,
                    decisions,
                };
                self.submit(agent, request, now_ms)
            }
            ClientMsg::OpenRace { .. }
            | ClientMsg::JoinRace { .. }
            | ClientMsg::PickWinner { .. }
            | ClientMsg::Review { .. }
            | ClientMsg::Watch { .. } => misrouted(),
        }
    }

    /// Races, review and watch.
    fn handle_collective(msg: ClientMsg) -> Vec<Effect> {
        match msg {
            ClientMsg::OpenRace { req, .. } => not_implemented(Some(req), "OpenRace"),
            ClientMsg::JoinRace { req, .. } => not_implemented(Some(req), "JoinRace"),
            ClientMsg::PickWinner { req, .. } => not_implemented(Some(req), "PickWinner"),
            ClientMsg::Review { req, .. } => not_implemented(Some(req), "Review"),
            ClientMsg::Watch { .. } => not_implemented(None, "Watch"),
            ClientMsg::Hello { .. }
            | ClientMsg::Claim { .. }
            | ClientMsg::Amend { .. }
            | ClientMsg::Heartbeat
            | ClientMsg::Release { .. }
            | ClientMsg::Submit { .. } => misrouted(),
        }
    }

    fn hello(
        &mut self,
        agent: &AgentId,
        declared: &AgentId,
        base: CommitId,
        protocol: u16,
        now_ms: u64,
    ) -> Vec<Effect> {
        if declared != agent {
            let message = "hello agent does not match connection";
            return vec![error(None, ErrorCode::Malformed, message)];
        }
        if protocol != PROTOCOL_VERSION {
            let message = format!(
                "client speaks protocol v{protocol}, coordinator speaks v{PROTOCOL_VERSION}"
            );
            return vec![error(None, ErrorCode::UnsupportedProtocol, message)];
        }
        let head = self.state.head.get_or_insert(base).clone();
        let connected = self.event(
            now_ms,
            EventKind::AgentConnected {
                agent: agent.clone(),
            },
        );
        let welcome = ServerMsg::Welcome {
            head,
            lease_ms: self.state.config.lease_ms,
            protocol: PROTOCOL_VERSION,
        };
        vec![connected, Effect::Reply(welcome)]
    }

    fn claim(
        &mut self,
        agent: &AgentId,
        on_conflict: OnConflict,
        request: ClaimRequest,
        now_ms: u64,
    ) -> Vec<Effect> {
        if request.scopes.is_empty() {
            return vec![error(
                Some(request.req),
                ErrorCode::Malformed,
                "claim names no scopes",
            )];
        }
        if self.is_waiting(agent) {
            let message = "agent has a queued request and may not claim until it is granted";
            return vec![error(
                Some(request.req),
                ErrorCode::WaitWhileHolding,
                message,
            )];
        }
        if on_conflict == OnConflict::Shadow && !self.state.config.shadow_enabled {
            let message = "shadow claims are only allowed in experiment runs";
            return vec![error(Some(request.req), ErrorCode::ShadowDisabled, message)];
        }
        let request = ClaimRequest {
            scopes: without_duplicates(request.scopes),
            ..request
        };
        let conflicts = self.find_conflicts(agent, &request.scopes);
        if conflicts.is_empty() {
            return self.grant(agent, request, now_ms);
        }
        match on_conflict {
            OnConflict::Wait => self.enqueue(agent, request),
            OnConflict::Fail => self.deny(agent, request, conflicts, now_ms),
            OnConflict::Shadow => self.shadow(agent, request, conflicts, now_ms),
        }
    }

    fn is_waiting(&self, agent: &AgentId) -> bool {
        self.state
            .waiting
            .iter()
            .any(|waiter| waiter.agent == *agent)
    }

    /// Whether `agent` holds any lock. A shadow claim holds none, so it cannot deadlock a waiter.
    fn holds_claims(&self, agent: &AgentId) -> bool {
        self.state
            .claims
            .values()
            .any(|claim| claim.agent == *agent && !claim.shadow)
    }

    /// Queue a blocked `Wait` request, unless the agent holds claims (invariant 2). Queueing is
    /// not an event in the protocol, so nothing is logged.
    fn enqueue(&mut self, agent: &AgentId, request: ClaimRequest) -> Vec<Effect> {
        if self.holds_claims(agent) {
            let message = "cannot wait for a conflicting claim while holding other claims";
            return vec![error(
                Some(request.req),
                ErrorCode::WaitWhileHolding,
                message,
            )];
        }
        let req = request.req;
        self.state.waiting.push(Waiting {
            agent: agent.clone(),
            request,
        });
        let position = u32::try_from(self.state.waiting.len()).unwrap_or(u32::MAX);
        vec![Effect::Reply(ServerMsg::Queued { req, position })]
    }

    /// Walk the Wait queue once, in order, granting every waiter that nothing blocks (see
    /// `CoordinatorState::waiting` for the rule). Each grant is notified under its original `req`.
    fn grant_unblocked_waiters(&mut self, now_ms: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut still_waiting: Vec<Waiting> = Vec::new();
        for waiter in std::mem::take(&mut self.state.waiting) {
            let blocked = !self
                .find_conflicts(&waiter.agent, &waiter.request.scopes)
                .is_empty()
                || still_waiting
                    .iter()
                    .any(|earlier| waiters_conflict(earlier, &waiter));
            if blocked {
                still_waiting.push(waiter);
                continue;
            }
            let (granted, msg) = self.place_grant(&waiter.agent, waiter.request, now_ms);
            effects.push(granted);
            effects.push(Effect::Notify {
                agent: waiter.agent,
                msg,
            });
        }
        self.state.waiting = still_waiting;
        effects
    }

    /// Renew every unsubmitted claim of `agent`, and no one else's. Submitted claims do not
    /// expire, so there is nothing to renew. Neither replied to nor logged.
    fn heartbeat(&mut self, agent: &AgentId, now_ms: u64) -> Vec<Effect> {
        let renewed = now_ms.saturating_add(self.state.config.lease_ms);
        for claim in self.state.claims.values_mut() {
            if claim.agent == *agent && claim.submitted.is_none() {
                claim.expires_at_ms = renewed;
            }
        }
        Vec::new()
    }

    /// Add scopes to a held claim (invariant 4). Atomic and never waits: on a conflict nothing
    /// changes, and no fence is consumed. The lease is not renewed.
    fn amend(&mut self, agent: &AgentId, request: AmendRequest, now_ms: u64) -> Vec<Effect> {
        let AmendRequest {
            req,
            claim,
            fence,
            add,
        } = request;
        let held = match self.authorize(agent, Some(req), claim, fence) {
            Ok(held) => held.clone(),
            Err(refusal) => return vec![*refusal],
        };
        if held.submitted.is_some() {
            return vec![already_submitted(Some(req), claim)];
        }
        if add.is_empty() {
            return vec![error(
                Some(req),
                ErrorCode::Malformed,
                "amend names no scopes",
            )];
        }
        let mut added = without_duplicates(add);
        added.retain(|scope| !held.scopes.contains(scope));
        let conflicts = if held.shadow {
            Vec::new()
        } else {
            self.find_conflicts(agent, &added)
        };
        if conflicts.is_empty() {
            let request = AmendRequest {
                req,
                claim,
                fence,
                add: added,
            };
            return self.apply_amend(held, request, now_ms);
        }
        let denied = self.event(
            now_ms,
            EventKind::ClaimDenied {
                agent: agent.clone(),
                scopes: added,
                intent: held.intent,
                conflicts: conflicts.clone(),
            },
        );
        vec![denied, Effect::Reply(ServerMsg::Denied { req, conflicts })]
    }

    /// Replace `held` by a copy that also covers `request.add`, under a new fence, which retires
    /// the old one. The lease is unchanged. A shadow claim places no locks and reports no risk.
    fn apply_amend(
        &mut self,
        held: ActiveClaim,
        request: AmendRequest,
        now_ms: u64,
    ) -> Vec<Effect> {
        let AmendRequest {
            req,
            claim,
            add: added,
            ..
        } = request;
        let at_risk = if held.shadow {
            Vec::new()
        } else {
            self.assumptions_at_risk(&held.agent, &added)
        };
        let fence = Fence(take_next(&mut self.state.next_fence));
        remove_locks(&mut self.locks, claim, &held);
        let mut scopes = held.scopes.clone();
        scopes.extend(added.iter().cloned());
        let updated = ActiveClaim {
            fence,
            scopes,
            ..held
        };
        place_locks(&mut self.locks, claim, &updated);
        let expires_at_ms = updated.expires_at_ms;
        self.state.claims.insert(claim.0, updated);
        let amended = self.event(
            now_ms,
            EventKind::ClaimAmended {
                claim,
                fence,
                added,
            },
        );
        let reply = ServerMsg::Granted {
            req,
            claim,
            fence,
            expires_at_ms,
            race: None,
            at_risk,
        };
        vec![amended, Effect::Reply(reply)]
    }

    /// Every active claim of another agent that blocks one of `scopes`. Looks only at the nodes
    /// the requested scopes lock. The result does not depend on storage or insertion order: it is
    /// sorted by (index of the requested scope in the request, blocking claim id, index of the
    /// held scope within its claim). `scopes` must not repeat a `ScopeClaim`.
    fn find_conflicts(&self, agent: &AgentId, scopes: &[ScopeClaim]) -> Vec<Conflict> {
        let mut found = Vec::new();
        for (index, requested) in scopes.iter().enumerate() {
            self.collect_blockers(agent, index, requested, &mut found);
        }
        found.sort_by_key(Blocked::key);
        found.into_iter().map(|blocked| blocked.conflict).collect()
    }

    fn collect_blockers(
        &self,
        agent: &AgentId,
        index: usize,
        requested: &ScopeClaim,
        found: &mut Vec<Blocked>,
    ) {
        for (node, lock) in requested.locks() {
            let Some(holders) = self.locks.get(&node) else {
                continue;
            };
            for holder in holders {
                if let Some(blocked) = blocked_by(agent, index, requested, lock, holder) {
                    found.push(blocked);
                }
            }
        }
    }

    /// Assumptions in other agents' active non-shadow claims that any of `scopes` could break
    /// (invariant 8), in claim id order. A submitted claim still counts: its work has not merged,
    /// so it still relies on what it assumed.
    fn assumptions_at_risk(&self, agent: &AgentId, scopes: &[ScopeClaim]) -> Vec<HeldAssumption> {
        let mut out = Vec::new();
        for (id, other) in &self.state.claims {
            if other.agent == *agent || other.shadow {
                continue;
            }
            for assumption in &other.intent.assumptions {
                if scopes.iter().any(|scope| assumption.threatened_by(scope)) {
                    out.push(HeldAssumption {
                        agent: other.agent.clone(),
                        claim: ClaimId(*id),
                        assumption: assumption.clone(),
                    });
                }
            }
        }
        out
    }

    fn deny(
        &mut self,
        agent: &AgentId,
        request: ClaimRequest,
        conflicts: Vec<Conflict>,
        now_ms: u64,
    ) -> Vec<Effect> {
        let denied = self.event(
            now_ms,
            EventKind::ClaimDenied {
                agent: agent.clone(),
                scopes: request.scopes,
                intent: request.intent,
                conflicts: conflicts.clone(),
            },
        );
        vec![
            denied,
            Effect::Reply(ServerMsg::Denied {
                req: request.req,
                conflicts,
            }),
        ]
    }

    /// A real denial that the agent may keep working past (invariant 10): the claim gets an id,
    /// a fence and a lease like any other, but no lock, so it blocks nobody.
    fn shadow(
        &mut self,
        agent: &AgentId,
        request: ClaimRequest,
        conflicts: Vec<Conflict>,
        now_ms: u64,
    ) -> Vec<Effect> {
        let claim = ClaimId(take_next(&mut self.state.next_claim));
        let fence = Fence(take_next(&mut self.state.next_fence));
        let expires_at_ms = now_ms.saturating_add(self.state.config.lease_ms);
        let scopes = request.scopes.clone();
        let active = ActiveClaim {
            agent: agent.clone(),
            fence,
            intent: request.intent,
            scopes: request.scopes,
            expires_at_ms,
            shadow: true,
            submitted: None,
        };
        self.state.claims.insert(claim.0, active);
        let shadowed = self.event(
            now_ms,
            EventKind::ClaimShadowed {
                agent: agent.clone(),
                claim,
                scopes,
                conflicts: conflicts.clone(),
            },
        );
        let reply = ServerMsg::Shadowed {
            req: request.req,
            claim,
            fence,
            expires_at_ms,
            conflicts,
        };
        vec![shadowed, Effect::Reply(reply)]
    }

    fn grant(&mut self, agent: &AgentId, request: ClaimRequest, now_ms: u64) -> Vec<Effect> {
        let (granted, msg) = self.place_grant(agent, request, now_ms);
        vec![granted, Effect::Reply(msg)]
    }

    /// Allocate the claim id and fence, place the locks and lease, and return the log event and
    /// the `Granted` message for the caller to deliver as a reply or a notice.
    fn place_grant(
        &mut self,
        agent: &AgentId,
        request: ClaimRequest,
        now_ms: u64,
    ) -> (Effect, ServerMsg) {
        let at_risk = self.assumptions_at_risk(agent, &request.scopes);
        let claim = ClaimId(take_next(&mut self.state.next_claim));
        let fence = Fence(take_next(&mut self.state.next_fence));
        let expires_at_ms = now_ms.saturating_add(self.state.config.lease_ms);

        let active = ActiveClaim {
            agent: agent.clone(),
            fence,
            intent: request.intent,
            scopes: request.scopes,
            expires_at_ms,
            shadow: false,
            submitted: None,
        };
        place_locks(&mut self.locks, claim, &active);
        let granted = self.event(
            now_ms,
            EventKind::ClaimGranted {
                agent: agent.clone(),
                claim,
                fence,
                scopes: active.scopes.clone(),
                intent: active.intent.clone(),
                race: None,
                at_risk: at_risk.clone(),
            },
        );
        self.state.claims.insert(claim.0, active);
        let msg = ServerMsg::Granted {
            req: request.req,
            claim,
            fence,
            expires_at_ms,
            race: None,
            at_risk,
        };
        (granted, msg)
    }

    fn release(
        &mut self,
        agent: &AgentId,
        claim: ClaimId,
        fence: Fence,
        now_ms: u64,
    ) -> Vec<Effect> {
        let released = match self.authorize(agent, None, claim, fence) {
            Ok(held) => held.clone(),
            Err(refusal) => return vec![*refusal],
        };
        if released.submitted.is_some() {
            return vec![already_submitted(None, claim)];
        }
        self.state.claims.remove(&claim.0);
        remove_locks(&mut self.locks, claim, &released);
        let reason = ReleaseReason::Agent;
        let mut effects = vec![self.event(now_ms, EventKind::ClaimReleased { claim, reason })];
        effects.extend(self.grant_unblocked_waiters(now_ms));
        effects
    }

    /// Submit finished work (invariants 4, 5, 8, 11). The claim must be current and not yet
    /// submitted, `touched` must be non-empty and covered by the claim. On success the claim is
    /// marked submitted, which stops its lease and holds it for the steward; it keeps its locks.
    /// A rejected submission changes nothing but the event log, and the claim stays submittable.
    ///
    /// `Accepted::queue_position` is the 1-based place among submitted non-shadow claims, in
    /// submission order (see `queue_position`). For a shadow claim it is 0, meaning "recorded for verification, never
    /// queued for merge": the protocol has no dedicated reply for that.
    fn submit(&mut self, agent: &AgentId, request: SubmitRequest, now_ms: u64) -> Vec<Effect> {
        let SubmitRequest {
            req,
            claim,
            fence,
            fork_commit,
            touched,
            decisions,
        } = request;
        let held = match self.authorize(agent, Some(req), claim, fence) {
            Ok(held) => held.clone(),
            Err(refusal) => return vec![*refusal],
        };
        if held.submitted.is_some() {
            return vec![already_submitted(Some(req), claim)];
        }
        if touched.is_empty() {
            let message = "submit names no touched scopes";
            return vec![error(Some(req), ErrorCode::Malformed, message)];
        }
        let touched = without_duplicates(touched);
        let missing = uncovered(&held.scopes, &touched);
        if !missing.is_empty() {
            return self.reject_uncovered(claim, missing, now_ms);
        }
        let shadow = held.shadow;
        let ordinal = take_next(&mut self.state.next_submission);
        let submitted = ActiveClaim {
            submitted: Some(ordinal),
            ..held
        };
        self.state.claims.insert(claim.0, submitted);
        let mut effects = vec![self.event(
            now_ms,
            EventKind::Submitted {
                claim,
                fork_commit: fork_commit.clone(),
                touched: touched.clone(),
                decisions,
            },
        )];
        let queue_position = if shadow {
            0
        } else {
            effects.extend(self.challenge_assumptions(agent, &fork_commit, &touched, now_ms));
            self.queue_position(ordinal)
        };
        let accepted = ServerMsg::Accepted {
            req,
            claim,
            queue_position,
        };
        effects.push(Effect::Reply(accepted));
        effects
    }

    /// Answer a submission that touched scopes outside its claim (invariant 11).
    fn reject_uncovered(
        &mut self,
        claim: ClaimId,
        missing: Vec<ScopeClaim>,
        now_ms: u64,
    ) -> Vec<Effect> {
        let reason = format!(
            "{} touched scope(s) not covered by the claim",
            missing.len()
        );
        let rejected = self.event(now_ms, EventKind::SubmitRejected { claim, reason });
        let reply = ServerMsg::Uncovered {
            claim,
            scopes: missing,
        };
        vec![rejected, Effect::Reply(reply)]
    }

    /// Tell every agent whose assumption the submission threatens (invariant 8): the other
    /// agents' active non-shadow claims, in claim id order, then each claim's assumptions in
    /// declared order. Already submitted claims are included: their work has not merged yet.
    /// Each (claim, assumption) is challenged once however many touched scopes threaten it. Challenges are notices and are logged; they never block the submission.
    fn challenge_assumptions(
        &mut self,
        submitter: &AgentId,
        fork_commit: &CommitId,
        touched: &[ScopeClaim],
        now_ms: u64,
    ) -> Vec<Effect> {
        let mut challenged = Vec::new();
        for (id, other) in &self.state.claims {
            if other.agent == *submitter || other.shadow {
                continue;
            }
            for assumption in &other.intent.assumptions {
                if touched.iter().any(|scope| assumption.threatened_by(scope)) {
                    challenged.push((other.agent.clone(), ClaimId(*id), assumption.clone()));
                }
            }
        }
        let mut effects = Vec::new();
        for (assuming, claim, assumption) in challenged {
            let kind = EventKind::AssumptionChallenged {
                claim,
                assumption: assumption.clone(),
                by: submitter.clone(),
                their_commit: fork_commit.clone(),
            };
            effects.push(self.event(now_ms, kind));
            effects.push(Effect::Notify {
                agent: assuming,
                msg: ServerMsg::AssumptionChallenged {
                    claim,
                    assumption,
                    by: submitter.clone(),
                    their_commit: fork_commit.clone(),
                },
            });
        }
        effects
    }

    /// The 1-based place of the submission numbered `ordinal`: the number of submitted
    /// non-shadow claims numbered at or before it. Stays right when later tasks remove merged
    /// claims.
    fn queue_position(&self, ordinal: u64) -> u32 {
        let mut queued = 0;
        for claim in self.state.claims.values() {
            if claim.shadow {
                continue;
            }
            if claim.submitted.is_some_and(|other| other <= ordinal) {
                queued += 1;
            }
        }
        queued
    }

    /// The claim, if `agent` may act on it with `fence`; otherwise the error to send. The message
    /// never reveals the current fence: a sender with a stale one is by definition not holding it.
    fn authorize(
        &self,
        agent: &AgentId,
        req: Option<RequestId>,
        claim: ClaimId,
        fence: Fence,
    ) -> Result<&ActiveClaim, Box<Effect>> {
        let Some(held) = self.state.claims.get(&claim.0) else {
            return Err(Box::new(unknown_claim(req, claim)));
        };
        if held.agent != *agent {
            let message = format!("claim {} belongs to another agent", claim.0);
            return Err(Box::new(error(req, ErrorCode::NotOwner, message)));
        }
        if held.fence != fence {
            let message = format!(
                "claim {} rejected fence {}: not the current fence",
                claim.0, fence.0
            );
            return Err(Box::new(error(req, ErrorCode::StaleFence, message)));
        }
        Ok(held)
    }

    fn event(&mut self, at_ms: u64, kind: EventKind) -> Effect {
        let seq = self.state.next_seq;
        self.state.next_seq += 1;
        Effect::Log(Event {
            seq,
            at_ms,
            run: self.state.config.run.clone(),
            kind,
        })
    }
}

fn unknown_claim(req: Option<RequestId>, claim: ClaimId) -> Effect {
    let message = format!("claim {} is not active", claim.0);
    error(req, ErrorCode::UnknownClaim, message)
}

fn already_submitted(req: Option<RequestId>, claim: ClaimId) -> Effect {
    let message = format!("claim {} is already submitted", claim.0);
    error(req, ErrorCode::AlreadySubmitted, message)
}

/// Returns the counter's value and advances it.
fn take_next(counter: &mut u64) -> u64 {
    let value = *counter;
    *counter += 1;
    value
}

/// Whether two queued requests from different agents would block each other.
fn waiters_conflict(earlier: &Waiting, later: &Waiting) -> bool {
    if earlier.agent == later.agent {
        return false;
    }
    for mine in &earlier.request.scopes {
        for theirs in &later.request.scopes {
            let overlap = mine.scope.covers(&theirs.scope) || theirs.scope.covers(&mine.scope);
            if overlap && mine.mode.conflicts_with(theirs.mode) {
                return true;
            }
        }
    }
    false
}

/// `Some` if `holder` blocks `requested`, whose lock on the shared node is `lock`.
fn blocked_by(
    agent: &AgentId,
    index: usize,
    requested: &ScopeClaim,
    lock: Lock,
    holder: &Holder,
) -> Option<Blocked> {
    if holder.agent == *agent || !lock.conflicts_with(holder.lock) {
        return None;
    }
    Some(Blocked {
        index,
        claim: holder.claim,
        slot: holder.slot,
        conflict: Conflict {
            requested: requested.clone(),
            held: holder.held.clone(),
            held_by: holder.agent.clone(),
            their_intent: holder.intent.clone(),
            race: None,
        },
    })
}

/// A shadow claim places no locks (invariant 10).
fn place_locks(locks: &mut LockTable, id: ClaimId, claim: &ActiveClaim) {
    if claim.shadow {
        return;
    }
    for (slot, held) in claim.scopes.iter().enumerate() {
        for (node, lock) in held.locks() {
            let holder = Holder {
                claim: id,
                agent: claim.agent.clone(),
                intent: claim.intent.clone(),
                held: held.clone(),
                slot,
                lock,
            };
            locks.entry(node).or_default().push(holder);
        }
    }
}

/// Removes the claim's locks and prunes nodes left empty.
fn remove_locks(locks: &mut LockTable, id: ClaimId, claim: &ActiveClaim) {
    if claim.shadow {
        return;
    }
    for held in &claim.scopes {
        for (node, _) in held.locks() {
            // A node shared by two of this claim's scopes is already pruned the second time.
            let hash_map::Entry::Occupied(mut slot) = locks.entry(node) else {
                continue;
            };
            slot.get_mut().retain(|holder| holder.claim != id);
            if slot.get().is_empty() {
                slot.remove();
            }
        }
    }
}

/// Drops exact repeats, keeping first occurrences in order. The same scope in two modes is two
/// distinct claims and both stay.
fn without_duplicates(scopes: Vec<ScopeClaim>) -> Vec<ScopeClaim> {
    let mut out: Vec<ScopeClaim> = Vec::with_capacity(scopes.len());
    for scope in scopes {
        if !out.contains(&scope) {
            out.push(scope);
        }
    }
    out
}

fn error(req: Option<RequestId>, code: ErrorCode, message: impl Into<String>) -> Effect {
    Effect::Reply(ServerMsg::Error {
        req,
        code,
        message: message.into(),
    })
}

fn not_implemented(req: Option<RequestId>, what: &str) -> Vec<Effect> {
    vec![error(
        req,
        ErrorCode::Malformed,
        format!("not implemented yet: {what}"),
    )]
}

/// `handle` sent a message to the wrong family handler.
fn misrouted() -> Vec<Effect> {
    vec![error(
        None,
        ErrorCode::Malformed,
        "internal error: message routed to the wrong handler",
    )]
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::protocol::{
        Assumption, CommitId, DecisionRecord, Mode, RaceId, RejectedApproach, SymbolId,
        TranscriptRef,
    };

    const NOW: u64 = 1_000;
    const LEASE: u64 = 30_000;

    fn coordinator() -> Coordinator {
        Coordinator::new(Config {
            run: RunId("test".into()),
            lease_ms: LEASE,
            shadow_enabled: false,
        })
        .unwrap()
    }

    fn agent(name: &str) -> AgentId {
        AgentId(name.into())
    }

    fn dir(path: &str) -> Scope {
        Scope::Dir { path: path.into() }
    }

    fn file(path: &str) -> Scope {
        Scope::File { path: path.into() }
    }

    fn sym(path: &str, name: &str) -> Scope {
        Scope::Symbol(SymbolId {
            path: path.into(),
            qualified_name: name.into(),
        })
    }

    fn sc(scope: Scope, mode: Mode) -> ScopeClaim {
        ScopeClaim { scope, mode }
    }

    fn intent(summary: &str) -> Intent {
        Intent {
            summary: summary.into(),
            task_ref: None,
            assumptions: vec![],
        }
    }

    fn claim_msg(intent: Intent, scopes: Vec<ScopeClaim>) -> ClientMsg {
        ClientMsg::Claim {
            req: RequestId(1),
            intent,
            scopes,
            on_conflict: OnConflict::Fail,
        }
    }

    fn claim_with(
        c: &mut Coordinator,
        who: &str,
        intent: Intent,
        scopes: Vec<ScopeClaim>,
    ) -> Vec<Effect> {
        c.handle(&agent(who), claim_msg(intent, scopes), NOW)
    }

    fn claim_as(c: &mut Coordinator, who: &str, scopes: Vec<ScopeClaim>) -> Vec<Effect> {
        claim_with(c, who, intent("test work"), scopes)
    }

    fn replies(effects: &[Effect]) -> Vec<&ServerMsg> {
        let mut out = Vec::new();
        for effect in effects {
            if let Effect::Reply(msg) = effect {
                out.push(msg);
            }
        }
        out
    }

    fn logged(effects: &[Effect]) -> Vec<&Event> {
        let mut out = Vec::new();
        for effect in effects {
            if let Effect::Log(event) = effect {
                out.push(event);
            }
        }
        out
    }

    fn only_reply(effects: &[Effect]) -> &ServerMsg {
        let all = replies(effects);
        assert_eq!(all.len(), 1, "{effects:?}");
        all[0]
    }

    fn grant_with(
        c: &mut Coordinator,
        who: &str,
        intent: Intent,
        scopes: Vec<ScopeClaim>,
    ) -> (ClaimId, Fence) {
        let effects = claim_with(c, who, intent, scopes);
        let ServerMsg::Granted { claim, fence, .. } = only_reply(&effects) else {
            panic!("expected Granted, got {effects:?}");
        };
        (*claim, *fence)
    }

    fn grant(c: &mut Coordinator, who: &str, scopes: Vec<ScopeClaim>) -> (ClaimId, Fence) {
        grant_with(c, who, intent("test work"), scopes)
    }

    fn deny(c: &mut Coordinator, who: &str, scopes: Vec<ScopeClaim>) -> Vec<Conflict> {
        let effects = claim_as(c, who, scopes);
        let ServerMsg::Denied { conflicts, .. } = only_reply(&effects) else {
            panic!("expected Denied, got {effects:?}");
        };
        conflicts.clone()
    }

    fn welcome_head(effects: &[Effect]) -> CommitId {
        let ServerMsg::Welcome { head, .. } = only_reply(effects) else {
            panic!("expected Welcome, got {effects:?}");
        };
        head.clone()
    }

    fn only_event(effects: &[Effect]) -> &EventKind {
        let events = logged(effects);
        assert_eq!(events.len(), 1, "{effects:?}");
        &events[0].kind
    }

    fn error_message(effects: &[Effect]) -> &str {
        let ServerMsg::Error { message, .. } = only_reply(effects) else {
            panic!("expected Error, got {effects:?}");
        };
        message
    }

    fn release(c: &mut Coordinator, who: &str, claim: ClaimId, fence: Fence) -> Vec<Effect> {
        c.handle(&agent(who), ClientMsg::Release { claim, fence }, NOW)
    }

    fn hello(c: &mut Coordinator, who: &str, base: &str, protocol: u16) -> Vec<Effect> {
        let msg = ClientMsg::Hello {
            agent: agent(who),
            base: CommitId(base.into()),
            protocol,
        };
        c.handle(&agent(who), msg, NOW)
    }

    fn state(c: &Coordinator) -> String {
        serde_json::to_string(c).unwrap()
    }

    fn assert_error(effects: &[Effect], expected: ErrorCode) {
        let ServerMsg::Error { code, .. } = only_reply(effects) else {
            panic!("expected Error({expected:?}), got {effects:?}");
        };
        assert_eq!(*code, expected);
        assert!(
            logged(effects).is_empty(),
            "errors must not log: {effects:?}"
        );
    }

    #[test]
    fn hello_welcomes_with_head_lease_and_protocol_and_logs_connection() {
        let mut c = coordinator();
        let effects = hello(&mut c, "a1", "abc", PROTOCOL_VERSION);
        let ServerMsg::Welcome {
            head,
            lease_ms,
            protocol,
        } = only_reply(&effects)
        else {
            panic!("expected Welcome, got {effects:?}");
        };
        assert_eq!(head, &CommitId("abc".into()));
        assert_eq!(*lease_ms, LEASE);
        assert_eq!(*protocol, PROTOCOL_VERSION);
        let events = logged(&effects);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[0].at_ms, NOW);
        assert_eq!(events[0].run, RunId("test".into()));
        let EventKind::AgentConnected { agent: connected } = &events[0].kind else {
            panic!("expected AgentConnected, got {:?}", events[0].kind);
        };
        assert_eq!(connected, &agent("a1"));
    }

    #[test]
    fn hello_for_another_agent_is_refused_without_logging_or_adopting_head() {
        let mut c = coordinator();
        let msg = ClientMsg::Hello {
            agent: agent("someone-else"),
            base: CommitId("spoofed".into()),
            protocol: PROTOCOL_VERSION,
        };
        let effects = c.handle(&agent("a1"), msg, NOW);
        assert_error(&effects, ErrorCode::Malformed);
        assert_eq!(
            error_message(&effects),
            "hello agent does not match connection"
        );
        let next = hello(&mut c, "a1", "real", PROTOCOL_VERSION);
        assert_eq!(welcome_head(&next), CommitId("real".into()));
        assert_eq!(logged(&next)[0].seq, 0, "the refused hello must not log");
    }

    #[test]
    fn unsupported_hello_leaves_the_head_unset() {
        let mut c = coordinator();
        hello(&mut c, "a1", "too-new", PROTOCOL_VERSION + 1);
        let next = hello(&mut c, "a2", "real", PROTOCOL_VERSION);
        assert_eq!(welcome_head(&next), CommitId("real".into()));
    }

    #[test]
    fn hello_keeps_the_first_head_it_adopted() {
        let mut c = coordinator();
        hello(&mut c, "a1", "first", PROTOCOL_VERSION);
        let effects = hello(&mut c, "a2", "second", PROTOCOL_VERSION);
        assert_eq!(welcome_head(&effects), CommitId("first".into()));
    }

    #[test]
    fn hello_with_unsupported_protocol_is_refused_and_logs_nothing() {
        let mut c = coordinator();
        c.expire(NOW);
        let before = state(&c);
        let effects = hello(&mut c, "a1", "abc", PROTOCOL_VERSION + 1);
        assert_error(&effects, ErrorCode::UnsupportedProtocol);
        assert_eq!(state(&c), before);
    }

    #[test]
    fn grant_carries_fence_lease_expiry_and_is_logged() {
        let mut c = coordinator();
        let effects = claim_with(
            &mut c,
            "a",
            intent("fix refresh"),
            vec![sc(sym("src/a.rs", "f"), Mode::EditBody)],
        );
        let ServerMsg::Granted {
            req,
            expires_at_ms,
            race,
            at_risk,
            ..
        } = only_reply(&effects)
        else {
            panic!("expected Granted, got {effects:?}");
        };
        assert_eq!(*req, RequestId(1));
        assert_eq!(*expires_at_ms, NOW + LEASE);
        assert_eq!(*race, None);
        assert!(at_risk.is_empty());
        let EventKind::ClaimGranted {
            agent: who,
            scopes,
            intent,
            ..
        } = only_event(&effects)
        else {
            panic!("expected ClaimGranted, got {effects:?}");
        };
        assert_eq!(who, &agent("a"));
        assert_eq!(scopes, &vec![sc(sym("src/a.rs", "f"), Mode::EditBody)]);
        assert_eq!(intent.summary, "fix refresh");
    }

    #[test]
    fn conflicting_claim_is_denied_with_holder_scope_agent_and_intent() {
        let mut c = coordinator();
        let held = sc(sym("src/a.rs", "refresh"), Mode::EditSignature);
        grant_with(&mut c, "a", intent("rename refresh"), vec![held.clone()]);

        let wanted = sc(sym("src/a.rs", "refresh"), Mode::Depend);
        let effects = claim_as(&mut c, "b", vec![wanted.clone()]);
        let ServerMsg::Denied { req, conflicts } = only_reply(&effects) else {
            panic!("expected Denied, got {effects:?}");
        };
        assert_eq!(*req, RequestId(1));
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].requested, wanted);
        assert_eq!(conflicts[0].held, held);
        assert_eq!(conflicts[0].held_by, agent("a"));
        assert_eq!(conflicts[0].their_intent.summary, "rename refresh");
        assert_eq!(conflicts[0].race, None);
        let EventKind::ClaimDenied { agent: denied, .. } = only_event(&effects) else {
            panic!("expected ClaimDenied, got {effects:?}");
        };
        assert_eq!(denied, &agent("b"));
    }

    #[test]
    fn depend_and_edit_signature_conflict_at_every_level_in_both_orders() {
        let symbol = sym("src/auth/session.rs", "refresh");
        let editors = [symbol.clone(), file("src/auth/session.rs"), dir("src/auth")];
        for editor in editors {
            let depend = sc(symbol.clone(), Mode::Depend);
            let edit = sc(editor.clone(), Mode::EditSignature);

            let mut c = coordinator();
            grant(&mut c, "a", vec![depend.clone()]);
            assert_eq!(
                deny(&mut c, "b", vec![edit.clone()]).len(),
                1,
                "edit {editor:?} after"
            );

            let mut c = coordinator();
            grant(&mut c, "a", vec![edit]);
            assert_eq!(
                deny(&mut c, "b", vec![depend]).len(),
                1,
                "depend before {editor:?}"
            );
        }
    }

    #[test]
    fn compatible_claims_are_both_granted() {
        let symbol = || sym("src/a.rs", "f");
        let pairs = [
            (sc(symbol(), Mode::Depend), sc(symbol(), Mode::Depend)),
            (sc(symbol(), Mode::Depend), sc(symbol(), Mode::EditBody)),
            (sc(symbol(), Mode::EditBody), sc(symbol(), Mode::Depend)),
            (
                sc(file("src/a.rs"), Mode::EditSignature),
                sc(file("src/b.rs"), Mode::EditSignature),
            ),
            (
                sc(dir("src/x"), Mode::EditBody),
                sc(dir("src/y"), Mode::EditBody),
            ),
        ];
        for (first, second) in pairs {
            let mut c = coordinator();
            grant(&mut c, "a", vec![first.clone()]);
            grant(&mut c, "b", vec![second.clone()]);
        }
    }

    #[test]
    fn denied_multi_scope_claim_places_no_locks() {
        let mut c = coordinator();
        let (blocker, blocker_fence) = grant(
            &mut c,
            "a",
            vec![sc(file("src/one.rs"), Mode::EditSignature)],
        );

        let conflicts = deny(
            &mut c,
            "b",
            vec![
                sc(file("src/two.rs"), Mode::EditBody),
                sc(file("src/one.rs"), Mode::EditBody),
            ],
        );
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            conflicts[0].requested,
            sc(file("src/one.rs"), Mode::EditBody)
        );

        let (third, third_fence) = grant(&mut c, "c", vec![sc(file("src/two.rs"), Mode::EditBody)]);
        release(&mut c, "c", third, third_fence);
        release(&mut c, "a", blocker, blocker_fence);
        grant(
            &mut c,
            "b",
            vec![
                sc(file("src/two.rs"), Mode::EditBody),
                sc(file("src/one.rs"), Mode::EditBody),
            ],
        );
    }

    #[test]
    fn same_agent_overlapping_claims_are_granted() {
        let mut c = coordinator();
        grant(&mut c, "a", vec![sc(file("src/a.rs"), Mode::EditSignature)]);
        grant(&mut c, "a", vec![sc(sym("src/a.rs", "f"), Mode::EditBody)]);
        grant(&mut c, "a", vec![sc(dir("src"), Mode::Depend)]);
        grant(&mut c, "a", vec![sc(file("src/a.rs"), Mode::EditSignature)]);
    }

    #[test]
    fn release_frees_the_scope_and_logs_without_replying() {
        let mut c = coordinator();
        let scopes = vec![sc(sym("src/a.rs", "f"), Mode::EditBody)];
        let (claim, fence) = grant(&mut c, "a", scopes.clone());
        deny(&mut c, "b", scopes.clone());

        let effects = release(&mut c, "a", claim, fence);
        assert!(replies(&effects).is_empty(), "{effects:?}");
        let EventKind::ClaimReleased {
            claim: released,
            reason,
        } = only_event(&effects)
        else {
            panic!("expected ClaimReleased, got {effects:?}");
        };
        assert_eq!(*released, claim);
        assert_eq!(*reason, ReleaseReason::Agent);

        grant(&mut c, "b", scopes);
    }

    #[test]
    fn stale_fence_message_does_not_reveal_the_current_fence() {
        let mut c = coordinator();
        let (claim, fence) = grant(&mut c, "a", vec![sc(file("src/a.rs"), Mode::EditBody)]);
        assert_eq!((claim.0, fence.0), (1, 1));

        let effects = release(&mut c, "a", claim, Fence(99));
        assert_error(&effects, ErrorCode::StaleFence);
        let message = error_message(&effects);
        assert_eq!(message, "claim 1 rejected fence 99: not the current fence");
        assert!(!message.contains("fence 1"), "{message}");
    }

    #[test]
    fn denial_consumes_no_claim_id_or_fence() {
        let mut with_denial = coordinator();
        let mut without = coordinator();
        for c in [&mut with_denial, &mut without] {
            grant(c, "a", vec![sc(file("src/a.rs"), Mode::EditBody)]);
        }
        deny(
            &mut with_denial,
            "b",
            vec![sc(file("src/a.rs"), Mode::EditBody)],
        );

        let next = vec![sc(file("src/other.rs"), Mode::EditBody)];
        let after_denial = grant(&mut with_denial, "c", next.clone());
        let baseline = grant(&mut without, "c", next);
        assert_eq!(after_denial, baseline);
    }

    #[test]
    fn lock_table_is_empty_after_every_claim_is_released() {
        let mut c = coordinator();
        let mut held = Vec::new();
        let scopes = [
            vec![sc(sym("src/a.rs", "f"), Mode::EditBody)],
            vec![
                sc(sym("src/a.rs", "g"), Mode::Depend),
                sc(file("src/b.rs"), Mode::EditBody),
            ],
            vec![sc(dir("src/deep/er"), Mode::Create)],
        ];
        for (who, scope) in ["a", "b", "c"].into_iter().zip(scopes) {
            held.push((who, grant(&mut c, who, scope)));
        }
        assert!(!c.locks.is_empty());
        for (who, (claim, fence)) in held {
            release(&mut c, who, claim, fence);
        }
        assert!(
            c.locks.is_empty(),
            "{:?}",
            c.locks.keys().collect::<Vec<_>>()
        );
    }

    fn busy_coordinator() -> Coordinator {
        let mut c = coordinator();
        hello(&mut c, "a", "abc", PROTOCOL_VERSION);
        let mut held = Vec::new();
        for n in 0..12 {
            let who = ["a", "b", "c"][n % 3];
            let scopes = vec![
                sc(sym(&format!("src/f{n}.rs"), "run"), Mode::EditBody),
                sc(dir("shared"), Mode::Depend),
            ];
            held.push((who, grant(&mut c, who, scopes)));
        }
        deny(&mut c, "d", vec![sc(file("src/f3.rs"), Mode::EditBody)]);
        let (who, (claim, fence)) = held.swap_remove(4);
        release(&mut c, who, claim, fence);
        c
    }

    #[test]
    fn identical_message_sequences_serialize_to_identical_json() {
        let first = state(&busy_coordinator());
        let second = state(&busy_coordinator());
        assert_eq!(first, second);

        let restored: Coordinator = serde_json::from_str(&first).unwrap();
        assert_eq!(state(&restored), first);
    }

    #[test]
    fn invalid_releases_are_rejected_and_change_nothing() {
        let mut c = coordinator();
        let scopes = vec![sc(file("src/a.rs"), Mode::EditBody)];
        let (claim, fence) = grant(&mut c, "a", scopes.clone());
        let before = state(&c);

        let unknown = release(&mut c, "a", ClaimId(claim.0 + 100), fence);
        assert_error(&unknown, ErrorCode::UnknownClaim);

        let not_owner = release(&mut c, "b", claim, fence);
        assert_error(&not_owner, ErrorCode::NotOwner);

        let stale = release(&mut c, "a", claim, Fence(fence.0 + 1));
        assert_error(&stale, ErrorCode::StaleFence);

        assert_eq!(state(&c), before);
        assert_eq!(
            deny(&mut c, "b", scopes).len(),
            1,
            "claim must still hold its locks"
        );
    }

    #[test]
    fn released_claim_cannot_be_released_twice() {
        let mut c = coordinator();
        let (claim, fence) = grant(&mut c, "a", vec![sc(file("src/a.rs"), Mode::EditBody)]);
        release(&mut c, "a", claim, fence);
        assert_error(&release(&mut c, "a", claim, fence), ErrorCode::UnknownClaim);
    }

    #[test]
    fn claim_ids_and_fences_strictly_increase_and_are_never_reused() {
        let mut c = coordinator();
        let mut ids = Vec::new();
        let mut fences = Vec::new();
        for round in 0..4 {
            let who = if round % 2 == 0 { "a" } else { "b" };
            let (claim, fence) = grant(&mut c, who, vec![sc(file("src/a.rs"), Mode::EditBody)]);
            ids.push(claim.0);
            fences.push(fence.0);
            release(&mut c, who, claim, fence);
        }
        for series in [ids, fences] {
            for pair in series.windows(2) {
                assert!(pair[0] < pair[1], "{series:?}");
            }
        }
    }

    #[test]
    fn event_seq_is_gapless_across_mixed_operations() {
        let mut c = coordinator();
        let scopes = vec![sc(file("src/a.rs"), Mode::EditBody)];
        let mut all = Vec::new();
        all.extend(hello(&mut c, "a", "abc", PROTOCOL_VERSION));
        all.extend(hello(&mut c, "a", "abc", PROTOCOL_VERSION + 1));
        let first = claim_as(&mut c, "a", scopes.clone());
        let ServerMsg::Granted { claim, fence, .. } = only_reply(&first).clone() else {
            panic!("expected Granted");
        };
        all.extend(first);
        all.extend(claim_as(&mut c, "b", scopes.clone()));
        all.extend(claim_as(&mut c, "b", vec![]));
        all.extend(release(&mut c, "b", claim, fence));
        all.extend(release(&mut c, "a", claim, fence));
        all.extend(claim_as(&mut c, "b", scopes));

        let seqs: Vec<u64> = logged(&all).iter().map(|event| event.seq).collect();
        assert_eq!(seqs, (0..5).collect::<Vec<u64>>());
    }

    #[test]
    fn at_risk_follows_claim_order_then_declared_assumption_order() {
        let mut c = coordinator();
        let target = sym("src/x.rs", "f");
        let assuming = |statements: &[&str]| Intent {
            summary: "assumes".into(),
            task_ref: None,
            assumptions: statements
                .iter()
                .map(|statement| Assumption {
                    scope: target.clone(),
                    statement: (*statement).into(),
                })
                .collect(),
        };
        let docs = |n: u32| vec![sc(file(&format!("docs/{n}.md")), Mode::EditBody)];
        let (first, _) = grant_with(&mut c, "a", assuming(&["s1", "s2"]), docs(1));
        let (second, _) = grant_with(&mut c, "b", assuming(&["s3"]), docs(2));
        let (third, _) = grant_with(&mut c, "a", assuming(&["s4"]), docs(3));

        let effects = claim_as(&mut c, "d", vec![sc(file("src/x.rs"), Mode::EditBody)]);
        let ServerMsg::Granted { at_risk, .. } = only_reply(&effects) else {
            panic!("expected Granted, got {effects:?}");
        };
        let listed: Vec<(ClaimId, &str)> = at_risk
            .iter()
            .map(|held| (held.claim, held.assumption.statement.as_str()))
            .collect();
        assert_eq!(
            listed,
            vec![(first, "s1"), (first, "s2"), (second, "s3"), (third, "s4")]
        );
    }

    #[test]
    fn threatened_assumption_is_listed_without_blocking_the_grant() {
        let mut c = coordinator();
        let refresh = sym("src/auth.rs", "refresh");
        let assuming = Intent {
            summary: "build login flow".into(),
            task_ref: None,
            assumptions: vec![Assumption {
                scope: refresh.clone(),
                statement: "returns Some after login".into(),
            }],
        };
        let (owner_claim, _) = grant_with(
            &mut c,
            "a",
            assuming,
            vec![sc(file("src/login.rs"), Mode::EditBody)],
        );

        let effects = claim_as(&mut c, "b", vec![sc(refresh.clone(), Mode::EditBody)]);
        let ServerMsg::Granted { at_risk, .. } = only_reply(&effects) else {
            panic!("a threatened assumption must not block: {effects:?}");
        };
        assert_eq!(at_risk.len(), 1);
        assert_eq!(at_risk[0].agent, agent("a"));
        assert_eq!(at_risk[0].claim, owner_claim);
        assert_eq!(at_risk[0].assumption.statement, "returns Some after login");
        let EventKind::ClaimGranted {
            at_risk: logged_at_risk,
            ..
        } = only_event(&effects)
        else {
            panic!("expected ClaimGranted, got {effects:?}");
        };
        assert_eq!(logged_at_risk.len(), 1);

        for (who, scope) in [
            ("c", sc(refresh.clone(), Mode::Depend)),
            ("d", sc(sym("src/auth.rs", "logout"), Mode::EditBody)),
        ] {
            let effects = claim_as(&mut c, who, vec![scope]);
            let ServerMsg::Granted { at_risk, .. } = only_reply(&effects) else {
                panic!("expected Granted: {effects:?}");
            };
            assert!(at_risk.is_empty(), "{who} threatens nothing");
        }
    }

    #[test]
    fn an_agents_own_assumptions_are_not_at_risk_from_its_own_claims() {
        let mut c = coordinator();
        let refresh = sym("src/auth.rs", "refresh");
        let assuming = Intent {
            summary: "x".into(),
            task_ref: None,
            assumptions: vec![Assumption {
                scope: refresh.clone(),
                statement: "y".into(),
            }],
        };
        grant_with(
            &mut c,
            "a",
            assuming,
            vec![sc(file("src/login.rs"), Mode::EditBody)],
        );
        let effects = claim_as(&mut c, "a", vec![sc(refresh, Mode::EditBody)]);
        let ServerMsg::Granted { at_risk, .. } = only_reply(&effects) else {
            panic!("expected Granted: {effects:?}");
        };
        assert!(at_risk.is_empty());
    }

    #[test]
    fn every_blocker_is_listed_once_in_a_stable_order() {
        let mut c = coordinator();
        let a_scopes = vec![
            sc(file("src/a.rs"), Mode::EditBody),
            sc(file("src/b.rs"), Mode::EditBody),
        ];
        grant(&mut c, "a", a_scopes.clone());
        grant(&mut c, "c", vec![sc(file("src/c.rs"), Mode::EditBody)]);

        let whole_dir = sc(dir("src"), Mode::EditBody);
        let conflicts = deny(&mut c, "b", vec![whole_dir.clone(), whole_dir]);
        let held: Vec<(AgentId, ScopeClaim)> = conflicts
            .iter()
            .map(|x| (x.held_by.clone(), x.held.clone()))
            .collect();
        assert_eq!(
            held,
            vec![
                (agent("a"), a_scopes[0].clone()),
                (agent("a"), a_scopes[1].clone()),
                (agent("c"), sc(file("src/c.rs"), Mode::EditBody)),
            ]
        );
    }

    #[test]
    fn conflicts_are_ordered_by_requested_scope_before_blocking_claim() {
        let mut c = coordinator();
        let on_a = sc(file("src/a.rs"), Mode::EditBody);
        let on_c = sc(file("src/c.rs"), Mode::EditBody);
        grant(&mut c, "a", vec![on_a.clone()]);
        grant(&mut c, "c", vec![on_c.clone()]);

        let conflicts = deny(&mut c, "b", vec![on_c.clone(), on_a.clone()]);
        let held: Vec<ScopeClaim> = conflicts.iter().map(|x| x.held.clone()).collect();
        assert_eq!(held, vec![on_c, on_a]);
    }

    #[test]
    fn holder_that_listed_a_scope_twice_blocks_with_one_conflict() {
        let mut c = coordinator();
        let held = sc(file("src/a.rs"), Mode::EditBody);
        grant(&mut c, "a", vec![held.clone(), held.clone()]);
        assert_eq!(deny(&mut c, "b", vec![held]).len(), 1);
    }

    #[test]
    fn requester_that_lists_a_scope_twice_gets_one_conflict() {
        let mut c = coordinator();
        let wanted = sc(file("src/a.rs"), Mode::EditBody);
        grant(&mut c, "a", vec![wanted.clone()]);
        assert_eq!(deny(&mut c, "b", vec![wanted.clone(), wanted]).len(), 1);
    }

    #[test]
    fn granted_claim_logs_a_repeated_scope_once() {
        let mut c = coordinator();
        let repeated = sc(file("src/a.rs"), Mode::EditBody);
        let other = sc(file("src/b.rs"), Mode::EditBody);
        let scopes = vec![repeated.clone(), other.clone(), repeated.clone()];
        let effects = claim_as(&mut c, "a", scopes);
        let EventKind::ClaimGranted { scopes, .. } = only_event(&effects) else {
            panic!("expected ClaimGranted, got {effects:?}");
        };
        assert_eq!(scopes, &vec![repeated, other]);
    }

    #[test]
    fn same_scope_in_two_modes_stays_two_claims() {
        let mut c = coordinator();
        let depend = sc(file("src/a.rs"), Mode::Depend);
        let edit_body = sc(file("src/a.rs"), Mode::EditBody);
        let effects = claim_as(&mut c, "a", vec![depend.clone(), edit_body.clone()]);
        let EventKind::ClaimGranted { scopes, .. } = only_event(&effects) else {
            panic!("expected ClaimGranted, got {effects:?}");
        };
        assert_eq!(scopes, &vec![depend.clone(), edit_body.clone()]);

        let conflicts = deny(&mut c, "b", vec![sc(file("src/a.rs"), Mode::EditSignature)]);
        let held: Vec<ScopeClaim> = conflicts.iter().map(|x| x.held.clone()).collect();
        assert_eq!(held, vec![depend, edit_body]);
    }

    #[test]
    fn claim_with_no_scopes_is_malformed() {
        let mut c = coordinator();
        c.expire(NOW);
        let before = state(&c);
        assert_error(&claim_as(&mut c, "a", vec![]), ErrorCode::Malformed);
        assert_eq!(state(&c), before);
    }

    #[test]
    fn restored_state_behaves_identically() {
        let mut c = coordinator();
        hello(&mut c, "a", "abc", PROTOCOL_VERSION);
        let (gone, gone_fence) = grant(&mut c, "a", vec![sc(file("src/gone.rs"), Mode::EditBody)]);
        release(&mut c, "a", gone, gone_fence);
        let (held, held_fence) = grant(
            &mut c,
            "a",
            vec![sc(sym("src/a.rs", "f"), Mode::EditSignature)],
        );
        deny(&mut c, "b", vec![sc(file("src/a.rs"), Mode::Depend)]);

        let mut restored: Coordinator = serde_json::from_str(&state(&c)).unwrap();

        let script = [
            (
                "b",
                claim_msg(intent("again"), vec![sc(file("src/a.rs"), Mode::Depend)]),
            ),
            (
                "c",
                claim_msg(intent("other"), vec![sc(file("src/z.rs"), Mode::EditBody)]),
            ),
            (
                "a",
                ClientMsg::Release {
                    claim: held,
                    fence: held_fence,
                },
            ),
            (
                "b",
                claim_msg(intent("after"), vec![sc(file("src/a.rs"), Mode::Depend)]),
            ),
            (
                "a",
                ClientMsg::Release {
                    claim: held,
                    fence: held_fence,
                },
            ),
        ];
        for (who, msg) in script {
            let live = c.handle(&agent(who), msg.clone(), NOW);
            let replayed = restored.handle(&agent(who), msg, NOW);
            assert_eq!(format!("{live:?}"), format!("{replayed:?}"));
        }
    }

    #[test]
    fn unimplemented_messages_error_and_change_nothing() {
        let mut c = coordinator();
        grant(&mut c, "a", vec![sc(file("src/a.rs"), Mode::EditBody)]);
        let before = state(&c);
        let scopes = vec![sc(file("src/a.rs"), Mode::EditBody)];
        let req = RequestId(7);
        let messages = vec![
            ClientMsg::OpenRace {
                req,
                intent: intent("r"),
                scopes,
                max_entrants: 2,
                deadline_ms: 10,
                criteria: vec![],
            },
            ClientMsg::JoinRace {
                req,
                race: RaceId(1),
            },
            ClientMsg::PickWinner {
                req,
                race: RaceId(1),
                claim: ClaimId(1),
            },
            ClientMsg::Review {
                req,
                claim: ClaimId(1),
                approve: true,
                note: None,
            },
            ClientMsg::Watch { from_seq: 0 },
        ];
        for msg in messages {
            let effects = c.handle(&agent("b"), msg.clone(), NOW);
            let ServerMsg::Error { code, message, .. } = only_reply(&effects) else {
                panic!("expected Error for {msg:?}");
            };
            assert_eq!(*code, ErrorCode::Malformed, "{msg:?}");
            assert!(message.starts_with("not implemented yet"), "{message}");
            assert!(logged(&effects).is_empty(), "{msg:?}");
            assert_eq!(state(&c), before, "{msg:?}");
        }
    }

    // ---- leases, heartbeat, expiry, amend and the wait queue ----

    fn handle_at(c: &mut Coordinator, who: &str, msg: ClientMsg, now: u64) -> Vec<Effect> {
        c.handle(&agent(who), msg, now)
    }

    fn grant_at(
        c: &mut Coordinator,
        who: &str,
        scopes: Vec<ScopeClaim>,
        now: u64,
    ) -> (ClaimId, Fence) {
        let effects = handle_at(c, who, claim_msg(intent("test work"), scopes), now);
        let ServerMsg::Granted { claim, fence, .. } = only_reply(&effects) else {
            panic!("expected Granted, got {effects:?}");
        };
        (*claim, *fence)
    }

    fn heartbeat(c: &mut Coordinator, who: &str, now: u64) -> Vec<Effect> {
        handle_at(c, who, ClientMsg::Heartbeat, now)
    }

    fn amend(
        c: &mut Coordinator,
        who: &str,
        claim: ClaimId,
        fence: Fence,
        add: Vec<ScopeClaim>,
    ) -> Vec<Effect> {
        let msg = ClientMsg::Amend {
            req: RequestId(5),
            claim,
            fence,
            add,
        };
        handle_at(c, who, msg, NOW)
    }

    fn wait_msg(req: u64, scopes: Vec<ScopeClaim>) -> ClientMsg {
        ClientMsg::Claim {
            req: RequestId(req),
            intent: intent("waiting"),
            scopes,
            on_conflict: OnConflict::Wait,
        }
    }

    fn wait_for(c: &mut Coordinator, who: &str, req: u64, scopes: Vec<ScopeClaim>) -> Vec<Effect> {
        handle_at(c, who, wait_msg(req, scopes), NOW)
    }

    fn assert_queued(effects: &[Effect], expected_req: u64, expected_position: u32) {
        let ServerMsg::Queued { req, position } = only_reply(effects) else {
            panic!("expected Queued, got {effects:?}");
        };
        assert_eq!((req.0, *position), (expected_req, expected_position));
        assert!(logged(effects).is_empty(), "queueing logs nothing");
    }

    /// The shape of the effects, to check ordering.
    fn kinds(effects: &[Effect]) -> Vec<&'static str> {
        let mut out = Vec::new();
        for effect in effects {
            out.push(match effect {
                Effect::Reply(_) => "reply",
                Effect::Notify { .. } => "notify",
                Effect::Log(_) => "log",
            });
        }
        out
    }

    fn released(effects: &[Effect]) -> Vec<(ClaimId, ReleaseReason)> {
        let mut out = Vec::new();
        for event in logged(effects) {
            if let EventKind::ClaimReleased { claim, reason } = &event.kind {
                out.push((*claim, *reason));
            }
        }
        out
    }

    fn expired_notices(effects: &[Effect]) -> Vec<(AgentId, ClaimId, Fence)> {
        let mut out = Vec::new();
        for effect in effects {
            if let Effect::Notify {
                agent: who,
                msg: ServerMsg::LeaseExpired { claim, fence },
            } = effect
            {
                out.push((who.clone(), *claim, *fence));
            }
        }
        out
    }

    /// A grant delivered as a notice: (agent, original req, claim, fence, expiry).
    type GrantNotice = (AgentId, u64, ClaimId, Fence, u64);

    fn granted_notices(effects: &[Effect]) -> Vec<GrantNotice> {
        let mut out = Vec::new();
        for effect in effects {
            if let Effect::Notify {
                agent: who,
                msg:
                    ServerMsg::Granted {
                        req,
                        claim,
                        fence,
                        expires_at_ms,
                        ..
                    },
            } = effect
            {
                out.push((who.clone(), req.0, *claim, *fence, *expires_at_ms));
            }
        }
        out
    }

    fn x_edit() -> ScopeClaim {
        sc(file("src/x.rs"), Mode::EditBody)
    }

    fn y_edit() -> ScopeClaim {
        sc(file("src/y.rs"), Mode::EditBody)
    }

    fn z_edit() -> ScopeClaim {
        sc(file("src/z.rs"), Mode::EditBody)
    }

    #[test]
    fn lease_expires_exactly_at_its_expiry_instant() {
        let mut c = coordinator();
        let (claim, fence) = grant(&mut c, "a", vec![x_edit()]);
        let expiry = NOW + LEASE;
        assert!(c.expire(expiry - 1).is_empty());
        assert_eq!(c.next_expiry_ms(), Some(expiry));

        let effects = c.expire(expiry);
        assert_eq!(
            released(&effects),
            vec![(claim, ReleaseReason::LeaseExpired)]
        );
        assert_eq!(expired_notices(&effects), vec![(agent("a"), claim, fence)]);
        assert_eq!(c.next_expiry_ms(), None);
        assert!(c.locks.is_empty());
    }

    #[test]
    fn handle_still_denies_one_millisecond_before_expiry_and_grants_at_it() {
        let mut c = coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        let msg = || claim_msg(intent("b work"), vec![x_edit()]);

        let early = handle_at(&mut c, "b", msg(), NOW + LEASE - 1);
        let ServerMsg::Denied { .. } = only_reply(&early) else {
            panic!("expected Denied, got {early:?}");
        };
        let on_time = handle_at(&mut c, "b", msg(), NOW + LEASE);
        let ServerMsg::Granted { .. } = only_reply(&on_time) else {
            panic!("expected Granted, got {on_time:?}");
        };
    }

    #[test]
    fn simultaneous_expiries_are_logged_and_notified_in_claim_id_order() {
        let mut c = coordinator();
        let (first, first_fence) = grant_at(&mut c, "b", vec![x_edit()], NOW);
        let (second, second_fence) = grant_at(&mut c, "a", vec![y_edit()], NOW);
        let (third, _) = grant_at(&mut c, "a", vec![z_edit()], NOW + 10);

        let effects = c.expire(NOW + LEASE + 5);
        assert_eq!(
            released(&effects),
            vec![
                (first, ReleaseReason::LeaseExpired),
                (second, ReleaseReason::LeaseExpired)
            ]
        );
        assert_eq!(
            expired_notices(&effects),
            vec![
                (agent("b"), first, first_fence),
                (agent("a"), second, second_fence)
            ]
        );
        let seqs: Vec<u64> = logged(&effects).iter().map(|e| e.seq).collect();
        assert_eq!(seqs[1], seqs[0] + 1);
        assert_eq!(c.next_expiry_ms(), Some(NOW + 10 + LEASE));

        let rest = c.expire(NOW + 10 + LEASE);
        assert_eq!(released(&rest), vec![(third, ReleaseReason::LeaseExpired)]);
    }

    #[test]
    fn handle_expires_the_blocker_first_and_grants_in_the_same_call() {
        let mut c = coordinator();
        let (claim, fence) = grant(&mut c, "a", vec![x_edit()]);

        let effects = handle_at(
            &mut c,
            "b",
            claim_msg(intent("b work"), vec![x_edit()]),
            NOW + LEASE,
        );
        assert_eq!(kinds(&effects), vec!["log", "notify", "log", "reply"]);
        assert_eq!(expired_notices(&effects), vec![(agent("a"), claim, fence)]);
        let ServerMsg::Granted { claim: new, .. } = only_reply(&effects) else {
            panic!("expected Granted, got {effects:?}");
        };
        assert_eq!(new.0, claim.0 + 1);
        let seqs: Vec<u64> = logged(&effects).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2]);
    }

    #[test]
    fn expired_claim_is_unknown_to_release_and_amend() {
        let mut c = coordinator();
        let (claim, fence) = grant(&mut c, "a", vec![x_edit()]);
        let late = NOW + LEASE;

        let mut lazy = c.clone();
        let effects = handle_at(&mut lazy, "a", ClientMsg::Release { claim, fence }, late);
        let ServerMsg::Error { code, .. } = only_reply(&effects) else {
            panic!("expected Error, got {effects:?}");
        };
        assert_eq!(*code, ErrorCode::UnknownClaim);
        assert_eq!(expired_notices(&effects).len(), 1);

        c.expire(late);
        let release = handle_at(&mut c, "a", ClientMsg::Release { claim, fence }, late);
        assert_error(&release, ErrorCode::UnknownClaim);
        let msg = ClientMsg::Amend {
            req: RequestId(5),
            claim,
            fence,
            add: vec![y_edit()],
        };
        assert_error(&handle_at(&mut c, "a", msg, late), ErrorCode::UnknownClaim);
        let stale = handle_at(&mut c, "a", submit_msg(claim, fence, vec![x_edit()]), late);
        assert_submit_error(&stale, ErrorCode::UnknownClaim);
    }

    #[test]
    fn heartbeat_renews_all_of_the_senders_claims_and_nobody_elses() {
        let mut c = coordinator();
        let (a1, _) = grant(&mut c, "a", vec![x_edit()]);
        let (a2, _) = grant(&mut c, "a", vec![y_edit()]);
        let (b1, _) = grant(&mut c, "b", vec![z_edit()]);

        let effects = heartbeat(&mut c, "a", NOW + 100);
        assert!(effects.is_empty(), "heartbeat has no reply and no event");

        let first = c.expire(NOW + LEASE);
        assert_eq!(released(&first), vec![(b1, ReleaseReason::LeaseExpired)]);
        let second = c.expire(NOW + 100 + LEASE);
        assert_eq!(
            released(&second),
            vec![
                (a1, ReleaseReason::LeaseExpired),
                (a2, ReleaseReason::LeaseExpired)
            ]
        );
    }

    #[test]
    fn heartbeat_cannot_resurrect_an_expired_claim() {
        let mut c = coordinator();
        let (claim, fence) = grant(&mut c, "a", vec![x_edit()]);

        let effects = heartbeat(&mut c, "a", NOW + LEASE);
        assert_eq!(expired_notices(&effects), vec![(agent("a"), claim, fence)]);
        assert!(replies(&effects).is_empty());
        assert_eq!(c.next_expiry_ms(), None);
        grant(&mut c, "b", vec![x_edit()]);
    }

    #[test]
    fn next_expiry_is_the_earliest_lease_and_follows_heartbeats() {
        let mut c = coordinator();
        assert_eq!(c.next_expiry_ms(), None);
        grant_at(&mut c, "a", vec![x_edit()], NOW + 10);
        assert_eq!(c.next_expiry_ms(), Some(NOW + 10 + LEASE));
        grant_at(&mut c, "b", vec![y_edit()], NOW + 50);
        grant_at(&mut c, "c", vec![z_edit()], NOW + 90);
        assert_eq!(c.next_expiry_ms(), Some(NOW + 10 + LEASE));

        heartbeat(&mut c, "a", NOW + 200);
        assert_eq!(c.next_expiry_ms(), Some(NOW + 50 + LEASE));
    }

    #[test]
    fn amend_issues_a_newer_fence_keeps_the_lease_and_reports_at_risk() {
        let mut c = coordinator();
        let assuming = Intent {
            summary: "b work".into(),
            task_ref: None,
            assumptions: vec![Assumption {
                scope: file("src/z.rs"),
                statement: "z stays pure".into(),
            }],
        };
        let (claim, old) = grant(&mut c, "a", vec![x_edit()]);
        let (_, other_fence) = grant_with(&mut c, "b", assuming, vec![y_edit()]);

        let msg = ClientMsg::Amend {
            req: RequestId(5),
            claim,
            fence: old,
            add: vec![z_edit()],
        };
        let effects = handle_at(&mut c, "a", msg, NOW + 500);
        let ServerMsg::Granted {
            req,
            claim: same,
            fence,
            expires_at_ms,
            race,
            at_risk,
        } = only_reply(&effects)
        else {
            panic!("expected Granted, got {effects:?}");
        };
        assert_eq!((*req, *same), (RequestId(5), claim));
        assert!(*fence > old && *fence > other_fence, "{fence:?}");
        assert_eq!(*expires_at_ms, NOW + LEASE, "amend is not a heartbeat");
        assert_eq!(*race, None);
        assert_eq!(at_risk.len(), 1);
        assert_eq!(at_risk[0].agent, agent("b"));
        assert_eq!(at_risk[0].assumption.statement, "z stays pure");
        assert_eq!(c.next_expiry_ms(), Some(NOW + LEASE));

        let EventKind::ClaimAmended {
            claim: logged_claim,
            fence: logged_fence,
            added,
        } = only_event(&effects)
        else {
            panic!("expected ClaimAmended, got {effects:?}");
        };
        assert_eq!((*logged_claim, *logged_fence), (claim, *fence));
        assert_eq!(added, &vec![z_edit()]);
    }

    #[test]
    fn amend_retires_the_old_fence_and_the_added_scopes_block_others() {
        let mut c = coordinator();
        let (claim, old) = grant(&mut c, "a", vec![x_edit()]);
        let effects = amend(&mut c, "a", claim, old, vec![z_edit()]);
        let ServerMsg::Granted { fence: new, .. } = only_reply(&effects) else {
            panic!("expected Granted, got {effects:?}");
        };
        let new = *new;

        assert_error(&release(&mut c, "a", claim, old), ErrorCode::StaleFence);
        let again = amend(&mut c, "a", claim, old, vec![y_edit()]);
        assert_error(&again, ErrorCode::StaleFence);

        let conflicts = deny(&mut c, "b", vec![z_edit()]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].held, z_edit());
        assert_eq!(conflicts[0].held_by, agent("a"));
        assert_eq!(deny(&mut c, "b", vec![x_edit()]).len(), 1);

        let freed = release(&mut c, "a", claim, new);
        assert_eq!(released(&freed), vec![(claim, ReleaseReason::Agent)]);
        assert!(c.locks.is_empty(), "release must free the amended scopes");
    }

    #[test]
    fn amend_conflict_is_denied_logged_and_changes_nothing() {
        let build = || {
            let mut c = coordinator();
            grant(&mut c, "a", vec![x_edit()]);
            let (claim, fence) = grant_with(&mut c, "b", intent("b work"), vec![y_edit()]);
            (c, claim, fence)
        };
        let (mut c, claim, fence) = build();
        let (mut baseline, _, _) = build();
        let claims_before = serde_json::to_string(&c.state.claims).unwrap();
        let locks_before = c.locks.len();

        let effects = amend(&mut c, "b", claim, fence, vec![x_edit(), x_edit()]);
        let ServerMsg::Denied { req, conflicts } = only_reply(&effects) else {
            panic!("expected Denied, got {effects:?}");
        };
        assert_eq!(*req, RequestId(5));
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].requested, x_edit());
        assert_eq!(conflicts[0].held_by, agent("a"));
        let EventKind::ClaimDenied {
            agent: who,
            scopes,
            intent,
            conflicts: logged_conflicts,
        } = only_event(&effects)
        else {
            panic!("expected ClaimDenied, got {effects:?}");
        };
        assert_eq!(who, &agent("b"));
        assert_eq!(scopes, &vec![x_edit()]);
        assert_eq!(intent.summary, "b work");
        assert_eq!(logged_conflicts.len(), 1);

        assert_eq!(
            serde_json::to_string(&c.state.claims).unwrap(),
            claims_before
        );
        assert_eq!(c.locks.len(), locks_before);
        assert_eq!(
            grant(&mut c, "c", vec![sc(file("src/w.rs"), Mode::EditBody)]),
            grant(
                &mut baseline,
                "c",
                vec![sc(file("src/w.rs"), Mode::EditBody)]
            ),
            "a denied amend must not consume a fence"
        );
        let freed = release(&mut c, "b", claim, fence);
        assert_eq!(released(&freed), vec![(claim, ReleaseReason::Agent)]);
    }

    #[test]
    fn a_denied_amend_places_no_lock_for_the_scope_it_asked_for() {
        let mut c = coordinator();
        let (blocker, blocker_fence) = grant(&mut c, "a", vec![x_edit()]);
        let (claim, fence) = grant(&mut c, "b", vec![y_edit()]);
        let effects = amend(&mut c, "b", claim, fence, vec![x_edit()]);
        let ServerMsg::Denied { .. } = only_reply(&effects) else {
            panic!("expected Denied, got {effects:?}");
        };

        release(&mut c, "a", blocker, blocker_fence);
        grant(&mut c, "c", vec![x_edit()]);
    }

    #[test]
    fn invalid_amends_are_rejected_and_change_nothing() {
        let mut c = coordinator();
        let (claim, fence) = grant(&mut c, "a", vec![x_edit()]);
        let before = state(&c);

        let unknown = amend(&mut c, "a", ClaimId(claim.0 + 99), fence, vec![y_edit()]);
        assert_error(&unknown, ErrorCode::UnknownClaim);
        let not_owner = amend(&mut c, "b", claim, fence, vec![y_edit()]);
        assert_error(&not_owner, ErrorCode::NotOwner);
        let stale = amend(&mut c, "a", claim, Fence(fence.0 + 7), vec![y_edit()]);
        assert_error(&stale, ErrorCode::StaleFence);
        assert_eq!(
            error_message(&stale),
            "claim 1 rejected fence 8: not the current fence"
        );
        let empty = amend(&mut c, "a", claim, fence, vec![]);
        assert_error(&empty, ErrorCode::Malformed);

        assert_eq!(state(&c), before);
    }

    #[test]
    fn amend_with_only_held_scopes_still_issues_a_fence_and_adds_nothing() {
        let mut c = coordinator();
        let (claim, old) = grant(&mut c, "a", vec![x_edit(), y_edit()]);
        let locks_before = c.locks.len();

        let effects = amend(&mut c, "a", claim, old, vec![x_edit(), x_edit()]);
        let ServerMsg::Granted { fence, .. } = only_reply(&effects) else {
            panic!("expected Granted, got {effects:?}");
        };
        assert!(*fence > old);
        let EventKind::ClaimAmended { added, .. } = only_event(&effects) else {
            panic!("expected ClaimAmended, got {effects:?}");
        };
        assert!(added.is_empty());
        assert_eq!(c.locks.len(), locks_before);

        let next = amend(
            &mut c,
            "a",
            claim,
            *fence,
            vec![z_edit(), z_edit(), y_edit()],
        );
        let EventKind::ClaimAmended { added, .. } = only_event(&next) else {
            panic!("expected ClaimAmended, got {next:?}");
        };
        assert_eq!(added, &vec![z_edit()]);
    }

    #[test]
    fn wait_is_granted_immediately_when_nothing_blocks() {
        let mut c = coordinator();
        let effects = wait_for(&mut c, "a", 9, vec![x_edit()]);
        let ServerMsg::Granted { req, .. } = only_reply(&effects) else {
            panic!("expected Granted, got {effects:?}");
        };
        assert_eq!(*req, RequestId(9));
        let EventKind::ClaimGranted { .. } = only_event(&effects) else {
            panic!("expected ClaimGranted, got {effects:?}");
        };
        assert!(c.state.waiting.is_empty());
    }

    #[test]
    fn wait_while_holding_a_claim_is_refused_and_not_queued() {
        let mut c = coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        grant(&mut c, "b", vec![y_edit()]);
        let before = state(&c);

        let effects = wait_for(&mut c, "b", 9, vec![x_edit()]);
        assert_error(&effects, ErrorCode::WaitWhileHolding);
        assert_eq!(state(&c), before);
        assert!(c.state.waiting.is_empty());
    }

    #[test]
    fn an_agent_with_a_queued_request_may_not_claim_anything_else() {
        let mut c = coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        assert_queued(&wait_for(&mut c, "b", 9, vec![x_edit()]), 9, 1);
        let before = state(&c);

        let free = vec![z_edit()];
        assert_error(
            &claim_as(&mut c, "b", free.clone()),
            ErrorCode::WaitWhileHolding,
        );
        assert_error(
            &wait_for(&mut c, "b", 10, free.clone()),
            ErrorCode::WaitWhileHolding,
        );
        let shadow = ClientMsg::Claim {
            req: RequestId(11),
            intent: intent("s"),
            scopes: free,
            on_conflict: OnConflict::Shadow,
        };
        let effects = handle_at(&mut c, "b", shadow, NOW);
        assert_error(&effects, ErrorCode::WaitWhileHolding);
        assert_eq!(state(&c), before);
        grant(&mut c, "c", vec![z_edit()]);
    }

    #[test]
    fn queued_requests_get_one_based_positions_and_consume_no_ids() {
        let mut c = coordinator();
        let mut baseline = coordinator();
        for who in [&mut c, &mut baseline] {
            grant(who, "h", vec![x_edit()]);
        }
        for (position, who) in [(1, "b"), (2, "c"), (3, "d")] {
            let req = 10 + u64::from(position);
            assert_queued(&wait_for(&mut c, who, req, vec![x_edit()]), req, position);
        }
        assert_eq!(
            grant(&mut c, "e", vec![y_edit()]),
            grant(&mut baseline, "e", vec![y_edit()])
        );
    }

    #[test]
    fn release_grants_waiters_in_fifo_order_under_their_original_req() {
        let mut c = coordinator();
        let (held, held_fence) = grant(&mut c, "h", vec![x_edit()]);
        wait_for(&mut c, "b", 11, vec![x_edit()]);
        wait_for(&mut c, "c", 12, vec![x_edit()]);

        let msg = ClientMsg::Release {
            claim: held,
            fence: held_fence,
        };
        let effects = handle_at(&mut c, "h", msg, NOW + 50);
        assert_eq!(kinds(&effects), vec!["log", "log", "notify"]);
        let EventKind::ClaimGranted { agent: who, .. } = &logged(&effects)[1].kind else {
            panic!("expected ClaimGranted, got {effects:?}");
        };
        assert_eq!(who, &agent("b"));
        let granted = granted_notices(&effects);
        assert_eq!(granted.len(), 1);
        let (to, req, claim, fence, expires) = granted[0].clone();
        assert_eq!((to, req), (agent("b"), 11));
        assert_eq!(expires, NOW + 50 + LEASE, "the lease starts at the grant");
        assert_eq!(deny(&mut c, "d", vec![x_edit()]).len(), 1);

        let next = handle_at(&mut c, "b", ClientMsg::Release { claim, fence }, NOW + 60);
        let granted = granted_notices(&next);
        assert_eq!(granted.len(), 1);
        assert_eq!((granted[0].0.clone(), granted[0].1), (agent("c"), 12));
    }

    #[test]
    fn expiry_grants_waiters_after_the_expiry_notice() {
        let mut c = coordinator();
        grant(&mut c, "h", vec![x_edit()]);
        wait_for(&mut c, "b", 11, vec![x_edit()]);

        let effects = c.expire(NOW + LEASE);
        assert_eq!(kinds(&effects), vec!["log", "notify", "log", "notify"]);
        assert_eq!(expired_notices(&effects).len(), 1);
        let granted = granted_notices(&effects);
        assert_eq!(granted.len(), 1);
        assert_eq!((granted[0].0.clone(), granted[0].1), (agent("b"), 11));
        assert_eq!(granted[0].4, NOW + 2 * LEASE);
        let seqs: Vec<u64> = logged(&effects).iter().map(|e| e.seq).collect();
        assert_eq!(seqs[1], seqs[0] + 1);
    }

    #[test]
    fn a_later_waiter_never_overtakes_an_earlier_one_it_conflicts_with() {
        let mut c = coordinator();
        let a_sig = sc(file("src/a.rs"), Mode::EditSignature);
        let a_body = sc(file("src/a.rs"), Mode::EditBody);
        let b_body = sc(file("src/b.rs"), Mode::EditBody);
        let (h1, h1_fence) = grant(&mut c, "h1", vec![a_sig]);
        let (h2, h2_fence) = grant(&mut c, "h2", vec![b_body.clone()]);
        assert_queued(
            &wait_for(&mut c, "w1", 11, vec![a_body.clone(), b_body]),
            11,
            1,
        );
        assert_queued(&wait_for(&mut c, "w2", 12, vec![a_body]), 12, 2);
        let a_depend = sc(file("src/a.rs"), Mode::Depend);
        assert_queued(&wait_for(&mut c, "w3", 13, vec![a_depend]), 13, 3);

        let msg = ClientMsg::Release {
            claim: h1,
            fence: h1_fence,
        };
        let freed = handle_at(&mut c, "h1", msg, NOW);
        let granted = granted_notices(&freed);
        assert_eq!(granted.len(), 1, "{freed:?}");
        assert_eq!((granted[0].0.clone(), granted[0].1), (agent("w3"), 13));

        let msg = ClientMsg::Release {
            claim: h2,
            fence: h2_fence,
        };
        let freed = handle_at(&mut c, "h2", msg, NOW);
        let granted = granted_notices(&freed);
        assert_eq!(granted.len(), 1, "{freed:?}");
        assert_eq!((granted[0].0.clone(), granted[0].1), (agent("w1"), 11));
    }

    /// Leases and a queue in flight, with time passing between steps.
    fn leasing_coordinator() -> Coordinator {
        let mut c = coordinator();
        hello(&mut c, "h", "abc", PROTOCOL_VERSION);
        grant_at(&mut c, "h", vec![x_edit()], NOW);
        grant_at(&mut c, "g", vec![y_edit()], NOW + 100);
        wait_for(&mut c, "b", 11, vec![x_edit()]);
        wait_for(&mut c, "d", 12, vec![x_edit()]);
        heartbeat(&mut c, "g", NOW + 150);
        amend(&mut c, "g", ClaimId(2), Fence(2), vec![z_edit()]);
        c.expire(NOW + LEASE);
        c
    }

    #[test]
    fn leases_and_queue_serialize_identically_and_survive_a_round_trip() {
        let first = state(&leasing_coordinator());
        assert_eq!(first, state(&leasing_coordinator()));
        let restored: Coordinator = serde_json::from_str(&first).unwrap();
        assert_eq!(state(&restored), first);
        assert_eq!(restored.state.waiting.len(), 1);
        assert_eq!(
            restored.next_expiry_ms(),
            leasing_coordinator().next_expiry_ms()
        );
    }

    #[test]
    fn a_restored_coordinator_expires_and_grants_like_the_original() {
        let mut live = leasing_coordinator();
        let mut restored: Coordinator = serde_json::from_str(&state(&live)).unwrap();
        let script = [
            ("g", ClientMsg::Heartbeat, NOW + LEASE + 10),
            (
                "d",
                claim_msg(intent("late"), vec![y_edit()]),
                NOW + LEASE + 15,
            ),
            (
                "b",
                ClientMsg::Release {
                    claim: ClaimId(3),
                    fence: Fence(4),
                },
                NOW + LEASE + 20,
            ),
            ("zz", ClientMsg::Heartbeat, NOW + 3 * LEASE),
        ];
        for (who, msg, now) in script {
            let from_live = handle_at(&mut live, who, msg.clone(), now);
            let from_restored = handle_at(&mut restored, who, msg, now);
            assert_eq!(format!("{from_live:?}"), format!("{from_restored:?}"));
            assert_eq!(state(&live), state(&restored));
        }
    }

    #[test]
    fn event_seq_stays_gapless_across_leases_amends_and_the_queue() {
        let mut c = coordinator();
        let mut all = Vec::new();
        all.extend(hello(&mut c, "h", "abc", PROTOCOL_VERSION));
        let first = handle_at(&mut c, "h", claim_msg(intent("h"), vec![x_edit()]), NOW);
        let ServerMsg::Granted { claim, fence, .. } = only_reply(&first).clone() else {
            panic!("expected Granted");
        };
        all.extend(first);
        all.extend(claim_as(&mut c, "g", vec![y_edit()]));
        all.extend(claim_as(&mut c, "b", vec![x_edit()]));
        all.extend(wait_for(&mut c, "b", 11, vec![x_edit()]));
        all.extend(amend(&mut c, "g", ClaimId(2), Fence(2), vec![z_edit()]));
        all.extend(amend(&mut c, "g", ClaimId(2), Fence(3), vec![x_edit()]));
        all.extend(heartbeat(&mut c, "g", NOW + 10));
        all.extend(handle_at(
            &mut c,
            "h",
            ClientMsg::Release { claim, fence },
            NOW + 20,
        ));
        all.extend(c.expire(NOW + LEASE));
        all.extend(c.expire(NOW + 10 + LEASE));

        let seqs: Vec<u64> = logged(&all).iter().map(|event| event.seq).collect();
        assert!(seqs.len() > 8, "{seqs:?}");
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<u64>>());
    }

    #[test]
    fn an_earlier_now_does_not_shorten_leases() {
        let mut c = coordinator();
        grant_at(&mut c, "a", vec![x_edit()], NOW + 1_000);
        let expiry = NOW + 1_000 + LEASE;
        assert_eq!(c.next_expiry_ms(), Some(expiry));

        assert!(heartbeat(&mut c, "a", NOW).is_empty());
        assert_eq!(c.next_expiry_ms(), Some(expiry));
        let effects = handle_at(&mut c, "b", claim_msg(intent("b"), vec![y_edit()]), NOW);
        let ServerMsg::Granted { expires_at_ms, .. } = only_reply(&effects) else {
            panic!("expected Granted, got {effects:?}");
        };
        assert_eq!(*expires_at_ms, expiry, "a grant uses the later clock");
    }

    #[test]
    fn an_earlier_now_does_not_unexpire_anything_or_move_events_back() {
        let mut c = coordinator();
        let (claim, fence) = grant(&mut c, "a", vec![x_edit()]);
        let late = NOW + LEASE;
        let mut all = c.expire(late);
        assert_eq!(released(&all).len(), 1);

        assert!(c.expire(NOW).is_empty());
        all.extend(handle_at(
            &mut c,
            "b",
            claim_msg(intent("b"), vec![x_edit()]),
            NOW,
        ));
        let msg = ClientMsg::Release { claim, fence };
        let stale = handle_at(&mut c, "a", msg, NOW);
        assert_error(&stale, ErrorCode::UnknownClaim);
        all.extend(stale);

        let times: Vec<u64> = logged(&all).iter().map(|e| e.at_ms).collect();
        assert_eq!(times, vec![late, late]);
    }

    #[test]
    fn the_clock_survives_a_round_trip() {
        let mut c = coordinator();
        grant_at(&mut c, "a", vec![x_edit()], NOW + 5_000);
        let mut restored: Coordinator = serde_json::from_str(&state(&c)).unwrap();

        let effects = handle_at(
            &mut restored,
            "b",
            claim_msg(intent("b"), vec![y_edit()]),
            NOW,
        );
        assert_eq!(logged(&effects)[0].at_ms, NOW + 5_000);
    }

    #[test]
    fn a_zero_lease_is_rejected_at_construction() {
        let config = |lease_ms| Config {
            run: RunId("test".into()),
            lease_ms,
            shadow_enabled: false,
        };
        let err = Coordinator::new(config(0)).unwrap_err();
        assert!(err.to_string().contains("lease_ms"), "{err}");
        assert!(Coordinator::new(config(1)).is_ok());
    }

    // ---- submit, assumption challenges and shadow claims ----

    fn shadow_coordinator() -> Coordinator {
        Coordinator::new(Config {
            run: RunId("test".into()),
            lease_ms: LEASE,
            shadow_enabled: true,
        })
        .unwrap()
    }

    fn submit_msg(claim: ClaimId, fence: Fence, touched: Vec<ScopeClaim>) -> ClientMsg {
        ClientMsg::Submit {
            req: RequestId(9),
            claim,
            fence,
            fork_commit: CommitId("fork".into()),
            touched,
            decisions: DecisionRecord::default(),
        }
    }

    fn submit(
        c: &mut Coordinator,
        who: &str,
        claim: ClaimId,
        fence: Fence,
        touched: Vec<ScopeClaim>,
    ) -> Vec<Effect> {
        handle_at(c, who, submit_msg(claim, fence, touched), NOW)
    }

    fn accepted_position(effects: &[Effect], claim: ClaimId) -> u32 {
        let ServerMsg::Accepted {
            req,
            claim: accepted,
            queue_position,
        } = only_reply(effects)
        else {
            panic!("expected Accepted, got {effects:?}");
        };
        assert_eq!((req.0, *accepted), (9, claim));
        *queue_position
    }

    /// An error that answers request 9 and logs nothing.
    fn assert_submit_error(effects: &[Effect], expected: ErrorCode) {
        assert_error(effects, expected);
        let ServerMsg::Error { req, .. } = only_reply(effects) else {
            panic!("expected Error, got {effects:?}");
        };
        assert_eq!(*req, Some(RequestId(9)));
    }

    fn shadow_with(
        c: &mut Coordinator,
        who: &str,
        intent: Intent,
        scopes: Vec<ScopeClaim>,
    ) -> Vec<Effect> {
        let msg = ClientMsg::Claim {
            req: RequestId(3),
            intent,
            scopes,
            on_conflict: OnConflict::Shadow,
        };
        handle_at(c, who, msg, NOW)
    }

    /// (claim, fence, lease expiry, conflicts) of a `Shadowed` reply.
    fn shadowed(effects: &[Effect]) -> (ClaimId, Fence, u64, Vec<Conflict>) {
        let ServerMsg::Shadowed {
            claim,
            fence,
            expires_at_ms,
            conflicts,
            ..
        } = only_reply(effects)
        else {
            panic!("expected Shadowed, got {effects:?}");
        };
        (*claim, *fence, *expires_at_ms, conflicts.clone())
    }

    fn shadow_as(c: &mut Coordinator, who: &str, scopes: Vec<ScopeClaim>) -> (ClaimId, Fence) {
        let effects = shadow_with(c, who, intent("shadow work"), scopes);
        let (claim, fence, _, _) = shadowed(&effects);
        (claim, fence)
    }

    fn intent_assuming(summary: &str, assumed: &[(Scope, &str)]) -> Intent {
        let mut assumptions = Vec::new();
        for (scope, statement) in assumed {
            assumptions.push(Assumption {
                scope: scope.clone(),
                statement: (*statement).into(),
            });
        }
        Intent {
            summary: summary.into(),
            task_ref: None,
            assumptions,
        }
    }

    type Challenge = (ClaimId, Assumption, AgentId, CommitId);

    fn challenge_events(effects: &[Effect]) -> Vec<Challenge> {
        let mut out = Vec::new();
        for event in logged(effects) {
            if let EventKind::AssumptionChallenged {
                claim,
                assumption,
                by,
                their_commit,
            } = &event.kind
            {
                out.push((*claim, assumption.clone(), by.clone(), their_commit.clone()));
            }
        }
        out
    }

    fn challenge_notices(effects: &[Effect]) -> Vec<(AgentId, Challenge)> {
        let mut out = Vec::new();
        for effect in effects {
            if let Effect::Notify {
                agent: who,
                msg:
                    ServerMsg::AssumptionChallenged {
                        claim,
                        assumption,
                        by,
                        their_commit,
                    },
            } = effect
            {
                let challenge = (*claim, assumption.clone(), by.clone(), their_commit.clone());
                out.push((who.clone(), challenge));
            }
        }
        out
    }

    fn holders_in(c: &Coordinator) -> Vec<u64> {
        let mut owners = Vec::new();
        for (_, claim, _, _) in lock_dump(&c.locks) {
            if !owners.contains(&claim) {
                owners.push(claim);
            }
        }
        owners.sort();
        owners
    }

    #[test]
    fn submit_is_accepted_in_submission_order_and_logs_the_decision_record() {
        let mut c = coordinator();
        let (a, a_fence) = grant(&mut c, "a", vec![x_edit()]);
        let (b, b_fence) = grant(&mut c, "b", vec![y_edit()]);
        let decisions = DecisionRecord {
            rejected: vec![RejectedApproach {
                approach: "retry loop".into(),
                reason: "flaky".into(),
            }],
            evidence: vec!["cargo test passed".into()],
            transcript: Some(TranscriptRef {
                repo: "transcripts/b".into(),
                path: "t.json".into(),
                sha256: "ab".into(),
            }),
        };
        let msg = ClientMsg::Submit {
            req: RequestId(9),
            claim: b,
            fence: b_fence,
            fork_commit: CommitId("fork-b".into()),
            touched: vec![y_edit(), y_edit()],
            decisions: decisions.clone(),
        };
        let first = handle_at(&mut c, "b", msg, NOW);
        assert_eq!(accepted_position(&first, b), 1);
        assert_eq!(kinds(&first), ["log", "reply"]);
        let EventKind::Submitted {
            claim,
            fork_commit,
            touched,
            decisions: logged_decisions,
        } = only_event(&first)
        else {
            panic!("expected Submitted, got {first:?}");
        };
        assert_eq!((*claim, fork_commit), (b, &CommitId("fork-b".into())));
        assert_eq!(touched, &vec![y_edit()]);
        assert_eq!(format!("{logged_decisions:?}"), format!("{decisions:?}"));

        let second = submit(&mut c, "a", a, a_fence, vec![x_edit()]);
        assert_eq!(accepted_position(&second, a), 2);
    }

    #[test]
    fn invalid_submits_are_rejected_and_change_nothing() {
        let mut c = coordinator();
        let (a, a_fence) = grant(&mut c, "a", vec![x_edit()]);
        let (done, done_fence) = grant(&mut c, "d", vec![z_edit()]);
        let first = submit(&mut c, "d", done, done_fence, vec![z_edit()]);
        assert_eq!(accepted_position(&first, done), 1);
        let before = state(&c);
        let cases = [
            ("a", ClaimId(99), a_fence, ErrorCode::UnknownClaim),
            ("b", a, a_fence, ErrorCode::NotOwner),
            ("a", a, Fence(a_fence.0 + 50), ErrorCode::StaleFence),
            ("d", done, done_fence, ErrorCode::AlreadySubmitted),
        ];
        for (who, claim, fence, code) in cases {
            let effects = submit(&mut c, who, claim, fence, vec![x_edit()]);
            assert_submit_error(&effects, code);
            assert_eq!(state(&c), before, "{code:?}");
        }
        let empty = submit(&mut c, "a", a, a_fence, vec![]);
        assert_submit_error(&empty, ErrorCode::Malformed);
        assert_eq!(state(&c), before);
        assert_eq!(grant(&mut c, "e", vec![y_edit()]), (ClaimId(3), Fence(3)));
    }

    #[test]
    fn a_fence_retired_by_amend_is_rejected_on_submit() {
        let mut c = coordinator();
        let (a, old) = grant(&mut c, "a", vec![x_edit()]);
        let amended = amend(&mut c, "a", a, old, vec![y_edit()]);
        let ServerMsg::Granted { fence: new, .. } = only_reply(&amended) else {
            panic!("expected Granted, got {amended:?}");
        };
        let stale = submit(&mut c, "a", a, old, vec![x_edit()]);
        assert_submit_error(&stale, ErrorCode::StaleFence);
        let fresh = submit(&mut c, "a", a, *new, vec![x_edit(), y_edit()]);
        assert_eq!(accepted_position(&fresh, a), 1);
    }

    #[test]
    fn uncovered_scopes_are_listed_logged_and_the_claim_stays_submittable() {
        let mut c = coordinator();
        let (a, a_fence) = grant(&mut c, "a", vec![x_edit()]);
        let (b, b_fence) = grant(&mut c, "b", vec![z_edit()]);
        let too_strong = sc(file("src/x.rs"), Mode::EditSignature);
        let touched = vec![x_edit(), y_edit(), too_strong.clone()];
        let effects = submit(&mut c, "a", a, a_fence, touched.clone());

        let ServerMsg::Uncovered { claim, scopes } = only_reply(&effects) else {
            panic!("expected Uncovered, got {effects:?}");
        };
        assert_eq!(*claim, a);
        assert_eq!(scopes, &vec![y_edit(), too_strong]);
        let EventKind::SubmitRejected { claim, reason } = only_event(&effects) else {
            panic!("expected SubmitRejected, got {effects:?}");
        };
        assert_eq!(*claim, a);
        assert_eq!(reason, "2 touched scope(s) not covered by the claim");

        // Same claim and fence: nothing was consumed or marked.
        let retry = submit(&mut c, "a", a, a_fence, vec![x_edit()]);
        assert_eq!(accepted_position(&retry, a), 1);

        // Another agent widens its claim, which issues a new fence, then submits.
        let outside = submit(&mut c, "b", b, b_fence, vec![y_edit()]);
        let ServerMsg::Uncovered { scopes, .. } = only_reply(&outside) else {
            panic!("expected Uncovered, got {outside:?}");
        };
        assert_eq!(scopes, &vec![y_edit()]);
        let amended = amend(&mut c, "b", b, b_fence, vec![y_edit()]);
        let ServerMsg::Granted { fence, .. } = only_reply(&amended) else {
            panic!("expected Granted, got {amended:?}");
        };
        let fixed = submit(&mut c, "b", b, *fence, vec![y_edit()]);
        assert_eq!(accepted_position(&fixed, b), 2);
        assert_eq!(
            grant(&mut c, "e", vec![sc(file("src/q.rs"), Mode::Depend)]).0,
            ClaimId(3)
        );
    }

    #[test]
    fn a_dir_claim_covers_a_touched_file_and_symbol_beneath_it() {
        let mut c = coordinator();
        let (a, fence) = grant(&mut c, "a", vec![sc(dir("src"), Mode::EditSignature)]);
        let touched = vec![
            sc(file("src/deep/x.rs"), Mode::EditBody),
            sc(sym("src/deep/x.rs", "f"), Mode::EditSignature),
        ];
        let effects = submit(&mut c, "a", a, fence, touched);
        assert_eq!(accepted_position(&effects, a), 1);
        let (b, b_fence) = grant(&mut c, "b", vec![sc(file("lib/x.rs"), Mode::EditBody)]);
        let outside = submit(
            &mut c,
            "b",
            b,
            b_fence,
            vec![sc(file("src/x.rs"), Mode::Depend)],
        );
        let ServerMsg::Uncovered { scopes, .. } = only_reply(&outside) else {
            panic!("expected Uncovered, got {outside:?}");
        };
        assert_eq!(scopes, &vec![sc(file("src/x.rs"), Mode::Depend)]);
    }

    #[test]
    fn a_submitted_claim_never_expires_and_is_not_renewed() {
        let mut c = coordinator();
        let (a, fence) = grant_at(&mut c, "a", vec![x_edit()], NOW);
        let (b, _) = grant_at(&mut c, "b", vec![y_edit()], NOW);
        handle_at(&mut c, "a", submit_msg(a, fence, vec![x_edit()]), NOW + 10);
        assert_eq!(c.next_expiry_ms(), Some(NOW + LEASE));

        let before = state(&c);
        assert!(heartbeat(&mut c, "a", NOW + 10).is_empty());
        assert_eq!(
            state(&c),
            before,
            "heartbeat must not touch a submitted claim"
        );

        let at_old_expiry = c.expire(NOW + LEASE);
        assert_eq!(released(&at_old_expiry), [(b, ReleaseReason::LeaseExpired)]);
        assert_eq!(c.next_expiry_ms(), None);
        assert!(c.expire(NOW + 10 * LEASE).is_empty());
        let blocked = handle_at(
            &mut c,
            "b",
            claim_msg(intent("x"), vec![x_edit()]),
            NOW + 10 * LEASE,
        );
        let ServerMsg::Denied { conflicts, .. } = only_reply(&blocked) else {
            panic!("expected Denied, got {blocked:?}");
        };
        assert_eq!(conflicts[0].held_by, agent("a"));
    }

    #[test]
    fn a_submitted_claim_still_blocks_and_refuses_release_and_amend() {
        let mut c = coordinator();
        let (a, fence) = grant(&mut c, "a", vec![x_edit()]);
        submit(&mut c, "a", a, fence, vec![x_edit()]);
        let conflicts = deny(&mut c, "b", vec![x_edit()]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].held_by, agent("a"));

        let before = state(&c);
        let freed = release(&mut c, "a", a, fence);
        assert_error(&freed, ErrorCode::AlreadySubmitted);
        let ServerMsg::Error { req, .. } = only_reply(&freed) else {
            panic!("expected Error");
        };
        assert_eq!(*req, None);
        let amended = amend(&mut c, "a", a, fence, vec![y_edit()]);
        assert_error(&amended, ErrorCode::AlreadySubmitted);
        let ServerMsg::Error { req, .. } = only_reply(&amended) else {
            panic!("expected Error");
        };
        assert_eq!(*req, Some(RequestId(5)));
        assert_eq!(state(&c), before);
        assert_eq!(deny(&mut c, "b", vec![x_edit()]).len(), 1);
    }

    #[test]
    fn a_threatened_assumption_is_challenged_with_a_notice_and_an_event() {
        let mut c = coordinator();
        let target = sym("src/auth.rs", "refresh");
        let sibling = sym("src/auth.rs", "login");
        let holder_intent = intent_assuming(
            "holder",
            &[(target.clone(), "returns Some"), (sibling, "unrelated")],
        );
        let depend = sc(file("src/other.rs"), Mode::Depend);
        let (holder, _) = grant_with(&mut c, "holder", holder_intent, vec![depend]);
        let own = intent_assuming("author", &[(target.clone(), "own belief")]);
        let edit = sc(target.clone(), Mode::EditBody);
        let (b, b_fence) = grant_with(&mut c, "b", own, vec![edit.clone()]);

        let msg = ClientMsg::Submit {
            req: RequestId(9),
            claim: b,
            fence: b_fence,
            fork_commit: CommitId("fork-b".into()),
            touched: vec![edit],
            decisions: DecisionRecord::default(),
        };
        let effects = handle_at(&mut c, "b", msg, NOW);
        assert_eq!(kinds(&effects), ["log", "log", "notify", "reply"]);
        assert_eq!(accepted_position(&effects, b), 1);
        let expected: Challenge = (
            holder,
            Assumption {
                scope: target,
                statement: "returns Some".into(),
            },
            agent("b"),
            CommitId("fork-b".into()),
        );
        assert_eq!(
            challenge_notices(&effects),
            [(agent("holder"), expected.clone())]
        );
        assert_eq!(challenge_events(&effects), [expected]);
    }

    #[test]
    fn challenges_follow_claim_id_then_declared_assumption_order() {
        let mut c = coordinator();
        let first = intent_assuming(
            "z",
            &[
                (file("src/p2.rs"), "declared first"),
                (file("src/p1.rs"), "declared second"),
            ],
        );
        let second = intent_assuming("a", &[(sym("src/p3.rs", "f"), "only one")]);
        let quiet = intent_assuming("q", &[(file("lib/far.rs"), "not touched")]);
        grant_with(
            &mut c,
            "z",
            first,
            vec![sc(file("src/p1.rs"), Mode::Depend)],
        );
        grant_with(
            &mut c,
            "a",
            second,
            vec![sc(file("src/p3.rs"), Mode::Depend)],
        );
        grant_with(&mut c, "q", quiet, vec![sc(file("lib/q.rs"), Mode::Depend)]);
        let wide = sc(dir("src"), Mode::EditBody);
        let (m, fence) = grant(&mut c, "m", vec![wide.clone()]);

        let effects = submit(&mut c, "m", m, fence, vec![wide]);
        let mut statements = Vec::new();
        for (who, (claim, assumption, by, _)) in challenge_notices(&effects) {
            assert_eq!(by, agent("m"));
            statements.push((who.0, claim.0, assumption.statement));
        }
        let expected = [
            ("z", 1, "declared first"),
            ("z", 1, "declared second"),
            ("a", 2, "only one"),
        ];
        let expected: Vec<(String, u64, String)> = expected
            .iter()
            .map(|(who, claim, text)| ((*who).into(), *claim, (*text).into()))
            .collect();
        assert_eq!(statements, expected);
        let logged_order: Vec<u64> = challenge_events(&effects).iter().map(|e| e.0 .0).collect();
        assert_eq!(logged_order, [1, 1, 2]);
    }

    #[test]
    fn shadow_claims_are_refused_when_shadow_is_disabled() {
        let mut c = coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        let before = state(&c);
        for scope in [x_edit(), y_edit()] {
            let effects = shadow_with(&mut c, "b", intent("s"), vec![scope]);
            assert_error(&effects, ErrorCode::ShadowDisabled);
            assert_eq!(state(&c), before);
        }
    }

    #[test]
    fn shadow_without_a_conflict_is_a_normal_grant() {
        let mut c = shadow_coordinator();
        let effects = shadow_with(&mut c, "a", intent("s"), vec![x_edit()]);
        assert_grant_reply(&effects, 3, ClaimId(1), Fence(1), NOW + LEASE);
        let EventKind::ClaimGranted { claim, .. } = only_event(&effects) else {
            panic!("expected ClaimGranted, got {effects:?}");
        };
        assert_eq!(*claim, ClaimId(1));
        assert_eq!(deny(&mut c, "b", vec![x_edit()]).len(), 1);
    }

    #[test]
    fn a_shadowed_claim_reports_the_conflicts_logs_them_and_takes_an_id_and_fence() {
        let mut c = shadow_coordinator();
        grant(&mut c, "a", vec![x_edit(), y_edit()]);
        let wanted = vec![y_edit(), x_edit()];
        let expected = deny(&mut c, "probe", wanted.clone());

        let effects = shadow_with(&mut c, "b", intent("s"), wanted.clone());
        let (claim, fence, expires, conflicts) = shadowed(&effects);
        assert_eq!((claim, fence, expires), (ClaimId(2), Fence(2), NOW + LEASE));
        assert_eq!(format!("{conflicts:?}"), format!("{expected:?}"));
        assert_eq!(conflicts.len(), 2);
        let EventKind::ClaimShadowed {
            agent: who,
            claim: logged_claim,
            scopes,
            conflicts: logged_conflicts,
        } = only_event(&effects)
        else {
            panic!("expected ClaimShadowed, got {effects:?}");
        };
        assert_eq!((who, *logged_claim, scopes), (&agent("b"), claim, &wanted));
        assert_eq!(format!("{logged_conflicts:?}"), format!("{expected:?}"));
        assert_eq!(kinds(&effects), ["log", "reply"]);
        assert_eq!(grant(&mut c, "d", vec![z_edit()]), (ClaimId(3), Fence(3)));
    }

    #[test]
    fn a_shadow_claim_places_no_lock_and_is_never_a_blocker() {
        let mut c = shadow_coordinator();
        let (a, a_fence) = grant(&mut c, "a", vec![x_edit()]);
        shadow_as(&mut c, "b", vec![x_edit()]);
        assert_eq!(holders_in(&c), [a.0]);

        let conflicts = deny(&mut c, "c", vec![x_edit()]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].held_by, agent("a"));

        release(&mut c, "a", a, a_fence);
        let (d, _) = grant(&mut c, "d", vec![x_edit()]);
        assert_eq!(d, ClaimId(3));
        assert_eq!(holders_in(&c), [3]);
    }

    #[test]
    fn a_shadow_claims_assumptions_are_not_at_risk_and_never_challenged() {
        let mut c = shadow_coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        let assuming = intent_assuming("s", &[(file("src/y.rs"), "y stays as it is")]);
        let effects = shadow_with(&mut c, "b", assuming, vec![x_edit()]);
        assert_eq!(shadowed(&effects).0, ClaimId(2));

        let granted = claim_as(&mut c, "e", vec![y_edit()]);
        let ServerMsg::Granted {
            claim,
            fence,
            at_risk,
            ..
        } = only_reply(&granted)
        else {
            panic!("expected Granted, got {granted:?}");
        };
        assert!(at_risk.is_empty(), "{at_risk:?}");

        let submitted = submit(&mut c, "e", *claim, *fence, vec![y_edit()]);
        assert!(challenge_notices(&submitted).is_empty());
        assert!(challenge_events(&submitted).is_empty());
        assert_eq!(accepted_position(&submitted, *claim), 1);
    }

    #[test]
    fn a_shadow_claim_does_not_count_as_holding_for_wait() {
        let mut c = shadow_coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        shadow_as(&mut c, "b", vec![x_edit()]);
        let effects = wait_for(&mut c, "b", 11, vec![x_edit()]);
        assert_queued(&effects, 11, 1);
    }

    #[test]
    fn a_queued_agent_may_not_claim_with_shadow_whatever_the_setting() {
        for mut c in [coordinator(), shadow_coordinator()] {
            grant(&mut c, "a", vec![x_edit()]);
            assert_queued(&wait_for(&mut c, "b", 11, vec![x_edit()]), 11, 1);
            let before = state(&c);
            let effects = shadow_with(&mut c, "b", intent("s"), vec![x_edit()]);
            assert_error(&effects, ErrorCode::WaitWhileHolding);
            assert_eq!(state(&c), before);
        }
    }

    #[test]
    fn a_shadow_claim_has_a_lease_that_heartbeats_renew_and_releases_with_its_fence() {
        let mut c = shadow_coordinator();
        let (a, _) = grant(&mut c, "a", vec![x_edit()]);
        let (renewed, renewed_fence) = shadow_as(&mut c, "b", vec![x_edit()]);
        let (dropped, dropped_fence) = shadow_as(&mut c, "c", vec![x_edit()]);
        assert!(heartbeat(&mut c, "b", NOW + 100).is_empty());

        let freed = release(&mut c, "c", dropped, dropped_fence);
        assert_eq!(released(&freed), [(dropped, ReleaseReason::Agent)]);
        assert!(replies(&freed).is_empty());

        let first = c.expire(NOW + LEASE);
        assert_eq!(released(&first), [(a, ReleaseReason::LeaseExpired)]);
        assert_eq!(c.next_expiry_ms(), Some(NOW + 100 + LEASE));

        let last = c.expire(NOW + 100 + LEASE);
        assert_eq!(released(&last), [(renewed, ReleaseReason::LeaseExpired)]);
        let notices = expired_notices(&last);
        assert_eq!(notices, [(agent("b"), renewed, renewed_fence)]);
        assert_eq!(c.next_expiry_ms(), None);
    }

    #[test]
    fn amending_a_shadow_claim_issues_a_fence_and_adds_scopes_without_locks_or_checks() {
        let mut c = shadow_coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        let (s, old) = shadow_as(&mut c, "b", vec![x_edit()]);
        let into_conflict = sc(file("src/x.rs"), Mode::EditSignature);
        let added = vec![y_edit(), into_conflict.clone()];
        let effects = amend(&mut c, "b", s, old, added.clone());

        let ServerMsg::Granted {
            fence,
            expires_at_ms,
            at_risk,
            ..
        } = only_reply(&effects)
        else {
            panic!("expected Granted, got {effects:?}");
        };
        assert_eq!((*fence, *expires_at_ms), (Fence(3), NOW + LEASE));
        assert!(at_risk.is_empty());
        let EventKind::ClaimAmended {
            claim,
            fence: logged,
            added: logged_added,
        } = only_event(&effects)
        else {
            panic!("expected ClaimAmended, got {effects:?}");
        };
        assert_eq!((*claim, *logged, logged_added), (s, Fence(3), &added));

        assert_error(&release(&mut c, "b", s, old), ErrorCode::StaleFence);
        assert_eq!(holders_in(&c), [1]);
        grant(&mut c, "d", vec![y_edit()]);
        let done = submit(&mut c, "b", s, Fence(3), vec![y_edit(), into_conflict]);
        assert_eq!(accepted_position(&done, s), 0);
    }

    #[test]
    fn a_shadow_submit_is_recorded_not_queued_and_leaves_real_positions_alone() {
        let mut c = shadow_coordinator();
        let assuming = intent_assuming("a", &[(file("src/x.rs"), "x is stable")]);
        grant_with(&mut c, "a", assuming, vec![x_edit()]);
        let (s, s_fence) = shadow_as(&mut c, "b", vec![x_edit()]);
        let (d, d_fence) = grant(&mut c, "d", vec![y_edit()]);
        let (e, e_fence) = grant(&mut c, "e", vec![z_edit()]);

        let first = submit(&mut c, "d", d, d_fence, vec![y_edit()]);
        assert_eq!(accepted_position(&first, d), 1);
        let shadow = submit(&mut c, "b", s, s_fence, vec![x_edit()]);
        assert_eq!(accepted_position(&shadow, s), 0);
        assert_eq!(kinds(&shadow), ["log", "reply"]);
        let EventKind::Submitted { claim, .. } = only_event(&shadow) else {
            panic!("expected Submitted, got {shadow:?}");
        };
        assert_eq!(*claim, s);
        assert!(challenge_notices(&shadow).is_empty());
        let second = submit(&mut c, "e", e, e_fence, vec![z_edit()]);
        assert_eq!(accepted_position(&second, e), 2);

        let lapsed = c.expire(NOW + 10 * LEASE);
        assert_eq!(
            released(&lapsed),
            [(ClaimId(1), ReleaseReason::LeaseExpired)]
        );
        assert_eq!(c.next_expiry_ms(), None);
    }

    #[test]
    fn a_shadow_submit_is_checked_like_any_other() {
        let mut c = shadow_coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        let (s, fence) = shadow_as(&mut c, "b", vec![x_edit()]);
        let before = state(&c);
        let stale = submit(&mut c, "b", s, Fence(fence.0 + 9), vec![x_edit()]);
        assert_submit_error(&stale, ErrorCode::StaleFence);
        let empty = submit(&mut c, "b", s, fence, vec![]);
        assert_submit_error(&empty, ErrorCode::Malformed);
        assert_eq!(state(&c), before);

        let outside = submit(&mut c, "b", s, fence, vec![y_edit()]);
        let ServerMsg::Uncovered { scopes, .. } = only_reply(&outside) else {
            panic!("expected Uncovered, got {outside:?}");
        };
        assert_eq!(scopes, &vec![y_edit()]);

        let ok = submit(&mut c, "b", s, fence, vec![x_edit()]);
        assert_eq!(accepted_position(&ok, s), 0);
        let again = submit(&mut c, "b", s, fence, vec![x_edit()]);
        assert_submit_error(&again, ErrorCode::AlreadySubmitted);
        assert_error(&release(&mut c, "b", s, fence), ErrorCode::AlreadySubmitted);
        assert_error(
            &amend(&mut c, "b", s, fence, vec![y_edit()]),
            ErrorCode::AlreadySubmitted,
        );
    }

    #[test]
    fn two_touched_scopes_threatening_one_assumption_challenge_it_once() {
        let mut c = coordinator();
        let target = sym("src/auth.rs", "refresh");
        let holder = intent_assuming("holder", &[(target, "returns Some")]);
        grant_with(
            &mut c,
            "holder",
            holder,
            vec![sc(file("src/other.rs"), Mode::Depend)],
        );
        let whole = sc(file("src/auth.rs"), Mode::EditBody);
        let (b, fence) = grant(&mut c, "b", vec![whole.clone()]);
        let touched = vec![whole, sc(sym("src/auth.rs", "refresh"), Mode::EditBody)];
        let effects = submit(&mut c, "b", b, fence, touched);
        assert_eq!(challenge_notices(&effects).len(), 1);
        assert_eq!(challenge_events(&effects).len(), 1);
    }

    #[test]
    fn a_submitted_claims_assumptions_are_still_at_risk_and_challenged() {
        let mut c = coordinator();
        let assuming = intent_assuming("a", &[(file("src/y.rs"), "y is stable")]);
        let (a, a_fence) = grant_with(
            &mut c,
            "a",
            assuming,
            vec![sc(file("src/y.rs"), Mode::Depend)],
        );
        submit(
            &mut c,
            "a",
            a,
            a_fence,
            vec![sc(file("src/y.rs"), Mode::Depend)],
        );

        let granted = claim_as(&mut c, "b", vec![y_edit()]);
        let ServerMsg::Granted {
            claim,
            fence,
            at_risk,
            ..
        } = only_reply(&granted)
        else {
            panic!("expected Granted, got {granted:?}");
        };
        assert_eq!(at_risk.len(), 1);
        assert_eq!((at_risk[0].claim, &at_risk[0].agent), (a, &agent("a")));
        let effects = submit(&mut c, "b", *claim, *fence, vec![y_edit()]);
        let notices = challenge_notices(&effects);
        assert_eq!(notices.len(), 1);
        assert_eq!((notices[0].0.clone(), notices[0].1 .0), (agent("a"), a));
    }

    fn state_value(c: &Coordinator) -> serde_json::Value {
        serde_json::to_value(c).unwrap()
    }

    #[test]
    fn only_accepted_submits_consume_an_ordinal_and_positions_follow_it() {
        let mut c = shadow_coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        let (s, s_fence) = shadow_as(&mut c, "s", vec![x_edit()]);
        let (b, b_fence) = grant(&mut c, "b", vec![y_edit()]);
        let (d, d_fence) = grant(&mut c, "d", vec![z_edit()]);
        let (e, e_fence) = grant(&mut c, "e", vec![sc(file("src/w.rs"), Mode::EditBody)]);

        assert_eq!(
            accepted_position(&submit(&mut c, "b", b, b_fence, vec![y_edit()]), b),
            1
        );
        let rejected = submit(&mut c, "d", d, d_fence, vec![x_edit()]);
        let ServerMsg::Uncovered { scopes, .. } = only_reply(&rejected) else {
            panic!("expected Uncovered, got {rejected:?}");
        };
        assert_eq!(scopes, &vec![x_edit()]);
        assert_eq!(state_value(&c)["next_submission"], 1);
        assert_eq!(
            accepted_position(&submit(&mut c, "s", s, s_fence, vec![x_edit()]), s),
            0
        );
        assert_eq!(
            accepted_position(&submit(&mut c, "d", d, d_fence, vec![z_edit()]), d),
            2
        );
        let touched = vec![sc(file("src/w.rs"), Mode::EditBody)];
        assert_eq!(
            accepted_position(&submit(&mut c, "e", e, e_fence, touched), e),
            3
        );
        let value = state_value(&c);
        assert_eq!(value["next_submission"], 4);
        assert_eq!(value["claims"]["4"]["submitted"], 2);
        assert_eq!(value["claims"]["5"]["submitted"], 3);
    }

    /// Leased, submitted and shadow claims side by side.
    fn mixed_coordinator() -> Coordinator {
        let mut c = shadow_coordinator();
        hello(&mut c, "h", "abc", PROTOCOL_VERSION);
        grant(&mut c, "a", vec![x_edit()]);
        let (b, b_fence) = grant(&mut c, "b", vec![y_edit()]);
        submit(&mut c, "b", b, b_fence, vec![y_edit()]);
        shadow_as(&mut c, "c", vec![x_edit()]);
        let (d, d_fence) = shadow_as(&mut c, "d", vec![x_edit()]);
        submit(&mut c, "d", d, d_fence, vec![x_edit()]);
        c
    }

    #[test]
    fn rebuilding_from_state_places_locks_only_for_real_claims_and_behaves_identically() {
        let json = state(&mixed_coordinator());
        assert_eq!(json, state(&mixed_coordinator()));
        let mut live = mixed_coordinator();
        let mut restored: Coordinator = serde_json::from_str(&json).unwrap();
        assert_eq!(state(&restored), json);
        assert_eq!(holders_in(&restored), [1, 2]);

        let release_b = ClientMsg::Release {
            claim: ClaimId(2),
            fence: Fence(2),
        };
        let script = [
            ("e", claim_msg(intent("e"), vec![x_edit()]), NOW),
            ("c", ClientMsg::Heartbeat, NOW + 10),
            ("b", release_b, NOW + 20),
            ("f", claim_msg(intent("f"), vec![y_edit()]), NOW + 30),
            ("zz", ClientMsg::Heartbeat, NOW + 3 * LEASE),
            ("e", claim_msg(intent("e"), vec![x_edit()]), NOW + 3 * LEASE),
        ];
        for (who, msg, now) in script {
            let from_live = handle_at(&mut live, who, msg.clone(), now);
            let from_restored = handle_at(&mut restored, who, msg, now);
            assert_eq!(format!("{from_live:?}"), format!("{from_restored:?}"));
            assert_eq!(state(&live), state(&restored));
            assert_eq!(lock_dump(&live.locks), lock_dump(&restored.locks));
        }
        assert_eq!(holders_in(&restored), [2, 5]);
    }

    #[test]
    fn state_saved_before_submit_and_shadow_existed_still_loads() {
        let mut c = coordinator();
        grant(&mut c, "a", vec![x_edit()]);
        grant(&mut c, "b", vec![y_edit()]);
        let mut value = serde_json::to_value(&c).unwrap();
        let Some(object) = value.as_object_mut() else {
            panic!("state is not an object");
        };
        assert!(object.remove("next_submission").is_some());
        let Some(config) = object.get_mut("config").and_then(|v| v.as_object_mut()) else {
            panic!("no config");
        };
        assert!(config.remove("shadow_enabled").is_some());
        let Some(claims) = object.get_mut("claims").and_then(|v| v.as_object_mut()) else {
            panic!("no claims");
        };
        for (_, claim) in claims.iter_mut() {
            let Some(fields) = claim.as_object_mut() else {
                panic!("claim is not an object");
            };
            assert!(fields.remove("shadow").is_some());
            assert!(fields.remove("submitted").is_some());
        }
        let mut old: Coordinator = serde_json::from_value(value).unwrap();
        assert_eq!(state(&old), state(&c));
        let (a, a_fence) = (ClaimId(1), Fence(1));
        assert_eq!(
            accepted_position(&submit(&mut old, "a", a, a_fence, vec![x_edit()]), a),
            1
        );
        assert_error(
            &shadow_with(&mut old, "c", intent("s"), vec![x_edit()]),
            ErrorCode::ShadowDisabled,
        );
    }

    #[test]
    fn event_seq_stays_gapless_across_submits_challenges_and_shadows() {
        let mut c = shadow_coordinator();
        let mut all = Vec::new();
        all.extend(hello(&mut c, "h", "abc", PROTOCOL_VERSION));
        let assuming = intent_assuming("a", &[(file("src/y.rs"), "y is stable")]);
        all.extend(claim_with(
            &mut c,
            "a",
            assuming,
            vec![sc(file("src/y.rs"), Mode::Depend)],
        ));
        all.extend(claim_as(&mut c, "b", vec![x_edit(), y_edit()]));
        all.extend(shadow_with(&mut c, "e", intent("s"), vec![x_edit()]));
        all.extend(submit(&mut c, "b", ClaimId(2), Fence(2), vec![z_edit()]));
        all.extend(submit(&mut c, "b", ClaimId(2), Fence(2), vec![]));
        all.extend(submit(
            &mut c,
            "b",
            ClaimId(2),
            Fence(2),
            vec![x_edit(), y_edit()],
        ));
        all.extend(submit(&mut c, "e", ClaimId(3), Fence(3), vec![x_edit()]));
        all.extend(amend(&mut c, "a", ClaimId(1), Fence(1), vec![z_edit()]));
        all.extend(c.expire(NOW + 10 * LEASE));

        let seqs: Vec<u64> = logged(&all).iter().map(|event| event.seq).collect();
        assert!(seqs.len() > 9, "{seqs:?}");
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<u64>>());
    }

    // ---- property: decisions, leases, the queue and amends match a brute-force model ----

    fn universe() -> Vec<Scope> {
        vec![
            dir(""),
            dir("d1"),
            dir("d1/d2"),
            dir("d3"),
            file("f0.rs"),
            file("d1/f1.rs"),
            file("d1/d2/f2.rs"),
            file("d3/f3.rs"),
            sym("d1/f1.rs", "s1"),
            sym("d1/f1.rs", "s2"),
            sym("d1/d2/f2.rs", "s3"),
            sym("d3/f3.rs", "s4"),
        ]
    }

    #[derive(Debug, Clone)]
    enum Op {
        Claim {
            agent: u8,
            scopes: Vec<ScopeClaim>,
        },
        Wait {
            agent: u8,
            scopes: Vec<ScopeClaim>,
        },
        Release {
            pick: usize,
        },
        Amend {
            pick: usize,
            scopes: Vec<ScopeClaim>,
        },
        /// Move the clock. `eager` also runs `expire` at the new time; otherwise the expiry is
        /// left for the next message to trigger lazily.
        Advance {
            ms: u64,
            eager: bool,
        },
        Heartbeat {
            agent: u8,
        },
        /// Submit one claim. `own` touches exactly what the claim holds, which is always covered;
        /// otherwise `touched` is arbitrary and usually not.
        Submit {
            pick: usize,
            own: bool,
            touched: Vec<ScopeClaim>,
        },
    }

    fn scope_claims() -> impl Strategy<Value = Vec<ScopeClaim>> {
        let one = (
            prop::sample::select(universe()),
            prop::sample::select(Mode::ALL.to_vec()),
        )
            .prop_map(|(scope, mode)| ScopeClaim { scope, mode });
        prop::collection::vec(one, 1..=3)
    }

    fn ops() -> impl Strategy<Value = Vec<Op>> {
        let jumps = vec![0, 1, LEASE / 2, LEASE - 1, LEASE, LEASE + 1];
        let op = prop_oneof![
            4 => (0u8..4, scope_claims()).prop_map(|(agent, scopes)| Op::Claim { agent, scopes }),
            3 => (0u8..4, scope_claims()).prop_map(|(agent, scopes)| Op::Wait { agent, scopes }),
            2 => any::<usize>().prop_map(|pick| Op::Release { pick }),
            2 => (any::<usize>(), scope_claims())
                .prop_map(|(pick, scopes)| Op::Amend { pick, scopes }),
            2 => (prop::sample::select(jumps), any::<bool>())
                .prop_map(|(ms, eager)| Op::Advance { ms, eager }),
            1 => (0u8..4).prop_map(|agent| Op::Heartbeat { agent }),
            3 => (any::<usize>(), any::<bool>(), scope_claims())
                .prop_map(|(pick, own, touched)| Op::Submit { pick, own, touched }),
        ];
        prop::collection::vec(op, 1..60)
    }

    struct Active {
        agent: AgentId,
        claim: ClaimId,
        fence: Fence,
        scopes: Vec<ScopeClaim>,
        /// Tracked here, independently of the coordinator.
        expires_at: u64,
        /// Submitted claims never expire, are not renewed, and keep blocking.
        submitted: bool,
        /// Fences an amend retired, which must never be accepted again.
        retired: Vec<Fence>,
    }

    struct Queued {
        agent: AgentId,
        req: u64,
        scopes: Vec<ScopeClaim>,
    }

    type Pair = (ScopeClaim, ScopeClaim, AgentId);

    /// Independent of the production helper: keeps first occurrences in order.
    fn distinct(scopes: &[ScopeClaim]) -> Vec<ScopeClaim> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for scope in scopes {
            if seen.insert(scope.clone()) {
                out.push(scope.clone());
            }
        }
        out
    }

    /// Two scope claims clash iff one scope covers the other and the modes conflict.
    fn clash(mine: &ScopeClaim, theirs: &ScopeClaim) -> bool {
        let overlap = mine.scope.covers(&theirs.scope) || theirs.scope.covers(&mine.scope);
        overlap && mine.mode.conflicts_with(theirs.mode)
    }

    /// One entry per (requested scope, blocking claim, held scope), in the documented order:
    /// request index, then claim id (`active` is in grant order), then held index. `scopes`
    /// must already be free of repeats.
    fn oracle(active: &[Active], who: &AgentId, scopes: &[ScopeClaim]) -> Vec<Pair> {
        let mut out = Vec::new();
        for mine in scopes {
            for other in active.iter().filter(|a| a.agent != *who) {
                for theirs in &other.scopes {
                    if clash(mine, theirs) {
                        out.push((mine.clone(), theirs.clone(), other.agent.clone()));
                    }
                }
            }
        }
        out
    }

    /// What one coordinator call must announce besides its own reply.
    #[derive(Default)]
    struct Announced {
        expired: Vec<(AgentId, ClaimId, Fence)>,
        granted: Vec<GrantNotice>,
    }

    /// The whole expected state, kept independently of the coordinator. It predicts claim ids and
    /// fences with its own counters: only real grants and successful amends consume one.
    struct Model {
        active: Vec<Active>,
        queue: Vec<Queued>,
        next_claim: u64,
        next_fence: u64,
        next_req: u64,
        now: u64,
    }

    impl Model {
        fn new() -> Self {
            Self {
                active: Vec::new(),
                queue: Vec::new(),
                next_claim: 1,
                next_fence: 1,
                next_req: 100,
                now: NOW,
            }
        }

        fn grant(&mut self, who: &AgentId, scopes: Vec<ScopeClaim>) -> (ClaimId, Fence) {
            let (claim, fence) = (ClaimId(self.next_claim), Fence(self.next_fence));
            self.next_claim += 1;
            self.next_fence += 1;
            let expires_at = self.now + LEASE;
            self.active.push(Active {
                agent: who.clone(),
                claim,
                fence,
                scopes,
                expires_at,
                submitted: false,
                retired: Vec::new(),
            });
            (claim, fence)
        }

        fn holds(&self, who: &AgentId) -> bool {
            self.active.iter().any(|a| a.agent == *who)
        }

        fn is_queued(&self, who: &AgentId) -> bool {
            self.queue.iter().any(|w| w.agent == *who)
        }

        /// Submitted claims are never removed in this model, so the latest submission's queue
        /// position is their count.
        fn submitted_count(&self) -> u32 {
            let mut count = 0;
            for a in &self.active {
                if a.submitted {
                    count += 1;
                }
            }
            count
        }

        /// Everything the coordinator does at the start of every call at `self.now`.
        fn lapse(&mut self) -> Announced {
            let mut announced = Announced::default();
            let mut kept = Vec::new();
            for a in self.active.drain(..) {
                if !a.submitted && a.expires_at <= self.now {
                    announced.expired.push((a.agent, a.claim, a.fence));
                } else {
                    kept.push(a);
                }
            }
            self.active = kept;
            if !announced.expired.is_empty() {
                announced.granted = self.walk();
            }
            announced
        }

        /// One FIFO pass: a waiter is granted iff it clashes with no active claim of another
        /// agent and with no earlier request still waiting.
        fn walk(&mut self) -> Vec<GrantNotice> {
            let mut granted = Vec::new();
            let mut waiting: Vec<Queued> = Vec::new();
            for w in std::mem::take(&mut self.queue) {
                let by_active = !oracle(&self.active, &w.agent, &w.scopes).is_empty();
                let by_earlier = waiting.iter().any(|e| {
                    e.agent != w.agent
                        && e.scopes
                            .iter()
                            .any(|x| w.scopes.iter().any(|y| clash(x, y)))
                });
                if by_active || by_earlier {
                    waiting.push(w);
                    continue;
                }
                let (claim, fence) = self.grant(&w.agent, w.scopes);
                granted.push((w.agent, w.req, claim, fence, self.now + LEASE));
            }
            self.queue = waiting;
            granted
        }
    }

    fn who_is(n: u8) -> AgentId {
        agent(&format!("agent-{n}"))
    }

    fn assert_announced(effects: &[Effect], expired: &Announced, extra: &[GrantNotice]) {
        let mut granted = expired.granted.clone();
        granted.extend(extra.iter().cloned());
        assert_eq!(expired_notices(effects), expired.expired);
        assert_eq!(granted_notices(effects), granted);
    }

    fn assert_refused(effects: &[Effect], expected: ErrorCode) {
        let ServerMsg::Error { code, .. } = only_reply(effects) else {
            panic!("expected Error({expected:?}), got {effects:?}");
        };
        assert_eq!(*code, expected);
    }

    fn assert_grant_reply(effects: &[Effect], req: u64, claim: ClaimId, fence: Fence, exp: u64) {
        let ServerMsg::Granted {
            req: got_req,
            claim: got_claim,
            fence: got_fence,
            expires_at_ms,
            ..
        } = only_reply(effects)
        else {
            panic!("expected Granted, got {effects:?}");
        };
        assert_eq!((got_req.0, *got_claim, *got_fence), (req, claim, fence));
        assert_eq!(*expires_at_ms, exp);
    }

    fn assert_denied_as(effects: &[Effect], expected: Vec<Pair>) {
        let ServerMsg::Denied { conflicts, .. } = only_reply(effects) else {
            panic!("expected Denied, got {effects:?}");
        };
        let mut got: Vec<Pair> = Vec::new();
        for x in conflicts {
            got.push((x.requested.clone(), x.held.clone(), x.held_by.clone()));
        }
        assert_eq!(got, expected);
    }

    /// Applies `Claim` (Fail policy) or `Wait` and checks the answer.
    fn step_claim(c: &mut Coordinator, m: &mut Model, n: u8, raw: Vec<ScopeClaim>, wait: bool) {
        let who = who_is(n);
        let lapse = m.lapse();
        let req = if wait { m.next_req } else { 1 };
        let msg = if wait {
            wait_msg(req, raw.clone())
        } else {
            claim_msg(intent("p"), raw.clone())
        };
        let effects = c.handle(&who, msg, m.now);
        let scopes = distinct(&raw);
        let blockers = oracle(&m.active, &who, &scopes);
        if m.is_queued(&who) {
            assert_refused(&effects, ErrorCode::WaitWhileHolding);
        } else if blockers.is_empty() {
            let (claim, fence) = m.grant(&who, scopes);
            assert_grant_reply(&effects, req, claim, fence, m.now + LEASE);
        } else if !wait {
            assert_denied_as(&effects, blockers);
        } else if m.holds(&who) {
            assert_refused(&effects, ErrorCode::WaitWhileHolding);
        } else {
            m.next_req += 1;
            m.queue.push(Queued {
                agent: who,
                req,
                scopes,
            });
            let ServerMsg::Queued { req: got, position } = only_reply(&effects) else {
                panic!("expected Queued, got {effects:?}");
            };
            assert_eq!((got.0, *position as usize), (req, m.queue.len()));
        }
        assert_announced(&effects, &lapse, &[]);
    }

    fn step_release(c: &mut Coordinator, m: &mut Model, pick: usize) {
        let lapse = m.lapse();
        if m.active.is_empty() {
            assert_announced(&c.expire(m.now), &lapse, &[]);
            return;
        }
        let idx = pick % m.active.len();
        if m.active[idx].submitted {
            let held = &m.active[idx];
            let msg = ClientMsg::Release {
                claim: held.claim,
                fence: held.fence,
            };
            let effects = c.handle(&held.agent, msg, m.now);
            assert_refused(&effects, ErrorCode::AlreadySubmitted);
            assert_announced(&effects, &lapse, &[]);
            return;
        }
        let gone = m.active.remove(idx);
        let granted = m.walk();
        let msg = ClientMsg::Release {
            claim: gone.claim,
            fence: gone.fence,
        };
        let effects = c.handle(&gone.agent, msg, m.now);
        assert!(replies(&effects).is_empty(), "release failed: {effects:?}");
        assert_announced(&effects, &lapse, &granted);
    }

    fn step_amend(c: &mut Coordinator, m: &mut Model, pick: usize, raw: Vec<ScopeClaim>) {
        let lapse = m.lapse();
        if m.active.is_empty() {
            assert_announced(&c.expire(m.now), &lapse, &[]);
            return;
        }
        let idx = pick % m.active.len();
        let (who, claim, old) = {
            let a = &m.active[idx];
            (a.agent.clone(), a.claim, a.fence)
        };
        let mut added = distinct(&raw);
        added.retain(|s| !m.active[idx].scopes.contains(s));
        let blockers = oracle(&m.active, &who, &added);
        let msg = ClientMsg::Amend {
            req: RequestId(900),
            claim,
            fence: old,
            add: raw,
        };
        let effects = c.handle(&who, msg, m.now);
        assert_announced(&effects, &lapse, &[]);
        if m.active[idx].submitted {
            assert_refused(&effects, ErrorCode::AlreadySubmitted);
            return;
        }
        if !blockers.is_empty() {
            assert_denied_as(&effects, blockers);
            return;
        }
        let new = Fence(m.next_fence);
        m.next_fence += 1;
        let expires_at = m.active[idx].expires_at;
        assert_grant_reply(&effects, 900, claim, new, expires_at);
        m.active[idx].fence = new;
        m.active[idx].retired.push(old);
        m.active[idx].scopes.extend(added);
        let stale = c.handle(&who, ClientMsg::Release { claim, fence: old }, m.now);
        assert_refused(&stale, ErrorCode::StaleFence);
    }

    /// The touched scopes no claimed scope covers in a permitting mode, in touched order.
    fn missing(claimed: &[ScopeClaim], touched: &[ScopeClaim]) -> Vec<ScopeClaim> {
        let mut out = Vec::new();
        for t in touched {
            let covered = claimed
                .iter()
                .any(|h| h.scope.covers(&t.scope) && h.mode.permits(t.mode));
            if !covered {
                out.push(t.clone());
            }
        }
        out
    }

    fn without_seq_of(value: &serde_json::Value) -> serde_json::Value {
        let mut value = value.clone();
        if let Some(object) = value.as_object_mut() {
            object.remove("next_seq");
        }
        value
    }

    fn without_seq(c: &Coordinator) -> serde_json::Value {
        without_seq_of(&state_value(c))
    }

    fn step_submit(
        c: &mut Coordinator,
        m: &mut Model,
        pick: usize,
        own: bool,
        raw: Vec<ScopeClaim>,
    ) {
        let lapse = m.lapse();
        if m.active.is_empty() {
            assert_announced(&c.expire(m.now), &lapse, &[]);
            return;
        }
        let idx = pick % m.active.len();
        let (who, claim, fence) = {
            let a = &m.active[idx];
            (a.agent.clone(), a.claim, a.fence)
        };
        let touched = if own {
            m.active[idx].scopes.clone()
        } else {
            distinct(&raw)
        };
        let stale = m.active[idx]
            .retired
            .first()
            .copied()
            .unwrap_or(Fence(fence.0 + 1));
        let first = c.handle(&who, submit_msg(claim, stale, touched.clone()), m.now);
        assert_refused(&first, ErrorCode::StaleFence);
        assert_announced(&first, &lapse, &[]);
        let settled = state_value(c);
        let second = c.handle(&who, submit_msg(claim, stale, touched.clone()), m.now);
        assert_refused(&second, ErrorCode::StaleFence);
        assert_eq!(
            state_value(c),
            settled,
            "a stale submit must change nothing"
        );

        let effects = c.handle(&who, submit_msg(claim, fence, touched.clone()), m.now);
        assert_announced(&effects, &Announced::default(), &[]);
        if m.active[idx].submitted {
            assert_refused(&effects, ErrorCode::AlreadySubmitted);
            return;
        }
        let expected = missing(&m.active[idx].scopes, &touched);
        if !expected.is_empty() {
            let ServerMsg::Uncovered { claim: got, scopes } = only_reply(&effects) else {
                panic!("expected Uncovered, got {effects:?}");
            };
            assert_eq!((*got, scopes), (claim, &expected));
            assert_eq!(
                without_seq(c),
                without_seq_of(&settled),
                "uncovered changes only seq"
            );
            return;
        }
        m.active[idx].submitted = true;
        assert_eq!(accepted_position(&effects, claim), m.submitted_count());
        let again = c.handle(&who, submit_msg(claim, fence, touched), m.now);
        assert_refused(&again, ErrorCode::AlreadySubmitted);
    }

    fn step_time(c: &mut Coordinator, m: &mut Model, op: &Op) -> bool {
        match op {
            Op::Advance { ms, eager } => {
                m.now += ms;
                if *eager {
                    let lapse = m.lapse();
                    assert_announced(&c.expire(m.now), &lapse, &[]);
                }
                *eager
            }
            Op::Heartbeat { agent: n } => {
                let who = who_is(*n);
                let lapse = m.lapse();
                let effects = c.handle(&who, ClientMsg::Heartbeat, m.now);
                assert!(replies(&effects).is_empty(), "{effects:?}");
                for a in m
                    .active
                    .iter_mut()
                    .filter(|a| a.agent == who && !a.submitted)
                {
                    a.expires_at = m.now + LEASE;
                }
                assert_announced(&effects, &lapse, &[]);
                true
            }
            Op::Claim { .. }
            | Op::Wait { .. }
            | Op::Release { .. }
            | Op::Amend { .. }
            | Op::Submit { .. } => {
                unreachable!("not a clock operation: {op:?}")
            }
        }
    }

    /// Runs one operation; false if the coordinator was not called (a lazy clock jump).
    fn step(c: &mut Coordinator, m: &mut Model, op: Op) -> bool {
        match op {
            Op::Claim { agent, scopes } => step_claim(c, m, agent, scopes, false),
            Op::Wait { agent, scopes } => step_claim(c, m, agent, scopes, true),
            Op::Release { pick } => step_release(c, m, pick),
            Op::Amend { pick, scopes } => step_amend(c, m, pick, scopes),
            Op::Submit { pick, own, touched } => step_submit(c, m, pick, own, touched),
            Op::Advance { .. } | Op::Heartbeat { .. } => return step_time(c, m, &op),
        }
        true
    }

    /// The lock table as a sorted list of (node, claim id, slot, lock).
    fn lock_dump(locks: &LockTable) -> Vec<(String, u64, usize, String)> {
        let mut out = Vec::new();
        for (node, holders) in locks {
            for h in holders {
                out.push((
                    format!("{node:?}"),
                    h.claim.0,
                    h.slot,
                    format!("{:?}", h.lock),
                ));
            }
        }
        out.sort();
        out
    }

    /// The live lock table must equal the one rebuilt from the claims.
    fn assert_locks_match_claims(c: &Coordinator) {
        let rebuilt = Coordinator::from(c.state.clone());
        assert_eq!(lock_dump(&c.locks), lock_dump(&rebuilt.locks));
    }

    fn assert_matches_model(c: &Coordinator, m: &Model) {
        assert_eq!(
            c.next_expiry_ms(),
            m.active
                .iter()
                .filter(|a| !a.submitted)
                .map(|a| a.expires_at)
                .min()
        );
        let mut queued = Vec::new();
        for w in &c.state.waiting {
            queued.push((w.agent.clone(), w.request.req.0));
        }
        let expected: Vec<(AgentId, u64)> =
            m.queue.iter().map(|w| (w.agent.clone(), w.req)).collect();
        assert_eq!(queued, expected);
    }

    proptest! {
        #[test]
        fn coordinator_matches_brute_force_model(sequence in ops()) {
            let mut c = coordinator();
            let mut m = Model::new();
            for op in sequence {
                // Every reachable state must survive a save and load, so run on the restored one.
                c = serde_json::from_str(&state(&c)).unwrap();
                if step(&mut c, &mut m, op) {
                    assert_matches_model(&c, &m);
                }
                assert_locks_match_claims(&c);
            }
        }
    }
}

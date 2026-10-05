//! Pure coordinator core: a deterministic, in-memory state machine for claims.
//!
//! No clock, no randomness, no `worker` dependency: the caller passes `now_ms` and delivers the
//! returned `Effect`s, so the whole thing is tested natively. The Durable Object wraps it.
//!
//! Decisions made here, beyond the protocol invariants:
//! - An agent's own claims never conflict with each other.
//! - Claim ids and fences start at 1 and are never reused. Event `seq` starts at 0.
//! - `Hello` does not have to precede other messages; connection state belongs to the caller.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into the Durable Object in COORD-4")
)]

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, Conflict, ErrorCode, Event, EventKind, Fence,
    HeldAssumption, Intent, Lock, OnConflict, ReleaseReason, RequestId, RunId, Scope, ScopeClaim,
    ServerMsg, PROTOCOL_VERSION,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub run: RunId,
    pub lease_ms: u64,
}

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
    #[cfg_attr(
        test,
        expect(dead_code, reason = "first constructed in COORD-2 (expiry notices)")
    )]
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
}

/// One lock on one node, with enough to report a conflict without scanning all claims.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Holder {
    claim: ClaimId,
    agent: AgentId,
    /// The claimed scope that placed this lock.
    held: ScopeClaim,
    lock: Lock,
}

type LockTable = HashMap<Scope, Vec<Holder>>;

/// JSON object keys must be strings, so the lock table is stored as a list of pairs.
mod lock_table_pairs {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::{Holder, LockTable};
    use crate::protocol::Scope;

    pub fn serialize<S: Serializer>(table: &LockTable, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(table.iter())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<LockTable, D::Error> {
        let pairs = Vec::<(Scope, Vec<Holder>)>::deserialize(deserializer)?;
        Ok(pairs.into_iter().collect())
    }
}

/// A `Claim` message minus the conflict policy.
struct ClaimRequest {
    req: RequestId,
    intent: Intent,
    scopes: Vec<ScopeClaim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coordinator {
    config: Config,
    /// Main's head as the coordinator knows it. The steward will own this later.
    head: Option<CommitId>,
    next_claim: u64,
    next_fence: u64,
    next_seq: u64,
    claims: HashMap<ClaimId, ActiveClaim>,
    #[serde(with = "lock_table_pairs")]
    locks: LockTable,
}

impl Coordinator {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            head: None,
            next_claim: 1,
            next_fence: 1,
            next_seq: 0,
            claims: HashMap::new(),
            locks: HashMap::new(),
        }
    }

    /// Apply one client message. `agent` is the sender, except for `Hello`, which logs the agent
    /// it declares (callers should pass the same value).
    pub fn handle(&mut self, agent: &AgentId, msg: ClientMsg, now_ms: u64) -> Vec<Effect> {
        match msg {
            ClientMsg::Hello {
                agent: declared,
                base,
                protocol,
            } => self.hello(declared, base, protocol, now_ms),
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
            ClientMsg::Amend { req, .. } => not_implemented(Some(req), "Amend"),
            ClientMsg::Heartbeat => not_implemented(None, "Heartbeat"),
            ClientMsg::Submit { req, .. } => not_implemented(Some(req), "Submit"),
            ClientMsg::OpenRace { req, .. } => not_implemented(Some(req), "OpenRace"),
            ClientMsg::JoinRace { req, .. } => not_implemented(Some(req), "JoinRace"),
            ClientMsg::PickWinner { req, .. } => not_implemented(Some(req), "PickWinner"),
            ClientMsg::Review { req, .. } => not_implemented(Some(req), "Review"),
            ClientMsg::Watch { .. } => not_implemented(None, "Watch"),
        }
    }

    fn hello(&mut self, agent: AgentId, base: CommitId, protocol: u16, now_ms: u64) -> Vec<Effect> {
        if protocol != PROTOCOL_VERSION {
            let message = format!(
                "client speaks protocol v{protocol}, coordinator speaks v{PROTOCOL_VERSION}"
            );
            return vec![error(None, ErrorCode::UnsupportedProtocol, message)];
        }
        let head = self.head.get_or_insert(base).clone();
        let connected = self.event(now_ms, EventKind::AgentConnected { agent });
        let welcome = ServerMsg::Welcome {
            head,
            lease_ms: self.config.lease_ms,
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
        match on_conflict {
            OnConflict::Fail => {}
            OnConflict::Wait => {
                return not_implemented(Some(request.req), "Claim with OnConflict::Wait");
            }
            OnConflict::Shadow => {
                return not_implemented(Some(request.req), "Claim with OnConflict::Shadow");
            }
        }
        let conflicts = self.find_conflicts(agent, &request.scopes);
        if conflicts.is_empty() {
            self.grant(agent, request, now_ms)
        } else {
            self.deny(agent, request, conflicts, now_ms)
        }
    }

    /// Every active claim of another agent that blocks one of `scopes`. Looks only at the nodes
    /// the requested scopes lock, so the cost is depth times holders at those nodes.
    fn find_conflicts(&self, agent: &AgentId, scopes: &[ScopeClaim]) -> Vec<Conflict> {
        let mut out = Vec::new();
        let mut seen: HashSet<(ScopeClaim, ClaimId, ScopeClaim)> = HashSet::new();
        for requested in scopes {
            for (node, lock) in requested.locks() {
                let Some(holders) = self.locks.get(&node) else {
                    continue;
                };
                for holder in holders {
                    if holder.agent == *agent || !lock.conflicts_with(holder.lock) {
                        continue;
                    }
                    let key = (requested.clone(), holder.claim, holder.held.clone());
                    if !seen.insert(key) {
                        continue;
                    }
                    debug_assert!(
                        self.claims.contains_key(&holder.claim),
                        "lock without claim"
                    );
                    let Some(blocker) = self.claims.get(&holder.claim) else {
                        continue;
                    };
                    out.push(Conflict {
                        requested: requested.clone(),
                        held: holder.held.clone(),
                        held_by: holder.agent.clone(),
                        their_intent: blocker.intent.clone(),
                        race: None,
                    });
                }
            }
        }
        out
    }

    /// Assumptions in other agents' active claims that any of `scopes` could break (invariant 8).
    fn assumptions_at_risk(&self, agent: &AgentId, scopes: &[ScopeClaim]) -> Vec<HeldAssumption> {
        let mut others: Vec<(&ClaimId, &ActiveClaim)> = self.claims.iter().collect();
        others.sort_by_key(|(id, _)| id.0);
        let mut out = Vec::new();
        for (id, other) in others {
            if other.agent == *agent {
                continue;
            }
            for assumption in &other.intent.assumptions {
                if scopes.iter().any(|scope| assumption.threatened_by(scope)) {
                    out.push(HeldAssumption {
                        agent: other.agent.clone(),
                        claim: *id,
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

    fn grant(&mut self, agent: &AgentId, request: ClaimRequest, now_ms: u64) -> Vec<Effect> {
        let at_risk = self.assumptions_at_risk(agent, &request.scopes);
        let claim = ClaimId(self.next_claim);
        self.next_claim += 1;
        let fence = Fence(self.next_fence);
        self.next_fence += 1;

        self.place_locks(claim, agent, &request.scopes);
        let granted = self.event(
            now_ms,
            EventKind::ClaimGranted {
                agent: agent.clone(),
                claim,
                fence,
                scopes: request.scopes.clone(),
                intent: request.intent.clone(),
                race: None,
                at_risk: at_risk.clone(),
            },
        );
        self.claims.insert(
            claim,
            ActiveClaim {
                agent: agent.clone(),
                fence,
                intent: request.intent,
                scopes: request.scopes,
            },
        );
        let reply = ServerMsg::Granted {
            req: request.req,
            claim,
            fence,
            expires_at_ms: now_ms.saturating_add(self.config.lease_ms),
            race: None,
            at_risk,
        };
        vec![granted, Effect::Reply(reply)]
    }

    fn release(
        &mut self,
        agent: &AgentId,
        claim: ClaimId,
        fence: Fence,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Entry::Occupied(entry) = self.claims.entry(claim) else {
            let message = format!("claim {} is not active", claim.0);
            return vec![error(None, ErrorCode::UnknownClaim, message)];
        };
        let held = entry.get();
        if held.agent != *agent {
            let message = format!("claim {} belongs to another agent", claim.0);
            return vec![error(None, ErrorCode::NotOwner, message)];
        }
        if held.fence != fence {
            let message = format!(
                "claim {} is at fence {}, release presented fence {}",
                claim.0, held.fence.0, fence.0
            );
            return vec![error(None, ErrorCode::StaleFence, message)];
        }
        let released = entry.remove();
        self.remove_locks(claim, &released.scopes);
        let reason = ReleaseReason::Agent;
        vec![self.event(now_ms, EventKind::ClaimReleased { claim, reason })]
    }

    fn place_locks(&mut self, claim: ClaimId, agent: &AgentId, scopes: &[ScopeClaim]) {
        for held in scopes {
            for (node, lock) in held.locks() {
                let holder = Holder {
                    claim,
                    agent: agent.clone(),
                    held: held.clone(),
                    lock,
                };
                self.locks.entry(node).or_default().push(holder);
            }
        }
    }

    /// Removes the claim's locks and prunes nodes left empty.
    fn remove_locks(&mut self, claim: ClaimId, scopes: &[ScopeClaim]) {
        for held in scopes {
            for (node, _) in held.locks() {
                // A node shared by two of this claim's scopes is already pruned the second time.
                let Entry::Occupied(mut slot) = self.locks.entry(node) else {
                    continue;
                };
                slot.get_mut().retain(|holder| holder.claim != claim);
                if slot.get().is_empty() {
                    slot.remove();
                }
            }
        }
    }

    fn event(&mut self, at_ms: u64, kind: EventKind) -> Effect {
        let seq = self.next_seq;
        self.next_seq += 1;
        Effect::Log(Event {
            seq,
            at_ms,
            run: self.config.run.clone(),
            kind,
        })
    }
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::*;
    use crate::protocol::{Assumption, CommitId, DecisionRecord, Mode, RaceId, SymbolId};

    const NOW: u64 = 1_000;
    const LEASE: u64 = 30_000;

    fn coordinator() -> Coordinator {
        Coordinator::new(Config {
            run: RunId("test".into()),
            lease_ms: LEASE,
        })
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
        match only_reply(&effects) {
            ServerMsg::Granted { claim, fence, .. } => (*claim, *fence),
            other => panic!("expected Granted, got {other:?}"),
        }
    }

    fn grant(c: &mut Coordinator, who: &str, scopes: Vec<ScopeClaim>) -> (ClaimId, Fence) {
        grant_with(c, who, intent("test work"), scopes)
    }

    fn deny(c: &mut Coordinator, who: &str, scopes: Vec<ScopeClaim>) -> Vec<Conflict> {
        let effects = claim_as(c, who, scopes);
        match only_reply(&effects) {
            ServerMsg::Denied { conflicts, .. } => conflicts.clone(),
            other => panic!("expected Denied, got {other:?}"),
        }
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
        match only_reply(effects) {
            ServerMsg::Error { code, .. } => assert_eq!(*code, expected),
            other => panic!("expected Error({expected:?}), got {other:?}"),
        }
        assert!(
            logged(effects).is_empty(),
            "errors must not log: {effects:?}"
        );
    }

    #[test]
    fn hello_welcomes_with_head_lease_and_protocol_and_logs_connection() {
        let mut c = coordinator();
        let effects = hello(&mut c, "a1", "abc", PROTOCOL_VERSION);
        match only_reply(&effects) {
            ServerMsg::Welcome {
                head,
                lease_ms,
                protocol,
            } => {
                assert_eq!(head, &CommitId("abc".into()));
                assert_eq!(*lease_ms, LEASE);
                assert_eq!(*protocol, PROTOCOL_VERSION);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
        let events = logged(&effects);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[0].at_ms, NOW);
        assert_eq!(events[0].run, RunId("test".into()));
        assert!(
            matches!(&events[0].kind, EventKind::AgentConnected { agent: a } if a == &agent("a1"))
        );
    }

    #[test]
    fn hello_keeps_the_first_head_it_adopted() {
        let mut c = coordinator();
        hello(&mut c, "a1", "first", PROTOCOL_VERSION);
        let effects = hello(&mut c, "a2", "second", PROTOCOL_VERSION);
        match only_reply(&effects) {
            ServerMsg::Welcome { head, .. } => assert_eq!(head, &CommitId("first".into())),
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    #[test]
    fn hello_with_unsupported_protocol_is_refused_and_logs_nothing() {
        let mut c = coordinator();
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
        match only_reply(&effects) {
            ServerMsg::Granted {
                req,
                expires_at_ms,
                race,
                at_risk,
                ..
            } => {
                assert_eq!(*req, RequestId(1));
                assert_eq!(*expires_at_ms, NOW + LEASE);
                assert_eq!(*race, None);
                assert!(at_risk.is_empty());
            }
            other => panic!("expected Granted, got {other:?}"),
        }
        let events = logged(&effects);
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            EventKind::ClaimGranted {
                agent: who,
                scopes,
                intent,
                ..
            } => {
                assert_eq!(who, &agent("a"));
                assert_eq!(scopes, &vec![sc(sym("src/a.rs", "f"), Mode::EditBody)]);
                assert_eq!(intent.summary, "fix refresh");
            }
            other => panic!("expected ClaimGranted, got {other:?}"),
        }
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
        let events = logged(&effects);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0].kind, EventKind::ClaimDenied { agent: a, .. } if a == &agent("b"))
        );
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
        let events = logged(&effects);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].kind,
            EventKind::ClaimReleased { claim: released, reason: ReleaseReason::Agent }
                if *released == claim
        ));

        grant(&mut c, "b", scopes);
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
        let events = logged(&effects);
        assert!(
            matches!(&events[0].kind, EventKind::ClaimGranted { at_risk, .. } if at_risk.len() == 1)
        );

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
    fn claim_with_no_scopes_is_malformed() {
        let mut c = coordinator();
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
            ClientMsg::Claim {
                req,
                intent: intent("w"),
                scopes: scopes.clone(),
                on_conflict: OnConflict::Wait,
            },
            ClientMsg::Claim {
                req,
                intent: intent("s"),
                scopes: scopes.clone(),
                on_conflict: OnConflict::Shadow,
            },
            ClientMsg::Amend {
                req,
                claim: ClaimId(1),
                fence: Fence(1),
                add: scopes.clone(),
            },
            ClientMsg::Heartbeat,
            ClientMsg::Submit {
                req,
                claim: ClaimId(1),
                fence: Fence(1),
                fork_commit: CommitId("c".into()),
                touched: scopes.clone(),
                decisions: DecisionRecord::default(),
            },
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

    // ---- property: decisions match a brute-force oracle ----

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
        Claim { agent: u8, scopes: Vec<ScopeClaim> },
        Release { pick: usize },
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
        let op = prop_oneof![
            4 => (0u8..3, scope_claims()).prop_map(|(agent, scopes)| Op::Claim { agent, scopes }),
            1 => any::<usize>().prop_map(|pick| Op::Release { pick }),
        ];
        prop::collection::vec(op, 1..40)
    }

    struct Active {
        agent: AgentId,
        claim: ClaimId,
        fence: Fence,
        scopes: Vec<ScopeClaim>,
    }

    type Pair = (ScopeClaim, ScopeClaim, AgentId);

    /// Two scope claims conflict iff one scope covers the other and the modes conflict.
    fn oracle(active: &[Active], who: &AgentId, scopes: &[ScopeClaim]) -> HashSet<Pair> {
        let mut out = HashSet::new();
        for other in active.iter().filter(|a| a.agent != *who) {
            for mine in scopes {
                for theirs in &other.scopes {
                    let overlap =
                        mine.scope.covers(&theirs.scope) || theirs.scope.covers(&mine.scope);
                    if overlap && mine.mode.conflicts_with(theirs.mode) {
                        out.insert((mine.clone(), theirs.clone(), other.agent.clone()));
                    }
                }
            }
        }
        out
    }

    proptest! {
        #[test]
        fn decisions_match_brute_force_oracle(sequence in ops()) {
            let mut c = coordinator();
            let mut active: Vec<Active> = Vec::new();
            for op in sequence {
                match op {
                    Op::Claim { agent: n, scopes } => {
                        let who = agent(&format!("agent-{n}"));
                        let expected = oracle(&active, &who, &scopes);
                        let effects = c.handle(&who, claim_msg(intent("p"), scopes.clone()), NOW);
                        match only_reply(&effects) {
                            ServerMsg::Granted { claim, fence, .. } => {
                                prop_assert!(expected.is_empty(), "granted despite {expected:?}");
                                let (claim, fence) = (*claim, *fence);
                                active.push(Active { agent: who, claim, fence, scopes });
                            }
                            ServerMsg::Denied { conflicts, .. } => {
                                let mut got: HashSet<Pair> = HashSet::new();
                                for x in conflicts {
                                    let (req, held) = (x.requested.clone(), x.held.clone());
                                    got.insert((req, held, x.held_by.clone()));
                                }
                                prop_assert_eq!(got, expected);
                            }
                            other => prop_assert!(false, "unexpected reply {other:?}"),
                        }
                    }
                    Op::Release { pick } => {
                        if active.is_empty() {
                            continue;
                        }
                        let gone = active.remove(pick % active.len());
                        let msg = ClientMsg::Release { claim: gone.claim, fence: gone.fence };
                        let effects = c.handle(&gone.agent, msg, NOW);
                        prop_assert!(replies(&effects).is_empty(), "release failed: {effects:?}");
                    }
                }
            }
        }
    }
}

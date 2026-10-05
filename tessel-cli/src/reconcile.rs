//! Reconciling the daemon's idea of its claims with the coordinator's, after a reconnect.
//!
//! A claim reply or a release can be lost with the socket, which would leave a claim that
//! heartbeats keep alive and nobody tracks. The protocol has no "list my claims" request, so
//! after each reconnect the daemon reads the event log (`Watch { from_seq: 0 }`) and rebuilds
//! the agent's live claims from it. This module is the pure part: log in, decisions out.

use std::collections::{BTreeMap, HashSet};

use tessel_coordinator::protocol::{AgentId, ClaimId, Event, EventKind, Fence, RaceId, ScopeClaim};

use crate::state::HeldClaim;

/// A claim the coordinator holds for this agent, as the log shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerClaim {
    pub fence: Fence,
    pub scopes: Vec<ScopeClaim>,
    pub race: Option<RaceId>,
    /// Submitted and not since rejected: the coordinator holds the claim for the steward.
    pub submitted: bool,
}

/// The claims `agent` holds after replaying `events` in order, with each claim's latest fence
/// and whether it is submitted. A claim that was released, expired or merged is not live. A
/// submitted claim is live: the coordinator keeps its locks until `Merged` or `SubmitRejected`.
pub fn live_claims(agent: &AgentId, events: &[Event]) -> BTreeMap<u64, ServerClaim> {
    let mut live = BTreeMap::new();
    for event in events {
        match &event.kind {
            EventKind::ClaimGranted {
                agent: owner,
                claim,
                fence,
                scopes,
                race,
                ..
            } if owner == agent => {
                live.insert(
                    claim.0,
                    ServerClaim {
                        fence: *fence,
                        scopes: scopes.clone(),
                        race: *race,
                        submitted: false,
                    },
                );
            }
            EventKind::ClaimAmended {
                claim,
                fence,
                added,
            } => {
                if let Some(held) = live.get_mut(&claim.0) {
                    held.fence = *fence;
                    held.scopes.extend(added.iter().cloned());
                }
            }
            EventKind::Submitted { claim, .. } => {
                if let Some(held) = live.get_mut(&claim.0) {
                    held.submitted = true;
                }
            }
            EventKind::SubmitRejected { claim, .. } => {
                if let Some(held) = live.get_mut(&claim.0) {
                    held.submitted = false;
                }
            }
            EventKind::ClaimReleased { claim, .. } | EventKind::Merged { claim, .. } => {
                live.remove(&claim.0);
            }
            EventKind::ClaimGranted { .. }
            | EventKind::AgentConnected { .. }
            | EventKind::ClaimDenied { .. }
            | EventKind::ClaimShadowed { .. }
            | EventKind::WaitQueued { .. }
            | EventKind::WaitWithdrawn { .. }
            | EventKind::ReviewRequested { .. }
            | EventKind::ReviewDecided { .. }
            | EventKind::BaseMoved { .. }
            | EventKind::AssumptionChallenged { .. }
            | EventKind::RaceOpened { .. }
            | EventKind::RaceDecided { .. }
            | EventKind::DenialVerified { .. }
            | EventKind::AssumptionVerified { .. }
            | EventKind::ReplayMerged { .. } => {}
        }
    }
    live
}

/// Claims that left the live set in `events`, whatever agent held them.
fn ended_claims(events: &[Event]) -> HashSet<ClaimId> {
    let mut ended = HashSet::new();
    for event in events {
        match &event.kind {
            EventKind::ClaimReleased { claim, .. } | EventKind::Merged { claim, .. } => {
                ended.insert(*claim);
            }
            EventKind::AgentConnected { .. }
            | EventKind::ClaimGranted { .. }
            | EventKind::ClaimDenied { .. }
            | EventKind::ClaimShadowed { .. }
            | EventKind::ClaimAmended { .. }
            | EventKind::WaitQueued { .. }
            | EventKind::WaitWithdrawn { .. }
            | EventKind::Submitted { .. }
            | EventKind::SubmitRejected { .. }
            | EventKind::ReviewRequested { .. }
            | EventKind::ReviewDecided { .. }
            | EventKind::BaseMoved { .. }
            | EventKind::AssumptionChallenged { .. }
            | EventKind::RaceOpened { .. }
            | EventKind::RaceDecided { .. }
            | EventKind::DenialVerified { .. }
            | EventKind::AssumptionVerified { .. }
            | EventKind::ReplayMerged { .. } => {}
        }
    }
    ended
}

/// What the daemon knew when the connection dropped.
pub struct Local<'a> {
    pub claims: &'a [HeldClaim],
    /// Granted, submitted or rejected since the new connection was welcomed: the log read may
    /// predate them.
    pub fresh: &'a HashSet<ClaimId>,
    /// Scopes of claim requests whose answer was lost with the socket.
    pub lost_requests: &'a [Vec<ScopeClaim>],
    /// Claims this daemon asked to release before the socket dropped.
    pub lost_releases: &'a HashSet<ClaimId>,
    /// Whether the log was read to its end marker. A truncated read can stop after a claim's
    /// grant and before its release, so it never justifies adopting, answering or releasing:
    /// only a logged ending is evidence, and a missing one is not.
    pub complete: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Local claims the log shows as ended. Absence from the log is not enough: the read may
    /// have been cut short, and forgetting a claim that is still held would be worse.
    pub forget: Vec<ClaimId>,
    /// Local claims whose fence is older than the log's latest. A fence never goes backwards.
    pub refresh: Vec<(ClaimId, Fence)>,
    /// Local claims whose submitted flag differs from the log: a submission whose reply was lost
    /// (now true), or one the steward rejected (now false). Only a complete read decides this,
    /// and never for a claim whose state changed since the new connection was welcomed.
    pub set_submitted: Vec<(ClaimId, bool)>,
    /// Live claims that answer a request whose reply was lost: index into `lost_requests`.
    pub answer_lost: Vec<(usize, ClaimId, ServerClaim)>,
    /// Live claims this daemon tried to release before the socket dropped.
    pub release_again: Vec<(ClaimId, Fence)>,
    /// Live claims no local request explains. They are kept, because heartbeats keep them
    /// alive and it cannot tell a sibling daemon of the same agent from a ghost; the inbox
    /// says so and `tessel release <id>` drops one.
    pub adopt: Vec<(ClaimId, ServerClaim)>,
}

/// Decides what to do about every difference between the local claims and the log.
pub fn plan(local: &Local<'_>, live: &BTreeMap<u64, ServerClaim>, events: &[Event]) -> Plan {
    let mut plan = Plan::default();
    let ended = ended_claims(events);
    for held in local.claims {
        match live.get(&held.claim.0) {
            Some(server) if server.fence > held.fence => {
                plan.refresh.push((held.claim, server.fence));
            }
            Some(server)
                if local.complete
                    && server.submitted != held.submitted
                    && !local.fresh.contains(&held.claim) =>
            {
                plan.set_submitted.push((held.claim, server.submitted));
            }
            None if ended.contains(&held.claim) && !local.fresh.contains(&held.claim) => {
                plan.forget.push(held.claim);
            }
            Some(_) | None => {}
        }
    }
    if !local.complete {
        return plan;
    }
    let known: HashSet<ClaimId> = local.claims.iter().map(|held| held.claim).collect();
    let mut answered = HashSet::new();
    for (&id, server) in live
        .iter()
        .rev()
        .filter(|(id, _)| !known.contains(&ClaimId(**id)))
    {
        let claim = ClaimId(id);
        if local.lost_releases.contains(&claim) {
            plan.release_again.push((claim, server.fence));
            continue;
        }
        let lost = local
            .lost_requests
            .iter()
            .enumerate()
            .find(|(index, scopes)| !answered.contains(index) && **scopes == server.scopes);
        match lost {
            Some((index, _)) => {
                answered.insert(index);
                plan.answer_lost.push((index, claim, server.clone()));
            }
            None => plan.adopt.push((claim, server.clone())),
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_coordinator::protocol::{
        Intent, Mode, ReleaseReason, RunId, Scope, ScopeClaim as Sc,
    };

    fn scopes(path: &str) -> Vec<Sc> {
        vec![Sc {
            scope: Scope::File { path: path.into() },
            mode: Mode::EditBody,
        }]
    }

    fn event(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: 0,
            run: RunId("t".into()),
            kind,
        }
    }

    fn granted(seq: u64, agent: &str, claim: u64, fence: u64, path: &str) -> Event {
        event(
            seq,
            EventKind::ClaimGranted {
                agent: AgentId(agent.into()),
                claim: ClaimId(claim),
                fence: Fence(fence),
                scopes: scopes(path),
                intent: Intent {
                    summary: String::new(),
                    task_ref: None,
                    assumptions: Vec::new(),
                },
                race: None,
                at_risk: Vec::new(),
            },
        )
    }

    fn held(claim: u64, fence: u64, path: &str) -> HeldClaim {
        HeldClaim {
            claim: ClaimId(claim),
            fence: Fence(fence),
            expires_at_ms: 0,
            race: None,
            scopes: scopes(path),
            submitted: false,
        }
    }

    fn released(seq: u64, claim: u64) -> Event {
        event(
            seq,
            EventKind::ClaimReleased {
                claim: ClaimId(claim),
                reason: ReleaseReason::Agent,
            },
        )
    }

    #[test]
    fn live_claims_follow_grants_amendments_and_endings() {
        let me = AgentId("a1".into());
        let events = vec![
            granted(0, "a1", 1, 1, "a.rs"),
            granted(1, "a2", 2, 2, "b.rs"),
            granted(2, "a1", 3, 3, "c.rs"),
            event(
                3,
                EventKind::ClaimAmended {
                    claim: ClaimId(1),
                    fence: Fence(4),
                    added: scopes("d.rs"),
                },
            ),
            released(4, 3),
        ];
        let live = live_claims(&me, &events);
        assert_eq!(live.keys().copied().collect::<Vec<_>>(), vec![1]);
        assert_eq!(live[&1].fence, Fence(4));
        assert_eq!(live[&1].scopes.len(), 2);
    }

    fn submitted_event(seq: u64, claim: u64, path: &str) -> Event {
        event(
            seq,
            EventKind::Submitted {
                claim: ClaimId(claim),
                fork_commit: tessel_coordinator::protocol::CommitId("f".into()),
                touched: scopes(path),
                decisions: tessel_coordinator::protocol::DecisionRecord::default(),
            },
        )
    }

    fn rejected_event(seq: u64, claim: u64) -> Event {
        event(
            seq,
            EventKind::SubmitRejected {
                claim: ClaimId(claim),
                reason: "conflict".into(),
            },
        )
    }

    fn merged_event(seq: u64, claim: u64) -> Event {
        event(
            seq,
            EventKind::Merged {
                claim: ClaimId(claim),
                head: tessel_coordinator::protocol::CommitId("h".into()),
            },
        )
    }

    #[test]
    fn a_submitted_claim_is_live_and_a_merged_one_is_not() {
        let me = AgentId("a1".into());
        let events = vec![
            granted(0, "a1", 1, 1, "a.rs"),
            granted(1, "a1", 2, 2, "b.rs"),
            merged_event(2, 1),
            submitted_event(3, 2, "b.rs"),
        ];
        let live = live_claims(&me, &events);
        assert_eq!(live.keys().copied().collect::<Vec<_>>(), vec![2]);
        assert!(live[&2].submitted);
    }

    #[test]
    fn a_rejected_submission_is_live_and_not_submitted_again() {
        let me = AgentId("a1".into());
        let events = vec![
            granted(0, "a1", 1, 1, "a.rs"),
            submitted_event(1, 1, "a.rs"),
            rejected_event(2, 1),
        ];
        let live = live_claims(&me, &events);
        assert!(!live[&1].submitted);
        assert_eq!(live[&1].fence, Fence(1));
    }

    #[test]
    fn a_local_submitted_claim_is_not_forgotten_but_a_merged_one_is() {
        let mut submitted = held(1, 1, "a.rs");
        submitted.submitted = true;
        let local = [submitted, held(2, 2, "b.rs")];
        let events = vec![
            granted(0, "a1", 1, 1, "a.rs"),
            granted(1, "a1", 2, 2, "b.rs"),
            submitted_event(2, 1, "a.rs"),
            submitted_event(3, 2, "b.rs"),
            merged_event(4, 2),
        ];
        let plan = plan_for(&local, &events, &[], &[]);
        assert_eq!(plan.forget, vec![ClaimId(2)]);
        assert!(plan.set_submitted.is_empty());
    }

    #[test]
    fn the_log_decides_whether_a_local_claim_is_submitted() {
        let mut was_submitted = held(2, 2, "b.rs");
        was_submitted.submitted = true;
        let local = [held(1, 1, "a.rs"), was_submitted];
        let events = vec![
            granted(0, "a1", 1, 1, "a.rs"),
            granted(1, "a1", 2, 2, "b.rs"),
            submitted_event(2, 1, "a.rs"),
            submitted_event(3, 2, "b.rs"),
            rejected_event(4, 2),
        ];
        let plan = plan_for(&local, &events, &[], &[]);
        assert_eq!(
            plan.set_submitted,
            vec![(ClaimId(1), true), (ClaimId(2), false)]
        );
        let incomplete = plan_incomplete(&local, &events, &[], &[]);
        assert!(incomplete.set_submitted.is_empty());
    }

    #[test]
    fn a_claim_changed_since_the_welcome_keeps_its_local_submitted_flag() {
        let local = [held(1, 1, "a.rs")];
        let events = vec![
            granted(0, "a1", 1, 1, "a.rs"),
            submitted_event(1, 1, "a.rs"),
        ];
        let live = live_claims(&AgentId("a1".into()), &events);
        let fresh: HashSet<ClaimId> = [ClaimId(1)].into();
        let none = HashSet::new();
        let local = Local {
            claims: &local,
            fresh: &fresh,
            lost_requests: &[],
            lost_releases: &none,
            complete: true,
        };
        assert!(super::plan(&local, &live, &events).set_submitted.is_empty());
    }

    #[test]
    fn an_unexplained_submitted_claim_is_adopted_as_submitted() {
        let events = vec![
            granted(0, "a1", 7, 9, "a.rs"),
            submitted_event(1, 7, "a.rs"),
        ];
        let plan = plan_for(&[], &events, &[], &[]);
        assert_eq!(plan.adopt.len(), 1);
        assert!(plan.adopt[0].1.submitted);
    }

    fn plan_for(
        local: &[HeldClaim],
        events: &[Event],
        lost_requests: &[Vec<Sc>],
        lost_releases: &[u64],
    ) -> Plan {
        let live = live_claims(&AgentId("a1".into()), events);
        let releases: HashSet<ClaimId> = lost_releases.iter().map(|id| ClaimId(*id)).collect();
        let fresh = HashSet::new();
        let local = Local {
            claims: local,
            fresh: &fresh,
            lost_requests,
            lost_releases: &releases,
            complete: true,
        };
        plan(&local, &live, events)
    }

    fn plan_incomplete(
        local: &[HeldClaim],
        events: &[Event],
        lost: &[Vec<Sc>],
        rel: &[u64],
    ) -> Plan {
        let live = live_claims(&AgentId("a1".into()), events);
        let releases: HashSet<ClaimId> = rel.iter().map(|id| ClaimId(*id)).collect();
        let fresh = HashSet::new();
        let local = Local {
            claims: local,
            fresh: &fresh,
            lost_requests: lost,
            lost_releases: &releases,
            complete: false,
        };
        plan(&local, &live, events)
    }

    #[test]
    fn a_truncated_read_never_adopts_answers_or_releases() {
        // The log stops after a grant whose release was cut off.
        let events = vec![
            granted(0, "a1", 7, 9, "a.rs"),
            granted(1, "a1", 8, 10, "b.rs"),
        ];
        let plan = plan_incomplete(&[], &events, &[scopes("a.rs")], &[8]);
        assert_eq!(plan, Plan::default());
    }

    #[test]
    fn a_truncated_read_still_forgets_a_logged_ending() {
        let local = [held(1, 1, "a.rs")];
        let events = vec![granted(0, "a1", 1, 1, "a.rs"), released(1, 1)];
        let plan = plan_incomplete(&local, &events, &[], &[]);
        assert_eq!(plan.forget, vec![ClaimId(1)]);
    }

    #[test]
    fn a_fence_never_goes_backwards() {
        let local = [held(1, 5, "a.rs")];
        let events = vec![granted(0, "a1", 1, 3, "a.rs")];
        assert!(plan_for(&local, &events, &[], &[]).refresh.is_empty());
    }

    #[test]
    fn the_latest_live_claim_answers_a_lost_request_when_several_match() {
        let events = vec![
            granted(0, "a1", 5, 5, "a.rs"),
            granted(1, "a1", 9, 9, "a.rs"),
            granted(2, "a1", 7, 7, "a.rs"),
        ];
        let plan = plan_for(&[], &events, &[scopes("a.rs")], &[]);
        assert_eq!(plan.answer_lost.len(), 1);
        assert_eq!(plan.answer_lost[0].1, ClaimId(9));
        let adopted: Vec<ClaimId> = plan.adopt.iter().map(|(claim, _)| *claim).collect();
        assert_eq!(adopted, vec![ClaimId(7), ClaimId(5)]);
    }

    #[test]
    fn a_lost_grant_reply_is_matched_to_its_request() {
        let events = vec![granted(0, "a1", 7, 9, "a.rs")];
        let plan = plan_for(&[], &events, &[scopes("a.rs")], &[]);
        assert_eq!(plan.answer_lost.len(), 1);
        assert_eq!(plan.answer_lost[0].0, 0);
        assert_eq!(plan.answer_lost[0].1, ClaimId(7));
        assert!(plan.adopt.is_empty() && plan.release_again.is_empty());
    }

    #[test]
    fn a_lost_release_is_sent_again_with_the_latest_fence() {
        let events = vec![granted(0, "a1", 7, 9, "a.rs")];
        let plan = plan_for(&[], &events, &[], &[7]);
        assert_eq!(plan.release_again, vec![(ClaimId(7), Fence(9))]);
        assert!(plan.adopt.is_empty());
    }

    #[test]
    fn an_unexplained_live_claim_is_adopted_not_released() {
        let events = vec![granted(0, "a1", 7, 9, "a.rs")];
        let plan = plan_for(&[], &events, &[scopes("other.rs")], &[]);
        assert_eq!(plan.adopt.len(), 1);
        assert!(plan.answer_lost.is_empty() && plan.release_again.is_empty());
    }

    #[test]
    fn only_a_logged_ending_makes_the_daemon_forget_a_claim() {
        let local = [held(1, 1, "a.rs"), held(2, 2, "b.rs")];
        let events = vec![
            granted(0, "a1", 1, 1, "a.rs"),
            granted(1, "a1", 2, 2, "b.rs"),
            released(2, 1),
        ];
        let plan = plan_for(&local, &events, &[], &[]);
        assert_eq!(plan.forget, vec![ClaimId(1)]);
        // A claim missing from a truncated log is kept.
        let plan = plan_for(&local, &[], &[], &[]);
        assert!(plan.forget.is_empty());
    }

    #[test]
    fn a_stale_fence_is_refreshed_and_a_fresh_claim_is_never_forgotten() {
        let local = [held(1, 1, "a.rs")];
        let events = vec![
            granted(0, "a1", 1, 1, "a.rs"),
            event(
                1,
                EventKind::ClaimAmended {
                    claim: ClaimId(1),
                    fence: Fence(5),
                    added: Vec::new(),
                },
            ),
        ];
        let plan = plan_for(&local, &events, &[], &[]);
        assert_eq!(plan.refresh, vec![(ClaimId(1), Fence(5))]);

        let ended = vec![granted(0, "a1", 1, 1, "a.rs"), released(1, 1)];
        let live = live_claims(&AgentId("a1".into()), &ended);
        let fresh: HashSet<ClaimId> = [ClaimId(1)].into();
        let none = HashSet::new();
        let local = Local {
            claims: &local,
            fresh: &fresh,
            lost_requests: &[],
            lost_releases: &none,
            complete: true,
        };
        assert!(super::plan(&local, &live, &ended).forget.is_empty());
    }
}

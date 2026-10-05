//! Questions about the event log that `Summary` does not answer. Every match over `EventKind` is
//! exhaustive with no wildcard, so a new event kind forces a decision here.

use std::collections::{HashMap, HashSet};

use tessel_coordinator::protocol::{AgentId, ClaimId, Event, EventKind, Outcome};

/// Counts taken from the log itself, next to what `Summary::from_events` gives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogCounts {
    /// `SubmitRejected` events.
    pub rejected: u64,
    /// `WaitQueued` events: claims that queued behind a holder.
    pub waits: u64,
    /// `ReviewDecided` events with `approve: true`.
    pub approvals: u64,
    /// `ReviewDecided` events with `approve: false`.
    pub review_rejections: u64,
    /// Claims that were flagged for review and never decided.
    pub held_for_review: u64,
    /// `task_ref` of the claims that were merged, by the intent they were granted with.
    pub merged_task_refs: HashSet<String>,
    /// `task_ref` of the claims whose submission was rejected.
    pub rejected_task_refs: HashSet<String>,
    /// `ClaimShadowed` events: denials the agent kept working past.
    pub shadow_claims: u64,
    /// `DenialVerified` events whose trial could not judge the work (`Inconclusive`).
    pub shadow_inconclusive: u64,
    /// Shadow claims with no `DenialVerified` event: the trial never ran (the claim had not
    /// submitted when its blocker merged, the blocker never merged, or the claim ended first).
    pub shadow_unverified: u64,
}

pub fn count(events: &[Event]) -> LogCounts {
    let mut counts = LogCounts::default();
    let mut requested: HashSet<ClaimId> = HashSet::new();
    let mut decided: HashSet<ClaimId> = HashSet::new();
    let mut refs: HashMap<ClaimId, String> = HashMap::new();
    let mut shadows: HashSet<ClaimId> = HashSet::new();
    let mut verified: HashSet<ClaimId> = HashSet::new();
    for event in events {
        match &event.kind {
            EventKind::ClaimGranted { claim, intent, .. } => {
                if let Some(task_ref) = &intent.task_ref {
                    refs.insert(*claim, task_ref.clone());
                }
            }
            EventKind::Merged { claim, .. } => {
                if let Some(task_ref) = refs.get(claim) {
                    counts.merged_task_refs.insert(task_ref.clone());
                }
            }
            EventKind::SubmitRejected { claim, .. } => {
                counts.rejected += 1;
                if let Some(task_ref) = refs.get(claim) {
                    counts.rejected_task_refs.insert(task_ref.clone());
                }
            }
            EventKind::WaitQueued { .. } => counts.waits += 1,
            EventKind::ReviewRequested { claim, .. } => {
                requested.insert(*claim);
            }
            EventKind::ReviewDecided { claim, approve, .. } => {
                decided.insert(*claim);
                if *approve {
                    counts.approvals += 1;
                } else {
                    counts.review_rejections += 1;
                }
            }
            EventKind::ClaimShadowed { claim, .. } => {
                shadows.insert(*claim);
            }
            EventKind::DenialVerified {
                shadow_claim,
                outcome,
                ..
            } => {
                verified.insert(*shadow_claim);
                match outcome {
                    Outcome::Inconclusive => counts.shadow_inconclusive += 1,
                    Outcome::Clean
                    | Outcome::TextualConflict
                    | Outcome::BuildFailed
                    | Outcome::TestsFailed => {}
                }
            }
            EventKind::AgentConnected { .. }
            | EventKind::ClaimDenied { .. }
            | EventKind::ClaimAmended { .. }
            | EventKind::ClaimReleased { .. }
            | EventKind::WaitWithdrawn { .. }
            | EventKind::Submitted { .. }
            | EventKind::BaseMoved { .. }
            | EventKind::AssumptionChallenged { .. }
            | EventKind::RaceOpened { .. }
            | EventKind::RaceDecided { .. }
            | EventKind::AssumptionVerified { .. }
            | EventKind::ReplayMerged { .. } => {}
        }
    }
    counts.held_for_review = requested.difference(&decided).count() as u64;
    counts.shadow_claims = shadows.len() as u64;
    counts.shadow_unverified = shadows.difference(&verified).count() as u64;
    counts
}

/// How many shadow trials the log still owes: pairs of a shadow claim and the claim that blocked it
/// where the shadow claim had submitted and is still live, and the blocker either merged after
/// that submission or has not been decided yet, with no `DenialVerified` for the pair. The
/// blocker is the claim its holder (`Conflict::held_by`) had been granted last when the shadow
/// claim was made. A shadow claim that submitted only after its blocker merged is never tried
/// (the coordinator has no baseline for it), so it is not owed anything.
pub fn awaiting_verification(events: &[Event]) -> usize {
    let mut latest_grant: HashMap<&AgentId, ClaimId> = HashMap::new();
    let mut pairs: Vec<(ClaimId, ClaimId)> = Vec::new();
    let mut submitted: HashMap<ClaimId, u64> = HashMap::new();
    let mut merged: HashMap<ClaimId, u64> = HashMap::new();
    let mut ended: HashSet<ClaimId> = HashSet::new();
    let mut verified: HashSet<(ClaimId, ClaimId)> = HashSet::new();
    for event in events {
        match &event.kind {
            EventKind::ClaimGranted { agent, claim, .. } => {
                latest_grant.insert(agent, *claim);
            }
            EventKind::ClaimShadowed {
                claim, conflicts, ..
            } => {
                for conflict in conflicts {
                    let Some(blocker) = latest_grant.get(&conflict.held_by) else {
                        continue;
                    };
                    if !pairs.contains(&(*claim, *blocker)) {
                        pairs.push((*claim, *blocker));
                    }
                }
            }
            EventKind::Submitted { claim, .. } => {
                submitted.entry(*claim).or_insert(event.seq);
            }
            EventKind::Merged { claim, .. } => {
                merged.insert(*claim, event.seq);
            }
            EventKind::SubmitRejected { claim, .. } | EventKind::ClaimReleased { claim, .. } => {
                ended.insert(*claim);
            }
            EventKind::DenialVerified {
                shadow_claim,
                blocking_claim,
                ..
            } => {
                verified.insert((*shadow_claim, *blocking_claim));
            }
            EventKind::AgentConnected { .. }
            | EventKind::ClaimDenied { .. }
            | EventKind::ClaimAmended { .. }
            | EventKind::WaitQueued { .. }
            | EventKind::WaitWithdrawn { .. }
            | EventKind::ReviewRequested { .. }
            | EventKind::ReviewDecided { .. }
            | EventKind::BaseMoved { .. }
            | EventKind::AssumptionChallenged { .. }
            | EventKind::RaceOpened { .. }
            | EventKind::RaceDecided { .. }
            | EventKind::AssumptionVerified { .. }
            | EventKind::ReplayMerged { .. } => {}
        }
    }
    let owed = |&&(shadow, blocker): &&(ClaimId, ClaimId)| {
        let Some(&submitted_at) = submitted.get(&shadow) else {
            return false;
        };
        if ended.contains(&shadow) || verified.contains(&(shadow, blocker)) {
            return false;
        }
        match merged.get(&blocker) {
            Some(&merged_at) => submitted_at < merged_at,
            None => !ended.contains(&blocker),
        }
    };
    pairs.iter().filter(owed).count()
}

/// The agent a `AgentConnected` event is about.
pub fn connected(kind: &EventKind) -> Option<&AgentId> {
    match kind {
        EventKind::AgentConnected { agent } => Some(agent),
        EventKind::ClaimGranted { .. }
        | EventKind::ClaimDenied { .. }
        | EventKind::ClaimShadowed { .. }
        | EventKind::ClaimAmended { .. }
        | EventKind::ClaimReleased { .. }
        | EventKind::WaitQueued { .. }
        | EventKind::WaitWithdrawn { .. }
        | EventKind::Submitted { .. }
        | EventKind::Merged { .. }
        | EventKind::SubmitRejected { .. }
        | EventKind::ReviewRequested { .. }
        | EventKind::ReviewDecided { .. }
        | EventKind::BaseMoved { .. }
        | EventKind::AssumptionChallenged { .. }
        | EventKind::RaceOpened { .. }
        | EventKind::RaceDecided { .. }
        | EventKind::DenialVerified { .. }
        | EventKind::AssumptionVerified { .. }
        | EventKind::ReplayMerged { .. } => None,
    }
}

/// The claim a `ReviewRequested` event holds for review.
pub fn review_requested(kind: &EventKind) -> Option<ClaimId> {
    match kind {
        EventKind::ReviewRequested { claim, .. } => Some(*claim),
        EventKind::AgentConnected { .. }
        | EventKind::ClaimGranted { .. }
        | EventKind::ClaimDenied { .. }
        | EventKind::ClaimShadowed { .. }
        | EventKind::ClaimAmended { .. }
        | EventKind::ClaimReleased { .. }
        | EventKind::WaitQueued { .. }
        | EventKind::WaitWithdrawn { .. }
        | EventKind::Submitted { .. }
        | EventKind::Merged { .. }
        | EventKind::SubmitRejected { .. }
        | EventKind::ReviewDecided { .. }
        | EventKind::BaseMoved { .. }
        | EventKind::AssumptionChallenged { .. }
        | EventKind::RaceOpened { .. }
        | EventKind::RaceDecided { .. }
        | EventKind::DenialVerified { .. }
        | EventKind::AssumptionVerified { .. }
        | EventKind::ReplayMerged { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_coordinator::protocol::RunId;

    fn event(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: 0,
            run: RunId("t".into()),
            kind,
        }
    }

    #[test]
    fn approvals_and_rejections_are_counted_apart_whatever_the_note_says() {
        let decided = |seq, claim, approve, note: &str| {
            event(
                seq,
                EventKind::ReviewDecided {
                    claim: ClaimId(claim),
                    approve,
                    note: Some(note.to_string()),
                },
            )
        };
        let log = [
            decided(0, 1, true, "anything"),
            decided(1, 2, true, ""),
            decided(2, 3, false, "scripted reviewer"),
        ];
        let counts = count(&log);
        assert_eq!((counts.approvals, counts.review_rejections), (2, 1));
    }

    #[test]
    fn held_claims_are_the_requested_ones_nobody_decided() {
        let log = [
            event(
                0,
                EventKind::ReviewRequested {
                    claim: ClaimId(1),
                    reasons: vec![],
                },
            ),
            event(
                1,
                EventKind::ReviewRequested {
                    claim: ClaimId(2),
                    reasons: vec![],
                },
            ),
            event(
                2,
                EventKind::ReviewDecided {
                    claim: ClaimId(1),
                    approve: true,
                    note: None,
                },
            ),
            event(
                3,
                EventKind::SubmitRejected {
                    claim: ClaimId(3),
                    reason: "x".into(),
                },
            ),
        ];
        let counts = count(&log);
        assert_eq!(counts.rejected, 1);
        assert_eq!(counts.waits, 0);
        assert_eq!(counts.approvals, 1);
        assert_eq!(counts.review_rejections, 0);
        assert_eq!(counts.held_for_review, 1);
        assert_eq!(review_requested(&log[0].kind), Some(ClaimId(1)));
        assert_eq!(review_requested(&log[2].kind), None);
        assert_eq!(connected(&log[0].kind), None);
    }

    /// Agent `holder` is granted claim 1; agent `shadow` is shadowed as claim 2 against it.
    fn shadowed_pair() -> Vec<Event> {
        use tessel_coordinator::protocol::{Conflict, Fence, Intent, Mode, Scope, ScopeClaim};
        let intent = || Intent {
            summary: "t01: x".into(),
            task_ref: Some("t01".into()),
            assumptions: Vec::new(),
        };
        let scope = ScopeClaim {
            scope: Scope::File {
                path: "src/a.ts".into(),
            },
            mode: Mode::EditBody,
        };
        vec![
            event(
                0,
                EventKind::ClaimGranted {
                    agent: AgentId("holder".into()),
                    claim: ClaimId(1),
                    fence: Fence(1),
                    scopes: vec![scope.clone()],
                    intent: intent(),
                    race: None,
                    at_risk: Vec::new(),
                },
            ),
            event(
                1,
                EventKind::ClaimShadowed {
                    agent: AgentId("shadow".into()),
                    claim: ClaimId(2),
                    scopes: vec![scope.clone()],
                    conflicts: vec![Conflict {
                        requested: scope.clone(),
                        held: scope,
                        held_by: AgentId("holder".into()),
                        their_intent: intent(),
                        race: None,
                    }],
                },
            ),
        ]
    }

    fn submitted(seq: u64, claim: u64) -> Event {
        use tessel_coordinator::protocol::{CommitId, DecisionRecord};
        event(
            seq,
            EventKind::Submitted {
                claim: ClaimId(claim),
                fork_commit: CommitId("c".repeat(40)),
                touched: Vec::new(),
                decisions: DecisionRecord::default(),
            },
        )
    }

    fn merged(seq: u64, claim: u64) -> Event {
        use tessel_coordinator::protocol::CommitId;
        event(
            seq,
            EventKind::Merged {
                claim: ClaimId(claim),
                head: CommitId("d".repeat(40)),
            },
        )
    }

    fn verified(seq: u64, outcome: Outcome) -> Event {
        event(
            seq,
            EventKind::DenialVerified {
                shadow_claim: ClaimId(2),
                blocking_claim: ClaimId(1),
                outcome,
            },
        )
    }

    #[test]
    fn a_trial_is_owed_when_the_shadow_claim_submitted_before_its_blocker_merged() {
        let mut log = shadowed_pair();
        log.push(submitted(2, 2));
        assert_eq!(awaiting_verification(&log), 1, "blocker not decided yet");
        log.push(submitted(3, 1));
        log.push(merged(4, 1));
        assert_eq!(
            awaiting_verification(&log),
            1,
            "merged, trial not logged yet"
        );
        log.push(verified(5, Outcome::TextualConflict));
        assert_eq!(awaiting_verification(&log), 0);
    }

    #[test]
    fn no_trial_is_owed_when_none_can_run() {
        let mut never_submitted = shadowed_pair();
        never_submitted.push(merged(2, 1));
        assert_eq!(awaiting_verification(&never_submitted), 0);

        let mut late = shadowed_pair();
        late.push(merged(2, 1));
        late.push(submitted(3, 2));
        assert_eq!(awaiting_verification(&late), 0, "submitted after the merge");

        let mut rejected = shadowed_pair();
        rejected.push(submitted(2, 2));
        rejected.push(event(
            3,
            EventKind::SubmitRejected {
                claim: ClaimId(1),
                reason: "x".into(),
            },
        ));
        assert_eq!(
            awaiting_verification(&rejected),
            0,
            "the blocker never merged"
        );

        let mut ended = shadowed_pair();
        ended.push(submitted(2, 2));
        ended.push(event(
            3,
            EventKind::ClaimReleased {
                claim: ClaimId(2),
                reason: tessel_coordinator::protocol::ReleaseReason::LeaseExpired,
            },
        ));
        ended.push(merged(4, 1));
        assert_eq!(
            awaiting_verification(&ended),
            0,
            "the shadow claim ended first"
        );
    }

    #[test]
    fn shadow_counts_separate_inconclusive_trials_from_claims_never_tried() {
        let mut log = shadowed_pair();
        let counts = count(&log);
        assert_eq!(
            (
                counts.shadow_claims,
                counts.shadow_inconclusive,
                counts.shadow_unverified
            ),
            (1, 0, 1)
        );
        log.push(verified(2, Outcome::Inconclusive));
        let counts = count(&log);
        assert_eq!(
            (
                counts.shadow_claims,
                counts.shadow_inconclusive,
                counts.shadow_unverified
            ),
            (1, 1, 0)
        );
        log.push(verified(3, Outcome::Clean));
        let counts = count(&log);
        assert_eq!(
            counts.shadow_inconclusive, 1,
            "a clean trial is not inconclusive"
        );
    }
}

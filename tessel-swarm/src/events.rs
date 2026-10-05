//! Questions about the event log that `Summary` does not answer. Every match over `EventKind` is
//! exhaustive with no wildcard, so a new event kind forces a decision here.

use std::collections::{HashMap, HashSet};

use tessel_coordinator::protocol::{AgentId, ClaimId, Event, EventKind};

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
}

pub fn count(events: &[Event]) -> LogCounts {
    let mut counts = LogCounts::default();
    let mut requested: HashSet<ClaimId> = HashSet::new();
    let mut decided: HashSet<ClaimId> = HashSet::new();
    let mut refs: HashMap<ClaimId, String> = HashMap::new();
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
            EventKind::AgentConnected { .. }
            | EventKind::ClaimDenied { .. }
            | EventKind::ClaimShadowed { .. }
            | EventKind::ClaimAmended { .. }
            | EventKind::ClaimReleased { .. }
            | EventKind::WaitWithdrawn { .. }
            | EventKind::Submitted { .. }
            | EventKind::BaseMoved { .. }
            | EventKind::AssumptionChallenged { .. }
            | EventKind::RaceOpened { .. }
            | EventKind::RaceDecided { .. }
            | EventKind::DenialVerified { .. }
            | EventKind::AssumptionVerified { .. }
            | EventKind::ReplayMerged { .. } => {}
        }
    }
    counts.held_for_review = requested.difference(&decided).count() as u64;
    counts
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
}

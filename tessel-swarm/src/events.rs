//! Questions about the event log that `Summary` does not answer. Every match over `EventKind` is
//! exhaustive with no wildcard, so a new event kind forces a decision here.

use std::collections::{HashMap, HashSet};

use tessel_coordinator::protocol::{
    AgentId, ClaimId, Event, EventKind, Fence, Outcome, ReleaseReason, RequestId, ScopeClaim,
};

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

/// Appends `event` to a log read from seq 0 in order. An event the log already has is ignored
/// (`false`); one that skips a seq is an error, because counts over a log with a hole are wrong.
pub fn append(log: &mut Vec<Event>, event: Event) -> anyhow::Result<bool> {
    let next = log.len() as u64;
    if event.seq < next {
        return Ok(false);
    }
    if event.seq > next {
        anyhow::bail!(
            "the event log has a gap: expected seq {next}, got {}",
            event.seq
        );
    }
    log.push(event);
    Ok(true)
}

/// How many shadow trials the log still owes: pairs of a shadow claim and the claim that blocked it
/// where the shadow claim had submitted and is still live, and the blocker either merged after
/// that submission or has not been decided yet, with no `DenialVerified` for the pair. The
/// blocker is the claim its holder (`Conflict::held_by`) had been granted last when the shadow
/// claim was made. A shadow claim that submitted only after its blocker merged is never tried
/// (the coordinator has no baseline for it), so it is not owed anything.
pub fn awaiting_verification(events: &[Event]) -> usize {
    owed_trials(events).len()
}

/// The trials `awaiting_verification` counts that are for `shadow` alone.
pub fn awaiting_verification_of(events: &[Event], shadow: ClaimId) -> usize {
    let owed = owed_trials(events);
    owed.iter().filter(|(claim, _)| *claim == shadow).count()
}

/// (shadow claim, blocking claim) pairs whose trial the log owes.
fn owed_trials(events: &[Event]) -> Vec<(ClaimId, ClaimId)> {
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
    let owed = |&(shadow, blocker): &(ClaimId, ClaimId)| {
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
    pairs.into_iter().filter(owed).collect()
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

/// The claim an agent holds for one task, as the log last showed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    pub claim: ClaimId,
    pub fence: Fence,
    pub scopes: Vec<ScopeClaim>,
    /// A `Submitted` is in the log: the work went in and an outcome is owed.
    pub submitted: bool,
}

/// What became of the claim an agent was granted for a task: what an agent whose connection broke
/// reads from the log to learn where it stands, since messages sent while it was gone are lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// No claim was granted for the task and no request for it is queued: a queued request is
    /// withdrawn when its agent's last socket closes.
    Unclaimed,
    /// A request for the task is still queued, because the coordinator closed the agent's socket
    /// without withdrawing it. The grant will be sent to the agent's next connection.
    Queued {
        req: RequestId,
        scopes: Vec<ScopeClaim>,
    },
    Open(Open),
    Merged,
    /// The submission was rejected. `open` is the claim while the agent has not released it.
    Rejected {
        reason: String,
        open: Option<Open>,
    },
    /// The claim ended without a merge and not by the agent's release: its lease ran out.
    Lapsed,
    /// The agent released the claim.
    Released,
}

/// The standing of the claim `agent` was granted for `task_ref`, from `log` read from seq 0. A
/// task is granted at most one claim per agent, so the last grant is the one.
pub fn standing(log: &[Event], agent: &str, task_ref: &str) -> Standing {
    let mut now = Standing::Unclaimed;
    for event in log {
        if let EventKind::WaitQueued {
            agent: who,
            req,
            scopes,
            intent,
            ..
        } = &event.kind
        {
            if who.0 == agent && intent.task_ref.as_deref() == Some(task_ref) {
                now = Standing::Queued {
                    req: *req,
                    scopes: scopes.clone(),
                };
            }
            continue;
        }
        if let EventKind::ClaimGranted {
            agent: who,
            claim,
            fence,
            scopes,
            intent,
            ..
        } = &event.kind
        {
            if who.0 == agent && intent.task_ref.as_deref() == Some(task_ref) {
                now = Standing::Open(Open {
                    claim: *claim,
                    fence: *fence,
                    scopes: scopes.clone(),
                    submitted: false,
                });
            }
            continue;
        }
        now = follow(now, &event.kind, agent);
    }
    now
}

/// How many times `agent` was denied a claim for `task_ref`, from `log` read from seq 0. A denial
/// is answered once; one the agent never heard shows here and not in its own count.
pub fn denials(log: &[Event], agent: &str, task_ref: &str) -> u32 {
    let denied = log.iter().filter(|e| {
        matches!(
            &e.kind,
            EventKind::ClaimDenied { agent: who, intent, .. }
                if who.0 == agent && intent.task_ref.as_deref() == Some(task_ref)
        )
    });
    u32::try_from(denied.count()).unwrap_or(u32::MAX)
}

/// `now` after `kind`, if `kind` is about the claim `now` is about.
fn follow(now: Standing, kind: &EventKind, agent: &str) -> Standing {
    match now {
        Standing::Queued { req, scopes } => {
            let withdrawn = matches!(
                kind,
                EventKind::WaitWithdrawn { agent: who, req: gone }
                    if *gone == req && who.0 == agent
            );
            if withdrawn {
                Standing::Unclaimed
            } else {
                Standing::Queued { req, scopes }
            }
        }
        Standing::Open(open) => follow_open(open, kind),
        Standing::Rejected {
            reason,
            open: Some(open),
        } => {
            let released = matches!(
                kind,
                EventKind::ClaimReleased { claim, .. } if *claim == open.claim
            );
            Standing::Rejected {
                reason,
                open: (!released).then_some(open),
            }
        }
        Standing::Unclaimed
        | Standing::Merged
        | Standing::Rejected { open: None, .. }
        | Standing::Lapsed
        | Standing::Released => now,
    }
}

fn follow_open(mut open: Open, kind: &EventKind) -> Standing {
    match kind {
        EventKind::ClaimAmended {
            claim,
            fence,
            added,
        } if *claim == open.claim => {
            open.fence = *fence;
            open.scopes.extend(added.iter().cloned());
        }
        EventKind::Submitted { claim, .. } if *claim == open.claim => open.submitted = true,
        EventKind::Merged { claim, .. } if *claim == open.claim => return Standing::Merged,
        EventKind::SubmitRejected { claim, reason } if *claim == open.claim => {
            return Standing::Rejected {
                reason: reason.clone(),
                open: Some(open),
            };
        }
        EventKind::ClaimReleased { claim, reason } if *claim == open.claim => {
            return match reason {
                ReleaseReason::Agent => Standing::Released,
                ReleaseReason::Merged => Standing::Merged,
                ReleaseReason::LeaseExpired | ReleaseReason::LostRace | ReleaseReason::Settled => {
                    Standing::Lapsed
                }
            };
        }
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
    Standing::Open(open)
}

/// The claim a `ReviewDecided` event decided.
pub fn review_decided(kind: &EventKind) -> Option<ClaimId> {
    match kind {
        EventKind::ReviewDecided { claim, .. } => Some(*claim),
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
        | EventKind::ReviewRequested { .. }
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
                    reviewer: None,
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
                    reviewer: None,
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
    fn an_appended_log_stays_gap_free_and_ignores_events_it_already_has() {
        let mut log = Vec::new();
        let released = |seq| {
            event(
                seq,
                EventKind::ClaimReleased {
                    claim: ClaimId(1),
                    reason: tessel_coordinator::protocol::ReleaseReason::Agent,
                },
            )
        };
        assert!(append(&mut log, released(0)).unwrap());
        assert!(append(&mut log, released(1)).unwrap());
        assert!(
            !append(&mut log, released(1)).unwrap(),
            "a replayed event is ignored"
        );
        let gap = append(&mut log, released(3)).unwrap_err();
        assert!(gap.to_string().contains("expected seq 2, got 3"), "{gap}");
        assert_eq!(log.len(), 2, "a gap is not appended");
        assert!(append(&mut log, released(2)).unwrap());
    }

    #[test]
    fn a_trial_owed_to_another_shadow_claim_is_not_owed_to_this_one() {
        let mut log = shadowed_pair();
        log.push(submitted(2, 2));
        assert_eq!(awaiting_verification_of(&log, ClaimId(2)), 1);
        assert_eq!(awaiting_verification_of(&log, ClaimId(7)), 0);
        log.push(verified(3, Outcome::Clean));
        assert_eq!(awaiting_verification_of(&log, ClaimId(2)), 0);
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

    fn file(path: &str, mode: tessel_coordinator::protocol::Mode) -> ScopeClaim {
        ScopeClaim {
            scope: tessel_coordinator::protocol::Scope::File { path: path.into() },
            mode,
        }
    }

    fn granted(seq: u64, agent: &str, claim: u64, fence: u64, task_ref: &str) -> Event {
        event(
            seq,
            EventKind::ClaimGranted {
                agent: AgentId(agent.into()),
                claim: ClaimId(claim),
                fence: Fence(fence),
                scopes: vec![file("a.ts", tessel_coordinator::protocol::Mode::EditBody)],
                intent: tessel_coordinator::protocol::Intent {
                    summary: "s".into(),
                    task_ref: Some(task_ref.into()),
                    assumptions: Vec::new(),
                },
                race: None,
                at_risk: Vec::new(),
            },
        )
    }

    fn released(seq: u64, claim: u64, reason: ReleaseReason) -> Event {
        let claim = ClaimId(claim);
        event(seq, EventKind::ClaimReleased { claim, reason })
    }

    fn open_of(standing: Standing) -> Open {
        let Standing::Open(open) = standing else {
            unreachable!("expected an open claim, got {standing:?}");
        };
        open
    }

    #[test]
    fn a_task_nobody_was_granted_is_unclaimed_whoever_else_holds_claims() {
        let log = [
            granted(0, "a01", 1, 10, "t01"),
            granted(1, "a02", 2, 11, "t02"),
        ];
        assert_eq!(standing(&log, "a01", "t02"), Standing::Unclaimed);
        assert_eq!(standing(&log, "a03", "t01"), Standing::Unclaimed);
        assert_eq!(standing(&[], "a01", "t01"), Standing::Unclaimed);
    }

    #[test]
    fn a_granted_claim_is_open_with_its_latest_fence_and_every_scope() {
        let amended = event(
            2,
            EventKind::ClaimAmended {
                claim: ClaimId(1),
                fence: Fence(12),
                added: vec![file("b.ts", tessel_coordinator::protocol::Mode::Create)],
            },
        );
        let other = event(
            3,
            EventKind::ClaimAmended {
                claim: ClaimId(2),
                fence: Fence(99),
                added: Vec::new(),
            },
        );
        let log = [
            granted(0, "a01", 1, 10, "t01"),
            granted(1, "a02", 2, 11, "t02"),
            amended,
            other,
        ];
        let open = open_of(standing(&log, "a01", "t01"));
        assert_eq!((open.claim, open.fence), (ClaimId(1), Fence(12)));
        assert_eq!(open.scopes.len(), 2);
        assert!(!open.submitted);
    }

    #[test]
    fn a_submitted_claim_is_open_and_marked_submitted_until_it_merges() {
        let submitted = |seq, claim| {
            event(
                seq,
                EventKind::Submitted {
                    claim: ClaimId(claim),
                    fork_commit: tessel_coordinator::protocol::CommitId("c".into()),
                    touched: Vec::new(),
                    decisions: tessel_coordinator::protocol::DecisionRecord::default(),
                },
            )
        };
        let mut log = vec![
            granted(0, "a01", 1, 10, "t01"),
            granted(1, "a02", 2, 11, "t02"),
            submitted(2, 2),
        ];
        assert!(
            !open_of(standing(&log, "a01", "t01")).submitted,
            "claim 2's"
        );
        log.push(submitted(3, 1));
        assert!(open_of(standing(&log, "a01", "t01")).submitted);
        let head = tessel_coordinator::protocol::CommitId("h".into());
        log.push(event(
            4,
            EventKind::Merged {
                claim: ClaimId(1),
                head,
            },
        ));
        assert_eq!(standing(&log, "a01", "t01"), Standing::Merged);
        log.push(released(5, 1, ReleaseReason::Merged));
        assert_eq!(standing(&log, "a01", "t01"), Standing::Merged);
    }

    #[test]
    fn a_rejected_claim_stays_open_for_release_until_the_agent_releases_it() {
        let rejected = event(
            1,
            EventKind::SubmitRejected {
                claim: ClaimId(1),
                reason: "tests failed".into(),
            },
        );
        let mut log = vec![granted(0, "a01", 1, 10, "t01"), rejected];
        let Standing::Rejected { reason, open } = standing(&log, "a01", "t01") else {
            unreachable!("expected a rejection");
        };
        assert_eq!(reason, "tests failed");
        assert_eq!(open.map(|o| o.claim), Some(ClaimId(1)));
        log.push(released(2, 1, ReleaseReason::Agent));
        let Standing::Rejected { open, .. } = standing(&log, "a01", "t01") else {
            unreachable!("expected a rejection");
        };
        assert_eq!(open, None, "released, nothing left to release");
    }

    #[test]
    fn a_claim_released_other_than_by_a_merge_is_lapsed_unless_the_agent_released_it() {
        let lapsed = |reason| {
            let log = [granted(0, "a01", 1, 10, "t01"), released(1, 1, reason)];
            standing(&log, "a01", "t01")
        };
        assert_eq!(lapsed(ReleaseReason::LeaseExpired), Standing::Lapsed);
        assert_eq!(lapsed(ReleaseReason::LostRace), Standing::Lapsed);
        assert_eq!(lapsed(ReleaseReason::Settled), Standing::Lapsed);
        assert_eq!(lapsed(ReleaseReason::Agent), Standing::Released);
        assert_eq!(lapsed(ReleaseReason::Merged), Standing::Merged);
    }

    #[test]
    fn another_claims_release_does_not_end_this_one() {
        let log = [
            granted(0, "a01", 1, 10, "t01"),
            granted(1, "a02", 2, 11, "t02"),
            released(2, 2, ReleaseReason::LeaseExpired),
        ];
        open_of(standing(&log, "a01", "t01"));
    }

    #[test]
    fn review_decided_names_the_claim_and_nothing_else_does() {
        let decided = EventKind::ReviewDecided {
            claim: ClaimId(7),
            approve: true,
            note: None,
            reviewer: None,
        };
        assert_eq!(review_decided(&decided), Some(ClaimId(7)));
        let requested = EventKind::ReviewRequested {
            claim: ClaimId(7),
            reasons: Vec::new(),
        };
        assert_eq!(review_decided(&requested), None);
    }

    fn queued(seq: u64, agent: &str, req: u64, task_ref: &str) -> Event {
        event(
            seq,
            EventKind::WaitQueued {
                agent: AgentId(agent.into()),
                req: RequestId(req),
                scopes: vec![file("a.ts", tessel_coordinator::protocol::Mode::EditBody)],
                intent: tessel_coordinator::protocol::Intent {
                    summary: "s".into(),
                    task_ref: Some(task_ref.into()),
                    assumptions: Vec::new(),
                },
                position: 1,
            },
        )
    }

    #[test]
    fn a_queued_request_stays_queued_until_it_is_withdrawn_or_granted() {
        let withdrawn = |req| {
            event(
                1,
                EventKind::WaitWithdrawn {
                    agent: AgentId("a01".into()),
                    req: RequestId(req),
                },
            )
        };
        let mut log = vec![queued(0, "a01", 5, "t01")];
        let Standing::Queued { req, scopes } = standing(&log, "a01", "t01") else {
            unreachable!("expected a queued request");
        };
        assert_eq!((req, scopes.len()), (RequestId(5), 1));
        assert_eq!(standing(&log, "a02", "t01"), Standing::Unclaimed);
        log.push(withdrawn(9));
        assert!(matches!(
            standing(&log, "a01", "t01"),
            Standing::Queued { .. }
        ));
        log.push(withdrawn(5));
        assert_eq!(standing(&log, "a01", "t01"), Standing::Unclaimed);
        log.push(queued(2, "a01", 6, "t01"));
        log.push(granted(3, "a01", 1, 10, "t01"));
        open_of(standing(&log, "a01", "t01"));
    }

    #[test]
    fn another_agents_withdrawal_with_the_same_request_id_leaves_the_request_queued() {
        let withdrawn = event(
            1,
            EventKind::WaitWithdrawn {
                agent: AgentId("a02".into()),
                req: RequestId(5),
            },
        );
        let log = [queued(0, "a01", 5, "t01"), withdrawn];
        assert!(matches!(
            standing(&log, "a01", "t01"),
            Standing::Queued { .. }
        ));
    }

    #[test]
    fn denials_are_counted_for_the_agent_and_task_alone() {
        let denied = |seq, agent: &str, task_ref: &str| {
            event(
                seq,
                EventKind::ClaimDenied {
                    agent: AgentId(agent.into()),
                    scopes: Vec::new(),
                    intent: tessel_coordinator::protocol::Intent {
                        summary: "s".into(),
                        task_ref: Some(task_ref.into()),
                        assumptions: Vec::new(),
                    },
                    conflicts: Vec::new(),
                },
            )
        };
        let log = [
            denied(0, "a01", "t01"),
            denied(1, "a01", "t01"),
            denied(2, "a02", "t01"),
            denied(3, "a01", "t02"),
        ];
        assert_eq!(denials(&log, "a01", "t01"), 2);
        assert_eq!(denials(&log, "a03", "t01"), 0);
    }
}

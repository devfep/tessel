//! A `Land` in doubt: the lander was asked to move main and cannot say whether it did.
//!
//! `Unknown` to a `Land` (a lost reply, a timeout, a restart that found the marker) says nothing
//! about the code, and the work may already be on main, so rejecting it would report a false
//! outcome. The merge stays in flight as a `MergeStage::Reconcile` under the `Land`'s dispatch id,
//! and the core asks the lander whether the journaled head is an ancestor of main:
//! - yes: the work landed, from the base the `Land` leased;
//! - no, and main is still at that base: the same `Land` goes out again, same dispatch id, base
//!   and head, so the candidate ref it pushes is the one already journaled;
//! - no, and main is elsewhere (or the answer does not say): the head cannot land any more, since
//!   a `Land` is a compare-and-swap on its base, so the claim goes back to `Prepare` under a new
//!   dispatch id as contention (`moved`), and the doubt is over.
//!
//! None of it counts toward `MAX_INFRA_RETRIES`, and none of it rejects. What bounds it is the
//! rate: each `Unknown`, from the `Land` or from a `Reconcile`, waits the capped infrastructure
//! backoff before the next `Reconcile`. A restart that finds a `Land` marker reconciles at once.
//! The operator's way out is `Coordinator::resolve_doubt`, which feeds the core the answer a
//! `Reconcile` would have given.

use super::MergeStage;
use crate::coordinator::{ActiveClaim, Coordinator, Effect, InFlight, Submission};
use crate::merge::{infra_backoff_ms, MAX_INFRA_RETRIES};
use crate::protocol::{ClaimId, CommitId, ServerMsg};

/// What the lander's `Reconcile` found out about the `Land` in doubt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The journaled head is an ancestor of main: the work landed.
    Landed,
    /// The head is not on main. `main` is where main is, if the answer said.
    NotLanded { main: Option<CommitId> },
    /// The reconcile itself got no answer.
    Unknown,
}

/// `Coordinator::resolve_doubt` found nothing to resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoLandInDoubt {
    pub claim: ClaimId,
}

impl std::fmt::Display for NoLandInDoubt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "claim {} has no Land in doubt", self.claim.0)
    }
}

impl std::error::Error for NoLandInDoubt {}

/// Record that an attempt may have landed. The first baseline is kept through later doubts: a
/// landing found later spans from the first main the work may have been landed on.
pub(super) fn mark_doubt(work: &mut Submission, from: Option<CommitId>) {
    if !work.in_doubt {
        work.in_doubt = true;
        work.unanswered_from = from;
    }
}

/// How the next `Reconcile` is paced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pace {
    /// The lander said `Unknown`: wait the capped backoff of the doubts so far.
    Backoff,
    /// A restart found the marker: this process's, not the lander's failure, so no wait.
    AtOnce,
}

impl Coordinator {
    /// The `Land` of `flight` is in doubt: keep the merge in flight as a `Reconcile` under the
    /// same dispatch id. Counts nothing. Tells the submitter the first time.
    pub(super) fn enter_doubt(
        &mut self,
        flight: InFlight,
        held: ActiveClaim,
        (base, head): (CommitId, CommitId),
        (pace, now_ms): (Pace, u64),
    ) -> Vec<Effect> {
        let Some(mut work) = held.work.clone() else {
            return Vec::new();
        };
        let mut doubts = flight.doubts;
        if pace == Pace::Backoff {
            doubts += 1;
            let wait = infra_backoff_ms(doubts.min(MAX_INFRA_RETRIES));
            work.retry_at_ms = Some(now_ms.saturating_add(wait));
        }
        mark_doubt(&mut work, Some(base.clone()));
        let first = flight.doubt_since_ms.is_none();
        let since = flight.doubt_since_ms.unwrap_or(now_ms);
        let agent = held.agent.clone();
        self.keep_work(flight.claim, held, work);
        let claim = flight.claim;
        self.state.merge_in_flight = Some(InFlight {
            stage: MergeStage::Reconcile { base, head },
            doubts,
            doubt_since_ms: Some(since),
            ..flight
        });
        if !first {
            return Vec::new();
        }
        let msg = ServerMsg::MergeDoubt {
            claim,
            since_ms: Some(since),
        };
        vec![Effect::Notify { agent, msg }]
    }

    /// The doubt about `claim`'s `Land` is over and the work did not land: forget the baseline and
    /// the wait, and tell the submitter if it was told of the doubt.
    pub(super) fn end_doubt(
        &mut self,
        claim: ClaimId,
        held: ActiveClaim,
        told: bool,
    ) -> (ActiveClaim, Vec<Effect>) {
        let agent = held.agent.clone();
        let work = held.work.clone().map(|work| Submission {
            in_doubt: false,
            unanswered_from: None,
            retry_at_ms: None,
            ..work
        });
        let held = ActiveClaim { work, ..held };
        if !told {
            return (held, Vec::new());
        }
        let msg = ServerMsg::MergeDoubt {
            claim,
            since_ms: None,
        };
        (held, vec![Effect::Notify { agent, msg }])
    }

    /// The answer to the `Reconcile` of the merge in flight under `dispatch`. Only that dispatch
    /// can be answered; any other answer is stale and changes nothing.
    pub fn reconcile_outcome(
        &mut self,
        dispatch: u64,
        found: &Reconciliation,
        now_ms: u64,
    ) -> Vec<Effect> {
        let now_ms = self.advance_clock(now_ms);
        let Some(flight) = self.state.merge_in_flight.take_if(|flight| {
            flight.dispatch == dispatch && matches!(flight.stage, MergeStage::Reconcile { .. })
        }) else {
            return Vec::new();
        };
        let MergeStage::Reconcile { base, head } = flight.stage.clone() else {
            return Vec::new();
        };
        let claim = flight.claim;
        let held = self.state.claims.get(&claim.0).cloned();
        let Some(held) = held.filter(|held| held.submitted.is_some() && held.work.is_some()) else {
            return Vec::new();
        };
        let told = flight.doubt_since_ms.is_some();
        match found {
            Reconciliation::Landed => self.land(claim, &held, Some(base), head, now_ms),
            Reconciliation::NotLanded { main: Some(main) } if *main == base => {
                self.resend_land(flight, held, (base, head))
            }
            Reconciliation::NotLanded {
                main: Some(_) | None,
            } => {
                let (held, mut effects) = self.end_doubt(claim, held, told);
                effects.extend(self.retry_after_move(claim, held, now_ms));
                effects
            }
            Reconciliation::Unknown => {
                self.enter_doubt(flight, held, (base, head), (Pace::Backoff, now_ms))
            }
        }
    }

    /// Main is still at the base the `Land` leased and does not hold the head: the same `Land`
    /// goes out again under its dispatch id, which `begin_merge` offers.
    fn resend_land(
        &mut self,
        flight: InFlight,
        held: ActiveClaim,
        (base, head): (CommitId, CommitId),
    ) -> Vec<Effect> {
        let Some(work) = held.work.clone() else {
            return Vec::new();
        };
        let work = Submission {
            retry_at_ms: None,
            ..work
        };
        self.keep_work(flight.claim, held, work);
        self.state.merge_in_flight = Some(InFlight {
            stage: MergeStage::Land { base, head },
            ..flight
        });
        Vec::new()
    }

    /// The operator's answer for the `Land` of `claim` that is in doubt, given after reading main:
    /// the core is fed what a `Reconcile` would have answered, so its rules still apply. `landed`
    /// lands the work from the base the `Land` leased; otherwise the claim goes back to `Prepare`,
    /// which is safe whatever main is (a head already on main is answered `already_merged`).
    /// Refused when `claim` has no `Land` in flight, in doubt or not yet recovered after a restart.
    pub fn resolve_doubt(
        &mut self,
        claim: ClaimId,
        landed: bool,
        now_ms: u64,
    ) -> Result<Vec<Effect>, NoLandInDoubt> {
        let now_ms = now_ms.max(self.state.clock_ms);
        let flight = self.state.merge_in_flight.clone();
        let Some(flight) = flight.filter(|flight| flight.claim == claim) else {
            return Err(NoLandInDoubt { claim });
        };
        let held = self.state.claims.get(&claim.0).cloned();
        let Some(held) = held.filter(|held| held.submitted.is_some() && held.work.is_some()) else {
            return Err(NoLandInDoubt { claim });
        };
        let dispatch = flight.dispatch;
        let mut effects = Vec::new();
        match flight.stage.clone() {
            MergeStage::Land { base, head } => {
                self.state.merge_in_flight = None;
                let pace = (Pace::AtOnce, now_ms);
                effects.extend(self.enter_doubt(flight, held, (base, head), pace));
            }
            MergeStage::Reconcile { .. } => {}
            MergeStage::Prepare | MergeStage::Test { .. } | MergeStage::Unstaged => {
                return Err(NoLandInDoubt { claim })
            }
        }
        let found = if landed {
            Reconciliation::Landed
        } else {
            Reconciliation::NotLanded { main: None }
        };
        effects.extend(self.reconcile_outcome(dispatch, &found, now_ms));
        Ok(effects)
    }
}

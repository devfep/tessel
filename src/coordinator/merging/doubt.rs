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
//! The doubt outlives the `Land`. A copy of it that has not run yet may land the head any time
//! main is still at its base, and the core's own `Prepare` re-makes the candidate ref it needs. The
//! doubt carries that `Land`'s own `(base, head)` (`Submission::doubted_land`, written at every
//! doubt), and while it does nothing in the merge rejects the work: a refusal at `Test` or `Land`,
//! a missing candidate, a failed `Prepare`, a cut-off and a restart all send it back to `Prepare`
//! after the capped backoff (`wait_in_doubt`), counting nothing. A refusal at `Prepare` asks a
//! `Reconcile` first, after that backoff (`reconcile_refused_prepare`), since it says nothing of
//! where main is. A `Prepare` that answers the doubted pair goes straight to `Land`. The doubt
//! ends only when the work lands, when the operator says it did not, or when main is known to have
//! left the doubted base: a `Prepared` on another base, a `MainMoved` once a `Reconcile` has found
//! the head not on main, a `MainMoved` answered to a `Land`, or a reconcile that finds main
//! elsewhere.
//!
//! A doubt with no pair (a marker stored before the staged landing) has no late `Land` to fence:
//! a refusal rejects it as it did, an infrastructure failure counts and is rejected at
//! `MAX_INFRA_RETRIES` like any other, and a `Prepared` never ends it. It cannot be resolved by
//! the operator, who resolves a `Land`; the runbook's "Nothing in flight" check forbids upgrading
//! with one.
//!
//! None of it counts toward `MAX_INFRA_RETRIES`, and none of it rejects. What bounds it is the
//! rate: each `Unknown`, from the `Land` or from a `Reconcile`, waits the capped infrastructure
//! backoff before the next `Reconcile`. A restart that finds a `Land` marker reconciles at once.
//! The operator's way out is `Coordinator::resolve_doubt`, which feeds the core the answer a
//! `Reconcile` would have given.

use super::{MergeStage, Rejection};
use crate::coordinator::{ActiveClaim, Coordinator, Effect, InFlight, Submission};
use crate::merge::{infra_backoff_ms, Verdict, MAX_INFRA_RETRIES};
use crate::protocol::{AgentId, ClaimId, ClaimInDoubt, CommitId, ServerMsg};

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
/// landing found later spans from the first main the work may have been landed on. `land` is the
/// `(base, head)` of the `Land` in doubt, if there is one; it replaces the pair of an earlier
/// doubt, since it is the lease of the latest `Land` that a late copy can still use.
pub(super) fn mark_doubt(
    work: &mut Submission,
    from: Option<CommitId>,
    land: Option<(CommitId, CommitId)>,
) {
    if !work.in_doubt {
        work.in_doubt = true;
        work.unanswered_from = from;
    }
    if land.is_some() {
        work.doubted_land = land;
    }
}

/// The submission is in doubt of a `Land` whose lease a late copy can still use.
pub(super) fn is_fenced(work: &Submission) -> bool {
    work.in_doubt && work.doubted_land.is_some()
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
        mark_doubt(
            &mut work,
            Some(base.clone()),
            Some((base.clone(), head.clone())),
        );
        let first = flight.doubt_since_ms.is_none();
        let since = flight.doubt_since_ms.unwrap_or(now_ms);
        let (claim, agent) = (flight.claim, held.agent.clone());
        self.keep_work(claim, held, work);
        self.state.merge_in_flight = Some(InFlight {
            stage: MergeStage::Reconcile { base, head },
            doubts,
            doubt_since_ms: Some(since),
            ..flight
        });
        Self::doubt_notice(first, claim, agent, since)
    }

    /// The submission is in doubt and a step of the merge failed or was refused: nothing is
    /// counted and nothing is rejected. After the doubts' capped backoff the merge goes back to
    /// `Prepare` under a new dispatch id, which makes the candidate ref again for a copy of the
    /// `Land` that has not run yet.
    pub(super) fn wait_in_doubt(
        &mut self,
        flight: InFlight,
        held: ActiveClaim,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Some(mut work) = held.work.clone() else {
            return Vec::new();
        };
        let doubts = flight.doubts + 1;
        let wait = infra_backoff_ms(doubts.min(MAX_INFRA_RETRIES));
        work.retry_at_ms = Some(now_ms.saturating_add(wait));
        let first = flight.doubt_since_ms.is_none();
        let since = flight.doubt_since_ms.unwrap_or(now_ms);
        let (claim, agent) = (flight.claim, held.agent.clone());
        self.keep_work(claim, held, work);
        let dispatch = self.state.last_dispatch + 1;
        self.state.last_dispatch = dispatch;
        self.state.merge_in_flight = Some(InFlight {
            dispatch,
            stage: MergeStage::Prepare,
            doubts,
            doubt_since_ms: Some(since),
            ..flight
        });
        Self::doubt_notice(first, claim, agent, since)
    }

    /// A `Prepare` or a `Test` of a submission in doubt of the `pair` cannot say where main is
    /// (`MainMoved` may be a late copy of that `Land`: the lander's pin and its clone are separate
    /// reads; a refusal says nothing of the base at all), so the doubt is neither ended nor waited
    /// out on its word. A `Reconcile` under a new dispatch id asks whether the head is on main;
    /// its answer lands the work, sends the `Land` again or ends the doubt as any `Reconcile`
    /// does. Nothing is counted. `pace` says whether it goes out at once or after the backoff.
    pub(super) fn reconcile_before_ending(
        &mut self,
        flight: InFlight,
        held: ActiveClaim,
        pair: (CommitId, CommitId),
        (pace, now_ms): (Pace, u64),
    ) -> Vec<Effect> {
        let dispatch = self.state.last_dispatch + 1;
        self.state.last_dispatch = dispatch;
        let flight = InFlight { dispatch, ..flight };
        let work = held.work.clone().map(|work| Submission {
            retry_at_ms: None,
            ..work
        });
        let held = ActiveClaim { work, ..held };
        self.enter_doubt(flight, held, pair, (pace, now_ms))
    }

    /// The `Prepare` of a submission in doubt was refused. A refusal carries no base, and the
    /// ones that persist (a fork branch gone, a conflict after main moved outside the queue) say
    /// nothing of whether main is still where the `Land` leased it, so a `Reconcile` asks, paced
    /// by the doubts' backoff: main at the base sends the `Land` again, main elsewhere ends the
    /// doubt so that the next `Prepare` rejects, and the head on main lands the work.
    pub(super) fn reconcile_refused_prepare(
        &mut self,
        flight: InFlight,
        held: ActiveClaim,
        now_ms: u64,
    ) -> Vec<Effect> {
        let pair = held
            .work
            .as_ref()
            .and_then(|work| work.doubted_land.clone());
        let Some(pair) = pair else {
            return self.wait_in_doubt(flight, held, now_ms);
        };
        self.reconcile_before_ending(flight, held, pair, (Pace::Backoff, now_ms))
    }

    /// Tell the submitter their landing is in doubt, the first time only.
    fn doubt_notice(first: bool, claim: ClaimId, agent: AgentId, since: u64) -> Vec<Effect> {
        if !first {
            return Vec::new();
        }
        let msg = ServerMsg::MergeDoubt {
            claim,
            since_ms: Some(since),
        };
        vec![Effect::Notify { agent, msg }]
    }

    /// The doubt about `claim`'s `Land` is over and the work did not land: forget the baseline, the
    /// doubted pair and the wait, and tell the submitter if it was told of the doubt. A doubt the
    /// submitter was not told of is left as it is: it began before the staged landing, and its
    /// baseline outlives a move of main (see `Submission::unanswered_from`).
    pub(super) fn end_doubt(
        &mut self,
        claim: ClaimId,
        held: ActiveClaim,
        told: bool,
    ) -> (ActiveClaim, Vec<Effect>) {
        if !told {
            return (held, Vec::new());
        }
        let agent = held.agent.clone();
        let work = held.work.clone().map(|work| Submission {
            in_doubt: false,
            unanswered_from: None,
            doubted_land: None,
            retry_at_ms: None,
            ..work
        });
        let held = ActiveClaim { work, ..held };
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
        let at_reconcile = self.state.merge_in_flight.as_ref();
        let Some(MergeStage::Reconcile { base, head }) = at_reconcile
            .filter(|flight| flight.dispatch == dispatch)
            .map(|flight| flight.stage.clone())
        else {
            return Vec::new();
        };
        let Some(flight) = self.state.merge_in_flight.take() else {
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
            Reconciliation::NotLanded { main: Some(_) } => {
                let (held, mut effects) = self.end_doubt(claim, held, told);
                effects.extend(self.retry_after_move(claim, held, now_ms));
                effects
            }
            // Only the operator leaves main unsaid. Nobody said main moved, so this is no
            // contention: the claim goes back to `Prepare` and nothing is counted.
            Reconciliation::NotLanded { main: None } => {
                let (held, effects) = self.end_doubt(claim, held, told);
                self.state.claims.insert(claim.0, held);
                effects
            }
            Reconciliation::Unknown => {
                self.enter_doubt(flight, held, (base, head), (Pace::Backoff, now_ms))
            }
        }
    }

    /// The claims of `agent` whose landing is in doubt now, for the `Welcome` it gets.
    pub(in crate::coordinator) fn merge_doubts_of(&self, agent: &AgentId) -> Vec<ClaimInDoubt> {
        let Some(doubt) = self.merge_in_doubt() else {
            return Vec::new();
        };
        let owner = self
            .state
            .claims
            .get(&doubt.claim.0)
            .map(|held| &held.agent);
        if owner == Some(agent) {
            vec![doubt]
        } else {
            Vec::new()
        }
    }

    /// The claim whose landing is in doubt now, if one is, and when the doubt began.
    pub fn merge_in_doubt(&self) -> Option<ClaimInDoubt> {
        let flight = self.state.merge_in_flight.as_ref()?;
        Some(ClaimInDoubt {
            claim: flight.claim,
            since_ms: flight.doubt_since_ms?,
        })
    }

    /// The base and head that the `Land` of `claim` journaled, while that `Land` (or the
    /// `Reconcile` that follows it) is the merge in flight, or while the claim is still in doubt
    /// at another stage of the same merge. The lander rebases, so this head, not
    /// the submitter's fork commit, is what must be on main for the work to have landed.
    pub fn journaled_land(&self, claim: ClaimId) -> Option<(CommitId, CommitId)> {
        let flight = self.state.merge_in_flight.as_ref()?;
        if flight.claim != claim {
            return None;
        }
        match &flight.stage {
            MergeStage::Land { base, head } | MergeStage::Reconcile { base, head } => {
                Some((base.clone(), head.clone()))
            }
            MergeStage::Prepare | MergeStage::Test { .. } | MergeStage::Unstaged => {
                self.doubted_pair(claim)
            }
        }
    }

    /// The base and head of the `Land` that left `claim` in doubt, kept while the merge goes
    /// round `Prepare` and `Test` again.
    fn doubted_pair(&self, claim: ClaimId) -> Option<(CommitId, CommitId)> {
        let work = self.state.claims.get(&claim.0)?.work.as_ref()?;
        work.in_doubt.then(|| work.doubted_land.clone())?
    }

    /// What the lander's answer means to a merge at its `Land` (or at the `Reconcile` that follows
    /// one). Only a landing of the journaled head acts at a `Reconcile`; a late answer to the
    /// `Land` otherwise says nothing the `Reconcile` will not. At a `Land`, a move of main ends a
    /// doubt a resent `Land` was in, and says so. A refusal or a missing candidate does not end
    /// it: a copy of the `Land` that has not run yet may still land the head, so a submission in
    /// doubt goes back to `Prepare` and is not rejected (`wait_in_doubt`). An answer the `Land`
    /// did not ask for leaves it in doubt, reconciled and not counted.
    pub(super) fn land_stage_outcome(
        &mut self,
        flight: InFlight,
        held: ActiveClaim,
        verdict: Verdict,
        now_ms: u64,
    ) -> Vec<Effect> {
        let claim = flight.claim;
        let (reconciling, base, head) = match flight.stage.clone() {
            MergeStage::Land { base, head } => (false, base, head),
            MergeStage::Reconcile { base, head } => (true, base, head),
            MergeStage::Prepare | MergeStage::Test { .. } | MergeStage::Unstaged => {
                self.state.merge_in_flight = Some(flight);
                return Vec::new();
            }
        };
        if let Verdict::Landed { base, head: landed } = &verdict {
            if *landed == head {
                return self.land(claim, &held, Some(base.clone()), landed.clone(), now_ms);
            }
        }
        if reconciling {
            self.state.merge_in_flight = Some(flight);
            return Vec::new();
        }
        let told = flight.doubt_since_ms.is_some();
        let fenced = held.work.as_ref().is_some_and(is_fenced);
        match verdict {
            Verdict::Rejected { .. } if fenced => self.wait_in_doubt(flight, held, now_ms),
            Verdict::Rejected { reason } => {
                self.reject_work(claim, held, Rejection::plain(reason), now_ms)
            }
            Verdict::MainMoved => {
                let (held, mut effects) = self.end_doubt(claim, held, told);
                effects.extend(self.retry_after_move(claim, held, now_ms));
                effects
            }
            Verdict::CandidateMissing => self.retry_after_missing_candidate(flight, held, now_ms),
            Verdict::Prepared { .. }
            | Verdict::Tested
            | Verdict::Landed { .. }
            | Verdict::AlreadyLanded { .. }
            | Verdict::Infrastructure => {
                self.enter_doubt(flight, held, (base, head), (Pace::Backoff, now_ms))
            }
        }
    }

    /// Main is still at the base the `Land` leased and does not hold the head: the same `Land`
    /// goes out again under its dispatch id, which `begin_merge` offers. Its pair is kept as the
    /// doubted one, which gives a doubt stored before the pair existed the pair it lacks.
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
            doubted_land: Some((base.clone(), head.clone())),
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
    /// Refused when `claim` has no `Land` in flight, in doubt at any stage of its merge or not yet
    /// recovered after a restart.
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
                let Some(pair) = self.doubted_pair(claim) else {
                    return Err(NoLandInDoubt { claim });
                };
                self.state.merge_in_flight = None;
                effects.extend(self.enter_doubt(flight, held, pair, (Pace::AtOnce, now_ms)));
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

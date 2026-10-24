//! Assumption verification: after a submission that challenged assumptions merges, the lander
//! tries each assuming agent's work on the new main, and the result is logged (invariant 8, and
//! invariant 10 for the evidence rule). A challenge is a warning at submit time; only this turns it
//! into evidence, so `Summary::assumptions_confirmed_broken` counts verified outcomes only.
//!
//! Decisions made here:
//! - Verifications are recorded when the challenging submission lands, one per challenged
//!   (claim, assumption), for claims still live. They are part of the persisted state, so they are
//!   stored before anything is sent (CLAUDE.md rule 6).
//! - A landing whose baseline is unknown queues nothing: a retry answered `already_merged` with no
//!   earlier attempt in doubt, or with main unchanged since that attempt began, has no span to
//!   compare. The challenged assumptions stay unverified, and the landing says so: `Merged` lists
//!   them as `unverified` (those whose assuming claim is still live, the ones that would have
//!   been queued) and the submitted shadow claims it did not try as `untried_shadows`.
//!   `Summary::verifications_unverifiable` counts the assumptions. It is neither a verified
//!   outcome nor a drop, and no baseline is invented for it.
//! - The commit to try is chosen when the trial is dispatched, not when it is recorded: the
//!   assuming claim's submitted `fork_commit` at that moment; for a claim that has not submitted
//!   it is `None`: there is nothing to try, and the trial ends Inconclusive.
//! - Each trial carries a baseline, `before`: main as it was before the merge that challenged the
//!   assumption. The lander tries the work on `before` first, and a failure on the new main counts
//!   only if the work was clean there (`TrialReport::verdict`). Without it, work that was already
//!   failing, or that clashes with an unrelated merge, would be counted as an assumption broken by
//!   this one.
//! - One verification runs at a time, and only while no merge is due: merges have priority. A
//!   merge in backoff does not hold a verification up. Priority is bounded (invariant I12 of
//!   `docs/proposals/real-use.md`): after `verify_slot_after` merges (`VERIFY_SLOT_AFTER`,
//!   4; never stored) are dispatched in a row while the head verification is ready (queued,
//!   not in backoff, none in flight), the next `begin_merge` returns `None` and the verification
//!   is dispatched first, which resets the count. A merge dispatched while no verification is
//!   ready also resets it, so only an unbroken run of merges past a ready verification counts. A
//!   merge in flight is never interrupted. Merge and verification never overlap: `begin_merge`
//!   and `next_merge_ms` return `None` while a verification is in flight, whatever the host's
//!   wakes do, and `begin_verification` refuses while a merge is due or in flight. Time-outs, the
//!   watchdog and the bounded infrastructure retries are the merge queue's
//!   (`merge::STEWARD_CALL_TIMEOUT_MS`, `MERGE_WATCHDOG_MS`, `MAX_INFRA_RETRIES`). The lander
//!   runs the two sides of a trial at the same time; a side that outlives the call timeout is an
//!   infrastructure failure, so the verification ends `Inconclusive` after the retries (see
//!   `STEWARD_CALL_TIMEOUT_MS`).
//! - A claim that ends before its verification runs (released, expired, lost a race or merged)
//!   drops it. An assumption verification is logged as `VerificationDropped` with the claim, the
//!   assumption and the reason the claim ended, and `Summary` counts it as `verifications_dropped`,
//!   apart from every verified outcome: nothing was tried, so it is never a broken or a held
//!   assumption. Race entry and shadow trials are dropped without an event; the race and
//!   `DenialVerified` are their record. A verification already running for a claim that ends is
//!   dropped (and logged) at once and has its answer discarded.
//! - Back-to-back merges that challenge the same (claim, assumption) before it runs keep only the
//!   newest main, with the baseline of the oldest of them: the second started from the main the
//!   first produced, so the span holds only challenging merges. If another merge came between,
//!   the break could be its doing, so the verifications stay separate, each with its own baseline. A verification already running is left to finish: it is evidence about the
//!   older main, and the newer one queues behind it.
//! - What the log records is the protocol `Outcome` (`TrialReport::verdict`). Anything that is
//!   not a verdict about the work (nothing to test, the commit is not on the fork, a timeout,
//!   infrastructure that kept failing) is `Inconclusive`, which no counter treats as broken.
//! - A race entry's tests are tried by the same queue (invariant 7, see `racing`): one trial per
//!   entry with `before` and `main` both the head when the race closed, which the lander runs once
//!   and reads as both sides. What the trial says is read by `TrialReport::tests_passed`, not by
//!   `verdict`: there is no baseline to compare against. Nothing about a race trial is logged as
//!   an `AssumptionVerified`; the race's own events are the record.
//! - A shadow claim's denial is verified by the same queue (invariant 10). When the claim that
//!   blocked a shadow claim merges, one trial per shadow claim it blocked is queued, with the
//!   shadow claim's submitted commit tried on the merge's `before` and `main`; the answer is logged
//!   as `DenialVerified`, which `Summary` counts as a prevention (a conflict) or a false alarm
//!   (clean) and ignores when `Inconclusive`. The baseline rule is the same: work that was already
//!   failing before the merge is `Inconclusive`, not a prevention. Nothing is queued, and nothing
//!   is logged, when the blocker ends without merging (no work landed to conflict with), when the
//!   shadow claim had not submitted by then (there is no commit to try; one that submits later
//!   has no baseline from before the merge, so it is not tried either). A shadow claim blocked by
//!   several claims is tried once for each that merges, each against its own baseline.
//! - A submitted shadow claim holds no lock, lease or queue place, so it is dropped as soon as
//!   nothing is owed through it: none of its blockers is still a live claim and none of its trials
//!   is queued or running (`drop_finished_shadows`). That covers a shadow whose blockers all
//!   merged and were tried, whose blockers all ended without merging, and one blocked only by a
//!   race (no blocker to wait for, so it can never be tried). A shadow with a live blocker stays:
//!   the blocker may still merge. A drop logs `ClaimReleased` with `ReleaseReason::Settled`: the
//!   log then says the claim ended, which no other reason says truly (the work was not merged, the
//!   agent did not release it, no lease ran out). `DenialVerified` stays the record of the outcome.
//! - A conflict is not sent to the assuming agent as a message: no `ServerMsg` says "your
//!   assumption broke" (`AssumptionChallenged` says "re-check it" and would be read as a second
//!   challenge). The event is the record, and watchers receive it.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{ClaimKind, Coordinator, Effect};
use crate::merge::{
    infra_backoff_ms, TestsVerdict, TrialReport, TrialVerdict, MAX_INFRA_RETRIES, MERGE_WATCHDOG_MS,
};
use crate::protocol::{
    AgentId, Assumption, ClaimId, CommitId, EventKind, Outcome, RaceId, ReleaseReason,
    UnverifiedAssumption,
};

/// The merges that may be dispatched in a row while a verification is ready. Not stored: every
/// load restores it.
const VERIFY_SLOT_AFTER: u32 = 4;

pub(super) fn default_slot_after() -> u32 {
    VERIFY_SLOT_AFTER
}

/// An assumption a submission challenged at submit time, kept with the submission until it merges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Challenged {
    pub(super) claim: ClaimId,
    pub(super) assumption: Assumption,
}

/// What a trial is for. Untagged, so a verification stored before races, which has an
/// `assumption` field, loads as `Assumption`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum Subject {
    /// An assumption a merge challenged (invariant 8).
    Assumption { assumption: Assumption },
    /// The tests of a race entry (invariant 7).
    RaceEntry { race: RaceId },
    /// A shadow claim's work, tried against the merge of the claim that blocked it (invariant 10).
    Shadow { blocking_claim: ClaimId },
}

/// One trial to run: an assuming agent's work, or a race entry, tried on `main`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Verification {
    id: u64,
    agent: AgentId,
    pub(super) claim: ClaimId,
    #[serde(flatten)]
    pub(super) subject: Subject,
    /// Main before the merge that challenged the assumption: the baseline of the trial.
    pub(super) before: CommitId,
    /// Main after the merge that challenged the assumption.
    pub(super) main: CommitId,
    /// Infrastructure failures so far.
    #[serde(default)]
    infra_failures: u32,
    /// Not dispatched before this instant: the backoff after an infrastructure failure.
    #[serde(default)]
    pub(super) retry_at_ms: Option<u64>,
}

/// The verification in flight: persisted before the lander is called.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct VerifyFlight {
    /// The queued verification being run.
    id: u64,
    /// The dispatch the answer must carry (see `CoordinatorState::last_dispatch`). 0 in a state
    /// stored before dispatch ids existed, which no issued id equals.
    #[serde(default)]
    dispatch: u64,
}

/// One trial for the shell to ask the lander for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyDispatch {
    /// Unique to this dispatch, never reused, even for a retry of the same verification. The
    /// answer carries it to `verification_outcome`.
    pub id: u64,
    pub agent: AgentId,
    /// The assuming claim's submitted commit now; `None`: not submitted, nothing to try.
    pub commit: Option<CommitId>,
    pub before: CommitId,
    pub main: CommitId,
    /// 1 for the first dispatch, counting every retry.
    pub attempt: u32,
}

impl Coordinator {
    /// Queue a verification for each assumption the landed submission challenged whose assuming
    /// claim is still live, against `main`, with `before` as the baseline. A verification already
    /// queued for the same (claim, assumption) is replaced by this newer one only if it is for
    /// the main this merge started from (`before`): back-to-back challenging merges. The
    /// replacement keeps the older baseline, so its span covers only merges that challenged this
    /// assumption. With any other merge in between, a break could come from that merge, so both
    /// are kept. One in flight is left to finish.
    pub(super) fn record_verifications(
        &mut self,
        challenged: &[Challenged],
        before: &CommitId,
        main: &CommitId,
    ) {
        for challenge in challenged {
            let Some(assuming) = self.state.claims.get(&challenge.claim.0) else {
                continue;
            };
            let agent = assuming.agent.clone();
            let running = self
                .state
                .verification_in_flight
                .as_ref()
                .map(|flight| flight.id);
            let subject = Subject::Assumption {
                assumption: challenge.assumption.clone(),
            };
            let mut baseline = before.clone();
            self.state.verifications.retain(|queued| {
                let replaced = running != Some(queued.id)
                    && queued.claim == challenge.claim
                    && queued.subject == subject
                    && queued.main == *before;
                if replaced {
                    baseline = queued.before.clone();
                }
                !replaced
            });
            let id = super::take_next(&mut self.state.next_verification);
            self.state.verifications.push(Verification {
                id,
                agent,
                claim: challenge.claim,
                subject,
                before: baseline,
                main: main.clone(),
                infra_failures: 0,
                retry_at_ms: None,
            });
        }
    }

    /// The challenged assumptions whose assuming claim is still live: the ones
    /// `record_verifications` queues, and the ones a landing without a baseline lists as
    /// unverified.
    pub(super) fn live_challenged(&self, challenged: &[Challenged]) -> Vec<UnverifiedAssumption> {
        let mut live = Vec::new();
        for challenge in challenged {
            if self.state.claims.contains_key(&challenge.claim.0) {
                live.push(UnverifiedAssumption {
                    claim: challenge.claim,
                    assumption: challenge.assumption.clone(),
                });
            }
        }
        live
    }

    /// The submitted shadow claims that `blocking` denied, with their agents, in claim id order.
    /// One that has not submitted has nothing to try and is left out.
    pub(super) fn submitted_shadows_blocked_by(
        &self,
        blocking: ClaimId,
    ) -> Vec<(ClaimId, AgentId)> {
        let mut shadows = Vec::new();
        for (id, shadow) in &self.state.claims {
            let Some(denial) = &shadow.denial else {
                continue;
            };
            if denial.blocked_by.contains(&blocking) && denial.fork_commit.is_some() {
                shadows.push((ClaimId(*id), shadow.agent.clone()));
            }
        }
        shadows
    }

    /// Queue a trial for each submitted shadow claim that `blocking` denied, now that `blocking`
    /// has merged: the shadow claim's commit tried on `main`, with `before` as the baseline. A
    /// shadow claim that has not submitted has nothing to try and is skipped.
    pub(super) fn record_shadow_verifications(
        &mut self,
        blocking: ClaimId,
        before: &CommitId,
        main: &CommitId,
    ) {
        let shadows = self.submitted_shadows_blocked_by(blocking);
        for (claim, agent) in shadows {
            let id = super::take_next(&mut self.state.next_verification);
            self.state.verifications.push(Verification {
                id,
                agent,
                claim,
                subject: Subject::Shadow {
                    blocking_claim: blocking,
                },
                before: before.clone(),
                main: main.clone(),
                infra_failures: 0,
                retry_at_ms: None,
            });
        }
    }

    /// Drop each submitted shadow claim that nothing is owed through (see the module docs). Run
    /// after anything that can end a claim or finish a trial. Each drop is logged as
    /// `ClaimReleased { Settled }`, in claim id order.
    pub(super) fn drop_finished_shadows(&mut self, now_ms: u64) -> Vec<Effect> {
        if self
            .state
            .claims
            .values()
            .all(|claim| claim.kind != ClaimKind::Shadow)
        {
            return Vec::new();
        }
        let trialled: HashSet<u64> = self
            .state
            .verifications
            .iter()
            .map(|queued| queued.claim.0)
            .collect();
        let claims = &self.state.claims;
        let finished: Vec<u64> = claims
            .iter()
            .filter(|(id, shadow)| {
                let mut blockers = shadow.denial.iter().flat_map(|denial| &denial.blocked_by);
                shadow.kind == ClaimKind::Shadow
                    && shadow.submitted.is_some()
                    && !trialled.contains(id)
                    && blockers.all(|blocker| !claims.contains_key(&blocker.0))
            })
            .map(|(id, _)| *id)
            .collect();
        let mut effects = Vec::new();
        for id in finished {
            self.state.claims.remove(&id);
            let claim = ClaimId(id);
            effects.extend(self.claim_released(claim, ReleaseReason::Settled, now_ms));
        }
        effects
    }

    /// Queue the tests of a race entry: `commit` of `agent`'s fork tried on `head`, which is both
    /// sides of the trial. Entries queued together see the same head.
    pub(super) fn record_race_trial(
        &mut self,
        race: RaceId,
        agent: &AgentId,
        claim: ClaimId,
        head: &CommitId,
    ) {
        let id = super::take_next(&mut self.state.next_verification);
        self.state.verifications.push(Verification {
            id,
            agent: agent.clone(),
            claim,
            subject: Subject::RaceEntry { race },
            before: head.clone(),
            main: head.clone(),
            infra_failures: 0,
            retry_at_ms: None,
        });
    }

    /// Whether a trial for `race` is still queued or running.
    pub(super) fn race_has_trials(&self, race: RaceId) -> bool {
        self.state
            .verifications
            .iter()
            .any(|queued| queued.subject == Subject::RaceEntry { race })
    }

    /// Log that `claim` ended for `reason` and forget its queued verifications. Every path that
    /// removes a claim ends with this call, where the reason is known, so the queue only ever
    /// holds live claims and the readers need not check. Claim ids are never reused.
    ///
    /// Each dropped assumption verification is logged as `VerificationDropped` after the
    /// `ClaimReleased`. Race entry and shadow trials are dropped without an event: the race and
    /// `DenialVerified` are their record.
    pub(super) fn claim_released(
        &mut self,
        claim: ClaimId,
        reason: ReleaseReason,
        now_ms: u64,
    ) -> Vec<Effect> {
        let mut effects = vec![self.event(now_ms, EventKind::ClaimReleased { claim, reason })];
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.state.verifications)
            .into_iter()
            .partition(|queued| queued.claim == claim);
        self.state.verifications = kept;
        for dropped in gone {
            let Subject::Assumption { assumption } = dropped.subject else {
                continue;
            };
            let kind = EventKind::VerificationDropped {
                claim,
                assumption,
                reason,
            };
            effects.push(self.event(now_ms, kind));
        }
        effects
    }

    /// Whether the head verification may be dispatched now: one is queued, it is not in backoff
    /// and none is in flight.
    fn verification_ready(&self, now_ms: u64) -> bool {
        if self.state.verification_in_flight.is_some() {
            return false;
        }
        let Some(next) = self.state.verifications.first() else {
            return false;
        };
        next.retry_at_ms.is_none_or(|due| due <= now_ms)
    }

    /// Whether the verification slot is due: `verify_slot_after` merges were dispatched in a row
    /// while the head verification was ready, and it still is. The next merge then waits for it.
    /// The callers (`begin_merge`, `next_merge_ms`, `merge_pending_at`) have all returned for a
    /// merge in flight before they ask, so it need not check for one.
    pub(super) fn verification_has_slot(&self, now_ms: u64) -> bool {
        self.state.merges_past_verification >= self.state.verify_slot_after
            && self.verification_ready(now_ms)
    }

    /// Whether no new merge may be dispatched now: a verification is in flight (the core never
    /// overlaps the two, whatever the host's wakes do) or the verification slot is due.
    pub(super) fn merge_must_wait(&self, now_ms: u64) -> bool {
        self.state.verification_in_flight.is_some() || self.verification_has_slot(now_ms)
    }

    /// Account for a merge just dispatched: it went ahead of a ready verification, or none was
    /// waiting and the run of merges is broken.
    pub(super) fn count_merge_past_verification(&mut self, now_ms: u64) {
        if self.verification_ready(now_ms) {
            self.state.merges_past_verification =
                self.state.merges_past_verification.saturating_add(1);
        } else {
            self.state.merges_past_verification = 0;
        }
    }

    /// The verification to run now, marking it in flight. `None` while one is in flight, while a
    /// merge is due or in flight (merges first, unless the verification slot is due), or when
    /// nothing is queued or the next one is in backoff. The queue is strict: a verification in
    /// backoff is waited for.
    ///
    /// The marker is part of the state: the caller persists it before calling the lander.
    pub fn begin_verification(&mut self, now_ms: u64) -> Option<VerifyDispatch> {
        let now_ms = self.advance_clock(now_ms);
        if self.state.verification_in_flight.is_some() || self.merge_pending_at(now_ms) {
            return None;
        }
        let next = self.state.verifications.first()?;
        if next.retry_at_ms.is_some_and(|due| due > now_ms) {
            return None;
        }
        let commit = match &next.subject {
            Subject::Assumption { .. } => self
                .state
                .claims
                .get(&next.claim.0)
                .and_then(|assuming| assuming.work.as_ref())
                .map(|work| work.fork_commit.clone()),
            Subject::RaceEntry { race } => self.entry_commit(*race, next.claim),
            Subject::Shadow { .. } => self
                .state
                .claims
                .get(&next.claim.0)
                .and_then(|shadow| shadow.denial.as_ref())
                .and_then(|denial| denial.fork_commit.clone()),
        };
        let dispatch_id = self.state.last_dispatch + 1;
        let dispatch = VerifyDispatch {
            id: dispatch_id,
            agent: next.agent.clone(),
            commit,
            before: next.before.clone(),
            main: next.main.clone(),
            attempt: next.infra_failures + 1,
        };
        self.state.verification_in_flight = Some(VerifyFlight {
            id: next.id,
            dispatch: dispatch_id,
        });
        self.state.last_dispatch = dispatch_id;
        self.state.merges_past_verification = 0;
        Some(dispatch)
    }

    /// Whether a verification is marked in flight.
    pub fn has_verification_in_flight(&self) -> bool {
        self.state.verification_in_flight.is_some()
    }

    /// Account for a verification that was in flight when the process died: its answer is lost,
    /// so the dispatch counts as an infrastructure failure, as for a merge.
    pub fn recover_verification(&mut self, now_ms: u64) -> Vec<Effect> {
        let now_ms = self.advance_clock(now_ms);
        let Some(flight) = self.state.verification_in_flight.take() else {
            return Vec::new();
        };
        let mut effects = self.retry_verification_after_infrastructure(flight.id, now_ms);
        effects.extend(self.drop_finished_shadows(now_ms));
        effects
    }

    /// When the shell should next run `begin_verification`, as an absolute time in milliseconds
    /// since the epoch that is never before `now_ms`. `None` when nothing is queued, or while a
    /// merge is due or in flight: the merge's own alarm comes first and reschedules.
    ///
    /// `verifying_here` is true while this instance is waiting for the lander: only a watchdog
    /// is scheduled then, as for a merge. Otherwise a stored marker means the process restarted
    /// and the alarm must recover it at once.
    pub fn next_verification_ms(&self, verifying_here: bool, now_ms: u64) -> Option<u64> {
        if self.state.verification_in_flight.is_some() {
            if verifying_here {
                return Some(now_ms.saturating_add(MERGE_WATCHDOG_MS));
            }
            return Some(now_ms);
        }
        if self.merge_pending_at(now_ms) {
            return None;
        }
        let next = self.state.verifications.first()?;
        Some(next.retry_at_ms.unwrap_or(0).max(now_ms))
    }

    /// The earliest of the next lease expiry, the next merge dispatch, the next verification and
    /// the next head read: the one alarm time, an absolute time in milliseconds since the epoch
    /// never before `now_ms`.
    pub fn next_wake_ms(
        &self,
        merging_here: bool,
        verifying_here: bool,
        now_ms: u64,
    ) -> Option<u64> {
        let merge_side = self.next_alarm_ms(merging_here, now_ms);
        let verification = self.next_verification_ms(verifying_here, now_ms);
        let head_read = self.next_head_read_ms(now_ms);
        [merge_side, verification, head_read]
            .into_iter()
            .flatten()
            .min()
    }

    /// Apply the lander's answer to the verification dispatch `dispatch`
    /// (`VerifyDispatch::id`). An answer to any other dispatch is stale, including an earlier
    /// dispatch of the verification now in flight, and changes nothing. An answer for a
    /// verification whose claim ended meanwhile is discarded and logs nothing.
    pub fn verification_outcome(
        &mut self,
        dispatch: u64,
        report: &TrialReport,
        now_ms: u64,
    ) -> Vec<Effect> {
        let now_ms = self.advance_clock(now_ms);
        let mut effects = self.apply_verification_outcome(dispatch, report, now_ms);
        effects.extend(self.drop_finished_shadows(now_ms));
        effects
    }

    fn apply_verification_outcome(
        &mut self,
        dispatch: u64,
        report: &TrialReport,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Some(VerifyFlight { id, .. }) = self
            .state
            .verification_in_flight
            .take_if(|flight| flight.dispatch == dispatch)
        else {
            return Vec::new();
        };
        let Some(subject) = self.subject_of(id) else {
            return Vec::new();
        };
        match subject {
            Subject::Assumption { .. } | Subject::Shadow { .. } => match report.verdict() {
                TrialVerdict::Decided(result) => {
                    self.finish_verification(id, &subject, result, now_ms)
                }
                TrialVerdict::Infrastructure => {
                    self.retry_verification_after_infrastructure(id, now_ms)
                }
            },
            Subject::RaceEntry { race } => match report.tests_passed() {
                TestsVerdict::Decided(passed) => self.finish_race_trial(id, race, passed, now_ms),
                TestsVerdict::Infrastructure => {
                    self.retry_verification_after_infrastructure(id, now_ms)
                }
            },
        }
    }

    fn subject_of(&self, id: u64) -> Option<Subject> {
        let queued = self.state.verifications.iter().find(|v| v.id == id)?;
        Some(queued.subject.clone())
    }

    /// The verification ran to a result: remove it and log it. The result is the only effect.
    fn finish_verification(
        &mut self,
        id: u64,
        subject: &Subject,
        result: Outcome,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Some(index) = self.state.verifications.iter().position(|v| v.id == id) else {
            return Vec::new();
        };
        let done = self.state.verifications.remove(index);
        let verified = match subject {
            Subject::Assumption { assumption } => EventKind::AssumptionVerified {
                claim: done.claim,
                assumption: assumption.clone(),
                outcome: result,
            },
            Subject::Shadow { blocking_claim } => EventKind::DenialVerified {
                shadow_claim: done.claim,
                blocking_claim: *blocking_claim,
                outcome: result,
            },
            Subject::RaceEntry { .. } => return Vec::new(),
        };
        vec![self.event(now_ms, verified)]
    }

    /// A race entry's trial ran to a result (or gave up: `None`): remove it and tell the race.
    fn finish_race_trial(
        &mut self,
        id: u64,
        race: RaceId,
        passed: Option<bool>,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Some(index) = self.state.verifications.iter().position(|v| v.id == id) else {
            return Vec::new();
        };
        let done = self.state.verifications.remove(index);
        self.record_trial(race, done.claim, passed, now_ms)
    }

    /// The attempt did not finish. Retry after a backoff, up to `MAX_INFRA_RETRIES` times; then
    /// log it as `Inconclusive`, so a verification that cannot run cannot wedge the queue.
    fn retry_verification_after_infrastructure(&mut self, id: u64, now_ms: u64) -> Vec<Effect> {
        let Some(index) = self.state.verifications.iter().position(|v| v.id == id) else {
            return Vec::new();
        };
        let queued = &mut self.state.verifications[index];
        queued.infra_failures += 1;
        if queued.infra_failures > MAX_INFRA_RETRIES {
            return match queued.subject.clone() {
                Subject::RaceEntry { race } => self.finish_race_trial(id, race, None, now_ms),
                subject @ (Subject::Assumption { .. } | Subject::Shadow { .. }) => {
                    self.finish_verification(id, &subject, Outcome::Inconclusive, now_ms)
                }
            };
        }
        let wait = infra_backoff_ms(queued.infra_failures);
        queued.retry_at_ms = Some(now_ms.saturating_add(wait));
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::merging::MergeDispatch;
    use crate::coordinator::Config;
    use crate::merge::{MergeOutcome, TrialOutcome, INFRA_BACKOFF_BASE_MS};
    use crate::protocol::{
        ClientMsg, DecisionRecord, Event, Fence, Intent, Mode, OnConflict, RequestId, RunId, Scope,
        ScopeClaim, ServerMsg, Summary,
    };

    const NOW: u64 = 1_000;
    const LEASE: u64 = 30_000;
    const MAIN: &str = "dddddddddddddddddddddddddddddddddddddddd";
    const OTHER: &str = "ffffffffffffffffffffffffffffffffffffffff";
    const MAIN2: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    fn core() -> Coordinator {
        let mut c = Coordinator::new(Config {
            run: RunId("test".into()),
            lease_ms: LEASE,
            shadow_enabled: true,
        })
        .unwrap();
        c.set_reviewers(vec![agent("felix")]);
        c
    }

    fn agent(name: &str) -> AgentId {
        AgentId(name.into())
    }

    fn file(path: &str) -> Scope {
        Scope::File { path: path.into() }
    }

    fn edit(path: &str) -> ScopeClaim {
        ScopeClaim {
            scope: file(path),
            mode: Mode::EditBody,
        }
    }

    fn assumes(path: &str, statement: &str) -> Assumption {
        Assumption {
            scope: file(path),
            statement: statement.into(),
        }
    }

    fn replies(effects: &[Effect]) -> Vec<&ServerMsg> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Reply(msg) => Some(msg),
                Effect::Notify { .. } | Effect::Log(_) => None,
            })
            .collect()
    }

    fn logged(effects: &[Effect]) -> Vec<&EventKind> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Log(event) => Some(&event.kind),
                Effect::Reply(_) | Effect::Notify { .. } => None,
            })
            .collect()
    }

    fn verified_events(effects: &[Effect]) -> Vec<(ClaimId, Outcome)> {
        logged(effects)
            .into_iter()
            .filter_map(|kind| {
                let EventKind::AssumptionVerified { claim, outcome, .. } = kind else {
                    return None;
                };
                Some((*claim, *outcome))
            })
            .collect()
    }

    fn dropped_events(effects: &[Effect]) -> Vec<(ClaimId, Assumption, ReleaseReason)> {
        logged(effects)
            .into_iter()
            .filter_map(|kind| {
                let EventKind::VerificationDropped {
                    claim,
                    assumption,
                    reason,
                } = kind
                else {
                    return None;
                };
                Some((*claim, assumption.clone(), *reason))
            })
            .collect()
    }

    fn notices(effects: &[Effect]) -> usize {
        effects
            .iter()
            .filter(|e| matches!(e, Effect::Notify { .. }))
            .count()
    }

    fn grant(
        c: &mut Coordinator,
        who: &str,
        scopes: Vec<ScopeClaim>,
        assumptions: Vec<Assumption>,
    ) -> (ClaimId, Fence) {
        let msg = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent {
                summary: "s".into(),
                task_ref: None,
                assumptions,
            },
            scopes,
            on_conflict: OnConflict::Fail,
        };
        let effects = c.handle(&agent(who), msg, NOW);
        let Some(ServerMsg::Granted { claim, fence, .. }) = replies(&effects).into_iter().next()
        else {
            panic!("expected Granted, got {effects:?}");
        };
        (*claim, *fence)
    }

    fn fork_sha(who: &str) -> String {
        let seed = who.bytes().fold(0u8, |sum, b| sum.wrapping_add(b));
        format!("{seed:02x}").repeat(20)
    }

    fn submit(c: &mut Coordinator, who: &str, claim: (ClaimId, Fence), path: &str) {
        let msg = ClientMsg::Submit {
            req: RequestId(9),
            claim: claim.0,
            fence: claim.1,
            fork_commit: CommitId(fork_sha(who)),
            touched: vec![edit(path)],
            decisions: DecisionRecord {
                evidence: vec!["tests passed".into()],
                ..DecisionRecord::default()
            },
        };
        let effects = c.handle(&agent(who), msg, NOW);
        match replies(&effects)[..] {
            [ServerMsg::Accepted { .. }] => {}
            // A submission that threatens an assumption waits for a reviewer (invariant 12).
            [ServerMsg::ReviewRequired { claim, .. }] => {
                let approve = ClientMsg::Review {
                    req: RequestId(10),
                    claim: *claim,
                    approve: true,
                    note: None,
                };
                c.handle(&agent("felix"), approve, NOW);
            }
            _ => panic!("expected Accepted or ReviewRequired, got {effects:?}"),
        }
    }

    fn landed(head: &str) -> MergeOutcome {
        MergeOutcome::Merged {
            base: CommitId("old".into()),
            head: CommitId(head.into()),
        }
    }

    /// `who` claims `path`, submits it and has it merged to `head`; returns the merge's effects.
    fn merge_edit(c: &mut Coordinator, who: &str, path: &str, head: &str) -> Vec<Effect> {
        let claim = grant(c, who, vec![edit(path)], Vec::new());
        submit(c, who, claim, path);
        let dispatch = c.begin_merge(NOW).expect("a merge is due");
        assert_eq!(dispatch.claim, claim.0);
        c.merge_outcome(c.merge_flight_id(claim.0), &landed(head), NOW)
    }

    /// `a1` holds a claim on `src/b.rs` that assumes `src/a.rs`, and `a2` edits `src/a.rs`.
    /// The claim is returned, with `a2`'s claim submitted but not merged.
    fn challenged(c: &mut Coordinator) -> (ClaimId, ClaimId) {
        let assuming = grant(
            c,
            "a1",
            vec![edit("src/b.rs")],
            vec![assumes("src/a.rs", "f returns Some")],
        );
        let challenger = grant(c, "a2", vec![edit("src/a.rs")], Vec::new());
        submit(c, "a2", challenger, "src/a.rs");
        (assuming.0, challenger.0)
    }

    fn merge_challenger(c: &mut Coordinator, challenger: ClaimId, head: &str) -> Vec<Effect> {
        let dispatch = c.begin_merge(NOW).expect("a merge is due");
        assert_eq!(dispatch.claim, challenger);
        c.merge_outcome(c.merge_flight_id(challenger), &landed(head), NOW)
    }

    fn already_merged(base: &str) -> MergeOutcome {
        MergeOutcome::AlreadyMerged {
            base: CommitId(base.into()),
        }
    }

    /// Lands an earlier merge so main is `MAIN`, then dispatches the challenger and loses the
    /// answer. Returns the challenger and the time its retry is due.
    fn challenger_with_lost_answer(c: &mut Coordinator) -> (ClaimId, u64) {
        merge_edit(c, "a0", "src/z.rs", MAIN);
        let (_, challenger) = challenged(c);
        let dispatch = c.begin_merge(NOW).expect("a merge is due");
        assert_eq!(dispatch.claim, challenger);
        c.recover_merge(NOW);
        let work = c.state.claims[&challenger.0].work.as_ref().unwrap();
        (challenger, work.retry_at_ms.expect("a retry is scheduled"))
    }

    #[test]
    fn a_recovered_merge_verifies_against_the_main_it_was_dispatched_from() {
        let mut c = core();
        let (challenger, due) = challenger_with_lost_answer(&mut c);
        c.begin_merge(due).expect("the retry is dispatched");
        c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN2), due);

        assert_eq!(pending(&c), 1);
        let queued = &c.state.verifications[0];
        assert_eq!(queued.before, CommitId(MAIN.into()));
        assert_eq!(queued.main, CommitId(MAIN2.into()));
    }

    #[test]
    fn a_recovered_merge_names_the_main_it_was_dispatched_from_as_the_landing_base() {
        let mut c = core();
        let (challenger, due) = challenger_with_lost_answer(&mut c);
        c.begin_merge(due).expect("the retry is dispatched");
        let effects = c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN2), due);

        let told = effects.iter().any(|effect| {
            matches!(
                effect,
                Effect::Notify {
                    msg: ServerMsg::Merged { base: Some(base), .. },
                    ..
                } if base.0 == MAIN
            )
        });
        assert!(told, "{effects:?}");
    }

    #[test]
    fn a_prepared_answer_at_main_after_a_lost_answer_verifies_against_the_dispatch_main() {
        use crate::host::call::CallAnswer;
        let mut c = core();
        let (challenger, due) = challenger_with_lost_answer(&mut c);
        c.begin_merge(due).expect("the retry is dispatched");
        let at_main = CallAnswer::Prepared {
            base: CommitId(MAIN2.into()),
            head: CommitId(MAIN2.into()),
        };
        let outcome = crate::host::merge_outcome_of(&at_main);
        c.merge_outcome(c.merge_flight_id(challenger), &outcome, due);

        assert_eq!(pending(&c), 1, "the assumption is verified");
        let queued = &c.state.verifications[0];
        assert_eq!(
            queued.before,
            CommitId(MAIN.into()),
            "against the dispatch main"
        );
        assert_eq!(queued.main, CommitId(MAIN2.into()));
    }

    /// The trunk head moves to `head` while no merge is in flight, as a head read applies it.
    fn trunk_moves_to(c: &mut Coordinator, head: &str, now: u64) {
        c.trunk_moved(now);
        let read = c.begin_head_read().expect("a head read is due");
        c.trunk_read(&read, Some(CommitId(head.into())), now);
    }

    #[test]
    fn a_head_read_between_the_attempts_does_not_move_the_baseline() {
        let mut c = core();
        let (challenger, due) = challenger_with_lost_answer(&mut c);
        trunk_moves_to(&mut c, MAIN2, NOW + 1);
        c.begin_merge(due).expect("the retry is dispatched");
        c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN2), due);

        assert_eq!(pending(&c), 1);
        assert_eq!(c.state.verifications[0].before, CommitId(MAIN.into()));
    }

    #[test]
    fn main_moved_after_a_lost_answer_keeps_the_first_baseline() {
        let mut c = core();
        let (challenger, due) = challenger_with_lost_answer(&mut c);
        // The cut-off attempt kept running at the lander and pushed; the retry finds main moved.
        c.begin_merge(due).expect("the retry is dispatched");
        c.merge_outcome(
            c.merge_flight_id(challenger),
            &MergeOutcome::MainMoved {},
            due,
        );
        trunk_moves_to(&mut c, MAIN2, due + 1);
        c.begin_merge(due + 1)
            .expect("main moved: dispatched at once");
        c.merge_outcome(
            c.merge_flight_id(challenger),
            &already_merged(OTHER),
            due + 1,
        );

        assert_eq!(pending(&c), 1);
        assert_eq!(c.state.verifications[0].before, CommitId(MAIN.into()));
        assert_eq!(c.state.verifications[0].main, CommitId(OTHER.into()));
    }

    #[test]
    fn a_second_lost_answer_keeps_the_first_baseline() {
        let mut c = core();
        let (challenger, due) = challenger_with_lost_answer(&mut c);
        trunk_moves_to(&mut c, MAIN2, NOW + 1);
        c.begin_merge(due).expect("the retry is dispatched");
        c.recover_merge(due);
        let work = c.state.claims[&challenger.0].work.as_ref().unwrap();
        let again = work.retry_at_ms.expect("a second retry is scheduled");
        c.begin_merge(again)
            .expect("the second retry is dispatched");
        c.merge_outcome(c.merge_flight_id(challenger), &already_merged(OTHER), again);

        assert_eq!(c.state.verifications[0].before, CommitId(MAIN.into()));
    }

    #[test]
    fn the_baseline_survives_a_save_and_load_before_the_retry() {
        let mut c = core();
        let (challenger, due) = challenger_with_lost_answer(&mut c);
        let stored = serde_json::to_string(&c).unwrap();
        let mut c: Coordinator = serde_json::from_str(&stored).unwrap();
        c.begin_merge(due).expect("the retry is dispatched");
        c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN2), due);

        assert_eq!(c.state.verifications[0].before, CommitId(MAIN.into()));
    }

    #[test]
    fn a_submission_in_doubt_is_not_overtaken_by_one_approved_meanwhile() {
        let mut c = core();
        c.set_reviewers(vec![agent("felix")]);
        merge_edit(&mut c, "a0", "src/z.rs", MAIN);
        // Lower ordinal, held for review (no test evidence).
        let held = grant(&mut c, "a3", vec![edit("src/y.rs")], Vec::new());
        let msg = ClientMsg::Submit {
            req: RequestId(9),
            claim: held.0,
            fence: held.1,
            fork_commit: CommitId(fork_sha("a3")),
            touched: vec![edit("src/y.rs")],
            decisions: DecisionRecord::default(),
        };
        c.handle(&agent("a3"), msg, NOW);
        let (_, challenger) = challenged(&mut c);
        let dispatch = c.begin_merge(NOW).expect("a merge is due");
        assert_eq!(dispatch.claim, challenger, "the held submission is skipped");
        c.recover_merge(NOW);
        let review = ClientMsg::Review {
            req: RequestId(7),
            claim: held.0,
            approve: true,
            note: None,
        };
        c.handle(&agent("felix"), review, NOW + 1);

        assert_eq!(
            c.begin_merge(NOW + 2),
            None,
            "the claim in doubt is waited for"
        );
        let work = c.state.claims[&challenger.0].work.as_ref().unwrap();
        let due = work.retry_at_ms.expect("a retry is scheduled");
        assert_eq!(c.begin_merge(due).map(|d| d.claim), Some(challenger));
    }

    #[test]
    fn an_unreachable_lander_leaves_the_same_baseline_as_a_restart() {
        let mut c = core();
        merge_edit(&mut c, "a0", "src/z.rs", MAIN);
        let (_, challenger) = challenged(&mut c);
        c.begin_merge(NOW).expect("a merge is due");
        c.merge_outcome(
            c.merge_flight_id(challenger),
            &MergeOutcome::ServiceUnavailable,
            NOW,
        );
        let work = c.state.claims[&challenger.0].work.as_ref().unwrap();
        let due = work.retry_at_ms.expect("a retry is scheduled");
        c.begin_merge(due).expect("the retry is dispatched");
        c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN2), due);

        assert_eq!(c.state.verifications[0].before, CommitId(MAIN.into()));
    }

    #[test]
    fn an_already_merged_answer_to_a_first_dispatch_queues_nothing() {
        let mut c = core();
        merge_edit(&mut c, "a0", "src/z.rs", MAIN);
        let (_, challenger) = challenged(&mut c);
        c.begin_merge(NOW).expect("a merge is due");
        c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN2), NOW);

        assert_eq!(
            pending(&c),
            0,
            "nothing of this submission is known to have landed"
        );
    }

    #[test]
    fn a_recovered_merge_that_left_main_unchanged_queues_nothing() {
        let mut c = core();
        let (challenger, due) = challenger_with_lost_answer(&mut c);
        c.begin_merge(due).expect("the retry is dispatched");
        c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN), due);

        assert_eq!(
            pending(&c),
            0,
            "main did not move, so there is no span to verify"
        );
    }

    fn pending(c: &Coordinator) -> usize {
        c.state.verifications.len()
    }

    fn all_events(effects_by_step: &[&[Effect]]) -> Vec<Event> {
        let mut events = Vec::new();
        for effects in effects_by_step {
            for effect in *effects {
                if let Effect::Log(event) = effect {
                    events.push(event.clone());
                }
            }
        }
        events
    }

    /// Work that was already failing on the baseline, and still fails on the new main.
    fn baseline_failing() -> TrialReport {
        TrialReport {
            before: Some(TrialOutcome::TestsFailed {}),
            after: None,
        }
    }

    /// A trial whose baseline was clean, so `after` is judged on its own.
    fn clean_then(after: TrialOutcome) -> TrialReport {
        TrialReport {
            before: Some(TrialOutcome::Clean {}),
            after: Some(after),
        }
    }

    #[test]
    fn a_merge_that_challenged_an_assumption_queues_one_verification_of_the_assuming_claim() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        assert_eq!(pending(&c), 0, "the challenge alone queues nothing");

        merge_challenger(&mut c, challenger, MAIN);

        assert_eq!(pending(&c), 1);
        let dispatch = c.begin_verification(NOW).expect("a verification is due");
        assert_eq!(dispatch.agent, agent("a1"));
        assert_eq!(dispatch.main, CommitId(MAIN.into()));
        assert_eq!(dispatch.attempt, 1);
        assert!(c.has_verification_in_flight());
        let queued = &c.state.verifications[0];
        assert_eq!(queued.claim, assuming);
        assert_eq!(
            queued.subject,
            Subject::Assumption {
                assumption: assumes("src/a.rs", "f returns Some")
            }
        );
    }

    #[test]
    fn nothing_is_queued_when_the_merge_challenged_nothing() {
        let mut c = core();
        grant(
            &mut c,
            "a1",
            vec![edit("src/b.rs")],
            vec![assumes("src/other.rs", "unrelated")],
        );
        let other = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        submit(&mut c, "a2", other, "src/a.rs");

        merge_challenger(&mut c, other.0, MAIN);

        assert_eq!(pending(&c), 0);
        assert_eq!(c.begin_verification(NOW), None);
        assert_eq!(c.next_verification_ms(false, NOW), None);
    }

    #[test]
    fn a_merge_without_any_assuming_claim_queues_nothing() {
        let mut c = core();
        merge_edit(&mut c, "a2", "src/a.rs", MAIN);
        assert_eq!(pending(&c), 0);
    }

    #[test]
    fn an_assuming_claim_that_ended_before_the_merge_is_not_verified() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        let fence = c.state.claims[&assuming.0].fence;
        let release = ClientMsg::Release {
            claim: assuming,
            fence,
            req: None,
        };
        c.handle(&agent("a1"), release, NOW);

        merge_challenger(&mut c, challenger, MAIN);

        assert_eq!(pending(&c), 0);
        assert_eq!(c.begin_verification(NOW), None);
    }

    #[test]
    fn one_verification_per_challenged_assumption_not_per_touched_scope() {
        let mut c = core();
        let assuming = grant(
            &mut c,
            "a1",
            vec![edit("src/b.rs")],
            vec![
                assumes("src/a.rs", "f returns Some"),
                assumes("src/a.rs", "g is pure"),
                assumes("src/z.rs", "not touched"),
            ],
        );
        let challenger = grant(
            &mut c,
            "a2",
            vec![edit("src/a.rs"), edit("src/a.rs")],
            Vec::new(),
        );
        submit(&mut c, "a2", challenger, "src/a.rs");

        merge_challenger(&mut c, challenger.0, MAIN);

        let mut statements = Vec::new();
        for queued in &c.state.verifications {
            let Subject::Assumption { assumption } = &queued.subject else {
                panic!("expected an assumption verification");
            };
            statements.push(assumption.statement.as_str());
        }
        assert_eq!(statements, ["f returns Some", "g is pure"]);
        assert!(c.state.verifications.iter().all(|v| v.claim == assuming.0));
    }

    #[test]
    fn the_commit_to_try_is_the_assuming_claims_submitted_commit_when_the_trial_is_dispatched() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        // a1 submits after the verification was recorded: the trial tries that commit.
        let a1 = c.state.claims[&assuming.0].fence;
        submit(&mut c, "a1", (assuming, a1), "src/b.rs");
        // a1's own merge is now due, and has priority; put it in backoff to let the trial run.
        c.begin_merge(NOW).unwrap();
        c.merge_outcome(
            c.merge_flight_id(assuming),
            &MergeOutcome::ServiceUnavailable,
            NOW,
        );
        let dispatch = c.begin_verification(NOW).unwrap();
        assert_eq!(dispatch.commit, Some(CommitId(fork_sha("a1"))));

        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let dispatch = c.begin_verification(NOW).unwrap();
        assert_eq!(
            dispatch.commit, None,
            "a1 has not submitted: the lander reads the fork"
        );
    }

    #[test]
    fn a_commit_submitted_before_the_merge_is_the_one_tried() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        let a1 = c.state.claims[&assuming.0].fence;
        submit(&mut c, "a1", (assuming, a1), "src/b.rs");
        merge_challenger(&mut c, challenger, MAIN);
        let dispatch = c.begin_merge(NOW).unwrap();
        c.merge_outcome(dispatch.id, &MergeOutcome::ServiceUnavailable, NOW);
        let dispatch = c.begin_verification(NOW).unwrap();
        assert_eq!(dispatch.commit, Some(CommitId(fork_sha("a1"))));
    }

    #[test]
    fn the_trial_carries_main_before_the_challenging_merge_as_its_baseline() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let dispatch = c.begin_verification(NOW).unwrap();
        assert_eq!(dispatch.before, CommitId("old".into()));
        assert_eq!(dispatch.main, CommitId(MAIN.into()));
    }

    #[test]
    fn verification_is_stored_with_the_state_so_it_survives_a_restart() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        c.begin_verification(NOW).expect("a verification is due");

        let stored = serde_json::to_string(&c).unwrap();
        let mut restarted: Coordinator = serde_json::from_str(&stored).unwrap();

        assert_eq!(pending(&restarted), 1);
        assert!(restarted.has_verification_in_flight());
        assert_eq!(restarted.next_verification_ms(false, NOW), Some(NOW));
        let recovered = restarted.recover_verification(NOW);
        assert!(recovered.is_empty());
        assert!(!restarted.has_verification_in_flight());
        assert_eq!(restarted.state.verifications[0].infra_failures, 1);
        let queued = serde_json::to_string(&restarted).unwrap();
        let again: Coordinator = serde_json::from_str(&queued).unwrap();
        assert_eq!(again.state.verifications[0].main, CommitId(MAIN.into()));
    }

    #[test]
    fn a_state_stored_before_verifications_existed_loads_with_none() {
        let mut c = core();
        challenged(&mut c);
        let mut value = serde_json::to_value(&c).unwrap();
        let object = value.as_object_mut().unwrap();
        for key in [
            "verifications",
            "verification_in_flight",
            "next_verification",
        ] {
            object.remove(key);
        }
        let loaded: Coordinator = serde_json::from_value(value).unwrap();
        assert_eq!(pending(&loaded), 0);
        assert!(!loaded.has_verification_in_flight());
    }

    #[test]
    fn a_merge_in_flight_or_due_holds_the_verification_back() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        // Another submission is due: the merge goes first.
        let next = grant(&mut c, "a3", vec![edit("src/c.rs")], Vec::new());
        submit(&mut c, "a3", next, "src/c.rs");

        assert_eq!(c.begin_verification(NOW), None, "a merge is due");
        assert_eq!(c.next_verification_ms(false, NOW), None);
        let dispatch = c.begin_merge(NOW).expect("the merge runs first");
        assert_eq!(dispatch.claim, next.0);
        assert_eq!(c.begin_verification(NOW), None, "a merge is in flight");

        c.merge_outcome(c.merge_flight_id(next.0), &landed(MAIN2), NOW);
        assert!(c.begin_verification(NOW).is_some(), "the queue is empty");
    }

    #[test]
    fn a_merge_in_backoff_does_not_hold_the_verification_back() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let next = grant(&mut c, "a3", vec![edit("src/c.rs")], Vec::new());
        submit(&mut c, "a3", next, "src/c.rs");
        c.begin_merge(NOW).expect("the merge runs");
        c.merge_outcome(
            c.merge_flight_id(next.0),
            &MergeOutcome::ServiceUnavailable,
            NOW,
        );

        assert!(c.begin_merge(NOW).is_none(), "the merge is in backoff");
        assert!(c.begin_verification(NOW).is_some());
    }

    #[test]
    fn only_one_verification_is_in_flight_at_a_time() {
        let mut c = core();
        let assuming = grant(
            &mut c,
            "a1",
            vec![edit("src/b.rs")],
            vec![assumes("src/a.rs", "one"), assumes("src/a.rs", "two")],
        );
        let challenger = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        submit(&mut c, "a2", challenger, "src/a.rs");
        merge_challenger(&mut c, challenger.0, MAIN);
        assert_eq!(pending(&c), 2);

        let first = c.begin_verification(NOW).expect("the first runs");
        assert_eq!(c.begin_verification(NOW), None, "one at a time");
        let first_entry = c.state.verifications[0].id;
        c.verification_outcome(first.id, &clean_then(TrialOutcome::Clean {}), NOW);
        c.begin_verification(NOW).expect("then the second");
        assert_eq!(c.state.verifications.len(), 1, "the first is done");
        assert_ne!(
            c.state.verifications[0].id, first_entry,
            "a different entry"
        );
        assert_eq!(c.state.verifications[0].claim, assuming.0);
    }

    #[test]
    fn each_trial_outcome_is_logged_as_the_protocol_outcome_it_is_evidence_for() {
        let cases = [
            (clean_then(TrialOutcome::Clean {}), Outcome::Clean),
            (
                clean_then(TrialOutcome::Conflict {}),
                Outcome::TextualConflict,
            ),
            (
                clean_then(TrialOutcome::TestsFailed {}),
                Outcome::TestsFailed,
            ),
            (
                clean_then(TrialOutcome::NothingToTest {}),
                Outcome::Inconclusive,
            ),
            (
                clean_then(TrialOutcome::CommitNotInFork {}),
                Outcome::Inconclusive,
            ),
            (
                TrialReport::stopped(TrialOutcome::Refused),
                Outcome::Inconclusive,
            ),
            (
                TrialReport::stopped(TrialOutcome::MainUnreachable {}),
                Outcome::Inconclusive,
            ),
            (baseline_failing(), Outcome::Inconclusive),
        ];
        for (trial, expected) in cases {
            let mut c = core();
            let (assuming, challenger) = challenged(&mut c);
            merge_challenger(&mut c, challenger, MAIN);
            let dispatch = c.begin_verification(NOW).unwrap();

            let effects = c.verification_outcome(dispatch.id, &trial, NOW);

            assert_eq!(
                verified_events(&effects),
                [(assuming, expected)],
                "{trial:?}"
            );
            assert_eq!(pending(&c), 0);
            assert!(!c.has_verification_in_flight());
            assert_eq!(notices(&effects), 0, "the event is the record: {trial:?}");
            assert!(replies(&effects).is_empty());
        }
    }

    #[test]
    fn the_verified_event_names_the_claim_and_the_assumption() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let dispatch = c.begin_verification(NOW).unwrap();
        let effects =
            c.verification_outcome(dispatch.id, &clean_then(TrialOutcome::Conflict {}), NOW);
        assert!(matches!(
            logged(&effects)[..],
            [EventKind::AssumptionVerified { assumption, .. }]
                if *assumption == assumes("src/a.rs", "f returns Some")
        ));
    }

    #[test]
    fn the_summary_counts_broken_only_for_verified_conflicts() {
        let cases = [
            (clean_then(TrialOutcome::Clean {}), 0),
            (clean_then(TrialOutcome::Conflict {}), 1),
            (clean_then(TrialOutcome::TestsFailed {}), 1),
            (baseline_failing(), 0),
            (clean_then(TrialOutcome::NothingToTest {}), 0),
            (clean_then(TrialOutcome::CommitNotInFork {}), 0),
            (clean_then(TrialOutcome::GitFailed {}), 0),
        ];
        for (trial, broken) in cases {
            let mut c = core();
            let (_, challenger) = challenged(&mut c);
            let mut steps = Vec::new();
            steps.push(merge_challenger(&mut c, challenger, MAIN));
            let mut now = NOW;
            let mut last = Vec::new();
            // Infrastructure outcomes are retried until they give up; the rest decide at once.
            for _ in 0..=MAX_INFRA_RETRIES {
                let dispatch = c.begin_verification(now).unwrap();
                last = c.verification_outcome(dispatch.id, &trial, now);
                if !last.is_empty() {
                    break;
                }
                now += infra_backoff_ms(MAX_INFRA_RETRIES) + 1;
            }
            steps.push(last);
            let slices: Vec<&[Effect]> = steps.iter().map(Vec::as_slice).collect();
            let summary = Summary::from_events(&all_events(&slices));
            assert_eq!(summary.assumptions_confirmed_broken, broken, "{trial:?}");
        }
    }

    #[test]
    fn a_challenge_counts_as_challenged_and_never_as_broken_until_verified() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        let merged = merge_challenger(&mut c, challenger, MAIN);
        let summary = Summary::from_events(&all_events(&[&merged]));
        assert_eq!(summary.assumptions_confirmed_broken, 0);
    }

    #[test]
    fn infrastructure_is_retried_with_backoff_then_logged_inconclusive_at_the_bound() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let mut now = NOW;
        for retry in 1..=MAX_INFRA_RETRIES {
            let dispatch = c.begin_verification(now).unwrap();
            assert_eq!(dispatch.attempt, retry);
            let effects =
                c.verification_outcome(dispatch.id, &clean_then(TrialOutcome::Clone {}), now);
            assert!(effects.is_empty(), "retry {retry} logs nothing");
            let wait = infra_backoff_ms(retry);
            assert_eq!(c.next_verification_ms(false, now), Some(now + wait));
            assert_eq!(c.begin_verification(now + wait - 1), None, "in backoff");
            now += wait;
        }
        let dispatch = c.begin_verification(now).unwrap();
        assert_eq!(dispatch.attempt, MAX_INFRA_RETRIES + 1);
        let effects = c.verification_outcome(
            dispatch.id,
            &clean_then(TrialOutcome::ServiceUnavailable),
            now,
        );
        assert_eq!(
            verified_events(&effects),
            [(assuming, Outcome::Inconclusive)]
        );
        assert_eq!(pending(&c), 0);
        assert_eq!(INFRA_BACKOFF_BASE_MS, infra_backoff_ms(1));
    }

    #[test]
    fn a_verification_cut_off_by_a_restart_counts_as_an_infrastructure_failure() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        c.begin_verification(NOW).unwrap();
        assert!(c.recover_verification(NOW).is_empty());
        assert!(!c.has_verification_in_flight());
        assert_eq!(pending(&c), 1);
        assert_eq!(c.begin_verification(NOW), None, "in backoff");
        assert!(c.begin_verification(NOW + INFRA_BACKOFF_BASE_MS).is_some());
    }

    #[test]
    fn a_released_assuming_claim_drops_its_verification_and_logs_the_drop() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let fence = c.state.claims[&assuming.0].fence;

        let release = ClientMsg::Release {
            claim: assuming,
            fence,
            req: None,
        };
        let effects = c.handle(&agent("a1"), release, NOW);

        assert_eq!(pending(&c), 0);
        assert!(verified_events(&effects).is_empty());
        assert_eq!(
            dropped_events(&effects),
            [(
                assuming,
                assumes("src/a.rs", "f returns Some"),
                ReleaseReason::Agent
            )]
        );
        assert_eq!(c.begin_verification(NOW), None);
        assert_eq!(c.next_verification_ms(false, NOW), None);
    }

    #[test]
    fn an_expired_assuming_claim_drops_its_verification_and_logs_the_drop() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);

        let effects = c.expire(NOW + LEASE + 1);

        assert_eq!(pending(&c), 0);
        assert!(verified_events(&effects).is_empty());
        let dropped = dropped_events(&effects);
        assert_eq!(dropped.len(), 1, "{effects:?}");
        assert_eq!(
            (dropped[0].0, dropped[0].2),
            (assuming, ReleaseReason::LeaseExpired)
        );
        assert_eq!(c.begin_verification(NOW + LEASE + 1), None);
    }

    #[test]
    fn a_merged_assuming_claim_drops_its_verification() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        let a1 = c.state.claims[&assuming.0].fence;
        submit(&mut c, "a1", (assuming, a1), "src/b.rs");
        merge_challenger(&mut c, challenger, MAIN);
        assert_eq!(pending(&c), 1);

        let dispatch = c.begin_merge(NOW).expect("a1's own merge");
        assert_eq!(dispatch.claim, assuming);
        let effects = c.merge_outcome(c.merge_flight_id(assuming), &landed(MAIN2), NOW);

        assert_eq!(pending(&c), 0);
        assert!(verified_events(&effects).is_empty());
        let dropped = dropped_events(&effects);
        assert_eq!(dropped.len(), 1, "{effects:?}");
        assert_eq!(
            (dropped[0].0, dropped[0].2),
            (assuming, ReleaseReason::Merged)
        );
    }

    #[test]
    fn a_dropped_verification_is_counted_on_its_own_and_never_as_verified() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        let merged = merge_challenger(&mut c, challenger, MAIN);
        let fence = c.state.claims[&assuming.0].fence;
        let release = ClientMsg::Release {
            claim: assuming,
            fence,
            req: None,
        };
        let released = c.handle(&agent("a1"), release, NOW);

        let summary = Summary::from_events(&all_events(&[&merged, &released]));

        assert_eq!(summary.verifications_dropped, 1);
        assert_eq!(summary.assumptions_confirmed_broken, 0);
        assert_eq!(summary.assumptions_challenged, 0);
        assert_eq!(
            summary.conflicts_prevented_verified + summary.false_alarms,
            0
        );
        assert_eq!(summary.precision, None);
    }

    #[test]
    fn a_ready_verification_is_not_starved_by_a_continuous_stream_of_merges() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        assert_eq!(pending(&c), 1);
        let mut merges = 0;
        let mut dispatched = None;
        while merges < 12 && dispatched.is_none() {
            let who = format!("m{merges}");
            let path = format!("src/m{merges}.rs");
            let claim = grant(&mut c, &who, vec![edit(&path)], Vec::new());
            submit(&mut c, &who, claim, &path);
            dispatched = c.begin_verification(NOW);
            if dispatched.is_some() {
                break;
            }
            c.begin_merge(NOW).expect("a merge is due");
            c.merge_outcome(c.merge_flight_id(claim.0), &landed(MAIN), NOW);
            merges += 1;
        }
        assert!(
            dispatched.is_some(),
            "no verification ran in {merges} merges"
        );
        assert!(merges <= 4, "the verification waited for {merges} merges");
    }

    /// Submit a fresh tested edit and merge it at once: one more merge dispatched.
    fn merge_next(c: &mut Coordinator, round: usize) -> Option<MergeDispatch> {
        let who = format!("m{round}");
        let path = format!("src/m{round}.rs");
        let claim = grant(c, &who, vec![edit(&path)], Vec::new());
        submit(c, &who, claim, &path);
        let dispatch = c.begin_merge(NOW)?;
        c.merge_outcome(dispatch.id, &landed(MAIN), NOW);
        Some(dispatch)
    }

    #[test]
    fn the_verification_gets_the_slot_after_exactly_the_configured_number_of_merges() {
        for after in [1, 2, 4, 7] {
            let mut c = core();
            c.set_verify_slot_after(after);
            let (_, challenger) = challenged(&mut c);
            merge_challenger(&mut c, challenger, MAIN);
            for round in 0..after as usize {
                let who = format!("m{round}");
                let path = format!("src/m{round}.rs");
                let claim = grant(&mut c, &who, vec![edit(&path)], Vec::new());
                submit(&mut c, &who, claim, &path);
                assert!(
                    c.begin_verification(NOW).is_none(),
                    "merge {round} has priority"
                );
                let dispatch = c.begin_merge(NOW).expect("the merge is dispatched");
                c.merge_outcome(dispatch.id, &landed(MAIN), NOW);
            }
            let claim = grant(&mut c, "late", vec![edit("src/late.rs")], Vec::new());
            submit(&mut c, "late", claim, "src/late.rs");
            assert_eq!(
                c.begin_merge(NOW),
                None,
                "after {after} merges the slot is the verification's"
            );
            assert_eq!(
                c.next_merge_ms(false, NOW),
                None,
                "no merge wake while the slot is due"
            );
            assert_eq!(c.next_verification_ms(false, NOW), Some(NOW));
            let trial = c.begin_verification(NOW).expect("the slot's verification");
            assert_eq!(c.begin_merge(NOW), None, "the merge waits for its answer");
            c.verification_outcome(trial.id, &clean_then(TrialOutcome::Clean {}), NOW);
            let after_slot = c.begin_merge(NOW).expect("merges go on after the slot");
            assert_eq!(after_slot.claim, claim.0);
        }
    }

    #[test]
    fn the_slot_is_not_taken_while_the_verification_is_in_backoff() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let first = c.begin_verification(NOW).unwrap();
        c.verification_outcome(first.id, &clean_then(TrialOutcome::Clone {}), NOW);
        assert_eq!(c.begin_verification(NOW), None, "in backoff");
        for round in 0..3 * 4 {
            assert!(
                merge_next(&mut c, round).is_some(),
                "merge {round} waited for a verification that cannot run"
            );
        }
    }

    #[test]
    fn a_merge_waits_while_a_verification_is_in_flight() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let claim = grant(&mut c, "m0", vec![edit("src/m0.rs")], Vec::new());
        submit(&mut c, "m0", claim, "src/m0.rs");
        let dispatch = c.begin_verification(NOW);
        assert_eq!(dispatch, None, "the merge is due and goes first");
        let first = c.begin_merge(NOW).expect("the merge is dispatched");
        c.merge_outcome(first.id, &landed(MAIN), NOW);

        let trial = c.begin_verification(NOW).expect("nothing else is due");
        let claim = grant(&mut c, "m1", vec![edit("src/m1.rs")], Vec::new());
        submit(&mut c, "m1", claim, "src/m1.rs");
        assert_eq!(
            c.begin_merge(NOW),
            None,
            "no merge beside a verification in flight"
        );
        assert_eq!(c.next_merge_ms(false, NOW), None);
        assert!(
            c.next_verification_ms(false, NOW).is_some(),
            "its watchdog is scheduled"
        );

        c.verification_outcome(trial.id, &clean_then(TrialOutcome::Clean {}), NOW);
        assert!(
            c.begin_merge(NOW).is_some(),
            "the merge goes on once the answer is in"
        );
    }

    #[test]
    fn a_run_broken_by_a_verification_that_is_not_ready_starts_the_count_again() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        for round in 0..3 {
            assert!(merge_next(&mut c, round).is_some());
        }
        let fence = c.state.claims[&assuming.0].fence;
        let release = ClientMsg::Release {
            claim: assuming,
            fence,
            req: None,
        };
        c.handle(&agent("a1"), release, NOW);
        assert_eq!(pending(&c), 0);
        assert!(
            merge_next(&mut c, 3).is_some(),
            "no verification is waiting"
        );

        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        assert_eq!(pending(&c), 1);
        for round in 4..8 {
            assert!(
                merge_next(&mut c, round).is_some(),
                "merge {round} is not yet past the slot"
            );
        }
        let claim = grant(&mut c, "late", vec![edit("src/late.rs")], Vec::new());
        submit(&mut c, "late", claim, "src/late.rs");
        assert_eq!(
            c.begin_merge(NOW),
            None,
            "four merges in a row: the slot is due"
        );
    }

    #[test]
    fn the_slot_setting_is_never_stored() {
        let mut c = core();
        c.set_verify_slot_after(2);
        let stored = serde_json::to_value(&c).unwrap();
        assert!(stored.get("verify_slot_after").is_none(), "{stored}");
        let loaded: Coordinator = serde_json::from_value(stored).unwrap();
        assert_eq!(loaded.state.verify_slot_after, VERIFY_SLOT_AFTER);
    }

    /// A race of `r1` and `r2`, both assuming `src/b.rs`, judged after its deadline: their two
    /// trials are queued. An entry places no locks and is never challenged, so no assumption
    /// verification can be queued for it. Returns the two entry claims.
    fn judged_race_with_two_entries(c: &mut Coordinator) -> (ClaimId, ClaimId) {
        let deadline = NOW + 10_000;
        let hello = ClientMsg::Hello {
            agent: agent("felix"),
            base: CommitId(MAIN.into()),
            protocol: crate::protocol::PROTOCOL_VERSION,
        };
        c.handle(&agent("felix"), hello, NOW);
        let open = ClientMsg::OpenRace {
            req: RequestId(5),
            intent: Intent {
                summary: "race".into(),
                task_ref: None,
                assumptions: vec![assumes("src/b.rs", "b stays")],
            },
            scopes: vec![edit("src/a.rs")],
            max_entrants: 4,
            deadline_ms: deadline,
            criteria: vec![crate::protocol::Criterion::TestsPass],
        };
        let effects = c.handle(&agent("felix"), open, NOW);
        let [ServerMsg::RaceOpened { race, .. }] = replies(&effects)[..] else {
            panic!("expected RaceOpened, got {effects:?}");
        };
        let race = *race;
        let mut entries = Vec::new();
        for who in ["r1", "r2"] {
            let join = ClientMsg::JoinRace {
                req: RequestId(2),
                race,
            };
            let effects = c.handle(&agent(who), join, NOW);
            let [ServerMsg::Granted { claim, fence, .. }] = replies(&effects)[..] else {
                panic!("expected Granted, got {effects:?}");
            };
            let (claim, fence) = (*claim, *fence);
            let entry = ClientMsg::Submit {
                req: RequestId(9),
                claim,
                fence,
                fork_commit: CommitId(fork_sha(who)),
                touched: vec![edit("src/a.rs")],
                decisions: DecisionRecord {
                    evidence: vec!["tests passed".into()],
                    ..DecisionRecord::default()
                },
            };
            c.handle(&agent(who), entry, NOW);
            entries.push(claim);
        }
        c.expire(deadline);
        assert_eq!(pending(c), 2, "one trial per entry");
        (entries[0], entries[1])
    }

    fn lost_the_race(effects: &[Effect]) -> ClaimId {
        let lost: Vec<ClaimId> = logged(effects)
            .into_iter()
            .filter_map(|kind| {
                let EventKind::ClaimReleased { claim, reason } = kind else {
                    return None;
                };
                (*reason == ReleaseReason::LostRace).then_some(*claim)
            })
            .collect();
        assert_eq!(lost.len(), 1, "one loser expected in {effects:?}");
        lost[0]
    }

    #[test]
    fn a_race_concluded_by_recovery_drops_the_loser_s_trial_without_an_assumption_event() {
        let mut c = core();
        let (_, second) = judged_race_with_two_entries(&mut c);
        let winner_trial = c.begin_verification(NOW).unwrap();
        assert_eq!(winner_trial.agent, agent("r1"));
        c.verification_outcome(winner_trial.id, &clean_then(TrialOutcome::Clean {}), NOW);

        let mut now = NOW + 10_000;
        let mut last = Vec::new();
        for attempt in 1..=MAX_INFRA_RETRIES + 1 {
            let dispatch = c.begin_verification(now).expect("the second entry's trial");
            assert_eq!(dispatch.agent, agent("r2"));
            last = c.recover_verification(now);
            now += infra_backoff_ms(attempt);
        }

        assert_eq!(lost_the_race(&last), second);
        assert!(
            dropped_events(&last).is_empty(),
            "a trial has no assumption to name"
        );
        assert_eq!(pending(&c), 0);
    }

    #[test]
    fn a_race_closed_by_an_answer_drops_the_loser_s_trial_without_an_assumption_event() {
        let mut c = core();
        let (_, second) = judged_race_with_two_entries(&mut c);
        let winner_trial = c.begin_verification(NOW).unwrap();
        c.verification_outcome(winner_trial.id, &clean_then(TrialOutcome::Clean {}), NOW);
        let loser_trial = c.begin_verification(NOW).unwrap();
        assert_eq!(loser_trial.agent, agent("r2"));

        let effects = c.verification_outcome(
            loser_trial.id,
            &TrialReport {
                before: Some(TrialOutcome::TestsFailed {}),
                after: None,
            },
            NOW,
        );

        assert_eq!(lost_the_race(&effects), second);
        assert!(
            dropped_events(&effects).is_empty(),
            "a trial has no assumption to name"
        );
        assert_eq!(pending(&c), 0);
    }

    #[test]
    fn merges_without_a_waiting_verification_do_not_count_toward_the_slot() {
        let mut c = core();
        for round in 0..10 {
            assert!(merge_next(&mut c, round).is_some());
        }
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        for round in 10..14 {
            assert!(merge_next(&mut c, round).is_some(), "merge {round}");
        }
        assert!(c.begin_verification(NOW).is_some());
    }

    #[test]
    fn the_merge_count_is_stored_and_a_stored_state_without_it_loads_with_the_default() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        for round in 0..3 {
            merge_next(&mut c, round);
        }
        let mut reloaded: Coordinator =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        merge_next(&mut reloaded, 3);
        assert_eq!(reloaded.state.merges_past_verification, 4);

        let mut stored: serde_json::Value = serde_json::to_value(&c).unwrap();
        let map = stored.as_object_mut().unwrap();
        assert!(map.remove("merges_past_verification").is_some());
        let old: Coordinator = serde_json::from_value(stored).unwrap();
        assert_eq!(old.state.merges_past_verification, 0);
        assert_eq!(old.state.verify_slot_after, VERIFY_SLOT_AFTER);
    }

    #[test]
    fn a_claim_that_ended_while_its_verification_ran_has_the_answer_discarded() {
        let mut c = core();
        let (assuming, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let dispatch = c.begin_verification(NOW).unwrap();
        let fence = c.state.claims[&assuming.0].fence;
        let release = ClientMsg::Release {
            claim: assuming,
            fence,
            req: None,
        };
        c.handle(&agent("a1"), release, NOW);

        let effects =
            c.verification_outcome(dispatch.id, &clean_then(TrialOutcome::Conflict {}), NOW);

        assert!(effects.is_empty(), "{effects:?}");
        assert!(!c.has_verification_in_flight(), "the queue is not wedged");
    }

    #[test]
    fn a_stale_answer_changes_nothing() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let dispatch = c.begin_verification(NOW).unwrap();

        assert!(c
            .verification_outcome(dispatch.id + 1, &clean_then(TrialOutcome::Conflict {}), NOW)
            .is_empty());
        assert!(c.has_verification_in_flight());
        assert_eq!(pending(&c), 1);
        let again = c.verification_outcome(dispatch.id, &clean_then(TrialOutcome::Clean {}), NOW);
        assert_eq!(verified_events(&again).len(), 1);
        assert!(c
            .verification_outcome(dispatch.id, &clean_then(TrialOutcome::Conflict {}), NOW)
            .is_empty());
    }

    #[test]
    fn a_late_answer_from_an_earlier_dispatch_is_not_applied_to_the_later_one() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let first = c.begin_verification(NOW).unwrap();
        assert!(c.recover_verification(NOW).is_empty());
        let retry = NOW + INFRA_BACKOFF_BASE_MS;
        let second = c.begin_verification(retry).unwrap();
        let before = serde_json::to_string(&c).unwrap();

        let late = c.verification_outcome(first.id, &clean_then(TrialOutcome::Clean {}), retry);

        assert!(late.is_empty(), "the first dispatch's answer was applied");
        assert_eq!(serde_json::to_string(&c).unwrap(), before);
        let answered =
            c.verification_outcome(second.id, &clean_then(TrialOutcome::Clean {}), retry);
        assert_eq!(verified_events(&answered).len(), 1);
    }

    #[test]
    fn a_verification_dispatch_id_is_not_reused_across_a_restart() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let first = c.begin_verification(NOW).unwrap();
        let stored = serde_json::to_string(&c).unwrap();
        let mut restarted: Coordinator = serde_json::from_str(&stored).unwrap();
        assert!(restarted.recover_verification(NOW).is_empty());
        let retry = NOW + INFRA_BACKOFF_BASE_MS;
        let second = restarted.begin_verification(retry).unwrap();
        assert!(
            second.id > first.id,
            "{} is not past {}",
            second.id,
            first.id
        );
        let before = serde_json::to_string(&restarted).unwrap();

        let late =
            restarted.verification_outcome(first.id, &clean_then(TrialOutcome::Clean {}), retry);

        assert!(late.is_empty());
        assert_eq!(serde_json::to_string(&restarted).unwrap(), before);
        let answered =
            restarted.verification_outcome(second.id, &clean_then(TrialOutcome::Clean {}), retry);
        assert_eq!(verified_events(&answered).len(), 1);
    }

    #[test]
    fn a_newer_merge_replaces_a_queued_verification_of_the_same_assumption() {
        let mut c = core();
        let (assuming, first) = challenged(&mut c);
        merge_challenger(&mut c, first, MAIN);
        assert_eq!(pending(&c), 1);

        let second = grant(&mut c, "a3", vec![edit("src/a.rs")], Vec::new());
        submit(&mut c, "a3", second, "src/a.rs");
        c.begin_merge(NOW).unwrap();
        let after_first = MergeOutcome::Merged {
            base: CommitId(MAIN.into()),
            head: CommitId(MAIN2.into()),
        };
        c.merge_outcome(c.merge_flight_id(second.0), &after_first, NOW);

        assert_eq!(pending(&c), 1, "deduplicated");
        let queued = &c.state.verifications[0];
        assert_eq!(
            queued.main,
            CommitId(MAIN2.into()),
            "the newest main is kept"
        );
        assert_eq!(queued.claim, assuming);
        let dispatch = c.begin_verification(NOW).unwrap();
        assert_eq!(dispatch.main, CommitId(MAIN2.into()));
        assert_eq!(
            dispatch.before,
            CommitId("old".into()),
            "the baseline is main before the first challenging merge"
        );
    }

    #[test]
    fn a_merge_in_between_keeps_both_verifications_each_with_its_own_baseline() {
        let mut c = core();
        let (_, first) = challenged(&mut c);
        merge_challenger(&mut c, first, MAIN);
        // An unrelated merge moves main from MAIN to OTHER without challenging anything.
        let unrelated = grant(&mut c, "a4", vec![edit("src/z.rs")], Vec::new());
        submit(&mut c, "a4", unrelated, "src/z.rs");
        c.begin_merge(NOW).unwrap();
        let moved = MergeOutcome::Merged {
            base: CommitId(MAIN.into()),
            head: CommitId(OTHER.into()),
        };
        c.merge_outcome(c.merge_flight_id(unrelated.0), &moved, NOW);
        // The second challenging merge starts from OTHER, not from the queued verification's main.
        let second = grant(&mut c, "a3", vec![edit("src/a.rs")], Vec::new());
        submit(&mut c, "a3", second, "src/a.rs");
        c.begin_merge(NOW).unwrap();
        let after = MergeOutcome::Merged {
            base: CommitId(OTHER.into()),
            head: CommitId(MAIN2.into()),
        };
        c.merge_outcome(c.merge_flight_id(second.0), &after, NOW);

        assert_eq!(pending(&c), 2, "not deduplicated across an unrelated merge");
        let spans: Vec<_> = c
            .state
            .verifications
            .iter()
            .map(|v| (v.before.0.as_str(), v.main.0.as_str()))
            .collect();
        assert_eq!(spans, [("old", MAIN), (OTHER, MAIN2)]);
    }

    #[test]
    fn different_assumptions_of_one_claim_are_not_deduplicated() {
        let mut c = core();
        grant(
            &mut c,
            "a1",
            vec![edit("src/b.rs")],
            vec![assumes("src/a.rs", "one"), assumes("src/c.rs", "two")],
        );
        let first = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        submit(&mut c, "a2", first, "src/a.rs");
        merge_challenger(&mut c, first.0, MAIN);
        let second = grant(&mut c, "a3", vec![edit("src/c.rs")], Vec::new());
        submit(&mut c, "a3", second, "src/c.rs");
        merge_challenger(&mut c, second.0, MAIN2);

        assert_eq!(pending(&c), 2);
    }

    #[test]
    fn the_older_verification_runs_first_and_the_newer_one_queues_behind_it() {
        let mut c = core();
        let (assuming, first) = challenged(&mut c);
        merge_challenger(&mut c, first, MAIN);
        let second = grant(&mut c, "a3", vec![edit("src/a.rs")], Vec::new());
        submit(&mut c, "a3", second, "src/a.rs");
        merge_challenger(&mut c, second.0, MAIN2);
        assert_eq!(pending(&c), 2);
        let running = c.begin_verification(NOW).unwrap();
        assert_eq!(running.main, CommitId(MAIN.into()));

        let done = c.verification_outcome(running.id, &clean_then(TrialOutcome::Conflict {}), NOW);
        assert_eq!(
            verified_events(&done),
            [(assuming, Outcome::TextualConflict)]
        );
        assert_eq!(pending(&c), 1);
        assert_eq!(
            c.begin_verification(NOW).unwrap().main,
            CommitId(MAIN2.into())
        );
    }

    #[test]
    fn the_alarm_waits_for_the_watchdog_while_a_verification_runs_here() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        assert_eq!(c.next_wake_ms(false, false, NOW), Some(NOW));
        c.begin_verification(NOW).unwrap();

        assert_eq!(
            c.next_verification_ms(true, NOW),
            Some(NOW + MERGE_WATCHDOG_MS)
        );
        assert_eq!(
            c.next_verification_ms(false, NOW),
            Some(NOW),
            "recover at once"
        );
        let wake = c.next_wake_ms(false, true, NOW).unwrap();
        assert!(
            wake > NOW,
            "only a lease expiry or the watchdog is scheduled"
        );
    }

    #[test]
    fn the_wake_time_is_never_before_now_and_covers_all_three_queues() {
        let mut c = core();
        assert_eq!(c.next_wake_ms(false, false, NOW), None);
        let (_, challenger) = challenged(&mut c);
        // a1's lease is the only thing due, and a2's submission is a merge.
        assert_eq!(c.next_wake_ms(false, false, NOW), Some(NOW));
        merge_challenger(&mut c, challenger, MAIN);
        let wake = c.next_wake_ms(false, false, NOW + 5).unwrap();
        assert_eq!(wake, NOW + 5, "a verification is due now");
    }

    #[test]
    fn a_shadow_claim_is_never_challenged_so_it_is_never_verified() {
        let mut c = core();
        let blocker = grant(&mut c, "a1", vec![edit("src/a.rs")], Vec::new());
        let shadow = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent {
                summary: "s".into(),
                task_ref: None,
                assumptions: vec![assumes("src/a.rs", "f returns Some")],
            },
            scopes: vec![edit("src/a.rs")],
            on_conflict: OnConflict::Shadow,
        };
        let effects = c.handle(&agent("a2"), shadow, NOW);
        assert!(
            matches!(replies(&effects)[..], [ServerMsg::Shadowed { .. }]),
            "{effects:?}"
        );
        submit(&mut c, "a1", blocker, "src/a.rs");
        merge_challenger(&mut c, blocker.0, MAIN);
        assert_eq!(pending(&c), 0);
    }
    #[test]
    fn a_verification_stored_before_races_loads_as_an_assumption_and_keeps_its_shape() {
        let stored = r#"{"id":4,"agent":"a1","claim":2,
            "assumption":{"scope":{"kind":"file","path":"src/a.rs"},"statement":"f returns Some"},
            "before":"old","main":"new"}"#;
        let queued: Verification = serde_json::from_str(stored).unwrap();
        assert_eq!(
            queued.subject,
            Subject::Assumption {
                assumption: assumes("src/a.rs", "f returns Some")
            }
        );
        let again = serde_json::to_value(&queued).unwrap();
        assert_eq!(again["assumption"]["statement"], "f returns Some");
        assert!(again.get("subject").is_none() && again.get("race").is_none());
    }

    #[test]
    fn a_race_trial_round_trips_and_is_not_mistaken_for_an_assumption() {
        let queued = Verification {
            id: 1,
            agent: agent("a1"),
            claim: ClaimId(3),
            subject: Subject::RaceEntry { race: RaceId(2) },
            before: CommitId(MAIN.into()),
            main: CommitId(MAIN.into()),
            infra_failures: 0,
            retry_at_ms: None,
        };
        let stored = serde_json::to_string(&queued).unwrap();
        assert!(
            stored.contains("\"race\":2") && !stored.contains("assumption"),
            "{stored}"
        );
        let loaded: Verification = serde_json::from_str(&stored).unwrap();
        assert_eq!(loaded.subject, Subject::RaceEntry { race: RaceId(2) });
    }

    // ---- shadow verification (invariant 10) ----

    /// `who` asks for `path` while it is blocked, with `OnConflict::Shadow`.
    fn shadow(c: &mut Coordinator, who: &str, path: &str) -> (ClaimId, Fence) {
        let msg = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent {
                summary: "s".into(),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes: vec![edit(path)],
            on_conflict: OnConflict::Shadow,
        };
        let effects = c.handle(&agent(who), msg, NOW);
        let Some(ServerMsg::Shadowed { claim, fence, .. }) = replies(&effects).into_iter().next()
        else {
            panic!("expected Shadowed, got {effects:?}");
        };
        (*claim, *fence)
    }

    /// `a2` holds `src/a.rs` and has submitted it; `a1` is shadowed on it and has submitted too.
    fn shadowed_and_submitted(c: &mut Coordinator) -> (ClaimId, ClaimId) {
        let blocker = grant(c, "a2", vec![edit("src/a.rs")], Vec::new());
        let shadowed = shadow(c, "a1", "src/a.rs");
        submit(c, "a1", shadowed, "src/a.rs");
        submit(c, "a2", blocker, "src/a.rs");
        (shadowed.0, blocker.0)
    }

    fn denial_events(effects: &[Effect]) -> Vec<(ClaimId, ClaimId, Outcome)> {
        logged(effects)
            .into_iter()
            .filter_map(|kind| {
                let EventKind::DenialVerified {
                    shadow_claim,
                    blocking_claim,
                    outcome,
                } = kind
                else {
                    return None;
                };
                Some((*shadow_claim, *blocking_claim, *outcome))
            })
            .collect()
    }

    fn run_trial(c: &mut Coordinator, report: &TrialReport) -> Vec<Effect> {
        let dispatch = c.begin_verification(NOW).expect("a trial is due");
        c.verification_outcome(dispatch.id, report, NOW)
    }

    fn summary_of(steps: &[&[Effect]]) -> Summary {
        Summary::from_events(&all_events(steps))
    }

    #[test]
    fn a_conflict_after_the_blocker_merged_is_a_verified_prevention() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        let merged = merge_challenger(&mut c, blocker, MAIN);
        assert_eq!(pending(&c), 1);
        assert!(
            denial_events(&merged).is_empty(),
            "nothing is logged by the merge"
        );

        let effects = run_trial(&mut c, &clean_then(TrialOutcome::Conflict {}));

        assert_eq!(
            denial_events(&effects),
            [(shadowed, blocker, Outcome::TextualConflict)]
        );
        let summary = summary_of(&[&merged, &effects]);
        assert_eq!(summary.conflicts_prevented_verified, 1);
        assert_eq!(summary.false_alarms, 0);
        assert_eq!(pending(&c), 0);
    }

    #[test]
    fn failing_tests_after_the_merge_count_as_a_prevention_too() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        merge_challenger(&mut c, blocker, MAIN);
        let effects = run_trial(&mut c, &clean_then(TrialOutcome::TestsFailed {}));
        assert_eq!(
            denial_events(&effects),
            [(shadowed, blocker, Outcome::TestsFailed)]
        );
    }

    #[test]
    fn a_clean_trial_is_a_false_alarm_and_not_a_prevention() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        merge_challenger(&mut c, blocker, MAIN);

        let effects = run_trial(&mut c, &clean_then(TrialOutcome::Clean {}));

        assert_eq!(
            denial_events(&effects),
            [(shadowed, blocker, Outcome::Clean)]
        );
        let summary = summary_of(&[&effects]);
        assert_eq!(summary.false_alarms, 1);
        assert_eq!(summary.conflicts_prevented_verified, 0);
    }

    #[test]
    fn work_already_red_on_the_baseline_is_not_a_prevention() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        merge_challenger(&mut c, blocker, MAIN);

        let red = TrialReport {
            before: Some(TrialOutcome::TestsFailed {}),
            after: Some(TrialOutcome::TestsFailed {}),
        };
        let effects = run_trial(&mut c, &red);

        assert_eq!(
            denial_events(&effects),
            [(shadowed, blocker, Outcome::Inconclusive)]
        );
        let summary = summary_of(&[&effects]);
        assert_eq!(summary.conflicts_prevented_verified, 0);
        assert_eq!(summary.false_alarms, 0);
    }

    #[test]
    fn a_trial_that_gave_up_records_no_prevention() {
        let mut c = core();
        let (_, blocker) = shadowed_and_submitted(&mut c);
        merge_challenger(&mut c, blocker, MAIN);
        let broken = clean_then(TrialOutcome::Timeout {});
        let mut now = NOW;
        let mut last = Vec::new();
        for _ in 0..=MAX_INFRA_RETRIES {
            let dispatch = c.begin_verification(now).unwrap();
            last = c.verification_outcome(dispatch.id, &broken, now);
            now += infra_backoff_ms(MAX_INFRA_RETRIES) + 1;
        }
        assert_eq!(denial_events(&last).len(), 1);
        let summary = summary_of(&[&last]);
        assert_eq!(summary.conflicts_prevented_verified, 0);
        assert_eq!(summary.false_alarms, 0);
        assert_eq!(pending(&c), 0);
    }

    #[test]
    fn a_blocker_released_without_merging_verifies_nothing() {
        let mut c = core();
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let shadowed = shadow(&mut c, "a1", "src/a.rs");
        submit(&mut c, "a1", shadowed, "src/a.rs");
        let release = ClientMsg::Release {
            claim: blocker.0,
            fence: blocker.1,
            req: None,
        };
        c.handle(&agent("a2"), release, NOW);

        assert_eq!(pending(&c), 0);
        assert_eq!(c.begin_verification(NOW), None);
    }

    #[test]
    fn a_shadow_claim_that_never_submitted_is_not_tried() {
        let mut c = core();
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        shadow(&mut c, "a1", "src/a.rs");
        submit(&mut c, "a2", blocker, "src/a.rs");

        merge_challenger(&mut c, blocker.0, MAIN);

        assert_eq!(pending(&c), 0);
        assert_eq!(c.begin_verification(NOW), None);
    }

    #[test]
    fn a_shadow_claim_that_submits_after_the_merge_is_not_tried() {
        let mut c = core();
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let shadowed = shadow(&mut c, "a1", "src/a.rs");
        submit(&mut c, "a2", blocker, "src/a.rs");
        merge_challenger(&mut c, blocker.0, MAIN);

        submit(&mut c, "a1", shadowed, "src/a.rs");

        assert_eq!(pending(&c), 0);
    }

    #[test]
    fn a_merge_of_a_claim_that_did_not_block_the_shadow_verifies_nothing() {
        let mut c = core();
        grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let shadowed = shadow(&mut c, "a1", "src/a.rs");
        submit(&mut c, "a1", shadowed, "src/a.rs");

        merge_edit(&mut c, "a3", "src/z.rs", MAIN);

        assert_eq!(pending(&c), 0);
    }

    #[test]
    fn several_shadow_claims_blocked_by_one_merge_get_one_verdict_each() {
        let mut c = core();
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let one = shadow(&mut c, "a1", "src/a.rs");
        let two = shadow(&mut c, "a3", "src/a.rs");
        let three = shadow(&mut c, "a4", "src/a.rs");
        for (who, claim) in [("a1", one), ("a3", two), ("a4", three)] {
            submit(&mut c, who, claim, "src/a.rs");
        }
        submit(&mut c, "a2", blocker, "src/a.rs");
        merge_challenger(&mut c, blocker.0, MAIN);
        assert_eq!(pending(&c), 3);

        let mut verdicts = Vec::new();
        for report in [
            clean_then(TrialOutcome::Conflict {}),
            clean_then(TrialOutcome::Clean {}),
            clean_then(TrialOutcome::Conflict {}),
        ] {
            verdicts.extend(denial_events(&run_trial(&mut c, &report)));
        }

        assert_eq!(
            verdicts,
            [
                (one.0, blocker.0, Outcome::TextualConflict),
                (two.0, blocker.0, Outcome::Clean),
                (three.0, blocker.0, Outcome::TextualConflict),
            ]
        );
        assert_eq!(c.begin_verification(NOW), None);
    }

    #[test]
    fn the_trial_tries_the_shadow_commit_on_the_merges_baseline_and_main() {
        let mut c = core();
        let (_, blocker) = shadowed_and_submitted(&mut c);
        merge_challenger(&mut c, blocker, MAIN);

        let dispatch = c.begin_verification(NOW).unwrap();

        assert_eq!(dispatch.agent, agent("a1"));
        assert_eq!(dispatch.commit, Some(CommitId(fork_sha("a1"))));
        assert_eq!(dispatch.before, CommitId("old".into()));
        assert_eq!(dispatch.main, CommitId(MAIN.into()));
    }

    #[test]
    fn a_shadow_claim_blocked_by_two_claims_is_tried_for_each_that_merges() {
        let mut c = core();
        let first = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let second = grant(&mut c, "a3", vec![edit("src/b.rs")], Vec::new());
        let msg = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent {
                summary: "s".into(),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes: vec![edit("src/a.rs"), edit("src/b.rs")],
            on_conflict: OnConflict::Shadow,
        };
        let effects = c.handle(&agent("a1"), msg, NOW);
        let Some(ServerMsg::Shadowed { claim, fence, .. }) = replies(&effects).into_iter().next()
        else {
            panic!("expected Shadowed, got {effects:?}");
        };
        submit(&mut c, "a1", (*claim, *fence), "src/a.rs");
        submit(&mut c, "a2", first, "src/a.rs");

        merge_challenger(&mut c, first.0, MAIN);
        assert_eq!(pending(&c), 1, "only the claim that merged is tried so far");
        let one = run_trial(&mut c, &clean_then(TrialOutcome::Clean {}));
        submit(&mut c, "a3", second, "src/b.rs");
        merge_challenger(&mut c, second.0, MAIN2);
        let two = run_trial(&mut c, &clean_then(TrialOutcome::Conflict {}));

        assert_eq!(denial_events(&one), [(*claim, first.0, Outcome::Clean)]);
        assert_eq!(
            denial_events(&two),
            [(*claim, second.0, Outcome::TextualConflict)]
        );
    }

    #[test]
    fn a_shadow_trial_survives_a_restart_queued_and_in_flight() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        merge_challenger(&mut c, blocker, MAIN);

        let stored = serde_json::to_string(&c).unwrap();
        let mut queued: Coordinator = serde_json::from_str(&stored).unwrap();
        assert_eq!(pending(&queued), 1);
        queued.begin_verification(NOW).unwrap();

        let stored = serde_json::to_string(&queued).unwrap();
        let mut restarted: Coordinator = serde_json::from_str(&stored).unwrap();
        assert!(restarted.has_verification_in_flight());
        assert!(restarted.recover_verification(NOW).is_empty());
        let retry = NOW + INFRA_BACKOFF_BASE_MS;
        let dispatch = restarted.begin_verification(retry).unwrap();
        assert_eq!(dispatch.attempt, 2);
        let report = clean_then(TrialOutcome::Conflict {});
        let effects = restarted.verification_outcome(dispatch.id, &report, retry);

        assert_eq!(
            denial_events(&effects),
            [(shadowed, blocker, Outcome::TextualConflict)]
        );
    }

    #[test]
    fn a_shadow_submission_is_never_queued_for_merge() {
        let mut c = core();
        let (shadowed, _) = shadowed_and_submitted(&mut c);
        let dispatch = c.begin_merge(NOW).unwrap();
        assert_ne!(dispatch.claim, shadowed);
        assert!(c.state.claims[&shadowed.0].work.is_none());
    }

    #[test]
    fn a_shadow_verification_round_trips_and_is_not_mistaken_for_another_subject() {
        let queued = Verification {
            id: 1,
            agent: agent("a1"),
            claim: ClaimId(3),
            subject: Subject::Shadow {
                blocking_claim: ClaimId(2),
            },
            before: CommitId("old".into()),
            main: CommitId(MAIN.into()),
            infra_failures: 0,
            retry_at_ms: None,
        };
        let stored = serde_json::to_string(&queued).unwrap();
        let loaded: Verification = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            loaded.subject,
            Subject::Shadow {
                blocking_claim: ClaimId(2)
            }
        );
    }

    // ---- dropping finished shadow claims ----

    fn is_live(c: &Coordinator, claim: ClaimId) -> bool {
        c.state.claims.contains_key(&claim.0)
    }

    fn reloaded(c: &Coordinator) -> Coordinator {
        serde_json::from_str(&serde_json::to_string(c).unwrap()).unwrap()
    }

    fn release(c: &mut Coordinator, who: &str, claim: (ClaimId, Fence)) {
        let release = ClientMsg::Release {
            claim: claim.0,
            fence: claim.1,
            req: None,
        };
        c.handle(&agent(who), release, NOW);
    }

    #[test]
    fn a_shadow_is_dropped_after_its_last_blocker_merged_and_its_trial_is_logged() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        merge_challenger(&mut c, blocker, MAIN);

        let effects = run_trial(&mut c, &clean_then(TrialOutcome::Conflict {}));

        assert_eq!(
            denial_events(&effects),
            [(shadowed, blocker, Outcome::TextualConflict)]
        );
        assert!(!is_live(&c, shadowed));
        assert!(!is_live(&reloaded(&c), shadowed));
    }

    #[test]
    fn a_shadow_stays_while_its_trial_is_queued_or_running() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        merge_challenger(&mut c, blocker, MAIN);
        assert!(is_live(&c, shadowed), "trial queued");
        c.expire(NOW + 1);
        assert!(is_live(&c, shadowed), "trial queued after an expiry sweep");

        let dispatch = c.begin_verification(NOW).unwrap();
        c.expire(NOW + 2);
        assert!(is_live(&c, shadowed), "trial running");
        let mut restarted = reloaded(&c);
        restarted.recover_verification(NOW);
        assert!(is_live(&restarted, shadowed), "trial waiting in backoff");

        let report = clean_then(TrialOutcome::Clean {});
        let effects = c.verification_outcome(dispatch.id, &report, NOW);
        assert_eq!(denial_events(&effects).len(), 1);
        assert!(!is_live(&c, shadowed));
    }

    #[test]
    fn a_shadow_is_dropped_when_recovery_gives_up_on_its_trial() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        merge_challenger(&mut c, blocker, MAIN);
        let mut now = NOW;
        let mut effects = Vec::new();
        for attempt in 0..=MAX_INFRA_RETRIES {
            assert!(is_live(&c, shadowed), "attempt {attempt}");
            now += 10 * INFRA_BACKOFF_BASE_MS * 2u64.pow(attempt);
            c.begin_verification(now).expect("the trial is due");
            effects = c.recover_verification(now);
        }

        assert_eq!(
            denial_events(&effects),
            [(shadowed, blocker, Outcome::Inconclusive)]
        );
        assert!(!is_live(&c, shadowed));
    }

    #[test]
    fn a_shadow_whose_blocker_was_released_without_merging_is_dropped() {
        let mut c = core();
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let shadowed = shadow(&mut c, "a1", "src/a.rs");
        submit(&mut c, "a1", shadowed, "src/a.rs");
        assert!(is_live(&c, shadowed.0), "the blocker is still live");

        release(&mut c, "a2", blocker);

        assert!(!is_live(&c, shadowed.0));
    }

    #[test]
    fn a_shadow_whose_blocker_expired_is_dropped_on_the_alarm() {
        let mut c = core();
        grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let shadowed = shadow(&mut c, "a1", "src/a.rs");
        submit(&mut c, "a1", shadowed, "src/a.rs");

        c.expire(NOW + LEASE + 1);

        assert!(!is_live(&c, shadowed.0));
    }

    #[test]
    fn a_shadow_that_submits_after_its_blockers_ended_is_dropped_at_submit() {
        let mut c = core();
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let shadowed = shadow(&mut c, "a1", "src/a.rs");
        release(&mut c, "a2", blocker);
        assert!(is_live(&c, shadowed.0), "unsubmitted: its lease governs");

        submit(&mut c, "a1", shadowed, "src/a.rs");

        assert!(!is_live(&c, shadowed.0));
    }

    #[test]
    fn a_shadow_blocked_only_by_a_race_is_dropped_at_submit() {
        let mut c = core();
        let open = ClientMsg::OpenRace {
            req: RequestId(5),
            intent: Intent {
                summary: "task".into(),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes: vec![edit("src/a.rs")],
            max_entrants: 2,
            deadline_ms: NOW + LEASE / 2,
            criteria: vec![crate::protocol::Criterion::FirstSubmitted],
        };
        c.handle(&agent("felix"), open, NOW);
        let shadowed = shadow(&mut c, "a1", "src/a.rs");
        assert!(c.state.claims[&shadowed.0 .0]
            .denial
            .as_ref()
            .is_some_and(|denial| denial.blocked_by.is_empty()));

        submit(&mut c, "a1", shadowed, "src/a.rs");

        assert!(!is_live(&c, shadowed.0));
    }

    #[test]
    fn shadows_on_one_blocker_are_each_dropped_after_their_own_trial() {
        let mut c = core();
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let mut shadows = Vec::new();
        for who in ["a1", "a3", "a4"] {
            let claim = shadow(&mut c, who, "src/a.rs");
            submit(&mut c, who, claim, "src/a.rs");
            shadows.push(claim.0);
        }
        submit(&mut c, "a2", blocker, "src/a.rs");
        merge_challenger(&mut c, blocker.0, MAIN);

        for (done, expected) in [(1, [false, true, true]), (2, [false, false, true])] {
            run_trial(&mut c, &clean_then(TrialOutcome::Clean {}));
            let live: Vec<bool> = shadows.iter().map(|id| is_live(&c, *id)).collect();
            assert_eq!(live, expected, "after {done} trials");
        }
        run_trial(&mut c, &clean_then(TrialOutcome::Clean {}));
        assert!(c.state.claims.is_empty());
    }

    #[test]
    fn a_shadow_with_two_blockers_stays_until_both_are_settled() {
        let mut c = core();
        let first = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let second = grant(&mut c, "a3", vec![edit("src/b.rs")], Vec::new());
        let msg = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent {
                summary: "s".into(),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes: vec![edit("src/a.rs"), edit("src/b.rs")],
            on_conflict: OnConflict::Shadow,
        };
        let effects = c.handle(&agent("a1"), msg, NOW);
        let Some(ServerMsg::Shadowed { claim, fence, .. }) = replies(&effects).into_iter().next()
        else {
            panic!("expected Shadowed, got {effects:?}");
        };
        submit(&mut c, "a1", (*claim, *fence), "src/a.rs");
        submit(&mut c, "a2", first, "src/a.rs");
        merge_challenger(&mut c, first.0, MAIN);
        run_trial(&mut c, &clean_then(TrialOutcome::Clean {}));
        assert!(is_live(&c, *claim), "the second blocker may still merge");

        let mut restarted = reloaded(&c);
        submit(&mut restarted, "a3", second, "src/b.rs");
        merge_challenger(&mut restarted, second.0, MAIN2);
        assert!(is_live(&restarted, *claim), "its second trial is owed");
        let effects = run_trial(&mut restarted, &clean_then(TrialOutcome::Conflict {}));

        assert_eq!(denial_events(&effects).len(), 1);
        assert!(!is_live(&restarted, *claim));
    }

    #[test]
    fn stored_state_stays_bounded_over_many_shadow_submits() {
        let mut c = core();
        for round in 0..1_000 {
            let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
            let shadowed = shadow(&mut c, "a1", "src/a.rs");
            submit(&mut c, "a1", shadowed, "src/a.rs");
            if round % 2 == 0 {
                release(&mut c, "a2", blocker);
            } else {
                submit(&mut c, "a2", blocker, "src/a.rs");
                merge_challenger(&mut c, blocker.0, MAIN);
                run_trial(&mut c, &clean_then(TrialOutcome::Clean {}));
            }
            assert!(c.state.claims.is_empty(), "round {round}");
        }
        assert!(c.state.verifications.is_empty());
        assert!(serde_json::to_string(&c).unwrap().len() < 2_000);
    }

    #[test]
    fn a_stored_submitted_shadow_without_a_denial_is_dropped_on_the_next_call() {
        let mut c = core();
        let (shadowed, _) = shadowed_and_submitted(&mut c);
        let mut value = serde_json::to_value(&c).unwrap();
        let claim = &mut value["claims"][shadowed.0.to_string()];
        assert!(claim["denial"].is_object());
        claim.as_object_mut().unwrap().remove("denial");
        let mut old: Coordinator = serde_json::from_value(value).unwrap();
        assert!(is_live(&old, shadowed));

        old.expire(NOW);

        assert!(!is_live(&old, shadowed));
    }

    #[test]
    fn a_stored_shadow_with_a_live_blocker_loads_and_is_still_tried() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        let mut stored = reloaded(&c);
        assert!(is_live(&stored, shadowed));

        merge_challenger(&mut stored, blocker, MAIN);
        let effects = run_trial(&mut stored, &clean_then(TrialOutcome::Conflict {}));

        assert_eq!(denial_events(&effects).len(), 1);
        assert!(!is_live(&stored, shadowed));
    }

    // ---- the log of a dropped shadow ----

    fn settled(effects: &[Effect]) -> Vec<ClaimId> {
        logged(effects)
            .into_iter()
            .filter_map(|kind| {
                let EventKind::ClaimReleased { claim, reason } = kind else {
                    return None;
                };
                match reason {
                    ReleaseReason::Settled => Some(*claim),
                    ReleaseReason::Agent
                    | ReleaseReason::LeaseExpired
                    | ReleaseReason::LostRace
                    | ReleaseReason::Merged => None,
                }
            })
            .collect()
    }

    #[test]
    fn each_dropped_shadow_logs_exactly_one_settled_release_in_claim_order() {
        let mut c = core();
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let mut shadows = Vec::new();
        for who in ["a1", "a3", "a4"] {
            let claim = shadow(&mut c, who, "src/a.rs");
            submit(&mut c, who, claim, "src/a.rs");
            shadows.push(claim.0);
        }

        let effects = c.handle(
            &agent("a2"),
            ClientMsg::Release {
                claim: blocker.0,
                fence: blocker.1,
                req: None,
            },
            NOW,
        );

        assert_eq!(settled(&effects), shadows);
        let again = c.expire(NOW + 1);
        assert!(
            settled(&again).is_empty(),
            "a dropped shadow is not logged twice"
        );
    }

    #[test]
    fn replaying_the_log_shows_the_shadow_ended_after_its_trial() {
        let mut c = core();
        let mut steps = Vec::new();
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let shadowed = shadow(&mut c, "a1", "src/a.rs");
        submit(&mut c, "a1", shadowed, "src/a.rs");
        submit(&mut c, "a2", blocker, "src/a.rs");
        steps.push(merge_challenger(&mut c, blocker.0, MAIN));
        steps.push(run_trial(&mut c, &clean_then(TrialOutcome::Conflict {})));

        let slices: Vec<&[Effect]> = steps.iter().map(Vec::as_slice).collect();
        let events = all_events(&slices);
        let mut live = HashSet::new();
        for event in &events {
            if let EventKind::ClaimShadowed { claim, .. } = &event.kind {
                live.insert(*claim);
            } else if let EventKind::ClaimReleased { claim, .. } = &event.kind {
                live.remove(claim);
            }
        }
        assert!(live.is_empty(), "the log still shows {live:?} alive");
        let verified = events
            .iter()
            .position(|e| matches!(e.kind, EventKind::DenialVerified { .. }));
        let released = events.iter().position(|e| {
            matches!(
                e.kind,
                EventKind::ClaimReleased { claim, reason: ReleaseReason::Settled }
                    if claim == shadowed.0
            )
        });
        assert!(
            verified < released,
            "the verdict is logged before the release"
        );
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        assert!(seqs.windows(2).all(|pair| pair[1] == pair[0] + 1));
    }

    #[test]
    fn a_shadow_stays_while_a_blocker_rejected_in_the_merge_is_back_to_active() {
        let mut c = core();
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        c.begin_merge(NOW).expect("a merge is due");
        let conflict = MergeOutcome::Conflict {
            files: vec!["src/a.rs".into()],
            total: None,
        };

        let effects = c.merge_outcome(c.merge_flight_id(blocker), &conflict, NOW);

        assert!(settled(&effects).is_empty());
        assert!(is_live(&c, blocker), "a rejected claim returns to active");
        assert!(
            is_live(&c, shadowed),
            "the blocker may still be resubmitted"
        );
    }

    #[test]
    fn a_settled_release_does_not_change_the_summary() {
        let mut c = core();
        let (_, blocker) = shadowed_and_submitted(&mut c);
        let merged = merge_challenger(&mut c, blocker, MAIN);
        let trial = run_trial(&mut c, &clean_then(TrialOutcome::Conflict {}));
        let all = all_events(&[&merged, &trial]);
        let without: Vec<Event> = all
            .iter()
            .filter(|e| {
                !matches!(
                    e.kind,
                    EventKind::ClaimReleased {
                        reason: ReleaseReason::Settled,
                        ..
                    }
                )
            })
            .cloned()
            .collect();
        assert!(without.len() < all.len(), "the trial settled the shadow");

        assert_eq!(Summary::from_events(&all), Summary::from_events(&without));
    }

    #[test]
    fn a_review_decision_names_the_reviewer() {
        let mut c = core();
        grant(
            &mut c,
            "a1",
            vec![edit("src/b.rs")],
            vec![assumes("src/a.rs", "f returns Some")],
        );
        let claim = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        let submit = ClientMsg::Submit {
            req: RequestId(9),
            claim: claim.0,
            fence: claim.1,
            fork_commit: CommitId(fork_sha("a2")),
            touched: vec![edit("src/a.rs")],
            decisions: DecisionRecord::default(),
        };
        c.handle(&agent("a2"), submit, NOW);
        let review = ClientMsg::Review {
            req: RequestId(10),
            claim: claim.0,
            approve: true,
            note: None,
        };

        let effects = c.handle(&agent("felix"), review, NOW);

        let reviewers: Vec<_> = logged(&effects)
            .into_iter()
            .filter_map(|kind| {
                let EventKind::ReviewDecided { reviewer, .. } = kind else {
                    return None;
                };
                Some(reviewer.clone())
            })
            .collect();
        assert_eq!(reviewers, [Some(agent("felix"))]);
    }

    // ---- landings with no baseline say what they skipped ----

    fn landing_of(effects: &[Effect]) -> (Vec<UnverifiedAssumption>, Vec<ClaimId>) {
        let mut merged = Vec::new();
        for kind in logged(effects) {
            if let EventKind::Merged {
                unverified,
                untried_shadows,
                ..
            } = kind
            {
                merged.push((unverified.clone(), untried_shadows.clone()));
            }
        }
        assert_eq!(merged.len(), 1, "{effects:?}");
        merged.remove(0)
    }

    /// The landing left the assumption unverified and counted, with nothing queued, dropped or
    /// broken.
    fn assert_unverifiable(c: &Coordinator, effects: &[Effect], assuming: ClaimId) {
        let (unverified, untried) = landing_of(effects);
        let expected = UnverifiedAssumption {
            claim: assuming,
            assumption: assumes("src/a.rs", "f returns Some"),
        };
        assert_eq!(unverified, [expected]);
        assert!(untried.is_empty());
        assert_eq!(pending(c), 0);
        let summary = summary_of(&[effects]);
        assert_eq!(summary.verifications_unverifiable, 1);
        assert_eq!(summary.verifications_dropped, 0);
        assert_eq!(summary.assumptions_confirmed_broken, 0);
    }

    #[test]
    fn an_already_merged_first_dispatch_lists_the_assumption_it_could_not_verify() {
        let mut c = core();
        merge_edit(&mut c, "a0", "src/z.rs", MAIN);
        let (assuming, challenger) = challenged(&mut c);
        c.begin_merge(NOW).expect("a merge is due");
        let effects = c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN2), NOW);

        assert_unverifiable(&c, &effects, assuming);
    }

    #[test]
    fn a_recovery_with_no_known_head_at_dispatch_lists_the_assumption() {
        let mut c = core();
        for who in ["a1", "a2"] {
            let hello = ClientMsg::Hello {
                agent: agent(who),
                base: CommitId("0".repeat(40)),
                protocol: 1,
            };
            c.handle(&agent(who), hello, NOW);
        }
        let (assuming, challenger) = challenged(&mut c);
        c.begin_merge(NOW).expect("a merge is due");
        c.recover_merge(NOW);
        let work = c.state.claims[&challenger.0].work.as_ref().unwrap();
        let due = work.retry_at_ms.expect("a retry is scheduled");
        c.begin_merge(due).expect("the retry is dispatched");
        let effects = c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN2), due);

        assert_unverifiable(&c, &effects, assuming);
    }

    #[test]
    fn a_recovery_that_left_main_unchanged_lists_the_assumption() {
        let mut c = core();
        let (challenger, due) = challenger_with_lost_answer(&mut c);
        let assuming = ClaimId(challenger.0 - 1);
        c.begin_merge(due).expect("the retry is dispatched");
        let effects = c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN), due);

        assert_unverifiable(&c, &effects, assuming);
    }

    #[test]
    fn a_landing_with_a_baseline_lists_nothing() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        let effects = merge_challenger(&mut c, challenger, MAIN);

        let (unverified, untried) = landing_of(&effects);
        assert!(unverified.is_empty() && untried.is_empty());
        assert_eq!(summary_of(&[&effects]).verifications_unverifiable, 0);
    }

    #[test]
    fn an_assumption_whose_claim_has_ended_is_not_listed() {
        let mut c = core();
        merge_edit(&mut c, "a0", "src/z.rs", MAIN);
        let (assuming, challenger) = challenged(&mut c);
        let release = ClientMsg::Release {
            req: Some(RequestId(11)),
            claim: assuming,
            fence: c.state.claims[&assuming.0].fence,
        };
        c.handle(&agent("a1"), release, NOW);
        c.begin_merge(NOW).expect("a merge is due");
        let effects = c.merge_outcome(c.merge_flight_id(challenger), &already_merged(MAIN2), NOW);

        let (unverified, _) = landing_of(&effects);
        assert!(
            unverified.is_empty(),
            "nothing was in doubt: {unverified:?}"
        );
        assert_eq!(summary_of(&[&effects]).verifications_unverifiable, 0);
    }

    #[test]
    fn a_submitted_shadow_the_landing_could_not_try_is_listed_and_then_settled() {
        let mut c = core();
        merge_edit(&mut c, "a0", "src/z.rs", MAIN);
        let (shadowed, blocker) = shadowed_and_submitted(&mut c);
        c.begin_merge(NOW).expect("a merge is due");
        let effects = c.merge_outcome(c.merge_flight_id(blocker), &already_merged(MAIN2), NOW);

        let (unverified, untried) = landing_of(&effects);
        assert!(unverified.is_empty());
        assert_eq!(untried, [shadowed]);
        assert_eq!(pending(&c), 0);
        assert!(denial_events(&effects).is_empty());
        assert_eq!(summary_of(&[&effects]).verifications_unverifiable, 0);
        let next = c.handle(&agent("a1"), ClientMsg::Heartbeat, NOW);
        assert_eq!(settled(&next), [shadowed], "{next:?}");
        assert!(!is_live(&c, shadowed));
    }

    #[test]
    fn an_unsubmitted_shadow_is_not_listed_as_untried() {
        let mut c = core();
        merge_edit(&mut c, "a0", "src/z.rs", MAIN);
        let blocker = grant(&mut c, "a2", vec![edit("src/a.rs")], Vec::new());
        shadow(&mut c, "a1", "src/a.rs");
        submit(&mut c, "a2", blocker, "src/a.rs");
        c.begin_merge(NOW).expect("a merge is due");
        let effects = c.merge_outcome(c.merge_flight_id(blocker.0), &already_merged(MAIN2), NOW);

        let (_, untried) = landing_of(&effects);
        assert!(untried.is_empty(), "nothing to try: {untried:?}");
    }
}

//! Assumption verification: after a submission that challenged assumptions merges, the steward
//! tries each assuming agent's work on the new main, and the result is logged (invariant 8, and
//! invariant 10 for the evidence rule). A challenge is a warning at submit time; only this turns it
//! into evidence, so `Summary::assumptions_confirmed_broken` counts verified outcomes only.
//!
//! Decisions made here:
//! - Verifications are recorded when the challenging submission lands, one per challenged
//!   (claim, assumption), for claims still live. They are part of the persisted state, so they are
//!   stored before anything is sent (CLAUDE.md rule 6).
//! - The commit to try is chosen when the trial is dispatched, not when it is recorded: the
//!   assuming claim's submitted `fork_commit` at that moment; for a claim that has not submitted
//!   it is `None`, and the steward reads the head of the agent's fork and says which commit that
//!   was.
//! - Each trial carries a baseline, `before`: main as it was before the merge that challenged the
//!   assumption. The steward tries the work on `before` first, and a failure on the new main counts
//!   only if the work was clean there (`TrialReport::verdict`). Without it, work that was already
//!   failing, or that clashes with an unrelated merge, would be counted as an assumption broken by
//!   this one.
//! - One verification runs at a time, and only while no merge is due: merges have priority. A
//!   merge in backoff does not hold a verification up. Time-outs, the watchdog and the bounded
//!   infrastructure retries are the merge queue's (`merge::STEWARD_CALL_TIMEOUT_MS`,
//!   `MERGE_WATCHDOG_MS`, `MAX_INFRA_RETRIES`).
//! - A claim that ends before its verification runs (released, expired or merged) drops it, and
//!   nothing is logged: there is no longer anyone whose assumption could be broken, and an event
//!   would count a verification that never happened. A verification already running for a claim
//!   that ends has its answer discarded.
//! - Several merges that challenge the same (claim, assumption) before it runs keep only the
//!   newest main, with the baseline of the oldest of them (main before the first challenge). A verification already running is left to finish: it is evidence about the
//!   older main, and the newer one queues behind it.
//! - What the log records is the protocol `Outcome` (`TrialReport::verdict`). Anything that is
//!   not a verdict about the work (nothing to test, the commit is not on the fork, a timeout,
//!   infrastructure that kept failing) is `Inconclusive`, which no counter treats as broken.
//! - A conflict is not sent to the assuming agent as a message: no `ServerMsg` says "your
//!   assumption broke" (`AssumptionChallenged` says "re-check it" and would be read as a second
//!   challenge). The event is the record, and watchers receive it.

use serde::{Deserialize, Serialize};

use super::{Coordinator, Effect};
use crate::merge::{
    infra_backoff_ms, TrialReport, TrialVerdict, MAX_INFRA_RETRIES, MERGE_WATCHDOG_MS,
};
use crate::protocol::{AgentId, Assumption, ClaimId, CommitId, EventKind, Outcome};

/// An assumption a submission challenged at submit time, kept with the submission until it merges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Challenged {
    pub(super) claim: ClaimId,
    pub(super) assumption: Assumption,
}

/// One assumption to verify: the assuming agent's work, tried on `main`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Verification {
    id: u64,
    agent: AgentId,
    claim: ClaimId,
    assumption: Assumption,
    /// Main before the merge that challenged the assumption: the baseline of the trial.
    before: CommitId,
    /// Main after the merge that challenged the assumption.
    main: CommitId,
    /// Infrastructure failures so far.
    #[serde(default)]
    infra_failures: u32,
    /// Not dispatched before this instant: the backoff after an infrastructure failure.
    #[serde(default)]
    retry_at_ms: Option<u64>,
}

/// The verification in flight: persisted before the steward is called.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct VerifyFlight {
    id: u64,
}

/// One trial for the shell to ask the steward for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyDispatch {
    pub id: u64,
    pub agent: AgentId,
    /// The assuming claim's submitted commit now; `None`: the steward tries the head of the fork.
    pub commit: Option<CommitId>,
    pub before: CommitId,
    pub main: CommitId,
    /// 1 for the first dispatch, counting every retry.
    pub attempt: u32,
}

impl Coordinator {
    /// Queue a verification for each assumption the landed submission challenged whose assuming
    /// claim is still live, against `main`, with `before` as the baseline. A verification already
    /// queued for the same (claim, assumption) is replaced by this newer one, which keeps the
    /// older one's baseline; one in flight is left to finish.
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
            let mut baseline = before.clone();
            self.state.verifications.retain(|queued| {
                let replaced = running != Some(queued.id)
                    && queued.claim == challenge.claim
                    && queued.assumption == challenge.assumption;
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
                assumption: challenge.assumption.clone(),
                before: baseline,
                main: main.clone(),
                infra_failures: 0,
                retry_at_ms: None,
            });
        }
    }

    /// Forget the verifications of claims that no longer exist. Claim ids are never reused, so a
    /// claim that is gone has ended. Every path that removes a claim (`handle`, `expire` and a
    /// merge's outcome) ends with this call, so the queue only ever holds live claims between
    /// calls and the readers need not check.
    pub(super) fn drop_ended_verifications(&mut self) {
        let claims = &self.state.claims;
        self.state
            .verifications
            .retain(|queued| claims.contains_key(&queued.claim.0));
    }

    /// The verification to run now, marking it in flight. `None` while one is in flight, while a
    /// merge is due or in flight (merges first), or when nothing is queued or the next one is in
    /// backoff. The queue is strict: a verification in backoff is waited for.
    ///
    /// The marker is part of the state: the caller persists it before calling the steward.
    pub fn begin_verification(&mut self, now_ms: u64) -> Option<VerifyDispatch> {
        let now_ms = self.advance_clock(now_ms);
        if self.state.verification_in_flight.is_some() || self.merge_pending_at(now_ms) {
            return None;
        }
        let next = self.state.verifications.first()?;
        if next.retry_at_ms.is_some_and(|due| due > now_ms) {
            return None;
        }
        let commit = self
            .state
            .claims
            .get(&next.claim.0)
            .and_then(|assuming| assuming.work.as_ref())
            .map(|work| work.fork_commit.clone());
        let dispatch = VerifyDispatch {
            id: next.id,
            agent: next.agent.clone(),
            commit,
            before: next.before.clone(),
            main: next.main.clone(),
            attempt: next.infra_failures + 1,
        };
        self.state.verification_in_flight = Some(VerifyFlight { id: next.id });
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
        self.retry_verification_after_infrastructure(flight.id, now_ms)
    }

    /// When the shell should next run `begin_verification`, as an absolute time in milliseconds
    /// since the epoch that is never before `now_ms`. `None` when nothing is queued, or while a
    /// merge is due or in flight: the merge's own alarm comes first and reschedules.
    ///
    /// `verifying_here` is true while this instance is waiting for the steward: only a watchdog
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

    /// The earliest of the next lease expiry, the next merge dispatch and the next verification:
    /// the one alarm time, an absolute time in milliseconds since the epoch never before `now_ms`.
    pub fn next_wake_ms(
        &self,
        merging_here: bool,
        verifying_here: bool,
        now_ms: u64,
    ) -> Option<u64> {
        let merge_side = self.next_alarm_ms(merging_here, now_ms);
        let verification = self.next_verification_ms(verifying_here, now_ms);
        match (merge_side, verification) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(due), None) | (None, Some(due)) => Some(due),
            (None, None) => None,
        }
    }

    /// Apply the steward's answer for the verification `id` in flight. An answer for any other
    /// verification is stale and changes nothing. An answer for a verification whose claim ended
    /// meanwhile is discarded and logs nothing.
    pub fn verification_outcome(
        &mut self,
        id: u64,
        report: &TrialReport,
        now_ms: u64,
    ) -> Vec<Effect> {
        let now_ms = self.advance_clock(now_ms);
        if self
            .state
            .verification_in_flight
            .as_ref()
            .map(|flight| flight.id)
            != Some(id)
        {
            return Vec::new();
        }
        self.state.verification_in_flight = None;
        match report.verdict() {
            TrialVerdict::Decided(result) => self.finish_verification(id, result, now_ms),
            TrialVerdict::Infrastructure => {
                self.retry_verification_after_infrastructure(id, now_ms)
            }
        }
    }

    /// The verification ran to a result: remove it and log it. The result is the only effect.
    fn finish_verification(&mut self, id: u64, result: Outcome, now_ms: u64) -> Vec<Effect> {
        let Some(index) = self.state.verifications.iter().position(|v| v.id == id) else {
            return Vec::new();
        };
        let done = self.state.verifications.remove(index);
        let verified = EventKind::AssumptionVerified {
            claim: done.claim,
            assumption: done.assumption,
            outcome: result,
        };
        vec![self.event(now_ms, verified)]
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
            return self.finish_verification(id, Outcome::Inconclusive, now_ms);
        }
        let wait = infra_backoff_ms(queued.infra_failures);
        queued.retry_at_ms = Some(now_ms.saturating_add(wait));
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::Config;
    use crate::merge::{MergeOutcome, TrialOutcome, INFRA_BACKOFF_BASE_MS};
    use crate::protocol::{
        ClientMsg, DecisionRecord, Event, Fence, Intent, Mode, OnConflict, RequestId, RunId, Scope,
        ScopeClaim, ServerMsg, Summary,
    };

    const NOW: u64 = 1_000;
    const LEASE: u64 = 30_000;
    const MAIN: &str = "dddddddddddddddddddddddddddddddddddddddd";
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
            .filter_map(|kind| match kind {
                EventKind::AssumptionVerified { claim, outcome, .. } => Some((*claim, *outcome)),
                _ => None,
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
        c.merge_outcome(claim.0, &landed(head), NOW)
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
        c.merge_outcome(challenger, &landed(head), NOW)
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
        assert_eq!(queued.assumption, assumes("src/a.rs", "f returns Some"));
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

        let statements: Vec<_> = c
            .state
            .verifications
            .iter()
            .map(|v| v.assumption.statement.as_str())
            .collect();
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
        c.merge_outcome(assuming, &MergeOutcome::ServiceUnavailable, NOW);
        let dispatch = c.begin_verification(NOW).unwrap();
        assert_eq!(dispatch.commit, Some(CommitId(fork_sha("a1"))));

        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);
        let dispatch = c.begin_verification(NOW).unwrap();
        assert_eq!(
            dispatch.commit, None,
            "a1 has not submitted: the steward reads the fork"
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
        c.merge_outcome(dispatch.claim, &MergeOutcome::ServiceUnavailable, NOW);
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

        c.merge_outcome(next.0, &landed(MAIN2), NOW);
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
        c.merge_outcome(next.0, &MergeOutcome::ServiceUnavailable, NOW);

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
        c.verification_outcome(first.id, &clean_then(TrialOutcome::Clean {}), NOW);
        let second = c.begin_verification(NOW).expect("then the second");
        assert_ne!(first.id, second.id);
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
    fn a_released_assuming_claim_drops_its_verification_without_logging() {
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
        assert_eq!(c.begin_verification(NOW), None);
        assert_eq!(c.next_verification_ms(false, NOW), None);
    }

    #[test]
    fn an_expired_assuming_claim_drops_its_verification_without_logging() {
        let mut c = core();
        let (_, challenger) = challenged(&mut c);
        merge_challenger(&mut c, challenger, MAIN);

        let effects = c.expire(NOW + LEASE + 1);

        assert_eq!(pending(&c), 0);
        assert!(verified_events(&effects).is_empty());
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
        let effects = c.merge_outcome(assuming, &landed(MAIN2), NOW);

        assert_eq!(pending(&c), 0);
        assert!(verified_events(&effects).is_empty());
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
        c.merge_outcome(second.0, &after_first, NOW);

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
    fn a_verification_in_flight_finishes_and_the_newer_one_queues_behind_it() {
        let mut c = core();
        let (assuming, first) = challenged(&mut c);
        merge_challenger(&mut c, first, MAIN);
        let running = c.begin_verification(NOW).unwrap();

        let second = grant(&mut c, "a3", vec![edit("src/a.rs")], Vec::new());
        submit(&mut c, "a3", second, "src/a.rs");
        merge_challenger(&mut c, second.0, MAIN2);
        assert_eq!(pending(&c), 2);

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
}

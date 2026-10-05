//! Races (invariant 7): N agents attempt one task in separate forks and the coordinator picks the
//! one to ship.
//!
//! Decisions made here:
//! - Only a configured reviewer may open a race or pick its winner, the same authority as
//!   `Review`. A reviewer who is an entrant may not pick, as a reviewer may not review their own
//!   submission.
//! - A race holds its scopes with locks of its own, placed at `OpenRace` under an id from the claim
//!   counter. They block everyone, the opener included: the opener is not an entrant, and an
//!   entrant's claim places no lock, so entrants never block each other. Opening a race checks the
//!   scopes against every claim, the opener's own too, for the same reason. Scopes must be
//!   canonical (`claim_fault`).
//! - `max_entrants` is 1 to 8, the deadline is in the future and at most two hours away, and there
//!   are one to five criteria. All are bounds on untrusted input, not policy.
//! - `JoinRace` never waits, so an agent that holds claims may join: invariant 2 forbids waiting
//!   while holding, and a grant that cannot wait cannot deadlock. An agent with a queued request
//!   may not join, as it may not make any other claim, and an entry counts as a hold, so an
//!   entrant may not `Wait`.
//! - An entry's submission is checked like any other (fence, coverage) and numbered, but it is not
//!   dispatched, challenges nothing and is not reviewed. A loser never merges, so flagging it would
//!   ask a human to read discarded work. The winner passes the assumption challenge and the review
//!   gate (invariant 12) when it is promoted, and merges at its original submission order.
//! - The race is judged at its deadline, or earlier once it is full (as many entrants as
//!   `max_entrants`) and every entrant has submitted. This refines invariant 7's "when every
//!   entrant has submitted": with room left, the first joiner to submit would end the race and
//!   shut out everyone else. Joining stays open until the race is full or the deadline passes.
//!   When judging starts, an entrant that has not submitted is cut off and released. Tests are tried by the verification queue (see
//!   `verifying`), one entry at a time with merges first, all on the head when judging began. A
//!   trial that never runs to a result leaves `tests_passed` as `None`. `risk_bp` and `diff_lines`
//!   are never measured, so `LowestRisk` and `SmallestDiff` are refused at `OpenRace`: ranking by
//!   them would present the earliest joiner as the lowest risk (CLAUDE.md rule 7). A winner whose
//!   trial passed counts as having test evidence for the review gate.
//! - With `HumanPick`, ranked entries are sent as a recommendation (`winner: None`) and the race
//!   waits for `PickWinner` for at most `MAX_RACE_MS` past its deadline. Then it is decided with
//!   no winner and every entry is rejected; nobody is picked for the reviewers. `RaceDecided` is
//!   logged once, when the outcome is known, so `races_decided` counts a race once. A race with
//!   no eligible entry, including one nobody entered, is decided at once with `winner: None`.
//! - `RaceResult` goes to every entrant and to the opener, who may not be one but is who decides a
//!   `HumanPick`. Losers get `SubmitRejected` with a fixed reason and lose their claim; their forks
//!   are untouched. The winner becomes a real claim holding the race's scopes.

use serde::{Deserialize, Serialize};

use super::{
    claim_fault, error, place_locks, place_scope_locks, remove_scope_locks, take_next,
    without_duplicates, ActiveClaim, ClaimKind, ClaimRequest, Coordinator, Effect, LockOwner,
    LockTable, Submission, SubmitRequest,
};
use crate::protocol::{
    rank_entries, review_reasons, AgentId, ClaimId, CommitId, Criterion, DecisionRecord, ErrorCode,
    EventKind, Fence, Intent, RaceEntry, RaceId, ReleaseReason, RequestId, ScopeClaim, ServerMsg,
};

/// The most agents one race may hold.
const MAX_ENTRANTS: u32 = 8;

/// The longest a race may stay open, from the moment it is opened.
const MAX_RACE_MS: u64 = 2 * 60 * 60 * 1000;

/// There are five criteria; more than that repeats one.
const MAX_CRITERIA: usize = 5;

/// What every loser is told. Fixed: nothing about the winning work is quoted.
const LOST_RACE: &str = "lost the race";

/// What every entry is told when a `HumanPick` race ran out of time. Fixed, like `LOST_RACE`.
const NOT_PICKED: &str = "the race was not picked in time";

/// An `OpenRace` message minus the sender. `claim` carries the race's task and scopes, so they are
/// checked as a claim's are.
pub(super) struct OpenRequest {
    pub(super) claim: ClaimRequest,
    pub(super) max_entrants: u32,
    pub(super) deadline_ms: u64,
    pub(super) criteria: Vec<Criterion>,
}

/// One race as the coordinator holds it until it is decided.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Race {
    opener: AgentId,
    intent: Intent,
    scopes: Vec<ScopeClaim>,
    /// The id of the race's locks, from the claim counter.
    lock_id: ClaimId,
    max_entrants: u32,
    deadline_ms: u64,
    criteria: Vec<Criterion>,
    /// In join order.
    entrants: Vec<Entrant>,
    phase: Phase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Phase {
    /// Taking entrants and submissions.
    Open,
    /// Every entry's tests are being tried.
    Judging,
    /// `HumanPick`: ranked, waiting for `PickWinner`.
    AwaitingPick {
        ranking: Vec<ClaimId>,
        entries: Vec<RaceEntry>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entrant {
    claim: ClaimId,
    agent: AgentId,
    /// Set when the entrant submits.
    entered: Option<Entered>,
}

/// What an entrant submitted, kept until the race is decided.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entered {
    fork_commit: CommitId,
    touched: Vec<ScopeClaim>,
    submit_req: RequestId,
    has_evidence: bool,
    submitted_at_ms: u64,
    tests_passed: Option<bool>,
}

/// How a race ended.
struct Verdict {
    winner: Option<ClaimId>,
    ranking: Vec<ClaimId>,
    entries: Vec<RaceEntry>,
    /// Why each entry other than the winner is rejected.
    rejection: &'static str,
}

impl Race {
    /// When the race needs the coordinator next, if it does: the deadline of an open race, or the
    /// last moment a `HumanPick` may be made.
    fn due_ms(&self) -> Option<u64> {
        match self.phase {
            Phase::Open => Some(self.deadline_ms),
            Phase::AwaitingPick { .. } => Some(self.deadline_ms.saturating_add(MAX_RACE_MS)),
            Phase::Judging => None,
        }
    }

    fn is_joinable(&self) -> bool {
        match self.phase {
            Phase::Open => {
                u32::try_from(self.entrants.len()).is_ok_and(|held| held < self.max_entrants)
            }
            Phase::Judging | Phase::AwaitingPick { .. } => false,
        }
    }

    /// Whether the race can be judged before its deadline: it is full and every entrant has
    /// submitted. A race with room is still taking entrants, so the first joiner to submit does
    /// not end it.
    fn ready_to_judge(&self) -> bool {
        let full = u32::try_from(self.entrants.len()).is_ok_and(|held| held == self.max_entrants);
        full && self.entrants.iter().all(|e| e.entered.is_some())
    }

    /// The submitted entries, in join order.
    fn entries(&self) -> Vec<RaceEntry> {
        let mut entries = Vec::new();
        for entrant in &self.entrants {
            let Some(entered) = &entrant.entered else {
                continue;
            };
            entries.push(RaceEntry {
                claim: entrant.claim,
                agent: entrant.agent.clone(),
                fork_commit: entered.fork_commit.clone(),
                submitted_at_ms: entered.submitted_at_ms,
                tests_passed: entered.tests_passed,
                risk_bp: None,
                diff_lines: None,
            });
        }
        entries
    }

    /// Everyone told how the race went: each entrant, the opener and `picker`, once each.
    fn recipients(&self, picker: Option<&AgentId>) -> Vec<AgentId> {
        let mut agents: Vec<AgentId> = Vec::new();
        for agent in self
            .entrants
            .iter()
            .map(|e| &e.agent)
            .chain(std::iter::once(&self.opener))
            .chain(picker)
        {
            if !agents.contains(agent) {
                agents.push(agent.clone());
            }
        }
        agents
    }
}

/// Place the locks of a race that is not decided: on load, and when it opens.
pub(super) fn place_race_locks(locks: &mut LockTable, id: RaceId, race: &Race) {
    let owner = LockOwner {
        id: race.lock_id,
        agent: &race.opener,
        intent: &race.intent,
        race: Some(id),
    };
    place_scope_locks(locks, &owner, &race.scopes);
}

fn unknown_race(req: Option<RequestId>) -> Effect {
    error(req, ErrorCode::UnknownRace, "no such race")
}

fn race_closed(req: Option<RequestId>) -> Effect {
    error(req, ErrorCode::RaceClosed, "the race is closed")
}

impl Coordinator {
    /// Whether `race` is open and has room: what `Conflict::race` promises.
    pub(super) fn race_is_joinable(&self, race: RaceId) -> bool {
        self.state.races.get(&race.0).is_some_and(Race::is_joinable)
    }

    /// The earliest deadline of a race still taking entrants or waiting for a pick.
    pub(super) fn next_race_deadline_ms(&self) -> Option<u64> {
        self.state.races.values().filter_map(Race::due_ms).min()
    }

    /// Whether `race` was ever opened. Ids start at 1, rise by one and are never reused.
    fn race_was_opened(&self, race: RaceId) -> bool {
        race.0 >= 1 && race.0 < self.state.next_race
    }

    /// The race, or the error to send: `UnknownRace` for an id never issued, `RaceClosed` for a
    /// race that is decided.
    fn race_for(&self, race: RaceId, req: Option<RequestId>) -> Result<&Race, Box<Effect>> {
        match self.state.races.get(&race.0) {
            Some(held) => Ok(held),
            None if self.race_was_opened(race) => Err(Box::new(race_closed(req))),
            None => Err(Box::new(unknown_race(req))),
        }
    }

    /// Open a race (invariant 7). Atomic: its scopes are held against everyone, or the race is
    /// denied with the conflicts.
    pub(super) fn open_race(
        &mut self,
        agent: &AgentId,
        request: OpenRequest,
        now_ms: u64,
    ) -> Vec<Effect> {
        if let Some(refusal) = self.refuse_open(agent, &request, now_ms) {
            return vec![refusal];
        }
        let OpenRequest {
            claim,
            max_entrants,
            deadline_ms,
            criteria,
        } = request;
        let claim = ClaimRequest {
            scopes: without_duplicates(claim.scopes),
            ..claim
        };
        let nobody = AgentId(String::new());
        let conflicts = self.find_conflicts(&nobody, &claim.scopes);
        if !conflicts.is_empty() {
            return self.deny(agent, claim, conflicts, now_ms);
        }
        let id = RaceId(take_next(&mut self.state.next_race));
        let lock_id = ClaimId(take_next(&mut self.state.next_claim));
        let req = claim.req;
        let race = Race {
            opener: agent.clone(),
            intent: claim.intent,
            scopes: claim.scopes,
            lock_id,
            max_entrants,
            deadline_ms,
            criteria,
            entrants: Vec::new(),
            phase: Phase::Open,
        };
        place_race_locks(&mut self.locks, id, &race);
        let opened = EventKind::RaceOpened {
            race: id,
            scopes: race.scopes.clone(),
            criteria: race.criteria.clone(),
        };
        let logged = self.event(now_ms, opened);
        self.state.races.insert(id.0, race);
        let reply = ServerMsg::RaceOpened {
            req,
            race: id,
            deadline_ms,
        };
        vec![logged, Effect::Reply(reply)]
    }

    /// The error to send for an `OpenRace` that must change nothing, or `None` if it may proceed.
    fn refuse_open(&self, agent: &AgentId, request: &OpenRequest, now_ms: u64) -> Option<Effect> {
        let req = Some(request.claim.req);
        let malformed = |message: String| Some(error(req, ErrorCode::Malformed, message));
        if !self.state.reviewers.contains(agent) {
            let message = "only a configured reviewer may open a race";
            return Some(error(req, ErrorCode::NotOwner, message));
        }
        if request.claim.scopes.is_empty() {
            return malformed("race names no scopes".to_string());
        }
        if let Some(message) = claim_fault(&request.claim) {
            return malformed(message);
        }
        if !(1..=MAX_ENTRANTS).contains(&request.max_entrants) {
            return malformed(format!("max_entrants must be 1 to {MAX_ENTRANTS}"));
        }
        if request.deadline_ms <= now_ms {
            return malformed("the race deadline must be in the future".to_string());
        }
        if request.deadline_ms - now_ms > MAX_RACE_MS {
            return malformed(format!(
                "the race deadline must be within {} minutes",
                MAX_RACE_MS / 60_000
            ));
        }
        if request.criteria.is_empty() || request.criteria.len() > MAX_CRITERIA {
            return malformed(format!("a race needs 1 to {MAX_CRITERIA} criteria"));
        }
        for criterion in &request.criteria {
            match criterion {
                Criterion::LowestRisk | Criterion::SmallestDiff => {
                    return malformed(format!("{criterion:?} is not measured yet"));
                }
                Criterion::TestsPass | Criterion::FirstSubmitted | Criterion::HumanPick => {}
            }
        }
        None
    }

    /// Enter a race: a claim on exactly the race's scopes that places no lock of its own, because
    /// the race already holds them. Never waits and checks no conflicts, for the same reason.
    pub(super) fn join_race(
        &mut self,
        agent: &AgentId,
        req: RequestId,
        race_id: RaceId,
        now_ms: u64,
    ) -> Vec<Effect> {
        let race = match self.race_for(race_id, Some(req)) {
            Ok(race) => race,
            Err(refusal) => return vec![*refusal],
        };
        let refusal = match race.phase {
            Phase::Judging | Phase::AwaitingPick { .. } => Some(race_closed(Some(req))),
            Phase::Open if race.deadline_ms <= now_ms => Some(race_closed(Some(req))),
            Phase::Open if race.entrants.iter().any(|e| e.agent == *agent) => {
                let message = "agent is already in this race";
                Some(error(Some(req), ErrorCode::Malformed, message))
            }
            Phase::Open if !race.is_joinable() => {
                Some(error(Some(req), ErrorCode::RaceFull, "the race is full"))
            }
            Phase::Open if self.has_queued_request(agent) => {
                let message = "agent has a queued request and may not join a race";
                Some(error(Some(req), ErrorCode::WaitWhileHolding, message))
            }
            Phase::Open => None,
        };
        if let Some(refusal) = refusal {
            return vec![refusal];
        }
        let (scopes, intent) = (race.scopes.clone(), race.intent.clone());
        let at_risk = self.assumptions_at_risk(agent, &scopes);
        let claim = ClaimId(take_next(&mut self.state.next_claim));
        let fence = Fence(take_next(&mut self.state.next_fence));
        let expires_at_ms = now_ms.saturating_add(self.state.config.lease_ms);
        let granted = self.event(
            now_ms,
            EventKind::ClaimGranted {
                agent: agent.clone(),
                claim,
                fence,
                scopes: scopes.clone(),
                intent: intent.clone(),
                race: Some(race_id),
                at_risk: at_risk.clone(),
            },
        );
        let entry = ActiveClaim {
            agent: agent.clone(),
            fence,
            intent,
            scopes,
            expires_at_ms,
            kind: ClaimKind::Entry(race_id),
            submitted: None,
            work: None,
        };
        self.state.claims.insert(claim.0, entry);
        if let Some(held) = self.state.races.get_mut(&race_id.0) {
            held.entrants.push(Entrant {
                claim,
                agent: agent.clone(),
                entered: None,
            });
        }
        let reply = ServerMsg::Granted {
            req,
            claim,
            fence,
            expires_at_ms,
            race: Some(race_id),
            at_risk,
        };
        vec![granted, Effect::Reply(reply)]
    }

    /// An entry's `Submit`, already past the fence and coverage checks: number it, remember what it
    /// holds, and judge the race if it was the last entrant to submit. The entry is not queued for
    /// merge, so its reply carries queue position 0, as a shadow claim's does.
    pub(super) fn submit_entry(
        &mut self,
        race_id: RaceId,
        request: SubmitRequest,
        now_ms: u64,
    ) -> Vec<Effect> {
        let SubmitRequest {
            req,
            claim,
            fork_commit,
            touched,
            decisions,
            ..
        } = request;
        let phase_open = self
            .state
            .races
            .get(&race_id.0)
            .is_some_and(|race| match race.phase {
                Phase::Open => true,
                Phase::Judging | Phase::AwaitingPick { .. } => false,
            });
        if !phase_open {
            return vec![race_closed(Some(req))];
        }
        let ordinal = take_next(&mut self.state.next_submission);
        if let Some(held) = self.state.claims.get_mut(&claim.0) {
            held.submitted = Some(ordinal);
        }
        let entered = Entered {
            fork_commit: fork_commit.clone(),
            touched: touched.clone(),
            submit_req: req,
            has_evidence: has_evidence(&decisions),
            submitted_at_ms: now_ms,
            tests_passed: None,
        };
        let mut ready = false;
        if let Some(race) = self.state.races.get_mut(&race_id.0) {
            for entrant in &mut race.entrants {
                if entrant.claim == claim {
                    entrant.entered = Some(entered.clone());
                }
            }
            ready = race.ready_to_judge();
        }
        let submitted = EventKind::Submitted {
            claim,
            fork_commit,
            touched,
            decisions,
        };
        let mut effects = vec![self.event(now_ms, submitted)];
        let accepted = ServerMsg::Accepted {
            req,
            claim,
            queue_position: 0,
        };
        effects.push(Effect::Reply(accepted));
        if ready {
            effects.extend(self.begin_judging(race_id, now_ms));
        }
        effects
    }

    /// An unsubmitted entry's claim ended (released or lease expired). The race goes on without
    /// the entrant, and is judged if everyone left has submitted.
    pub(super) fn entrant_left(
        &mut self,
        race_id: RaceId,
        claim: ClaimId,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Some(race) = self.state.races.get_mut(&race_id.0) else {
            return Vec::new();
        };
        match race.phase {
            Phase::Open => {}
            Phase::Judging | Phase::AwaitingPick { .. } => return Vec::new(),
        }
        race.entrants.retain(|entrant| entrant.claim != claim);
        if race.ready_to_judge() {
            return self.begin_judging(race_id, now_ms);
        }
        Vec::new()
    }

    /// Judge every open race whose deadline has come, and end every `HumanPick` race that was not
    /// picked in time.
    pub(super) fn judge_races_past_deadline(&mut self, now_ms: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        let ids: Vec<u64> = self.state.races.keys().copied().collect();
        for id in ids {
            let race_id = RaceId(id);
            let Some(race) = self.state.races.get(&id) else {
                continue;
            };
            if race.due_ms().is_none_or(|due| due > now_ms) {
                continue;
            }
            match &race.phase {
                Phase::Open => effects.extend(self.begin_judging(race_id, now_ms)),
                Phase::AwaitingPick { ranking, entries } => {
                    let verdict = Verdict {
                        winner: None,
                        ranking: ranking.clone(),
                        entries: entries.clone(),
                        rejection: NOT_PICKED,
                    };
                    effects.extend(self.conclude(race_id, verdict, None, now_ms));
                }
                Phase::Judging => {}
            }
        }
        effects
    }

    /// Close the race to entries: release entrants that never submitted, and queue the tests of
    /// each entry that did, all on the head as it is now. With nothing to try, the race is judged
    /// at once.
    fn begin_judging(&mut self, race_id: RaceId, now_ms: u64) -> Vec<Effect> {
        let Some(race) = self.state.races.get_mut(&race_id.0) else {
            return Vec::new();
        };
        race.phase = Phase::Judging;
        let mut cut_off = Vec::new();
        let mut submitted = Vec::new();
        for entrant in &race.entrants {
            match entrant.entered {
                Some(_) => submitted.push((entrant.claim, entrant.agent.clone())),
                None => cut_off.push(entrant.claim),
            }
        }
        let mut effects = Vec::new();
        for claim in cut_off {
            self.state.claims.remove(&claim.0);
            let reason = ReleaseReason::LostRace;
            effects.push(self.event(now_ms, EventKind::ClaimReleased { claim, reason }));
        }
        if let Some(head) = self.state.head.clone() {
            for (claim, agent) in submitted {
                self.record_race_trial(race_id, &agent, claim, &head);
            }
        }
        if !self.race_has_trials(race_id) {
            effects.extend(self.finish_judging(race_id, now_ms));
        }
        effects
    }

    /// The commit a race entry submitted, for the trial of its tests.
    pub(super) fn entry_commit(&self, race: RaceId, claim: ClaimId) -> Option<CommitId> {
        let held = self.state.races.get(&race.0)?;
        let entrant = held
            .entrants
            .iter()
            .find(|entrant| entrant.claim == claim)?;
        let entered = entrant.entered.as_ref()?;
        Some(entered.fork_commit.clone())
    }

    /// An entry's tests ran to a result, or gave up (`None`). When it was the last one, the race
    /// is judged.
    pub(super) fn record_trial(
        &mut self,
        race_id: RaceId,
        claim: ClaimId,
        passed: Option<bool>,
        now_ms: u64,
    ) -> Vec<Effect> {
        if let Some(race) = self.state.races.get_mut(&race_id.0) {
            for entrant in &mut race.entrants {
                if let (true, Some(entered)) = (entrant.claim == claim, entrant.entered.as_mut()) {
                    entered.tests_passed = passed;
                }
            }
        }
        if self.race_has_trials(race_id) {
            return Vec::new();
        }
        self.finish_judging(race_id, now_ms)
    }

    /// Every entry is measured: rank them. With `HumanPick` and anything ranked, the race waits
    /// for `PickWinner`; otherwise the best entry wins, or nobody does.
    fn finish_judging(&mut self, race_id: RaceId, now_ms: u64) -> Vec<Effect> {
        let Some(race) = self.state.races.get_mut(&race_id.0) else {
            return Vec::new();
        };
        let entries = race.entries();
        let ranking = rank_entries(&race.criteria, &entries);
        let human = race.criteria.contains(&Criterion::HumanPick);
        if !(human && !ranking.is_empty()) {
            let winner = ranking.first().copied();
            let verdict = Verdict {
                winner,
                ranking,
                entries,
                rejection: LOST_RACE,
            };
            return self.conclude(race_id, verdict, None, now_ms);
        }
        let result = ServerMsg::RaceResult {
            race: race_id,
            winner: None,
            ranking: ranking.clone(),
            entries: entries.clone(),
        };
        let recipients = race.recipients(None);
        race.phase = Phase::AwaitingPick { ranking, entries };
        recipients
            .into_iter()
            .map(|agent| Effect::Notify {
                agent,
                msg: result.clone(),
            })
            .collect()
    }

    /// A reviewer's choice for a `HumanPick` race.
    pub(super) fn pick_winner(
        &mut self,
        agent: &AgentId,
        req: RequestId,
        race_id: RaceId,
        claim: ClaimId,
        now_ms: u64,
    ) -> Vec<Effect> {
        if !self.state.reviewers.contains(agent) {
            let message = "only a configured reviewer may pick a winner";
            return vec![error(Some(req), ErrorCode::NotOwner, message)];
        }
        let race = match self.race_for(race_id, Some(req)) {
            Ok(race) => race,
            Err(refusal) => return vec![*refusal],
        };
        let Phase::AwaitingPick { ranking, entries } = &race.phase else {
            let message = "the race is not waiting for a pick";
            return vec![error(Some(req), ErrorCode::Malformed, message)];
        };
        if race.entrants.iter().any(|entrant| entrant.agent == *agent) {
            let message = "an entrant may not pick the winner";
            return vec![error(Some(req), ErrorCode::NotOwner, message)];
        }
        if !ranking.contains(&claim) {
            let message = format!("claim {} is not an eligible entry in this race", claim.0);
            return vec![error(Some(req), ErrorCode::NotAnEntrant, message)];
        }
        let verdict = Verdict {
            winner: Some(claim),
            ranking: ranking.clone(),
            entries: entries.clone(),
            rejection: LOST_RACE,
        };
        self.conclude(race_id, verdict, Some(agent.clone()), now_ms)
    }

    /// The race is decided: log it, tell everyone, end the losers' claims, promote the winner and
    /// free the scopes. `picker` is the reviewer who chose, if one did.
    fn conclude(
        &mut self,
        race_id: RaceId,
        verdict: Verdict,
        picker: Option<AgentId>,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Some(race) = self.state.races.remove(&race_id.0) else {
            return Vec::new();
        };
        remove_scope_locks(&mut self.locks, race.lock_id, &race.scopes);
        let decided = EventKind::RaceDecided {
            race: race_id,
            winner: verdict.winner,
            ranking: verdict.ranking.clone(),
        };
        let mut effects = vec![self.event(now_ms, decided)];
        let result = ServerMsg::RaceResult {
            race: race_id,
            winner: verdict.winner,
            ranking: verdict.ranking,
            entries: verdict.entries,
        };
        for agent in race.recipients(picker.as_ref()) {
            effects.push(Effect::Notify {
                agent,
                msg: result.clone(),
            });
        }
        let mut winning = None;
        for entrant in race.entrants {
            let Some(entered) = entrant.entered else {
                continue;
            };
            if Some(entrant.claim) == verdict.winner {
                winning = Some((entrant.claim, entrant.agent, entered));
            } else {
                let rejection = verdict.rejection;
                effects.extend(self.reject_loser(entrant.claim, entrant.agent, rejection, now_ms));
            }
        }
        if let Some((claim, agent, entered)) = winning {
            effects.extend(self.promote_winner(claim, &agent, entered, now_ms));
        }
        effects.extend(self.grant_unblocked_waiters(now_ms));
        effects
    }

    /// A loser's claim ends. Its fork is not touched.
    fn reject_loser(
        &mut self,
        claim: ClaimId,
        agent: AgentId,
        reason: &str,
        now_ms: u64,
    ) -> Vec<Effect> {
        self.state.claims.remove(&claim.0);
        let reason = reason.to_string();
        let rejected = EventKind::SubmitRejected {
            claim,
            reason: reason.clone(),
        };
        let released = EventKind::ClaimReleased {
            claim,
            reason: ReleaseReason::LostRace,
        };
        vec![
            self.event(now_ms, rejected),
            self.event(now_ms, released),
            Effect::Notify {
                agent,
                msg: ServerMsg::SubmitRejected { claim, reason },
            },
        ]
    }

    /// The winner's entry becomes a real claim that holds the race's scopes with locks of its own
    /// and waits in the merge queue at its original submission order. Its work meets the
    /// assumption challenge and the review gate now, as any submission does at submit time.
    fn promote_winner(
        &mut self,
        claim: ClaimId,
        agent: &AgentId,
        entered: Entered,
        now_ms: u64,
    ) -> Vec<Effect> {
        let Entered {
            fork_commit,
            touched,
            submit_req,
            has_evidence,
            tests_passed,
            ..
        } = entered;
        let (mut effects, challenged) =
            self.challenge_assumptions(agent, &fork_commit, &touched, now_ms);
        let threatened = u32::try_from(challenged.len()).unwrap_or(u32::MAX);
        let evidence = has_evidence || tests_passed == Some(true);
        let reasons = review_reasons(&touched, threatened, evidence, &[]);
        let submission = Submission::new(
            fork_commit,
            touched,
            !reasons.is_empty(),
            submit_req,
            challenged,
        );
        let Some(held) = self.state.claims.get_mut(&claim.0) else {
            return effects;
        };
        held.kind = ClaimKind::Real;
        held.work = Some(submission);
        let ordinal = held.submitted;
        let placed = held.clone();
        place_locks(&mut self.locks, claim, &placed);
        if reasons.is_empty() {
            let queue_position = ordinal.map_or(0, |ordinal| self.queue_position(ordinal));
            let accepted = ServerMsg::Accepted {
                req: submit_req,
                claim,
                queue_position,
            };
            effects.push(Effect::Notify {
                agent: agent.clone(),
                msg: accepted,
            });
            return effects;
        }
        let requested = EventKind::ReviewRequested {
            claim,
            reasons: reasons.clone(),
        };
        effects.push(self.event(now_ms, requested));
        effects.push(Effect::Notify {
            agent: agent.clone(),
            msg: ServerMsg::ReviewRequired { claim, reasons },
        });
        effects
    }
}

fn has_evidence(decisions: &DecisionRecord) -> bool {
    !decisions.evidence.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::Config;
    use crate::merge::{MergeOutcome, TrialOutcome, TrialReport};
    use crate::protocol::{
        Assumption, ClientMsg, Event, Fence, Mode, OnConflict, RunId, Scope, Summary,
        PROTOCOL_VERSION,
    };
    use proptest::prelude::*;

    const NOW: u64 = 1_000;
    const LEASE: u64 = 30_000;
    /// Inside the first lease, so a lease never expires before a deadline unless a test wants it.
    const DEADLINE: u64 = NOW + 20_000;
    const MAIN: &str = "dddddddddddddddddddddddddddddddddddddddd";

    fn agent(name: &str) -> AgentId {
        AgentId(name.into())
    }

    fn edit(path: &str) -> ScopeClaim {
        ScopeClaim {
            scope: Scope::File { path: path.into() },
            mode: Mode::EditBody,
        }
    }

    fn intent(summary: &str) -> Intent {
        Intent {
            summary: summary.into(),
            task_ref: None,
            assumptions: Vec::new(),
        }
    }

    fn bare_core() -> Coordinator {
        let mut c = Coordinator::new(Config {
            run: RunId("test".into()),
            lease_ms: LEASE,
            shadow_enabled: true,
        })
        .unwrap();
        c.set_reviewers(vec![agent("felix"), agent("boss")]);
        c
    }

    /// A coordinator that knows main's head and has felix and boss as reviewers.
    fn core() -> Coordinator {
        let mut c = bare_core();
        let hello = ClientMsg::Hello {
            agent: agent("felix"),
            base: CommitId(MAIN.into()),
            protocol: PROTOCOL_VERSION,
        };
        c.handle(&agent("felix"), hello, NOW);
        c
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

    fn notices<'a>(effects: &'a [Effect], who: &str) -> Vec<&'a ServerMsg> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Notify { agent: to, msg } if *to == agent(who) => Some(msg),
                Effect::Notify { .. } | Effect::Reply(_) | Effect::Log(_) => None,
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

    fn events(effects: &[Effect]) -> Vec<Event> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Log(event) => Some(event.clone()),
                Effect::Reply(_) | Effect::Notify { .. } => None,
            })
            .collect()
    }

    fn state(c: &Coordinator) -> String {
        serde_json::to_string(c).unwrap()
    }

    fn error_code(effects: &[Effect]) -> ErrorCode {
        let [ServerMsg::Error { code, .. }] = replies(effects)[..] else {
            panic!("expected one Error, got {effects:?}");
        };
        *code
    }

    fn open_msg(
        scopes: Vec<ScopeClaim>,
        max_entrants: u32,
        deadline_ms: u64,
        criteria: Vec<Criterion>,
    ) -> ClientMsg {
        ClientMsg::OpenRace {
            req: RequestId(5),
            intent: intent("the task"),
            scopes,
            max_entrants,
            deadline_ms,
            criteria,
        }
    }

    fn open_effects(c: &mut Coordinator, criteria: Vec<Criterion>) -> Vec<Effect> {
        let msg = open_msg(vec![edit("src/a.rs")], 4, DEADLINE, criteria);
        c.handle(&agent("felix"), msg, NOW)
    }

    /// A race with room for `max` entrants.
    fn open_for(c: &mut Coordinator, max: u32, criteria: Vec<Criterion>) -> RaceId {
        let msg = open_msg(vec![edit("src/a.rs")], max, DEADLINE, criteria);
        let effects = c.handle(&agent("felix"), msg, NOW);
        let [ServerMsg::RaceOpened { race, .. }] = replies(&effects)[..] else {
            panic!("expected RaceOpened, got {effects:?}");
        };
        *race
    }

    fn join_effects(c: &mut Coordinator, who: &str, race: RaceId, now: u64) -> Vec<Effect> {
        let msg = ClientMsg::JoinRace {
            req: RequestId(2),
            race,
        };
        c.handle(&agent(who), msg, now)
    }

    fn join(c: &mut Coordinator, who: &str, race: RaceId) -> (ClaimId, Fence) {
        let effects = join_effects(c, who, race, NOW);
        let [ServerMsg::Granted {
            claim,
            fence,
            race: granted,
            ..
        }] = replies(&effects)[..]
        else {
            panic!("expected Granted, got {effects:?}");
        };
        assert_eq!(*granted, Some(race));
        (*claim, *fence)
    }

    /// A distinct, valid commit id per agent name.
    fn fork_sha(who: &str) -> String {
        let seed = who.bytes().fold(0u8, |sum, b| sum.wrapping_add(b));
        format!("{seed:02x}").repeat(20)
    }

    fn submit_effects(
        c: &mut Coordinator,
        who: &str,
        entry: (ClaimId, Fence),
        evidence: bool,
        now: u64,
    ) -> Vec<Effect> {
        let decisions = DecisionRecord {
            evidence: if evidence {
                vec!["tests passed".into()]
            } else {
                Vec::new()
            },
            ..DecisionRecord::default()
        };
        let msg = ClientMsg::Submit {
            req: RequestId(9),
            claim: entry.0,
            fence: entry.1,
            fork_commit: CommitId(fork_sha(who)),
            touched: vec![edit("src/a.rs")],
            decisions,
        };
        c.handle(&agent(who), msg, now)
    }

    fn submit_at(c: &mut Coordinator, who: &str, entry: (ClaimId, Fence), now: u64) {
        let effects = submit_effects(c, who, entry, true, now);
        let [ServerMsg::Accepted { queue_position, .. }] = replies(&effects)[..] else {
            panic!("expected Accepted, got {effects:?}");
        };
        assert_eq!(*queue_position, 0, "an entry is not queued for merge");
    }

    fn passed() -> TrialReport {
        TrialReport {
            before: Some(TrialOutcome::Clean {}),
            after: Some(TrialOutcome::Clean {}),
        }
    }

    fn failed() -> TrialReport {
        TrialReport {
            before: Some(TrialOutcome::TestsFailed {}),
            after: None,
        }
    }

    /// Run every trial the coordinator offers, answering each from `results` by agent.
    fn run_trials(c: &mut Coordinator, now: u64, results: &[(&str, TrialReport)]) -> Vec<Effect> {
        let mut effects = Vec::new();
        while let Some(dispatch) = c.begin_verification(now) {
            let report = results
                .iter()
                .find(|(who, _)| agent(who) == dispatch.agent)
                .map(|(_, report)| report.clone())
                .expect("a trial for an agent the test did not expect");
            effects.extend(c.verification_outcome(dispatch.id, &report, now));
        }
        effects
    }

    /// The result `who` was sent: (winner, ranking, entries).
    fn result_for(
        effects: &[Effect],
        who: &str,
    ) -> Option<(Option<ClaimId>, Vec<ClaimId>, Vec<RaceEntry>)> {
        for msg in notices(effects, who) {
            if let ServerMsg::RaceResult {
                winner,
                ranking,
                entries,
                ..
            } = msg
            {
                return Some((*winner, ranking.clone(), entries.clone()));
            }
        }
        None
    }

    fn claim_as(
        c: &mut Coordinator,
        who: &str,
        path: &str,
        on_conflict: OnConflict,
    ) -> Vec<Effect> {
        let msg = ClientMsg::Claim {
            req: RequestId(1),
            intent: intent("other"),
            scopes: vec![edit(path)],
            on_conflict,
        };
        c.handle(&agent(who), msg, NOW)
    }

    fn denial_race(effects: &[Effect]) -> Option<RaceId> {
        let [ServerMsg::Denied { conflicts, .. }] = replies(effects)[..] else {
            panic!("expected Denied, got {effects:?}");
        };
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        conflicts[0].race
    }

    fn is_granted(effects: &[Effect]) -> bool {
        matches!(replies(effects)[..], [ServerMsg::Granted { .. }])
    }

    fn all_events(batches: &[&[Effect]]) -> Vec<Event> {
        let mut all = Vec::new();
        for batch in batches {
            all.extend(events(batch));
        }
        all
    }

    // ---- opening and joining ----

    #[test]
    fn an_open_race_holds_its_scopes_and_names_itself_in_the_denial() {
        let mut c = core();
        let effects = open_effects(&mut c, vec![Criterion::FirstSubmitted]);

        let [ServerMsg::RaceOpened {
            req,
            race,
            deadline_ms,
        }] = replies(&effects)[..]
        else {
            panic!("expected RaceOpened, got {effects:?}");
        };
        assert_eq!(
            (*req, *race, *deadline_ms),
            (RequestId(5), RaceId(1), DEADLINE)
        );
        let [EventKind::RaceOpened {
            race,
            scopes,
            criteria,
        }] = logged(&effects)[..]
        else {
            panic!("expected one RaceOpened event, got {effects:?}");
        };
        assert_eq!(*race, RaceId(1));
        assert_eq!(*scopes, vec![edit("src/a.rs")]);
        assert_eq!(*criteria, vec![Criterion::FirstSubmitted]);

        let denied = claim_as(&mut c, "outsider", "src/a.rs", OnConflict::Fail);
        assert_eq!(denial_race(&denied), Some(RaceId(1)));
        let ServerMsg::Denied { conflicts, .. } = replies(&denied)[0] else {
            panic!("expected Denied");
        };
        assert_eq!(conflicts[0].held_by, agent("felix"));
        assert_eq!(conflicts[0].their_intent.summary, "the task");
        let ancestor = ClientMsg::Claim {
            req: RequestId(1),
            intent: intent("dir"),
            scopes: vec![ScopeClaim {
                scope: Scope::Dir { path: "src".into() },
                mode: Mode::EditBody,
            }],
            on_conflict: OnConflict::Fail,
        };
        let effects = c.handle(&agent("outsider"), ancestor, NOW);
        assert_eq!(denial_race(&effects), Some(RaceId(1)));
        let unrelated = claim_as(&mut c, "outsider", "src/b.rs", OnConflict::Fail);
        assert!(is_granted(&unrelated));
    }

    #[test]
    fn an_outsider_who_waits_for_a_race_is_queued() {
        let mut c = core();
        open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let effects = claim_as(&mut c, "outsider", "src/a.rs", OnConflict::Wait);
        assert!(matches!(
            replies(&effects)[..],
            [ServerMsg::Queued { position: 1, .. }]
        ));
    }

    #[test]
    fn the_race_blocks_its_opener_too_and_a_second_race_over_it_is_denied() {
        let mut c = core();
        open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let own = claim_as(&mut c, "felix", "src/a.rs", OnConflict::Fail);
        assert_eq!(denial_race(&own), Some(RaceId(1)));

        let msg = open_msg(
            vec![edit("src/a.rs")],
            2,
            DEADLINE,
            vec![Criterion::TestsPass],
        );
        let again = c.handle(&agent("boss"), msg, NOW);
        assert_eq!(denial_race(&again), Some(RaceId(1)));
        assert!(matches!(
            logged(&again)[..],
            [EventKind::ClaimDenied { .. }]
        ));
    }

    #[test]
    fn a_race_cannot_open_over_a_claim_even_the_openers_own() {
        let mut c = core();
        let own = claim_as(&mut c, "felix", "src/a.rs", OnConflict::Fail);
        assert!(is_granted(&own));
        let effects = open_effects(&mut c, vec![Criterion::FirstSubmitted]);
        let [ServerMsg::Denied { conflicts, .. }] = replies(&effects)[..] else {
            panic!("expected Denied, got {effects:?}");
        };
        assert_eq!(conflicts[0].held_by, agent("felix"));
        assert_eq!(conflicts[0].race, None);
        assert!(c.state.races.is_empty());
        assert_eq!(c.state.next_race, 1, "a denied race consumes no id");
    }

    #[test]
    fn joining_grants_a_fenced_entry_on_exactly_the_races_scopes() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let effects = join_effects(&mut c, "a1", race, NOW + 5);

        let [ServerMsg::Granted {
            req,
            claim,
            fence,
            expires_at_ms,
            race: granted,
            at_risk,
        }] = replies(&effects)[..]
        else {
            panic!("expected Granted, got {effects:?}");
        };
        assert_eq!(*req, RequestId(2));
        assert_eq!(*granted, Some(race));
        assert_eq!(*expires_at_ms, NOW + 5 + LEASE);
        assert!(at_risk.is_empty());
        let [EventKind::ClaimGranted {
            agent: who,
            claim: logged_claim,
            fence: logged_fence,
            scopes,
            race: logged_race,
            ..
        }] = logged(&effects)[..]
        else {
            panic!("expected one ClaimGranted, got {effects:?}");
        };
        assert_eq!(*who, agent("a1"));
        assert_eq!((*logged_claim, *logged_fence), (*claim, *fence));
        assert_eq!(*scopes, vec![edit("src/a.rs")]);
        assert_eq!(*logged_race, Some(race));
    }

    #[test]
    fn entrants_never_conflict_with_each_other() {
        let mut c = core();
        let race = open_for(&mut c, 3, vec![Criterion::FirstSubmitted]);
        let first = join(&mut c, "a1", race);
        let second = join(&mut c, "a2", race);
        let third = join(&mut c, "a3", race);
        assert!(first.0 .0 < second.0 .0 && second.0 .0 < third.0 .0);
        assert!(first.1 < second.1 && second.1 < third.1);
    }

    #[test]
    fn an_entry_cannot_amend_its_scopes() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let (claim, fence) = join(&mut c, "a1", race);
        let before = state(&c);
        let amend = ClientMsg::Amend {
            req: RequestId(3),
            claim,
            fence,
            add: vec![edit("src/b.rs")],
        };
        let effects = c.handle(&agent("a1"), amend, NOW);
        assert_eq!(error_code(&effects), ErrorCode::RaceScopeFixed);
        assert!(logged(&effects).is_empty());
        assert_eq!(state(&c), before);
    }

    #[test]
    fn joining_an_unknown_race_is_refused() {
        let mut c = core();
        open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let before = state(&c);
        for id in [0, 2, 99] {
            let effects = join_effects(&mut c, "a1", RaceId(id), NOW);
            assert_eq!(error_code(&effects), ErrorCode::UnknownRace, "race {id}");
        }
        assert_eq!(state(&c), before);
    }

    #[test]
    fn joining_a_full_race_is_refused_and_it_stops_advertising_itself() {
        let mut c = core();
        let msg = open_msg(
            vec![edit("src/a.rs")],
            2,
            DEADLINE,
            vec![Criterion::FirstSubmitted],
        );
        c.handle(&agent("felix"), msg, NOW);
        let race = RaceId(1);
        join(&mut c, "a1", race);
        let denied = claim_as(&mut c, "outsider", "src/a.rs", OnConflict::Fail);
        assert_eq!(denial_race(&denied), Some(race), "one place is left");
        join(&mut c, "a2", race);

        let before = state(&c);
        let effects = join_effects(&mut c, "a3", race, NOW);
        assert_eq!(error_code(&effects), ErrorCode::RaceFull);
        assert_eq!(state(&c), before);
        let denied = claim_as(&mut c, "outsider", "src/a.rs", OnConflict::Fail);
        assert_eq!(denial_race(&denied), None, "a full race cannot be joined");
    }

    #[test]
    fn joining_the_same_race_twice_is_refused() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        join(&mut c, "a1", race);
        let before = state(&c);
        let effects = join_effects(&mut c, "a1", race, NOW);
        assert_eq!(error_code(&effects), ErrorCode::Malformed);
        assert_eq!(state(&c), before);
    }

    #[test]
    fn joining_a_decided_race_or_one_past_its_deadline_is_closed() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let late = join_effects(&mut c, "a1", race, DEADLINE);
        assert_eq!(
            error_code(&late),
            ErrorCode::RaceClosed,
            "the deadline closes the race before the join is read"
        );
        let decided = join_effects(&mut c, "a2", race, DEADLINE + 1);
        assert_eq!(error_code(&decided), ErrorCode::RaceClosed);
        assert!(c.state.races.is_empty());
    }

    #[test]
    fn a_submission_into_a_race_with_room_does_not_end_it() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        submit_at(&mut c, "a1", one, NOW + 1);

        assert_eq!(c.begin_verification(NOW + 2), None, "nothing is judged yet");
        assert!(c.state.races.contains_key(&race.0));
        let two = join(&mut c, "a2", race);
        submit_at(&mut c, "a2", two, NOW + 3);
        let first = c
            .begin_verification(NOW + 4)
            .expect("full and submitted: judged");
        assert_eq!(first.agent, agent("a1"));
    }

    #[test]
    fn a_full_race_with_every_entry_submitted_is_judged_at_once() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        submit_at(&mut c, "a2", two, NOW + 1);
        assert_eq!(c.begin_verification(NOW + 2), None, "a1 has not submitted");
        submit_at(&mut c, "a1", one, NOW + 3);
        assert!(c.begin_verification(NOW + 4).is_some());
        let closed = join_effects(&mut c, "a3", race, NOW + 4);
        assert_eq!(error_code(&closed), ErrorCode::RaceClosed);
    }

    #[test]
    fn at_its_deadline_a_race_with_room_and_one_entry_is_judged() {
        let mut c = core();
        let race = open_for(&mut c, 3, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        assert_eq!(c.begin_verification(NOW + 2), None);
        c.expire(DEADLINE);
        let effects = run_trials(&mut c, DEADLINE, &[("a1", passed())]);
        let (winner, _, _) = result_for(&effects, "a1").expect("decided");
        assert_eq!(winner, Some(one.0));
    }

    #[test]
    fn joining_a_race_that_is_being_judged_is_closed() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let entry = join(&mut c, "a1", race);
        submit_at(&mut c, "a1", entry, NOW + 1);
        let effects = join_effects(&mut c, "a2", race, NOW + 2);
        assert_eq!(error_code(&effects), ErrorCode::RaceClosed);
    }

    #[test]
    fn an_agent_holding_claims_may_join_but_one_with_a_queued_request_may_not() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::FirstSubmitted]);
        let held = claim_as(&mut c, "a1", "src/other.rs", OnConflict::Fail);
        assert!(is_granted(&held));
        join(&mut c, "a1", race);

        let blocker = claim_as(&mut c, "b1", "src/b.rs", OnConflict::Fail);
        assert!(is_granted(&blocker));
        let queued = claim_as(&mut c, "w1", "src/b.rs", OnConflict::Wait);
        assert!(matches!(replies(&queued)[..], [ServerMsg::Queued { .. }]));
        let before = state(&c);
        let effects = join_effects(&mut c, "w1", race, NOW);
        assert_eq!(error_code(&effects), ErrorCode::WaitWhileHolding);
        assert_eq!(state(&c), before);
    }

    #[test]
    fn an_entrant_may_not_wait_for_another_claim() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        join(&mut c, "a1", race);
        let blocker = claim_as(&mut c, "b1", "src/b.rs", OnConflict::Fail);
        assert!(is_granted(&blocker));
        let effects = claim_as(&mut c, "a1", "src/b.rs", OnConflict::Wait);
        assert_eq!(error_code(&effects), ErrorCode::WaitWhileHolding);
    }

    #[test]
    fn only_a_reviewer_may_open_a_race() {
        let mut c = core();
        let before = state(&c);
        let msg = open_msg(
            vec![edit("src/a.rs")],
            2,
            DEADLINE,
            vec![Criterion::TestsPass],
        );
        let effects = c.handle(&agent("a1"), msg, NOW);
        assert_eq!(error_code(&effects), ErrorCode::NotOwner);
        assert!(logged(&effects).is_empty());
        assert_eq!(state(&c), before);
    }

    #[test]
    fn a_race_with_unusable_terms_is_refused_and_changes_nothing() {
        let bad_scope = ScopeClaim {
            scope: Scope::File {
                path: "../etc/passwd".into(),
            },
            mode: Mode::EditBody,
        };
        let day = 24 * 60 * 60 * 1000;
        let terms = [
            ("no scopes", vec![], 2, DEADLINE, vec![Criterion::TestsPass]),
            (
                "bad scope",
                vec![bad_scope],
                2,
                DEADLINE,
                vec![Criterion::TestsPass],
            ),
            (
                "zero entrants",
                vec![edit("a")],
                0,
                DEADLINE,
                vec![Criterion::TestsPass],
            ),
            (
                "nine entrants",
                vec![edit("a")],
                9,
                DEADLINE,
                vec![Criterion::TestsPass],
            ),
            (
                "deadline now",
                vec![edit("a")],
                2,
                NOW,
                vec![Criterion::TestsPass],
            ),
            (
                "deadline past",
                vec![edit("a")],
                2,
                NOW - 1,
                vec![Criterion::TestsPass],
            ),
            (
                "deadline far",
                vec![edit("a")],
                2,
                NOW + day,
                vec![Criterion::TestsPass],
            ),
            (
                "deadline one past the limit",
                vec![edit("a")],
                2,
                NOW + MAX_RACE_MS + 1,
                vec![Criterion::TestsPass],
            ),
            ("no criteria", vec![edit("a")], 2, DEADLINE, vec![]),
            (
                "six criteria",
                vec![edit("a")],
                2,
                DEADLINE,
                vec![Criterion::TestsPass; 6],
            ),
        ];
        for (name, scopes, max, deadline, criteria) in terms {
            let mut c = core();
            let before = state(&c);
            let effects = c.handle(
                &agent("felix"),
                open_msg(scopes, max, deadline, criteria),
                NOW,
            );
            assert_eq!(error_code(&effects), ErrorCode::Malformed, "{name}");
            assert!(logged(&effects).is_empty(), "{name}");
            assert_eq!(state(&c), before, "{name}");
        }
    }

    #[test]
    fn the_limits_themselves_are_accepted() {
        for (max, deadline) in [(1, NOW + 1), (8, NOW + MAX_RACE_MS)] {
            let mut c = core();
            let msg = open_msg(vec![edit("a")], max, deadline, vec![Criterion::TestsPass]);
            let effects = c.handle(&agent("felix"), msg, NOW);
            assert!(
                matches!(replies(&effects)[..], [ServerMsg::RaceOpened { .. }]),
                "{effects:?}"
            );
        }
    }

    // ---- submitting and judging ----

    #[test]
    fn an_entry_is_checked_like_any_submission_and_never_dispatched() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::FirstSubmitted]);
        let (claim, fence) = join(&mut c, "a1", race);
        join(&mut c, "a2", race);

        let stale = submit_effects(&mut c, "a1", (claim, Fence(fence.0 + 50)), true, NOW);
        assert_eq!(error_code(&stale), ErrorCode::StaleFence);
        let stranger = submit_effects(&mut c, "a2", (claim, fence), true, NOW);
        assert_eq!(error_code(&stranger), ErrorCode::NotOwner);
        let msg = ClientMsg::Submit {
            req: RequestId(9),
            claim,
            fence,
            fork_commit: CommitId(fork_sha("a1")),
            touched: vec![edit("src/elsewhere.rs")],
            decisions: DecisionRecord::default(),
        };
        let uncovered = c.handle(&agent("a1"), msg, NOW);
        assert!(matches!(
            replies(&uncovered)[..],
            [ServerMsg::Uncovered { .. }]
        ));

        let accepted = submit_effects(&mut c, "a1", (claim, fence), false, NOW + 1);
        assert!(matches!(
            replies(&accepted)[..],
            [ServerMsg::Accepted {
                queue_position: 0,
                ..
            }]
        ));
        assert!(matches!(
            logged(&accepted)[..],
            [EventKind::Submitted { .. }]
        ));
        assert_eq!(c.begin_merge(NOW + 2), None, "an entry waits for the race");
        let again = submit_effects(&mut c, "a1", (claim, fence), true, NOW + 3);
        assert_eq!(error_code(&again), ErrorCode::AlreadySubmitted);
        assert_eq!(
            c.begin_verification(NOW + 2),
            None,
            "a2 has not submitted, so nothing is judged"
        );
    }

    #[test]
    fn a_race_is_judged_when_every_entrant_has_submitted() {
        let mut c = core();
        let race = open_for(
            &mut c,
            2,
            vec![Criterion::TestsPass, Criterion::FirstSubmitted],
        );
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        submit_at(&mut c, "a2", two, NOW + 2);

        let first = c.begin_verification(NOW + 3).expect("a1's tests are due");
        assert_eq!(first.agent, agent("a1"));
        assert_eq!(first.commit, Some(CommitId(fork_sha("a1"))));
        assert_eq!(first.before, CommitId(MAIN.into()));
        assert_eq!(first.main, CommitId(MAIN.into()));
        c.verification_outcome(first.id, &failed(), NOW + 3);
        let second = c.begin_verification(NOW + 4).expect("a2's tests are due");
        assert_eq!(second.agent, agent("a2"));
        assert_eq!((second.before, second.main), (first.before, first.main));
        let effects = c.verification_outcome(second.id, &passed(), NOW + 4);

        let [EventKind::RaceDecided {
            race: decided,
            winner,
            ranking,
        }, ..] = logged(&effects)[..]
        else {
            panic!("expected RaceDecided first, got {effects:?}");
        };
        assert_eq!(*decided, race);
        assert_eq!(*winner, Some(two.0));
        assert_eq!(*ranking, vec![two.0]);
        for who in ["a1", "a2", "felix"] {
            let (winner, ranking, entries) = result_for(&effects, who).expect(who);
            assert_eq!(winner, Some(two.0), "{who}");
            assert_eq!(ranking, vec![two.0], "{who}");
            let tests: Vec<_> = entries.iter().map(|e| (e.claim, e.tests_passed)).collect();
            assert_eq!(tests, [(one.0, Some(false)), (two.0, Some(true))], "{who}");
            assert!(entries
                .iter()
                .all(|e| e.risk_bp.is_none() && e.diff_lines.is_none()));
            assert_eq!(entries[1].submitted_at_ms, NOW + 2);
            assert_eq!(entries[1].fork_commit, CommitId(fork_sha("a2")));
        }
    }

    #[test]
    fn the_winner_merges_and_the_losers_are_rejected_with_a_fixed_reason() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        submit_at(&mut c, "a2", two, NOW + 2);
        let effects = run_trials(&mut c, NOW + 3, &[("a1", passed()), ("a2", passed())]);

        let to_loser = notices(&effects, "a2");
        assert!(
            to_loser.iter().any(|msg| matches!(msg,
                ServerMsg::SubmitRejected { claim, reason }
                    if *claim == two.0 && reason == "lost the race")),
            "{to_loser:?}"
        );
        let kinds = logged(&effects);
        assert!(kinds.iter().any(|k| matches!(k,
            EventKind::SubmitRejected { claim, reason } if *claim == two.0 && reason == "lost the race")));
        assert!(kinds.iter().any(|k| matches!(k,
            EventKind::ClaimReleased { claim, reason: ReleaseReason::LostRace } if *claim == two.0)));
        let to_winner = notices(&effects, "a1");
        assert!(
            to_winner
                .iter()
                .any(|msg| matches!(msg, ServerMsg::Accepted { claim, queue_position: 1, .. } if *claim == one.0)),
            "{to_winner:?}"
        );

        let loser = ClientMsg::Release {
            claim: two.0,
            fence: two.1,
            req: None,
        };
        assert_eq!(
            error_code(&c.handle(&agent("a2"), loser, NOW + 4)),
            ErrorCode::StaleFence,
            "the loser's claim has ended"
        );
        let dispatch = c.begin_merge(NOW + 5).expect("the winner is due");
        assert_eq!(dispatch.claim, one.0);
        assert_eq!(dispatch.fork_commit, CommitId(fork_sha("a1")));
        assert_eq!(dispatch.scopes, vec![edit("src/a.rs")]);

        let landed = MergeOutcome::Merged {
            base: CommitId(MAIN.into()),
            head: CommitId("e".repeat(40)),
        };
        let merged = c.merge_outcome(one.0, &landed, NOW + 6);
        assert!(notices(&merged, "a1")
            .iter()
            .any(|msg| matches!(msg, ServerMsg::Merged { claim, .. } if *claim == one.0)));
        let free = claim_as(&mut c, "outsider", "src/a.rs", OnConflict::Fail);
        assert!(
            is_granted(&free),
            "the scopes are free once the winner lands"
        );
    }

    #[test]
    fn the_winner_holds_the_scopes_until_it_merges() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        run_trials(&mut c, NOW + 2, &[("a1", passed())]);
        assert!(c.state.races.is_empty());

        let denied = claim_as(&mut c, "outsider", "src/a.rs", OnConflict::Fail);
        let ServerMsg::Denied { conflicts, .. } = replies(&denied)[0] else {
            panic!("expected Denied, got {denied:?}");
        };
        assert_eq!(
            conflicts[0].held_by,
            agent("a1"),
            "the winner's own claim blocks now"
        );
        assert_eq!(conflicts[0].race, None, "a decided race cannot be joined");
    }

    #[test]
    fn a_race_is_judged_at_its_deadline_and_a_slow_entrant_is_cut_off() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        submit_at(&mut c, "a1", one, NOW + 1);

        assert_eq!(c.next_expiry_ms(), Some(DEADLINE));
        assert!(c.has_due_expiry(DEADLINE));
        assert!(!c.has_due_expiry(DEADLINE - 1));
        assert!(c.expire(DEADLINE - 1).is_empty());
        let cut = c.expire(DEADLINE);
        assert!(logged(&cut).iter().any(|k| matches!(k,
            EventKind::ClaimReleased { claim, reason: ReleaseReason::LostRace } if *claim == two.0)));
        let effects = run_trials(&mut c, DEADLINE, &[("a1", passed())]);

        let (winner, ranking, entries) = result_for(&effects, "a1").expect("winner told");
        assert_eq!((winner, ranking), (Some(one.0), vec![one.0]));
        assert_eq!(entries.len(), 1, "only a submitted entry is an entry");
        let (winner, _, _) = result_for(&effects, "a2").expect("the cut-off entrant is told too");
        assert_eq!(winner, Some(one.0));
        let late = submit_effects(&mut c, "a2", two, true, DEADLINE + 1);
        assert_eq!(error_code(&late), ErrorCode::StaleFence);
        assert_eq!(c.next_expiry_ms(), None);
    }

    #[test]
    fn the_wake_up_follows_the_deadline_of_an_open_race_only() {
        let mut c = core();
        assert_eq!(c.next_wake_ms(false, false, NOW), None);
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        assert_eq!(c.next_wake_ms(false, false, NOW), Some(DEADLINE));
        let one = join(&mut c, "a1", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        assert_ne!(
            c.next_expiry_ms(),
            Some(DEADLINE),
            "a judged race has no deadline left"
        );
        assert_eq!(c.next_expiry_ms(), None);
    }

    #[test]
    fn a_race_nobody_entered_is_decided_with_no_winner_and_frees_its_scopes() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let waiting = claim_as(&mut c, "w1", "src/a.rs", OnConflict::Wait);
        assert!(matches!(replies(&waiting)[..], [ServerMsg::Queued { .. }]));

        let effects = c.expire(DEADLINE);
        let [EventKind::RaceDecided {
            winner, ranking, ..
        }, EventKind::ClaimGranted { agent: granted, .. }] = logged(&effects)[..]
        else {
            panic!("expected RaceDecided then the waiter's grant, got {effects:?}");
        };
        assert_eq!((*winner, ranking.len()), (None, 0));
        assert_eq!(*granted, agent("w1"));
        let (winner, ranking, entries) = result_for(&effects, "felix").expect("the opener is told");
        assert_eq!((winner, ranking.len(), entries.len()), (None, 0, 0));
        assert!(c.state.races.is_empty());
        let closed = join_effects(&mut c, "a1", race, DEADLINE + 1);
        assert_eq!(error_code(&closed), ErrorCode::RaceClosed);
    }

    #[test]
    fn a_lease_that_runs_out_removes_the_entrant_and_the_race_waits_for_its_deadline() {
        let mut c = core();
        let deadline = NOW + MAX_RACE_MS;
        let msg = open_msg(
            vec![edit("src/a.rs")],
            2,
            deadline,
            vec![Criterion::FirstSubmitted],
        );
        c.handle(&agent("felix"), msg, NOW);
        let race = RaceId(1);
        let one = join(&mut c, "a1", race);
        join(&mut c, "a2", race);
        submit_at(&mut c, "a1", one, NOW + 1);

        let expired = c.expire(NOW + LEASE);
        assert!(logged(&expired).iter().any(|k| matches!(
            k,
            EventKind::ClaimReleased {
                reason: ReleaseReason::LeaseExpired,
                ..
            }
        )));
        assert_eq!(
            c.begin_verification(NOW + LEASE),
            None,
            "the race has room again, so it is not judged yet"
        );
        join(&mut c, "a3", race);
        c.expire(deadline);
        let trial = c
            .begin_verification(deadline)
            .expect("judged at the deadline");
        assert_eq!(trial.agent, agent("a1"));
    }

    #[test]
    fn releasing_an_entry_frees_its_place() {
        let mut c = core();
        let msg = open_msg(
            vec![edit("src/a.rs")],
            1,
            DEADLINE,
            vec![Criterion::FirstSubmitted],
        );
        c.handle(&agent("felix"), msg, NOW);
        let race = RaceId(1);
        let (claim, fence) = join(&mut c, "a1", race);
        let full = join_effects(&mut c, "a2", race, NOW);
        assert_eq!(error_code(&full), ErrorCode::RaceFull);

        let release = ClientMsg::Release {
            claim,
            fence,
            req: None,
        };
        let effects = c.handle(&agent("a1"), release, NOW + 1);
        assert!(matches!(
            logged(&effects)[..],
            [EventKind::ClaimReleased { .. }]
        ));
        join(&mut c, "a2", race);
        assert!(
            c.state.races.contains_key(&1),
            "a race with no entrants stays open"
        );
    }

    // ---- ranking and filtering ----

    #[test]
    fn first_submitted_ranks_by_submission_not_by_joining() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        submit_at(&mut c, "a2", two, NOW + 1);
        submit_at(&mut c, "a1", one, NOW + 2);
        let effects = run_trials(&mut c, NOW + 3, &[("a1", passed()), ("a2", passed())]);
        let (winner, ranking, _) = result_for(&effects, "felix").expect("told");
        assert_eq!(winner, Some(two.0));
        assert_eq!(ranking, vec![two.0, one.0]);
    }

    #[test]
    fn criteria_that_are_not_measured_are_refused() {
        for criterion in [Criterion::LowestRisk, Criterion::SmallestDiff] {
            let mut c = core();
            let before = state(&c);
            let criteria = vec![Criterion::TestsPass, criterion];
            let msg = open_msg(vec![edit("src/a.rs")], 2, DEADLINE, criteria);
            let effects = c.handle(&agent("felix"), msg, NOW);
            assert_eq!(error_code(&effects), ErrorCode::Malformed, "{criterion:?}");
            let ServerMsg::Error { message, .. } = replies(&effects)[0] else {
                panic!("expected Error");
            };
            assert!(message.contains("not measured"), "{message}");
            assert!(logged(&effects).is_empty());
            assert_eq!(state(&c), before);
        }
    }

    #[test]
    fn tests_pass_drops_entries_whose_tests_failed_or_did_not_run() {
        let unknown = TrialReport {
            before: Some(TrialOutcome::Conflict {}),
            after: None,
        };
        let mut c = core();
        let race = open_for(
            &mut c,
            3,
            vec![Criterion::TestsPass, Criterion::FirstSubmitted],
        );
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        let three = join(&mut c, "a3", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        submit_at(&mut c, "a2", two, NOW + 2);
        submit_at(&mut c, "a3", three, NOW + 3);
        let results = [("a1", failed()), ("a2", unknown), ("a3", passed())];
        let effects = run_trials(&mut c, NOW + 4, &results);

        let (winner, ranking, entries) = result_for(&effects, "felix").expect("told");
        assert_eq!((winner, ranking), (Some(three.0), vec![three.0]));
        let tests: Vec<_> = entries.iter().map(|e| e.tests_passed).collect();
        assert_eq!(tests, [Some(false), None, Some(true)]);
    }

    #[test]
    fn without_tests_pass_a_failing_entry_can_still_win_and_is_recorded_as_failing() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        submit_at(&mut c, "a2", two, NOW + 2);
        let effects = run_trials(&mut c, NOW + 3, &[("a1", failed()), ("a2", passed())]);
        let (winner, _, entries) = result_for(&effects, "felix").expect("told");
        assert_eq!(winner, Some(one.0));
        assert_eq!(entries[0].tests_passed, Some(false));
    }

    #[test]
    fn no_eligible_entry_rejects_everyone_and_frees_the_scopes() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::TestsPass]);
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        submit_at(&mut c, "a2", two, NOW + 2);
        let effects = run_trials(&mut c, NOW + 3, &[("a1", failed()), ("a2", failed())]);

        let (winner, ranking, entries) = result_for(&effects, "a1").expect("told");
        assert_eq!((winner, ranking.len(), entries.len()), (None, 0, 2));
        for (who, entry) in [("a1", one), ("a2", two)] {
            assert!(
                notices(&effects, who).iter().any(|msg| matches!(msg,
                    ServerMsg::SubmitRejected { claim, reason }
                        if *claim == entry.0 && reason == "lost the race")),
                "{who}"
            );
        }
        let [EventKind::RaceDecided { winner, .. }, ..] = logged(&effects)[..] else {
            panic!("expected RaceDecided first, got {effects:?}");
        };
        assert_eq!(*winner, None);
        assert_eq!(c.begin_merge(NOW + 4), None);
        let free = claim_as(&mut c, "outsider", "src/a.rs", OnConflict::Fail);
        assert!(is_granted(&free));
    }

    // ---- trials ----

    #[test]
    fn trials_wait_for_merges() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        let other = claim_as(&mut c, "x1", "src/x.rs", OnConflict::Fail);
        let [ServerMsg::Granted { claim, fence, .. }] = replies(&other)[..] else {
            panic!("expected Granted");
        };
        let msg = ClientMsg::Submit {
            req: RequestId(9),
            claim: *claim,
            fence: *fence,
            fork_commit: CommitId(fork_sha("x1")),
            touched: vec![edit("src/x.rs")],
            decisions: DecisionRecord {
                evidence: vec!["tests passed".into()],
                ..DecisionRecord::default()
            },
        };
        c.handle(&agent("x1"), msg, NOW + 1);
        submit_at(&mut c, "a1", one, NOW + 2);

        assert_eq!(c.begin_verification(NOW + 3), None, "a merge is due first");
        let merge = c.begin_merge(NOW + 3).expect("x1's merge");
        assert_eq!(merge.claim, *claim);
        let landed = MergeOutcome::Merged {
            base: CommitId(MAIN.into()),
            head: CommitId("e".repeat(40)),
        };
        c.merge_outcome(*claim, &landed, NOW + 4);
        let trial = c
            .begin_verification(NOW + 5)
            .expect("the race's tests run next");
        assert_eq!(trial.agent, agent("a1"));
        assert_eq!(
            trial.before, trial.main,
            "both sides are the head when the race closed"
        );
        assert_eq!(trial.main, CommitId(MAIN.into()));
    }

    #[test]
    fn a_trial_that_keeps_failing_to_run_leaves_the_tests_unknown() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        let down = TrialReport::stopped(TrialOutcome::ServiceUnavailable);
        let mut now = NOW + 2;
        let mut effects = Vec::new();
        for attempt in 1..=crate::merge::MAX_INFRA_RETRIES + 1 {
            let dispatch = c.begin_verification(now).expect("another attempt is due");
            assert_eq!(dispatch.attempt, attempt);
            assert!(c.state.races.contains_key(&race.0), "still being judged");
            effects = c.verification_outcome(dispatch.id, &down, now);
            now += 10 * crate::merge::infra_backoff_ms(attempt);
        }
        let (winner, _, entries) = result_for(&effects, "a1").expect("decided after the retries");
        assert_eq!(winner, Some(one.0));
        assert_eq!(entries[0].tests_passed, None);
    }

    #[test]
    fn with_no_known_head_nothing_is_tried_and_the_tests_stay_unknown() {
        let mut c = bare_core();
        let msg = open_msg(
            vec![edit("src/a.rs")],
            1,
            DEADLINE,
            vec![Criterion::TestsPass],
        );
        c.handle(&agent("felix"), msg, NOW);
        let one = join(&mut c, "a1", RaceId(1));
        let effects = submit_effects(&mut c, "a1", one, true, NOW + 1);

        assert_eq!(c.begin_verification(NOW + 2), None);
        let (winner, ranking, entries) = result_for(&effects, "a1").expect("decided at once");
        assert_eq!(
            (winner, ranking.len()),
            (None, 0),
            "unknown tests do not pass"
        );
        assert_eq!(entries[0].tests_passed, None);
    }

    // ---- HumanPick ----

    fn human_race() -> (Coordinator, [(ClaimId, Fence); 3]) {
        let mut c = core();
        let race = open_for(&mut c, 3, vec![Criterion::TestsPass, Criterion::HumanPick]);
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        let three = join(&mut c, "a3", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        submit_at(&mut c, "a2", two, NOW + 2);
        submit_at(&mut c, "a3", three, NOW + 3);
        (c, [one, two, three])
    }

    fn pick(c: &mut Coordinator, who: &str, claim: ClaimId) -> Vec<Effect> {
        let msg = ClientMsg::PickWinner {
            req: RequestId(8),
            race: RaceId(1),
            claim,
        };
        c.handle(&agent(who), msg, NOW + 10)
    }

    #[test]
    fn a_human_pick_race_recommends_then_waits_for_the_pick() {
        let (mut c, [one, two, three]) = human_race();
        let results = [("a1", passed()), ("a2", passed()), ("a3", failed())];
        let effects = run_trials(&mut c, NOW + 4, &results);

        for who in ["a1", "a2", "a3", "felix"] {
            let (winner, ranking, entries) = result_for(&effects, who).expect(who);
            assert_eq!(winner, None, "{who}: no winner until a pick");
            assert_eq!(ranking, vec![one.0, two.0], "{who}");
            assert_eq!(entries.len(), 3, "{who}");
        }
        assert!(
            logged(&effects).is_empty(),
            "nothing is decided, so nothing is logged: {effects:?}"
        );
        assert_eq!(c.begin_merge(NOW + 5), None);
        let held = claim_as(&mut c, "outsider", "src/a.rs", OnConflict::Fail);
        assert_eq!(denial_race(&held), None, "the scopes stay held");
        let _ = three;
    }

    #[test]
    fn a_pick_decides_the_race_once_and_promotes_the_chosen_entry() {
        let (mut c, [one, two, three]) = human_race();
        let results = [("a1", passed()), ("a2", passed()), ("a3", failed())];
        run_trials(&mut c, NOW + 4, &results);

        let effects = pick(&mut c, "felix", two.0);
        let [EventKind::RaceDecided {
            winner, ranking, ..
        }, ..] = logged(&effects)[..]
        else {
            panic!("expected RaceDecided first, got {effects:?}");
        };
        assert_eq!(*winner, Some(two.0));
        assert_eq!(*ranking, vec![one.0, two.0], "the recommendation is kept");
        let (winner, _, _) = result_for(&effects, "a1").expect("entrants are told");
        assert_eq!(winner, Some(two.0));
        assert!(
            result_for(&effects, "felix").is_some(),
            "the picker is told"
        );
        let rejected: Vec<_> = [("a1", one), ("a3", three)]
            .into_iter()
            .filter(|(who, entry)| {
                notices(&effects, who).iter().any(|msg| {
                    matches!(msg, ServerMsg::SubmitRejected { claim, .. } if *claim == entry.0)
                })
            })
            .map(|(who, _)| who)
            .collect();
        assert_eq!(rejected, ["a1", "a3"]);
        assert_eq!(c.begin_merge(NOW + 11).map(|d| d.claim), Some(two.0));

        let summary = Summary::from_events(&all_events(&[&effects]));
        assert_eq!(summary.races_decided, 1);
    }

    #[test]
    fn only_a_reviewer_who_is_not_an_entrant_may_pick() {
        let (mut c, [one, ..]) = human_race();
        run_trials(
            &mut c,
            NOW + 4,
            &[("a1", passed()), ("a2", passed()), ("a3", passed())],
        );
        let stranger = pick(&mut c, "bystander", one.0);
        assert_eq!(error_code(&stranger), ErrorCode::NotOwner);
        let before = state(&c);
        c.set_reviewers(vec![agent("felix"), agent("a2")]);
        let entrant = pick(&mut c, "a2", one.0);
        assert_eq!(
            error_code(&entrant),
            ErrorCode::NotOwner,
            "an entrant may not pick"
        );
        c.set_reviewers(vec![agent("felix"), agent("boss")]);
        assert_eq!(state(&c), before);
    }

    #[test]
    fn a_pick_must_name_an_eligible_entry_in_a_race_that_is_waiting() {
        let (mut c, [_, _, three]) = human_race();
        let early = pick(&mut c, "felix", three.0);
        assert_eq!(
            error_code(&early),
            ErrorCode::Malformed,
            "still being judged"
        );

        run_trials(
            &mut c,
            NOW + 4,
            &[("a1", passed()), ("a2", passed()), ("a3", failed())],
        );
        let before = state(&c);
        let filtered = pick(&mut c, "felix", three.0);
        assert_eq!(
            error_code(&filtered),
            ErrorCode::NotAnEntrant,
            "its tests failed"
        );
        let stranger = pick(&mut c, "felix", ClaimId(999));
        assert_eq!(error_code(&stranger), ErrorCode::NotAnEntrant);
        let unknown = c.handle(
            &agent("felix"),
            ClientMsg::PickWinner {
                req: RequestId(8),
                race: RaceId(7),
                claim: three.0,
            },
            NOW + 10,
        );
        assert_eq!(error_code(&unknown), ErrorCode::UnknownRace);
        assert_eq!(state(&c), before);
    }

    #[test]
    fn a_pick_for_a_decided_race_is_closed() {
        let (mut c, [one, ..]) = human_race();
        run_trials(
            &mut c,
            NOW + 4,
            &[("a1", passed()), ("a2", passed()), ("a3", passed())],
        );
        let first = pick(&mut c, "felix", one.0);
        assert!(!logged(&first).is_empty());
        let second = pick(&mut c, "felix", one.0);
        assert_eq!(error_code(&second), ErrorCode::RaceClosed);
    }

    #[test]
    fn a_race_nobody_picked_is_decided_without_a_winner_when_the_pick_window_ends() {
        let (mut c, [one, two, three]) = human_race();
        let results = [("a1", passed()), ("a2", passed()), ("a3", failed())];
        run_trials(&mut c, NOW + 4, &results);
        let due = DEADLINE + MAX_RACE_MS;
        assert_eq!(c.next_expiry_ms(), Some(due));
        assert_eq!(c.next_wake_ms(false, false, NOW + 5), Some(due));
        assert!(
            c.expire(due - 1).is_empty(),
            "still waiting one tick before"
        );

        let effects = c.expire(due);
        let [EventKind::RaceDecided {
            winner, ranking, ..
        }, ..] = logged(&effects)[..]
        else {
            panic!("expected RaceDecided first, got {effects:?}");
        };
        assert_eq!((*winner, ranking.clone()), (None, vec![one.0, two.0]));
        for (who, entry) in [("a1", one), ("a2", two), ("a3", three)] {
            assert!(
                notices(&effects, who).iter().any(|msg| matches!(msg,
                    ServerMsg::SubmitRejected { claim, reason }
                        if *claim == entry.0 && reason == "the race was not picked in time")),
                "{who}"
            );
            let (winner, _, entries) = result_for(&effects, who).expect(who);
            assert_eq!((winner, entries.len()), (None, 3), "{who}");
        }
        assert_eq!(c.begin_merge(due + 1), None);
        let free = claim_as(&mut c, "outsider", "src/a.rs", OnConflict::Fail);
        assert!(is_granted(&free));
        assert_eq!(
            c.next_expiry_ms(),
            Some(due + 1 + LEASE),
            "only the new claim's lease is left"
        );
        let late = pick(&mut c, "felix", one.0);
        assert_eq!(error_code(&late), ErrorCode::RaceClosed);
    }

    #[test]
    fn a_pick_made_in_time_stops_the_pick_window() {
        let (mut c, [one, ..]) = human_race();
        run_trials(
            &mut c,
            NOW + 4,
            &[("a1", passed()), ("a2", passed()), ("a3", passed())],
        );
        pick(&mut c, "felix", one.0);
        assert_eq!(c.next_expiry_ms(), None);
        assert!(c.expire(DEADLINE + MAX_RACE_MS).is_empty());
    }

    #[test]
    fn a_human_pick_race_with_nothing_eligible_is_decided_without_waiting() {
        let (mut c, _) = human_race();
        let effects = run_trials(
            &mut c,
            NOW + 4,
            &[("a1", failed()), ("a2", failed()), ("a3", failed())],
        );
        let (winner, ranking, _) = result_for(&effects, "felix").expect("told");
        assert_eq!((winner, ranking.len()), (None, 0));
        assert!(matches!(
            logged(&effects)[0],
            EventKind::RaceDecided { winner: None, .. }
        ));
    }

    // ---- the winner's submission ----

    #[test]
    fn a_winner_without_test_evidence_waits_for_review_before_it_is_dispatched() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        let bare = submit_effects(&mut c, "a1", one, false, NOW + 1);
        assert!(matches!(
            replies(&bare)[..],
            [ServerMsg::Accepted {
                queue_position: 0,
                ..
            }]
        ));

        let unknown = TrialReport {
            before: Some(TrialOutcome::Conflict {}),
            after: None,
        };
        let effects = run_trials(&mut c, NOW + 3, &[("a1", unknown)]);
        assert!(notices(&effects, "a1").iter().any(|msg| matches!(msg,
            ServerMsg::ReviewRequired { claim, .. } if *claim == one.0)));
        assert!(logged(&effects).iter().any(|k| matches!(k,
            EventKind::ReviewRequested { claim, .. } if *claim == one.0)));
        assert_eq!(c.begin_merge(NOW + 4), None, "held for a reviewer");

        let approve = ClientMsg::Review {
            req: RequestId(4),
            claim: one.0,
            approve: true,
            note: None,
        };
        let approved = c.handle(&agent("felix"), approve, NOW + 6);
        assert!(notices(&approved, "a1").iter().any(|msg| matches!(msg,
            ServerMsg::Accepted { claim, queue_position: 1, .. } if *claim == one.0)));
        assert_eq!(c.begin_merge(NOW + 7).map(|d| d.claim), Some(one.0));
    }

    #[test]
    fn a_passing_trial_counts_as_test_evidence_for_the_review_gate() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        submit_effects(&mut c, "a1", one, false, NOW + 1);
        let effects = run_trials(&mut c, NOW + 3, &[("a1", passed())]);
        assert!(notices(&effects, "a1").iter().any(|msg| matches!(msg,
            ServerMsg::Accepted { claim, .. } if *claim == one.0)));
        assert!(!logged(&effects)
            .iter()
            .any(|k| matches!(k, EventKind::ReviewRequested { .. })));
        assert_eq!(c.begin_merge(NOW + 4).map(|d| d.claim), Some(one.0));
    }

    #[test]
    fn a_failing_trial_is_not_test_evidence() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        submit_effects(&mut c, "a1", one, false, NOW + 1);
        let effects = run_trials(&mut c, NOW + 3, &[("a1", failed())]);
        assert!(notices(&effects, "a1")
            .iter()
            .any(|msg| matches!(msg, ServerMsg::ReviewRequired { .. })));
    }

    #[test]
    fn the_winner_keeps_its_submission_order_in_the_merge_queue() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", race);
        submit_at(&mut c, "a1", one, NOW + 1);
        let held = claim_as(&mut c, "x1", "src/x.rs", OnConflict::Fail);
        let [ServerMsg::Granted { claim, fence, .. }] = replies(&held)[..] else {
            panic!("expected Granted");
        };
        let flagged = ClientMsg::Submit {
            req: RequestId(9),
            claim: *claim,
            fence: *fence,
            fork_commit: CommitId(fork_sha("x1")),
            touched: vec![edit("src/x.rs")],
            decisions: DecisionRecord::default(),
        };
        let reply = c.handle(&agent("x1"), flagged, NOW + 2);
        assert!(matches!(
            replies(&reply)[..],
            [ServerMsg::ReviewRequired { .. }]
        ));

        let effects = run_trials(&mut c, NOW + 3, &[("a1", passed())]);
        assert!(
            notices(&effects, "a1").iter().any(|msg| matches!(
                msg,
                ServerMsg::Accepted {
                    queue_position: 1,
                    ..
                }
            )),
            "the winner was submitted before x1, so it is first in line: {effects:?}"
        );
    }

    // ---- evidence and persistence ----

    #[test]
    fn races_decided_counts_each_decided_race_once() {
        let mut c = core();
        let msg = open_msg(
            vec![edit("src/a.rs")],
            1,
            DEADLINE,
            vec![Criterion::FirstSubmitted],
        );
        let opened = c.handle(&agent("felix"), msg, NOW);
        let joined = join_effects(&mut c, "a1", RaceId(1), NOW);
        let [ServerMsg::Granted { claim, fence, .. }] = replies(&joined)[..] else {
            panic!("expected Granted");
        };
        let submitted = submit_effects(&mut c, "a1", (*claim, *fence), true, NOW + 1);
        let decided = run_trials(&mut c, NOW + 2, &[("a1", passed())]);
        let all = all_events(&[&opened, &joined, &submitted, &decided]);
        let summary = Summary::from_events(&all);
        assert_eq!(summary.races_decided, 1);
        assert_eq!(summary.claims_granted, 1);
        assert_eq!(summary.merges, 0, "a decided race is not yet a merge");
        assert_eq!(summary.denials, 0);
    }

    #[test]
    fn a_race_survives_a_save_and_load_at_every_stage() {
        let mut c = core();
        let race = open_for(&mut c, 2, vec![Criterion::TestsPass, Criterion::HumanPick]);
        let one = join(&mut c, "a1", race);
        let two = join(&mut c, "a2", race);
        let mut stages = vec![state(&c)];
        submit_at(&mut c, "a1", one, NOW + 1);
        stages.push(state(&c));
        submit_at(&mut c, "a2", two, NOW + 2);
        stages.push(state(&c));
        run_trials(&mut c, NOW + 3, &[("a1", passed()), ("a2", failed())]);
        stages.push(state(&c));
        for stored in stages {
            let restored: Coordinator = serde_json::from_str(&stored).unwrap();
            assert_eq!(state(&restored), stored);
        }

        let mut probe: Coordinator = serde_json::from_str(&state(&c)).unwrap();
        let denied = claim_as(&mut probe, "outsider", "src/a.rs", OnConflict::Fail);
        assert_eq!(
            denial_race(&denied),
            None,
            "the restored race holds its scopes"
        );
        let mut restored: Coordinator = serde_json::from_str(&state(&c)).unwrap();
        let live = pick(&mut c, "felix", one.0);
        let replayed = pick(&mut restored, "felix", one.0);
        assert_eq!(format!("{live:?}"), format!("{replayed:?}"));
        assert_eq!(state(&c), state(&restored));
    }

    #[test]
    fn an_open_race_still_advertises_itself_after_a_restore() {
        let mut c = core();
        open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let mut restored: Coordinator = serde_json::from_str(&state(&c)).unwrap();
        let denied = claim_as(&mut restored, "outsider", "src/a.rs", OnConflict::Fail);
        assert_eq!(denial_race(&denied), Some(RaceId(1)));
        join(&mut restored, "a1", RaceId(1));
    }

    #[test]
    fn a_state_stored_before_races_loads_with_no_races_and_the_first_race_id() {
        let mut c = core();
        let mut stored: serde_json::Value = serde_json::from_str(&state(&c)).unwrap();
        let fields = stored.as_object_mut().unwrap();
        assert!(fields.remove("races").is_some());
        assert!(fields.remove("next_race").is_some());
        let mut restored: Coordinator = serde_json::from_value(stored).unwrap();
        assert!(restored.state.races.is_empty());
        let msg = open_msg(
            vec![edit("src/a.rs")],
            2,
            DEADLINE,
            vec![Criterion::TestsPass],
        );
        let effects = restored.handle(&agent("felix"), msg, NOW);
        assert!(matches!(
            replies(&effects)[..],
            [ServerMsg::RaceOpened {
                race: RaceId(1),
                ..
            }]
        ));
        let _ = &mut c;
    }

    #[test]
    fn a_claim_stored_before_entries_loads_as_a_real_claim() {
        let mut c = core();
        let claimed = claim_as(&mut c, "a1", "src/a.rs", OnConflict::Fail);
        assert!(is_granted(&claimed));
        let stored = state(&c);
        assert!(stored.contains("\"kind\":\"real\""), "{stored}");
        let restored: Coordinator = serde_json::from_str(&stored).unwrap();
        assert_eq!(state(&restored), stored);
    }

    #[test]
    fn an_entry_is_stored_with_its_race() {
        let mut c = core();
        let race = open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        join(&mut c, "a1", race);
        let stored = state(&c);
        assert!(stored.contains("\"kind\":{\"entry\":1}"), "{stored}");
    }

    #[test]
    fn a_waiter_blocked_by_a_race_is_granted_when_the_race_ends_without_a_winner() {
        let mut c = core();
        open_for(&mut c, 1, vec![Criterion::TestsPass]);
        let queued = claim_as(&mut c, "w1", "src/a.rs", OnConflict::Wait);
        assert!(matches!(replies(&queued)[..], [ServerMsg::Queued { .. }]));
        let one = join(&mut c, "a1", RaceId(1));
        submit_at(&mut c, "a1", one, NOW + 1);
        let effects = run_trials(&mut c, NOW + 2, &[("a1", failed())]);
        assert!(notices(&effects, "w1")
            .iter()
            .any(|msg| matches!(msg, ServerMsg::Granted { race: None, .. })));
    }

    #[test]
    fn a_waiter_stays_queued_while_the_winner_waits_to_merge() {
        let mut c = core();
        open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        claim_as(&mut c, "w1", "src/a.rs", OnConflict::Wait);
        let one = join(&mut c, "a1", RaceId(1));
        submit_at(&mut c, "a1", one, NOW + 1);
        let effects = run_trials(&mut c, NOW + 2, &[("a1", passed())]);
        assert!(notices(&effects, "w1").is_empty(), "{effects:?}");
        assert!(c.has_queued_request(&agent("w1")));
    }

    #[test]
    fn a_winner_rejected_by_the_steward_gets_its_claim_back_as_any_claim_would() {
        let mut c = core();
        open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", RaceId(1));
        submit_at(&mut c, "a1", one, NOW + 1);
        run_trials(&mut c, NOW + 2, &[("a1", passed())]);
        assert_eq!(c.begin_merge(NOW + 3).map(|d| d.claim), Some(one.0));
        let refused = MergeOutcome::Conflict {
            files: vec!["src/a.rs".into()],
        };
        let effects = c.merge_outcome(one.0, &refused, NOW + 4);
        assert!(notices(&effects, "a1")
            .iter()
            .any(|msg| matches!(msg, ServerMsg::SubmitRejected { .. })));
        let again = submit_effects(&mut c, "a1", one, true, NOW + 5);
        assert!(
            matches!(
                replies(&again)[..],
                [ServerMsg::Accepted {
                    queue_position: 1,
                    ..
                }]
            ),
            "{again:?}"
        );
    }

    #[test]
    fn the_winners_claim_challenges_assumptions_when_it_is_promoted() {
        let mut c = core();
        let assumes = Assumption {
            scope: Scope::File {
                path: "src/a.rs".into(),
            },
            statement: "f returns Some".into(),
        };
        let held = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent {
                summary: "needs a".into(),
                task_ref: None,
                assumptions: vec![assumes],
            },
            scopes: vec![ScopeClaim {
                scope: Scope::File {
                    path: "src/b.rs".into(),
                },
                mode: Mode::EditBody,
            }],
            on_conflict: OnConflict::Fail,
        };
        let granted = c.handle(&agent("dep"), held, NOW);
        assert!(is_granted(&granted));
        open_for(&mut c, 1, vec![Criterion::FirstSubmitted]);
        let one = join(&mut c, "a1", RaceId(1));
        let entry_reply = submit_effects(&mut c, "a1", one, true, NOW + 1);
        assert!(
            notices(&entry_reply, "dep").is_empty(),
            "an entry challenges nothing until it wins"
        );
        let effects = run_trials(&mut c, NOW + 2, &[("a1", passed())]);
        assert!(notices(&effects, "dep")
            .iter()
            .any(|msg| matches!(msg, ServerMsg::AssumptionChallenged { .. })));
        assert!(notices(&effects, "a1").iter().any(|msg| matches!(msg,
            ServerMsg::ReviewRequired { reasons, .. }
                if reasons.iter().any(|r| matches!(r, crate::protocol::ReviewReason::ThreatensAssumptions { count: 1 })))));
    }
    // ---- a random script of race operations ----

    #[derive(Debug, Clone)]
    enum Op {
        Open(Vec<Criterion>),
        Join(usize),
        Submit(usize, bool),
        Release(usize),
        Outsider(usize, bool),
        Trial(bool),
        Merge,
        Pick(usize),
        Tick(u64),
    }

    fn criteria() -> impl Strategy<Value = Vec<Criterion>> {
        proptest::collection::vec(
            prop_oneof![
                Just(Criterion::TestsPass),
                Just(Criterion::FirstSubmitted),
                Just(Criterion::HumanPick),
            ],
            1..4,
        )
    }

    fn ops() -> impl Strategy<Value = Vec<Op>> {
        let op = prop_oneof![
            criteria().prop_map(Op::Open),
            (0..4usize).prop_map(Op::Join),
            (0..4usize, any::<bool>()).prop_map(|(who, evidence)| Op::Submit(who, evidence)),
            (0..4usize).prop_map(Op::Release),
            (0..4usize, any::<bool>()).prop_map(|(who, wait)| Op::Outsider(who, wait)),
            any::<bool>().prop_map(Op::Trial),
            Just(Op::Merge),
            (0..4usize).prop_map(Op::Pick),
            (1..40_000u64).prop_map(Op::Tick),
        ];
        proptest::collection::vec(op, 1..40)
    }

    fn name(who: usize) -> String {
        format!("a{who}")
    }

    /// The agent's unsubmitted-or-not claim, if it has one.
    fn claim_of(c: &Coordinator, who: &str) -> Option<(ClaimId, Fence)> {
        c.state
            .claims
            .iter()
            .find(|(_, held)| held.agent == agent(who))
            .map(|(id, held)| (ClaimId(*id), held.fence))
    }

    fn step(c: &mut Coordinator, now: &mut u64, op: Op) {
        match op {
            Op::Open(criteria) => {
                let msg = open_msg(vec![edit("src/a.rs")], 3, *now + 20_000, criteria);
                c.handle(&agent("felix"), msg, *now);
            }
            Op::Join(who) => {
                let msg = ClientMsg::JoinRace {
                    req: RequestId(2),
                    race: RaceId(c.state.next_race.saturating_sub(1).max(1)),
                };
                c.handle(&agent(&name(who)), msg, *now);
            }
            Op::Submit(who, evidence) => {
                if let Some(entry) = claim_of(c, &name(who)) {
                    submit_effects(c, &name(who), entry, evidence, *now);
                }
            }
            Op::Release(who) => {
                if let Some((claim, fence)) = claim_of(c, &name(who)) {
                    let msg = ClientMsg::Release {
                        claim,
                        fence,
                        req: None,
                    };
                    c.handle(&agent(&name(who)), msg, *now);
                }
            }
            Op::Outsider(who, wait) => {
                let mode = if wait {
                    OnConflict::Wait
                } else {
                    OnConflict::Fail
                };
                claim_as(c, &name(who), "src/a.rs", mode);
            }
            Op::Trial(pass) => {
                let report = if pass { passed() } else { failed() };
                if let Some(dispatch) = c.begin_verification(*now) {
                    c.verification_outcome(dispatch.id, &report, *now);
                }
            }
            Op::Merge => {
                if let Some(dispatch) = c.begin_merge(*now) {
                    let landed = MergeOutcome::Merged {
                        base: CommitId(MAIN.into()),
                        head: CommitId(MAIN.into()),
                    };
                    c.merge_outcome(dispatch.claim, &landed, *now);
                }
            }
            Op::Pick(who) => {
                let race = RaceId(c.state.next_race.saturating_sub(1).max(1));
                let claim = c
                    .state
                    .claims
                    .keys()
                    .nth(who)
                    .map_or(ClaimId(1), |id| ClaimId(*id));
                let msg = ClientMsg::PickWinner {
                    req: RequestId(8),
                    race,
                    claim,
                };
                c.handle(&agent("felix"), msg, *now);
            }
            Op::Tick(ms) => {
                *now += ms;
                c.expire(*now);
            }
        }
        *now += 1;
    }

    fn lock_dump(locks: &LockTable) -> Vec<String> {
        let mut dump = Vec::new();
        for (node, holders) in locks {
            for h in holders {
                dump.push(format!(
                    "{node:?} {} {:?} {:?} {:?}",
                    h.claim.0, h.agent, h.lock, h.race
                ));
            }
        }
        dump.sort();
        dump
    }

    /// What must hold after every operation, however the script went.
    fn assert_race_invariants(c: &Coordinator) {
        let rebuilt = Coordinator::from(c.state.clone());
        assert_eq!(
            lock_dump(&c.locks),
            lock_dump(&rebuilt.locks),
            "locks follow the state"
        );
        let mut real_holders: Vec<&AgentId> = Vec::new();
        for (id, held) in &c.state.claims {
            match held.kind {
                ClaimKind::Entry(race) => {
                    let listed = c.state.races.get(&race.0).is_some_and(|r| {
                        r.entrants
                            .iter()
                            .any(|e| e.claim.0 == *id && e.agent == held.agent)
                    });
                    assert!(listed, "entry {id} belongs to a live race that lists it");
                    assert!(held.work.is_none(), "an entry is never dispatched");
                }
                ClaimKind::Real if !real_holders.contains(&&held.agent) => {
                    real_holders.push(&held.agent);
                }
                ClaimKind::Real => {}
                ClaimKind::Shadow => {}
            }
        }
        assert!(
            real_holders.len() <= 1,
            "two agents hold src/a.rs: {real_holders:?}"
        );
        if !c.state.races.is_empty() {
            assert!(
                real_holders.is_empty(),
                "a live race and a real claim share src/a.rs"
            );
        }
        for race in c.state.races.values() {
            assert!(race.entrants.len() <= usize::try_from(MAX_ENTRANTS).unwrap());
            for entrant in &race.entrants {
                let alive = c.state.claims.contains_key(&entrant.claim.0);
                match race.phase {
                    Phase::Open => assert!(alive, "an open race lists a dead claim: {entrant:?}"),
                    Phase::Judging | Phase::AwaitingPick { .. } => assert!(
                        alive || entrant.entered.is_none(),
                        "a submitted entry lost its claim: {entrant:?}"
                    ),
                }
            }
        }
    }

    proptest! {
        #[test]
        fn random_race_scripts_keep_the_state_consistent_and_restorable(script in ops()) {
            let mut c = core();
            let mut now = NOW;
            for op in script {
                c = serde_json::from_str(&state(&c)).unwrap();
                step(&mut c, &mut now, op);
                assert_race_invariants(&c);
            }
        }
    }
}

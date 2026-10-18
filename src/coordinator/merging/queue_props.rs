//! Property tests for the merge queue: random interleavings of claims, submits, reviews, races,
//! lease expiry, dispatches, steward answers and restarts, with the queue's documented rules
//! checked after every step. The model below is deliberately independent of `Submission`: it counts
//! answers itself and only reads the core's state to find out which claims are live.
//!
//! The case count is proptest's default (256), so `PROPTEST_CASES=10000` raises it.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;

use crate::coordinator::{Config, Coordinator, Effect};
use crate::merge::{
    infra_backoff_ms, MergeOutcome, StepExit, TrialOutcome, TrialReport, MAX_INFRA_RETRIES,
    MAX_MAIN_MOVED, MERGE_WATCHDOG_MS,
};
use crate::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, Criterion, DecisionRecord, EventKind, Intent, Mode,
    OnConflict, RaceId, RequestId, RunId, Scope, ScopeClaim, ServerMsg,
};

const START: u64 = 1_000;
const LEASE: u64 = 30_000;
const MAIN: &str = "dddddddddddddddddddddddddddddddddddddddd";
const AGENTS: usize = 4;
/// Four files and the directory above them, so some scopes conflict and some do not.
const TARGETS: usize = 5;
const UNKNOWN_CLAIM: u64 = 999_999;

fn agent(name: &str) -> AgentId {
    AgentId(name.into())
}

fn who(index: usize) -> AgentId {
    agent(&format!("a{}", index % AGENTS))
}

fn scope_of(target: usize, mode: Mode) -> ScopeClaim {
    let scope = match target % TARGETS {
        4 => Scope::Dir { path: "src".into() },
        n => Scope::File {
            path: format!("src/p{n}.rs"),
        },
    };
    ScopeClaim { scope, mode }
}

fn fork_sha(agent: &AgentId) -> String {
    let seed = agent.0.bytes().fold(0u8, |sum, b| sum.wrapping_add(b));
    format!("{seed:02x}").repeat(20)
}

/// What the steward can answer, as the test generates it.
#[derive(Debug, Clone, Copy)]
enum Answer {
    Merged,
    AlreadyMerged,
    Conflict,
    TestsFailed,
    Uncovered,
    GateChanged,
    GateInvalid,
    CommitNotInFork,
    Refused,
    MainMoved,
    Clone,
    GitFailed,
    Install,
    Timeout,
    PushFailed,
    ServiceUnavailable,
}

/// What an answer means for the claim, written from the documented rules: `merged` lands it,
/// verified rejections send it back, `main_moved` retries at once, anything that did not finish
/// is an infrastructure failure (`src/merge.rs`, `MergeOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Landed,
    Rejected,
    Moved,
    Infra,
}

impl Answer {
    fn class(self) -> Class {
        match self {
            Answer::Merged | Answer::AlreadyMerged => Class::Landed,
            Answer::Conflict
            | Answer::TestsFailed
            | Answer::Uncovered
            | Answer::GateChanged
            | Answer::GateInvalid
            | Answer::CommitNotInFork
            | Answer::Refused => Class::Rejected,
            Answer::MainMoved => Class::Moved,
            Answer::Clone
            | Answer::GitFailed
            | Answer::Install
            | Answer::Timeout
            | Answer::PushFailed
            | Answer::ServiceUnavailable => Class::Infra,
        }
    }

    fn outcome(self) -> MergeOutcome {
        match self {
            Answer::Merged => MergeOutcome::Merged {
                base: CommitId("old".into()),
                head: CommitId(MAIN.into()),
            },
            Answer::AlreadyMerged => MergeOutcome::AlreadyMerged {
                base: CommitId(MAIN.into()),
            },
            Answer::Conflict => MergeOutcome::Conflict {
                files: vec!["a.rs".into()],
            },
            Answer::TestsFailed => MergeOutcome::TestsFailed {
                result: StepExit {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            },
            Answer::Uncovered => MergeOutcome::Uncovered { total: 1 },
            Answer::GateChanged => MergeOutcome::GateChanged {},
            Answer::GateInvalid => MergeOutcome::GateInvalid {},
            Answer::CommitNotInFork => MergeOutcome::CommitNotInFork {},
            Answer::Refused => MergeOutcome::Refused,
            Answer::MainMoved => MergeOutcome::MainMoved {},
            Answer::Clone => MergeOutcome::Clone {},
            Answer::GitFailed => MergeOutcome::GitFailed {},
            Answer::Install => MergeOutcome::Install {},
            Answer::Timeout => MergeOutcome::Timeout {},
            Answer::PushFailed => MergeOutcome::PushFailed {},
            Answer::ServiceUnavailable => MergeOutcome::ServiceUnavailable,
        }
    }
}

fn answers() -> impl Strategy<Value = Answer> {
    prop_oneof![
        3 => Just(Answer::Merged),
        1 => Just(Answer::AlreadyMerged),
        1 => Just(Answer::Conflict),
        1 => Just(Answer::TestsFailed),
        1 => Just(Answer::Uncovered),
        1 => Just(Answer::GateChanged),
        1 => Just(Answer::GateInvalid),
        1 => Just(Answer::CommitNotInFork),
        1 => Just(Answer::Refused),
        8 => Just(Answer::MainMoved),
        2 => Just(Answer::Clone),
        2 => Just(Answer::GitFailed),
        2 => Just(Answer::Install),
        2 => Just(Answer::Timeout),
        2 => Just(Answer::PushFailed),
        3 => Just(Answer::ServiceUnavailable),
    ]
}

fn retryable() -> impl Strategy<Value = Answer> {
    prop_oneof![
        Just(Answer::MainMoved),
        Just(Answer::ServiceUnavailable),
        Just(Answer::Timeout),
    ]
}

#[derive(Debug, Clone)]
enum Op {
    Claim {
        who: usize,
        target: usize,
        wait: bool,
        signature: bool,
    },
    Heartbeat(usize),
    Tick(u64),
    Submit {
        who: usize,
        evidence: bool,
    },
    Release(usize),
    Review {
        approve: bool,
        pick: usize,
    },
    OpenRace,
    JoinRace(usize),
    Trial(bool),
    PickWinner(usize),
    Begin,
    /// An answer for `target`: 0 is the merge in flight, any other value a claim seen before
    /// (merged, queued, rejected, released) or one that never existed.
    Answer {
        answer: Answer,
        target: usize,
    },
    /// Wait out any backoff, dispatch the next merge and answer it.
    Cycle {
        answer: Answer,
    },
    /// A whole race from opening to the winner entering the queue, with `entrants` agents.
    Race {
        entrants: usize,
        evidence: bool,
    },
    /// Approve a held submission, then ask for the next merge: a held claim submitted before the
    /// one in flight is now the head of the queue.
    ApproveThenBegin(usize),
    /// The same non-final answer `times` in a row, to reach the retry bounds.
    Repeat {
        answer: Answer,
        times: usize,
    },
    /// The process died with a merge in flight and came back.
    Restart,
    /// The instance hung: the watchdog fires `MERGE_WATCHDOG_MS` after the dispatch.
    Watchdog,
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    let op = prop_oneof![
        6 => (0..AGENTS, 0..TARGETS, any::<bool>(), any::<bool>()).prop_map(
            |(who, target, wait, signature)| Op::Claim { who, target, wait, signature }
        ),
        2 => (0..AGENTS).prop_map(Op::Heartbeat),
        3 => (1..90_000u64).prop_map(Op::Tick),
        8 => (0..AGENTS, any::<bool>()).prop_map(|(who, evidence)| Op::Submit { who, evidence }),
        1 => (0..AGENTS).prop_map(Op::Release),
        5 => (any::<bool>(), 0..8usize).prop_map(|(approve, pick)| Op::Review { approve, pick }),
        1 => Just(Op::OpenRace),
        1 => (0..AGENTS).prop_map(Op::JoinRace),
        1 => any::<bool>().prop_map(Op::Trial),
        1 => (0..8usize).prop_map(Op::PickWinner),
        10 => Just(Op::Begin),
        6 => (answers(), 0..10usize).prop_map(|(answer, target)| Op::Answer { answer, target }),
        5 => (1..4usize, any::<bool>())
            .prop_map(|(entrants, evidence)| Op::Race { entrants, evidence }),
        3 => (0..8usize).prop_map(Op::ApproveThenBegin),
        5 => answers().prop_map(|answer| Op::Cycle { answer }),
        3 => (retryable(), 1..8usize).prop_map(|(answer, times)| Op::Repeat { answer, times }),
        1 => Just(Op::Restart),
        1 => Just(Op::Watchdog),
    ];
    proptest::collection::vec(op, 1..80)
}

/// What the test tracks on its own, never read back from the core's counters.
#[derive(Debug, Clone, Default)]
struct Model {
    /// Claims whose submission was held and not yet decided.
    held: BTreeSet<u64>,
    /// Every claim a `Merged` was ever sent for.
    merged: BTreeSet<u64>,
    /// `main_moved` answers since the claim was last submitted.
    moved: BTreeMap<u64, u32>,
    /// Infrastructure failures since the claim was last submitted.
    infra: BTreeMap<u64, u32>,
    /// Every claim id seen, for stale answers.
    seen: Vec<u64>,
    /// The merge the test believes is in flight, with its attempt and dispatch time.
    inflight: Option<(u64, u32, u64)>,
}

#[derive(Debug, Clone)]
struct World {
    c: Coordinator,
    now: u64,
    m: Model,
    /// Debug text of every effect since the step began, to compare a restored core with the original.
    trace: String,
}

fn new_world() -> World {
    let mut c = Coordinator::new(Config {
        run: RunId("props".into()),
        lease_ms: LEASE,
        shadow_enabled: true,
    })
    .unwrap();
    c.set_reviewers(vec![agent("felix")]);
    World {
        c,
        now: START,
        m: Model::default(),
        trace: String::new(),
    }
}

fn save(c: &Coordinator) -> String {
    serde_json::to_string(c).unwrap()
}

fn load(saved: &str) -> Coordinator {
    serde_json::from_str(saved).unwrap()
}

fn kinds(effects: &[Effect]) -> Vec<&EventKind> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Log(event) => Some(&event.kind),
            Effect::Reply(_) | Effect::Notify { .. } => None,
        })
        .collect()
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

impl World {
    /// The claim's submission, if it is submitted with work to merge.
    fn live(&self, claim: u64) -> bool {
        self.c
            .state
            .claims
            .get(&claim)
            .is_some_and(|held| held.submitted.is_some() && held.work.is_some())
    }

    fn handle(&mut self, who: &AgentId, msg: ClientMsg) -> Vec<Effect> {
        self.c.handle(who, msg, self.now)
    }

    /// The first claim of `who` that has not been submitted.
    fn open_claim(&self, who: &AgentId) -> Option<(ClaimId, crate::protocol::Fence)> {
        self.c
            .state
            .claims
            .iter()
            .find(|(_, held)| held.agent == *who && held.submitted.is_none())
            .map(|(id, held)| (ClaimId(*id), held.fence))
    }

    /// The submitted, not held claim with the lowest submission ordinal: the one merge order
    /// ("one merge is in flight per repo, taken in submission order") dispatches next.
    fn head_of_queue(&self) -> Option<u64> {
        let mut best: Option<(u64, u64)> = None;
        for (id, held) in &self.c.state.claims {
            let (Some(ordinal), Some(work)) = (held.submitted, held.work.as_ref()) else {
                continue;
            };
            if work.awaiting_review {
                continue;
            }
            if best.is_none_or(|(low, _)| ordinal < low) {
                best = Some((ordinal, *id));
            }
        }
        best.map(|(_, id)| id)
    }
}

fn apply(w: &mut World, op: &Op) {
    w.now += 1;
    let effects = match op {
        Op::Claim {
            who: index,
            target,
            wait,
            signature,
        } => {
            let mode = if *signature {
                Mode::EditSignature
            } else {
                Mode::EditBody
            };
            let msg = ClientMsg::Claim {
                req: RequestId(1),
                intent: Intent {
                    summary: "work".into(),
                    task_ref: None,
                    assumptions: Vec::new(),
                },
                scopes: vec![scope_of(*target, mode)],
                on_conflict: if *wait {
                    OnConflict::Wait
                } else {
                    OnConflict::Fail
                },
            };
            w.handle(&who(*index), msg)
        }
        Op::Heartbeat(index) => w.handle(&who(*index), ClientMsg::Heartbeat),
        Op::Tick(ms) => {
            w.now += ms;
            w.c.expire(w.now)
        }
        Op::Submit {
            who: index,
            evidence,
        } => submit(w, &who(*index), *evidence),
        Op::Release(index) => release(w, &who(*index)),
        Op::Review { approve, pick } => review(w, *approve, *pick),
        Op::OpenRace => {
            let msg = ClientMsg::OpenRace {
                req: RequestId(5),
                intent: Intent {
                    summary: "race".into(),
                    task_ref: None,
                    assumptions: Vec::new(),
                },
                scopes: vec![scope_of(0, Mode::EditBody)],
                max_entrants: 3,
                deadline_ms: w.now + 20_000,
                criteria: vec![Criterion::TestsPass, Criterion::FirstSubmitted],
            };
            w.handle(&agent("felix"), msg)
        }
        Op::JoinRace(index) => {
            let race = RaceId(w.c.state.next_race.saturating_sub(1).max(1));
            let msg = ClientMsg::JoinRace {
                req: RequestId(2),
                race,
            };
            w.handle(&who(*index), msg)
        }
        Op::Trial(pass) => trial(w, *pass),
        Op::PickWinner(pick) => pick_winner(w, *pick),
        Op::Begin => {
            begin_checked(w);
            Vec::new()
        }
        Op::Answer { answer, target } => {
            let claim = target_claim(w, *target);
            answer_checked(w, claim, *answer)
        }
        Op::Cycle { answer } => {
            cycle(w, *answer);
            Vec::new()
        }
        Op::Race { entrants, evidence } => {
            apply(w, &Op::OpenRace);
            for index in 0..*entrants {
                apply(w, &Op::JoinRace(index));
                apply(
                    w,
                    &Op::Submit {
                        who: index,
                        evidence: *evidence,
                    },
                );
            }
            for _ in 0..*entrants {
                apply(w, &Op::Trial(true));
            }
            apply(w, &Op::Tick(25_000));
            Vec::new()
        }
        Op::ApproveThenBegin(pick) => {
            let effects = review(w, true, *pick);
            emit(w, effects);
            begin_checked(w);
            Vec::new()
        }
        Op::Repeat { answer, times } => {
            for _ in 0..*times {
                cycle(w, *answer);
            }
            Vec::new()
        }
        Op::Restart => recover_checked(w),
        Op::Watchdog => {
            if let Some((_, _, dispatched)) = w.m.inflight {
                w.now = w.now.max(dispatched + MERGE_WATCHDOG_MS);
            }
            recover_checked(w)
        }
    };
    emit(w, effects);
}

fn submit(w: &mut World, who: &AgentId, evidence: bool) -> Vec<Effect> {
    let Some((claim, fence)) = w.open_claim(who) else {
        return Vec::new();
    };
    let touched = w.c.state.claims[&claim.0].scopes.clone();
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
        claim,
        fence,
        fork_commit: CommitId(fork_sha(who)),
        touched,
        decisions,
    };
    w.handle(who, msg)
}

fn release(w: &mut World, who: &AgentId) -> Vec<Effect> {
    let found =
        w.c.state
            .claims
            .iter()
            .find(|(_, held)| held.agent == *who)
            .map(|(id, held)| (ClaimId(*id), held.fence));
    let Some((claim, fence)) = found else {
        return Vec::new();
    };
    let msg = ClientMsg::Release {
        claim,
        fence,
        req: None,
    };
    w.handle(who, msg)
}

fn review(w: &mut World, approve: bool, pick: usize) -> Vec<Effect> {
    let held: Vec<u64> = w.m.held.iter().copied().collect();
    let claim = if held.is_empty() {
        w.m.seen.get(pick).copied().unwrap_or(UNKNOWN_CLAIM)
    } else {
        held[pick % held.len()]
    };
    let msg = ClientMsg::Review {
        req: RequestId(7),
        claim: ClaimId(claim),
        approve,
        note: None,
    };
    w.handle(&agent("felix"), msg)
}

fn trial(w: &mut World, pass: bool) -> Vec<Effect> {
    let report = if pass {
        TrialReport {
            before: Some(TrialOutcome::Clean {}),
            after: Some(TrialOutcome::Clean {}),
        }
    } else {
        TrialReport {
            before: Some(TrialOutcome::TestsFailed {}),
            after: None,
        }
    };
    match w.c.begin_verification(w.now) {
        Some(dispatch) => w.c.verification_outcome(dispatch.id, &report, w.now),
        None => Vec::new(),
    }
}

fn pick_winner(w: &mut World, pick: usize) -> Vec<Effect> {
    let race = RaceId(w.c.state.next_race.saturating_sub(1).max(1));
    let claim =
        w.c.state
            .claims
            .keys()
            .nth(pick)
            .map_or(ClaimId(1), |id| ClaimId(*id));
    let msg = ClientMsg::PickWinner {
        req: RequestId(8),
        race,
        claim,
    };
    w.handle(&agent("felix"), msg)
}

fn target_claim(w: &World, target: usize) -> u64 {
    if target == 0 {
        return w.m.inflight.map_or(UNKNOWN_CLAIM, |(id, _, _)| id);
    }
    w.m.seen
        .get((target - 1) % w.m.seen.len().max(1))
        .copied()
        .unwrap_or(UNKNOWN_CLAIM)
}

/// `begin_merge`, with the rules about what it may return checked.
fn begin_checked(w: &mut World) {
    let was_live = w.m.inflight.filter(|(id, _, _)| w.live(*id));
    let before_ordinal_head = w.head_of_queue();
    let dispatch = w.c.begin_merge(w.now);
    let Some(dispatch) = dispatch else {
        // Documented: `None` when nothing is ready. An in-flight merge is always returned.
        assert!(
            was_live.is_none(),
            "begin_merge dropped the merge in flight"
        );
        w.m.inflight = None;
        return;
    };
    let id = dispatch.claim.0;
    assert!(w.c.has_merge_in_flight());
    let again = w.c.clone().begin_merge(w.now);
    assert_eq!(
        again.as_ref().map(|d| (d.claim, d.attempt)),
        Some((dispatch.claim, dispatch.attempt)),
        "check 3: a second begin_merge returned something other than the merge in flight"
    );
    if let Some((live, attempt, _)) = was_live {
        assert_eq!(id, live, "check 3: begin_merge started a second merge");
        assert_eq!(
            dispatch.attempt, attempt,
            "the in-flight attempt is unchanged"
        );
        return;
    }
    let work = w.c.state.claims[&id].work.as_ref().unwrap();
    assert!(
        !w.m.held.contains(&id) && !work.awaiting_review,
        "check 2: a submission held for review was dispatched"
    );
    assert_eq!(
        Some(id),
        before_ordinal_head,
        "merges are taken in submission order, held claims skipped"
    );
    assert!(
        work.retry_at_ms.is_none_or(|due| due <= w.c.state.clock_ms),
        "a claim in backoff was dispatched"
    );
    let expected =
        w.m.moved.get(&id).copied().unwrap_or(0) + w.m.infra.get(&id).copied().unwrap_or(0) + 1;
    assert_eq!(
        dispatch.attempt, expected,
        "check 4: attempt counts every answer so far"
    );
    // Documented on `next_merge_ms`: the watchdog is `MERGE_WATCHDOG_MS` after the call starts.
    assert_eq!(
        w.c.next_merge_ms(true, w.now),
        Some(w.now + MERGE_WATCHDOG_MS)
    );
    assert_eq!(w.c.next_merge_ms(false, w.now), Some(w.now));
    w.m.inflight = Some((id, dispatch.attempt, w.now));
}

/// Apply the steward's answer for `claim` through `merge_outcome` and check what it did.
fn answer_checked(w: &mut World, claim: u64, answer: Answer) -> Vec<Effect> {
    let current = w.m.inflight.is_some_and(|(id, _, _)| id == claim);
    if !current {
        // Documented: an answer for any other claim is stale and changes nothing.
        w.c.expire(w.now);
        let before = save(&w.c);
        let effects = w.c.merge_outcome(ClaimId(claim), &answer.outcome(), w.now);
        assert!(
            effects.is_empty(),
            "a stale answer had effects: {effects:?}"
        );
        assert_eq!(before, save(&w.c), "a stale answer changed the state");
        return effects;
    }
    let was_live = w.live(claim);
    let effects = w.c.merge_outcome(ClaimId(claim), &answer.outcome(), w.now);
    w.m.inflight = None;
    assert!(
        !w.c.has_merge_in_flight(),
        "the answer left a marker behind"
    );
    if !was_live {
        assert!(
            effects.is_empty(),
            "an answer for a vanished claim had effects"
        );
        return effects;
    }
    expect_class(w, claim, answer.class(), &effects);
    effects
}

/// What `class` must have done to a live claim.
fn expect_class(w: &mut World, claim: u64, class: Class, effects: &[Effect]) {
    let merged_events = kinds(effects)
        .into_iter()
        .filter(|kind| matches!(kind, EventKind::Merged { claim: c, .. } if c.0 == claim))
        .count();
    let rejected_events = kinds(effects)
        .into_iter()
        .filter(|kind| matches!(kind, EventKind::SubmitRejected { claim: c, .. } if c.0 == claim))
        .count();
    match class {
        Class::Landed => {
            assert_eq!(merged_events, 1, "check 1: one Merged for a landed claim");
            assert!(
                !w.c.state.claims.contains_key(&claim),
                "a merged claim is released"
            );
        }
        Class::Rejected => {
            assert_eq!(rejected_events, 1);
            assert_reopened(w, claim);
        }
        Class::Moved => {
            let moved = w.m.moved.entry(claim).or_insert(0);
            *moved += 1;
            let moved = *moved;
            assert_eq!(merged_events, 0);
            // `MAX_MAIN_MOVED` answers reject the claim, never fewer.
            if moved >= MAX_MAIN_MOVED {
                assert_eq!(
                    rejected_events, 1,
                    "check 4: rejected at {moved} main-moved"
                );
                assert_reopened(w, claim);
            } else {
                assert_eq!(rejected_events, 0, "check 4: rejected after only {moved}");
                assert!(
                    w.live(claim),
                    "check 4: still queued after {moved} main-moved"
                );
            }
        }
        Class::Infra => {
            let failures = w.m.infra.entry(claim).or_insert(0);
            *failures += 1;
            let failures = *failures;
            assert_eq!(merged_events, 0);
            // `MAX_INFRA_RETRIES` retries are allowed: the failure after them rejects.
            if failures > MAX_INFRA_RETRIES {
                assert_eq!(
                    rejected_events, 1,
                    "check 4: rejected at failure {failures}"
                );
                assert_reopened(w, claim);
            } else {
                assert_eq!(
                    rejected_events, 0,
                    "check 4: rejected at failure {failures}"
                );
                let due = w.c.state.claims[&claim].work.as_ref().unwrap().retry_at_ms;
                assert_eq!(
                    due,
                    Some(w.c.state.clock_ms + infra_backoff_ms(failures)),
                    "check 4: backoff after failure {failures}"
                );
            }
        }
    }
}

/// A rejected claim is active again: unsubmitted, nothing to merge, still holding its scopes.
fn assert_reopened(w: &World, claim: u64) {
    let held =
        w.c.state
            .claims
            .get(&claim)
            .expect("a rejected claim stays");
    assert!(held.submitted.is_none() && held.work.is_none());
}

/// The process died with a merge in flight. `recover_merge` counts it as an infrastructure failure.
fn recover_checked(w: &mut World) -> Vec<Effect> {
    let Some((claim, _, _)) = w.m.inflight else {
        let effects = w.c.recover_merge(w.now);
        assert!(effects.is_empty() && !w.c.has_merge_in_flight());
        return effects;
    };
    let was_live = w.live(claim);
    let effects = w.c.recover_merge(w.now);
    w.m.inflight = None;
    assert!(!w.c.has_merge_in_flight());
    if was_live {
        expect_class(w, claim, Class::Infra, &effects);
    }
    effects
}

/// Wait out the head's backoff, dispatch, answer.
fn cycle(w: &mut World, answer: Answer) {
    if w.m.inflight.is_none() {
        if let Some(due) = w.c.next_merge_ms(false, w.now) {
            w.now = w.now.max(due);
            w.c.expire(w.now);
        }
    }
    begin_checked(w);
    if let Some((claim, _, _)) = w.m.inflight {
        let effects = answer_checked(w, claim, answer);
        emit(w, effects);
    }
}

/// Record what the core just did in the model and in the trace compared with the restored core.
fn emit(w: &mut World, effects: Vec<Effect>) {
    observe(w, &effects);
    w.trace.push_str(&format!("{effects:?}"));
}

/// Update the model from what the core just did, and check that no claim merges twice.
fn observe(w: &mut World, effects: &[Effect]) {
    for kind in kinds(effects) {
        if let EventKind::Merged { claim, .. } = kind {
            assert!(
                w.m.merged.insert(claim.0),
                "check 1: claim {} was merged twice",
                claim.0
            );
        }
        if let EventKind::ReviewDecided { claim, .. } | EventKind::SubmitRejected { claim, .. } =
            kind
        {
            w.m.held.remove(&claim.0);
        }
    }
    for msg in replies(effects) {
        if let ServerMsg::ReviewRequired { claim, .. } = msg {
            w.m.held.insert(claim.0);
        }
    }
}

/// Checks that hold after every step, whatever the step was.
fn check_after(w: &mut World) {
    for id in w.c.state.claims.keys() {
        assert!(!w.m.merged.contains(id), "check 1: a merged claim is back");
        if !w.m.seen.contains(id) {
            w.m.seen.push(*id);
        }
    }
    let ids: Vec<u64> = w.c.state.claims.keys().copied().collect();
    w.m.held.retain(|id| ids.contains(id));
    let live: Vec<u64> = ids.into_iter().filter(|id| w.live(*id)).collect();
    w.m.moved.retain(|id, _| live.contains(id));
    w.m.infra.retain(|id, _| live.contains(id));
    assert_eq!(
        w.c.has_merge_in_flight(),
        w.m.inflight.is_some(),
        "check 3: the marker and the dispatched merge disagree"
    );
    check_no_wedge(w);
}

/// Check 6: if the queue has work, the core dispatches it or reports a wake no later than the
/// moment it becomes dispatchable. Wakes are never before `now` (`next_merge_ms`).
fn check_no_wedge(w: &World) {
    let now = w.now.max(w.c.state.clock_ms);
    if w.c.has_merge_in_flight() {
        // Not merging here: a stored marker means a restart, recovered at once.
        assert_eq!(
            w.c.next_wake_ms(false, false, now),
            Some(now),
            "check 6: marker"
        );
        // Merging here: the watchdog covers a hung instance.
        let wake = w.c.next_wake_ms(true, false, now);
        assert!(
            wake.is_some_and(|at| at <= now + MERGE_WATCHDOG_MS),
            "check 6: watchdog"
        );
        return;
    }
    let Some(head) = w.head_of_queue() else {
        return;
    };
    let work = w.c.state.claims[&head].work.as_ref().unwrap();
    let eligible_at = work.retry_at_ms.unwrap_or(0).max(now);
    let wake = w.c.next_wake_ms(false, false, now);
    assert!(
        wake.is_some_and(|at| now <= at && at <= eligible_at),
        "check 6: wake {wake:?} is after claim {head} becomes eligible at {eligible_at}"
    );
    if eligible_at == now {
        assert!(
            w.c.clone().begin_merge(now).is_some(),
            "check 6: claim {head} is eligible now and begin_merge did not dispatch it"
        );
    }
}

fn run(script: &[Op]) {
    let mut w = new_world();
    for op in script {
        // Check 5: the state survives a save and load. The restored core and the original take
        // the same step and must end identical.
        let saved = save(&w.c);
        let mut twin = World {
            c: load(&saved),
            now: w.now,
            m: w.m.clone(),
            trace: String::new(),
        };
        assert_eq!(
            saved,
            save(&twin.c),
            "check 5: save then load changed the state"
        );
        w.trace.clear();
        apply(&mut w, op);
        apply(&mut twin, op);
        assert_eq!(
            w.trace, twin.trace,
            "check 5: a restored core behaved differently"
        );
        assert_eq!(
            save(&w.c),
            save(&twin.c),
            "check 5: restored state diverged"
        );
        check_after(&mut w);
    }
}

proptest! {
    #[test]
    fn random_merge_queue_scripts_keep_the_queue_rules(script in ops()) {
        run(&script);
    }
}

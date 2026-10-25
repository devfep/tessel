//! Property tests for the merge queue: random interleavings of claims, submits, reviews, races,
//! lease expiry, dispatches, lander answers and restarts, with the queue's documented rules
//! checked after every step. The model below is deliberately independent of `Submission`: it counts
//! answers itself and only reads the core's state to find out which claims are live.
//!
//! A merge is three calls (`Prepare`, `Test`, `Land`), each with its own dispatch id. An `Answer`
//! is how an attempt ends, and `Answer::stage` is the earliest call it can come from: the model
//! answers the calls before it (`advance_to`) with `Prepared` and `Tested`, checking that each
//! moves the merge on under a new dispatch id with the head journaled, and then gives the answer.
//!
//! The case count is proptest's default (256), so `PROPTEST_CASES=10000` raises it.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use std::sync::atomic::{AtomicUsize, Ordering};

use proptest::prelude::*;
use proptest::test_runner::TestRunner;

use crate::coordinator::verifying::{Subject, Verification};
use crate::coordinator::{Config, Coordinator, Effect, MergeStage};
use crate::merge::{
    infra_backoff_ms, MergeOutcome, RuleKind, StepExit, TrialOutcome, TrialReport,
    MAX_INFRA_RETRIES, MAX_MAIN_MOVED, MERGE_WATCHDOG_MS,
};
use crate::protocol::{
    AgentId, Assumption, ClaimId, ClientMsg, CommitId, Criterion, DecisionRecord, EventKind, Fence,
    Intent, Mode, OnConflict, RaceId, RequestId, RunId, Scope, ScopeClaim, ServerMsg,
};

const START: u64 = 1_000;
const LEASE: u64 = 30_000;
const MAIN: &str = "dddddddddddddddddddddddddddddddddddddddd";
/// Main as an `already_merged` answer reports it: never equal to `MAIN`, which a merge lands at.
const LANDED: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const AGENTS: usize = 4;
/// Merges that may be dispatched in a row while a verification is ready (I12).
const SLOT_AFTER: u32 = 4;
/// Four files and the directory above them, so some scopes conflict and some do not.
const TARGETS: usize = 5;
const UNKNOWN_CLAIM: u64 = 999_999;
/// A dispatch id the core never issues in a script.
const UNISSUED_DISPATCH: u64 = 1_000_000_000;

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

/// The call of a merge an answer comes from, in the order the core makes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Prepare,
    Test,
    Land,
}

impl Stage {
    fn of(stage: &MergeStage) -> Stage {
        match stage {
            MergeStage::Prepare => Stage::Prepare,
            MergeStage::Test { .. } => Stage::Test,
            MergeStage::Land { .. } => Stage::Land,
        }
    }
}

/// What the lander can answer, as the test generates it.
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
    /// The `Prepare` did not finish.
    UnavailableAtPrepare,
    /// The candidate was gone when the tests were to run.
    CandidateMissingAtTest,
    /// The candidate was gone, and not on main, when the landing was to run.
    CandidateMissingAtLand,
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
    /// The earliest call of a merge this answer comes from. The tests (`TestsFailed`, `Install`,
    /// `Timeout`) are run by `Test`; only a `Land` can leave main changed, so only an attempt that
    /// ends there can be in doubt.
    fn stage(self) -> Stage {
        match self {
            Answer::AlreadyMerged
            | Answer::Conflict
            | Answer::Uncovered
            | Answer::GateChanged
            | Answer::GateInvalid
            | Answer::CommitNotInFork
            | Answer::Refused
            | Answer::Clone
            | Answer::GitFailed
            | Answer::UnavailableAtPrepare => Stage::Prepare,
            Answer::TestsFailed
            | Answer::Install
            | Answer::Timeout
            | Answer::CandidateMissingAtTest => Stage::Test,
            Answer::Merged
            | Answer::MainMoved
            | Answer::PushFailed
            | Answer::ServiceUnavailable
            | Answer::CandidateMissingAtLand => Stage::Land,
        }
    }

    /// Whether the answer says the candidate is gone: an infrastructure failure that cannot have
    /// landed.
    fn candidate_missing(self) -> bool {
        matches!(
            self,
            Answer::CandidateMissingAtTest | Answer::CandidateMissingAtLand
        )
    }

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
            | Answer::ServiceUnavailable
            | Answer::UnavailableAtPrepare
            | Answer::CandidateMissingAtTest
            | Answer::CandidateMissingAtLand => Class::Infra,
        }
    }

    fn outcome(self) -> MergeOutcome {
        match self {
            Answer::Merged => MergeOutcome::Merged {
                base: CommitId("old".into()),
                head: CommitId(MAIN.into()),
            },
            Answer::AlreadyMerged => MergeOutcome::AlreadyMerged {
                base: CommitId(LANDED.into()),
            },
            Answer::Conflict => MergeOutcome::Conflict {
                files: vec!["a.rs".into()],
                total: None,
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
            Answer::Refused => MergeOutcome::Refused {
                kind: RuleKind::Other,
            },
            Answer::MainMoved => MergeOutcome::MainMoved {},
            Answer::Clone => MergeOutcome::Clone {},
            Answer::GitFailed => MergeOutcome::GitFailed {},
            Answer::Install => MergeOutcome::Install {},
            Answer::Timeout => MergeOutcome::Timeout {},
            Answer::PushFailed => MergeOutcome::PushFailed {},
            Answer::ServiceUnavailable | Answer::UnavailableAtPrepare => {
                MergeOutcome::ServiceUnavailable
            }
            Answer::CandidateMissingAtTest | Answer::CandidateMissingAtLand => {
                MergeOutcome::CandidateMissing {}
            }
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
        2 => Just(Answer::UnavailableAtPrepare),
        2 => Just(Answer::CandidateMissingAtTest),
        2 => Just(Answer::CandidateMissingAtLand),
    ]
}

/// An answer that leaves the attempt in doubt: a `Land` that did not finish may have landed.
fn lost_answer() -> impl Strategy<Value = Answer> {
    prop_oneof![Just(Answer::ServiceUnavailable), Just(Answer::PushFailed)]
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
    /// A claim that shadows whatever blocks it, so a landing can owe it a trial.
    Shadow {
        who: usize,
        target: usize,
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
    /// A trial whose process died after the dispatch: the core is saved and loaded, the cut-off
    /// dispatch is recovered and tried again, and the first dispatch's late answer arrives.
    CutOffTrial(bool),
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
    /// A whole race from opening to the winner entering the queue, with `entrants` agents, then
    /// the queue is drained.
    Race {
        entrants: usize,
        evidence: bool,
    },
    /// Approve a held submission, then ask for the next merge: a held claim submitted before the
    /// one in flight is now the head of the queue.
    ApproveThenBegin(usize),
    /// A held submission and a tested one behind it: the tested one is dispatched, the held one
    /// is approved while it is in flight and must wait its turn.
    Overtake {
        first: usize,
        offset: usize,
    },
    /// Every agent submits tested work on its own file, then the queue is answered in order.
    Pipeline(Vec<Answer>),
    /// The same non-final answer `times` in a row, to reach the retry bounds.
    Repeat {
        answer: Answer,
        times: usize,
    },
    /// The queue is drained, then an agent's claim assumes the file `on` and another agent's
    /// work on it is submitted. Its first dispatch is lost (a restart, or `lost` as the answer),
    /// and the retry is answered `already_merged`: the landing must not queue a trial that
    /// compares main with itself.
    RecoveredLanding {
        who: usize,
        on: usize,
        restart: bool,
        lost: Answer,
        moved_between: bool,
    },
    /// A claim that assumes the file `on` keeps its current behaviour: when another agent's work
    /// on that file merges, a verification of this claim is queued.
    Assume {
        who: usize,
        target: usize,
        on: usize,
    },
    /// The queue is drained, then an agent's work on `on` is submitted with another agent's
    /// shadow claim on the same file submitted behind it, and the landing is answered
    /// `already_merged` at the first dispatch: the shadow claim cannot be tried and is listed.
    ShadowedLanding {
        who: usize,
        on: usize,
    },
    /// A verification that gets no answer in time: it is tried, fails on infrastructure and waits
    /// out its backoff.
    StallTrial,
    /// Continuous merge load with a verification waiting: each round is one alarm, a merge and
    /// then a verification when the core offers one. A ready verification must run within
    /// `SLOT_AFTER` merges.
    Storm {
        rounds: usize,
    },
    /// The process died with a merge in flight and came back.
    Restart,
    /// The instance hung: the watchdog fires `MERGE_WATCHDOG_MS` after the dispatch.
    Watchdog,
    /// An answer carrying a dispatch id issued earlier (a call that outlived its dispatch). It is
    /// stale whatever claim it was for, even the one in flight now, and must change nothing.
    LateAnswer {
        answer: Answer,
        pick: usize,
    },
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    let op = prop_oneof![
        6 => (0..AGENTS, 0..TARGETS, any::<bool>(), any::<bool>()).prop_map(
            |(who, target, wait, signature)| Op::Claim { who, target, wait, signature }
        ),
        3 => (0..AGENTS, 0..TARGETS).prop_map(|(who, target)| Op::Shadow { who, target }),
        2 => (0..AGENTS).prop_map(Op::Heartbeat),
        3 => (1..90_000u64).prop_map(Op::Tick),
        8 => (0..AGENTS, any::<bool>()).prop_map(|(who, evidence)| Op::Submit { who, evidence }),
        1 => (0..AGENTS).prop_map(Op::Release),
        5 => (any::<bool>(), 0..8usize).prop_map(|(approve, pick)| Op::Review { approve, pick }),
        1 => Just(Op::OpenRace),
        1 => (0..AGENTS).prop_map(Op::JoinRace),
        1 => any::<bool>().prop_map(Op::Trial),
        1 => any::<bool>().prop_map(Op::CutOffTrial),
        1 => (0..8usize).prop_map(Op::PickWinner),
        10 => Just(Op::Begin),
        6 => (answers(), 0..10usize).prop_map(|(answer, target)| Op::Answer { answer, target }),
        8 => (1..4usize, any::<bool>())
            .prop_map(|(entrants, evidence)| Op::Race { entrants, evidence }),
        3 => (0..8usize).prop_map(Op::ApproveThenBegin),
        4 => (0..AGENTS, 1..AGENTS).prop_map(|(first, offset)| Op::Overtake { first, offset }),
        4 => proptest::collection::vec(answers(), 1..6).prop_map(Op::Pipeline),
        8 => answers().prop_map(|answer| Op::Cycle { answer }),
        3 => (retryable(), 1..8usize).prop_map(|(answer, times)| Op::Repeat { answer, times }),
        3 => (0..AGENTS, 0..TARGETS - 1, 0..TARGETS - 1)
            .prop_map(|(who, target, on)| Op::Assume { who, target, on }),
        4 => (0..AGENTS, 0..TARGETS - 1, any::<bool>(), lost_answer(), any::<bool>()).prop_map(
            |(who, on, restart, lost, moved_between)| Op::RecoveredLanding {
                who, on, restart, lost, moved_between
            }
        ),
        2 => (0..AGENTS, 0..TARGETS - 1).prop_map(|(who, on)| Op::ShadowedLanding { who, on }),
        2 => Just(Op::StallTrial),
        3 => (SLOT_AFTER as usize..3 * SLOT_AFTER as usize)
            .prop_map(|rounds| Op::Storm { rounds }),
        1 => Just(Op::Restart),
        1 => Just(Op::Watchdog),
        6 => (answers(), 0..16usize).prop_map(|(answer, pick)| Op::LateAnswer { answer, pick }),
    ];
    proptest::collection::vec(op, 1..40)
}

/// The merge the model believes is in flight.
#[derive(Debug, Clone, Copy)]
struct Flight {
    /// The dispatch of the call that is out: it changes with each stage.
    id: u64,
    stage: Stage,
    claim: u64,
    attempt: u32,
    dispatched_ms: u64,
}

/// What the test tracks on its own, never read back from the core's counters or ordinals.
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
    /// Claims in the order their submissions were accepted: "taken in submission order". An
    /// approval does not move a claim, a new submission goes to the end.
    order: Vec<u64>,
    inflight: Option<Flight>,
    /// Claims whose dispatch was answered with an infrastructure failure or lost: the lander may
    /// have landed them. They are dispatched before any other claim, held-and-approved ones
    /// included, until they land or are rejected.
    doubt: BTreeSet<u64>,
    /// Every dispatch id the core issued (merge and verification), oldest first, with the claim
    /// of a merge dispatch. Ids are unique and only grow, across restarts too.
    issued: Vec<(Option<u64>, u64)>,
    /// Merges dispatched in a row while a verification was ready, counted from the test's own
    /// reading of the queue; a verification dispatch resets it (I12).
    ahead: u32,
}

impl Model {
    /// Record a dispatch the core just issued: its id is new and larger than every earlier one.
    fn issue(&mut self, claim: Option<u64>, id: u64) {
        let last = self.issued.last().map_or(0, |(_, last)| *last);
        assert!(
            id > last,
            "dispatch id {id} is not past the last one, {last}"
        );
        self.issued.push((claim, id));
    }
}

#[derive(Debug, Clone)]
struct World {
    core: Coordinator,
    now: u64,
    model: Model,
    /// Debug text of every effect and dispatch since the step began, to compare a restored core
    /// with the original.
    trace: String,
}

fn new_world() -> World {
    let mut core = Coordinator::new(Config {
        run: RunId("props".into()),
        lease_ms: LEASE,
        shadow_enabled: true,
    })
    .unwrap();
    core.set_reviewers(vec![agent("felix")]);
    core.set_verify_slot_after(SLOT_AFTER);
    World {
        core,
        now: START,
        model: Model::default(),
        trace: String::new(),
    }
}

fn save(core: &Coordinator) -> String {
    serde_json::to_string(core).unwrap()
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
        self.core
            .state
            .claims
            .get(&claim)
            .is_some_and(|held| held.submitted.is_some() && held.work.is_some())
    }

    fn handle(&mut self, who: &AgentId, msg: ClientMsg) -> Vec<Effect> {
        self.core.handle(who, msg, self.now)
    }

    /// The first claim of `who` that has not been submitted.
    fn open_claim(&self, who: &AgentId) -> Option<(ClaimId, Fence)> {
        self.core
            .state
            .claims
            .iter()
            .find(|(_, held)| held.agent == *who && held.submitted.is_none())
            .map(|(id, held)| (ClaimId(*id), held.fence))
    }

    /// Whether the verification at the head of the queue could be dispatched now: one is queued,
    /// none is in flight and the head is not in backoff.
    fn verification_ready(&self) -> bool {
        let state = &self.core.state;
        let now = self.now.max(state.clock_ms);
        state.verification_in_flight.is_none()
            && state
                .verifications
                .first()
                .is_some_and(|next| next.retry_at_ms.is_none_or(|due| due <= now))
    }

    /// I12: `SLOT_AFTER` merges went ahead of a verification that is still ready, and no merge
    /// is in flight, so the next merge must wait for the verification.
    fn slot_due(&self) -> bool {
        !self.core.has_merge_in_flight()
            && self.model.ahead >= SLOT_AFTER
            && self.verification_ready()
    }

    /// The next claim to merge by the model's own order: the earliest accepted submission that is
    /// still queued and not held for review.
    fn head_of_queue(&self) -> Option<u64> {
        let ready = |id: &u64| self.live(*id) && !self.model.held.contains(id);
        let in_doubt = |id: &u64| self.model.doubt.contains(id);
        let order = self.model.order.iter().copied();
        order
            .clone()
            .find(|id| ready(id) && in_doubt(id))
            .or_else(|| order.clone().find(ready))
    }
}

fn apply(world: &mut World, op: &Op) {
    world.now += 1;
    let effects = match op {
        Op::Claim {
            who: index,
            target,
            wait,
            signature,
        } => claim(world, *index, *target, *wait, *signature),
        Op::Shadow { who: index, target } => shadow(world, *index, *target),
        Op::Heartbeat(index) => world.handle(&who(*index), ClientMsg::Heartbeat),
        Op::Tick(ms) => {
            world.now += ms;
            world.core.expire(world.now)
        }
        Op::Submit {
            who: index,
            evidence,
        } => submit(world, &who(*index), *evidence),
        Op::Release(index) => release(world, &who(*index)),
        Op::Review { approve, pick } => review(world, *approve, *pick),
        Op::OpenRace => open_race(world),
        Op::JoinRace(index) => join_race(world, *index),
        Op::Trial(pass) => trial(world, *pass),
        Op::CutOffTrial(pass) => cut_off_trial(world, *pass),
        Op::PickWinner(pick) => pick_winner(world, *pick),
        Op::Begin => {
            begin_checked(world);
            Vec::new()
        }
        Op::Answer { answer, target } => {
            let claim = target_claim(world, *target);
            answer_checked(world, claim, *answer)
        }
        Op::Cycle { answer } => {
            cycle(world, *answer);
            Vec::new()
        }
        Op::Race { entrants, evidence } => {
            run_race(world, *entrants, *evidence);
            Vec::new()
        }
        Op::ApproveThenBegin(pick) => {
            approve_then_begin(world, *pick);
            Vec::new()
        }
        Op::Overtake { first, offset } => {
            run_overtake(world, *first, *offset);
            Vec::new()
        }
        Op::Pipeline(answers) => {
            run_pipeline(world, answers);
            Vec::new()
        }
        Op::RecoveredLanding {
            who,
            on,
            restart,
            lost,
            moved_between,
        } => {
            run_recovered_landing(world, (*who, *on), (*restart, *lost), *moved_between);
            Vec::new()
        }
        Op::ShadowedLanding { who, on } => {
            run_shadowed_landing(world, *who, *on);
            Vec::new()
        }
        Op::Repeat { answer, times } => {
            for _ in 0..*times {
                cycle(world, *answer);
            }
            Vec::new()
        }
        Op::Assume {
            who: index,
            target,
            on,
        } => assume(world, *index, *target, *on),
        Op::StallTrial => stall_trial(world),
        Op::Storm { rounds } => {
            storm(world, *rounds);
            Vec::new()
        }
        Op::Restart => recover_checked(world),
        Op::LateAnswer { answer, pick } => late_answer(world, *pick, *answer),
        Op::Watchdog => {
            if let Some(flight) = world.model.inflight {
                world.now = world.now.max(flight.dispatched_ms + MERGE_WATCHDOG_MS);
            }
            recover_checked(world)
        }
    };
    emit(world, effects);
}

fn plain_intent(summary: &str) -> Intent {
    Intent {
        summary: summary.into(),
        task_ref: None,
        assumptions: Vec::new(),
    }
}

fn claim(
    world: &mut World,
    index: usize,
    target: usize,
    wait: bool,
    signature: bool,
) -> Vec<Effect> {
    let mode = if signature {
        Mode::EditSignature
    } else {
        Mode::EditBody
    };
    let msg = ClientMsg::Claim {
        req: RequestId(1),
        intent: plain_intent("work"),
        scopes: vec![scope_of(target, mode)],
        on_conflict: if wait {
            OnConflict::Wait
        } else {
            OnConflict::Fail
        },
    };
    world.handle(&who(index), msg)
}

fn shadow(world: &mut World, index: usize, target: usize) -> Vec<Effect> {
    let msg = ClientMsg::Claim {
        req: RequestId(1),
        intent: plain_intent("shadow"),
        scopes: vec![scope_of(target, Mode::EditBody)],
        on_conflict: OnConflict::Shadow,
    };
    world.handle(&who(index), msg)
}

fn assume(world: &mut World, index: usize, target: usize, on: usize) -> Vec<Effect> {
    let intent = Intent {
        summary: "assumes".into(),
        task_ref: None,
        assumptions: vec![Assumption {
            scope: scope_of(on, Mode::Depend).scope,
            statement: "unchanged".into(),
        }],
    };
    let msg = ClientMsg::Claim {
        req: RequestId(1),
        intent,
        scopes: vec![scope_of(target, Mode::EditBody)],
        on_conflict: OnConflict::Fail,
    };
    world.handle(&who(index), msg)
}

fn open_race(world: &mut World) -> Vec<Effect> {
    let msg = ClientMsg::OpenRace {
        req: RequestId(5),
        intent: plain_intent("race"),
        scopes: vec![scope_of(0, Mode::EditBody)],
        max_entrants: 3,
        deadline_ms: world.now + 20_000,
        criteria: vec![Criterion::TestsPass, Criterion::FirstSubmitted],
    };
    world.handle(&agent("felix"), msg)
}

fn latest_race(world: &World) -> RaceId {
    RaceId(world.core.state.next_race.saturating_sub(1).max(1))
}

fn join_race(world: &mut World, index: usize) -> Vec<Effect> {
    let msg = ClientMsg::JoinRace {
        req: RequestId(2),
        race: latest_race(world),
    };
    world.handle(&who(index), msg)
}

fn submit(world: &mut World, who: &AgentId, evidence: bool) -> Vec<Effect> {
    let Some((claim, fence)) = world.open_claim(who) else {
        return Vec::new();
    };
    let touched = world.core.state.claims[&claim.0].scopes.clone();
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
    world.handle(who, msg)
}

fn release_claim(world: &mut World, claim: u64) -> Vec<Effect> {
    let Some(held) = world.core.state.claims.get(&claim) else {
        return Vec::new();
    };
    let (agent, fence) = (held.agent.clone(), held.fence);
    let msg = ClientMsg::Release {
        claim: ClaimId(claim),
        fence,
        req: None,
    };
    world.handle(&agent, msg)
}

fn release(world: &mut World, who: &AgentId) -> Vec<Effect> {
    let found = world
        .core
        .state
        .claims
        .iter()
        .find(|(_, held)| held.agent == *who)
        .map(|(id, _)| *id);
    match found {
        Some(claim) => release_claim(world, claim),
        None => Vec::new(),
    }
}

fn review(world: &mut World, approve: bool, pick: usize) -> Vec<Effect> {
    let held: Vec<u64> = world.model.held.iter().copied().collect();
    let claim = if held.is_empty() {
        world.model.seen.get(pick).copied().unwrap_or(UNKNOWN_CLAIM)
    } else {
        held[pick % held.len()]
    };
    let msg = ClientMsg::Review {
        req: RequestId(7),
        claim: ClaimId(claim),
        approve,
        note: None,
    };
    world.handle(&agent("felix"), msg)
}

fn trial_report(pass: bool) -> TrialReport {
    if pass {
        TrialReport {
            before: Some(TrialOutcome::Clean {}),
            after: Some(TrialOutcome::Clean {}),
        }
    } else {
        TrialReport {
            before: Some(TrialOutcome::TestsFailed {}),
            after: None,
        }
    }
}

/// A verification dispatched, then cut off by a restart: its ids are never reused, and the first
/// dispatch's late answer changes nothing, whether or not the trial is dispatched again.
fn cut_off_trial(world: &mut World, pass: bool) -> Vec<Effect> {
    let report = trial_report(pass);
    let Some(first) = world.core.begin_verification(world.now) else {
        return Vec::new();
    };
    world.model.ahead = 0;
    world.model.issue(None, first.id);
    world.core = load(&save(&world.core));
    let mut effects = world.core.recover_verification(world.now);
    world.now = world
        .core
        .next_verification_ms(false, world.now)
        .unwrap_or(world.now);
    let again = world.core.begin_verification(world.now);
    if let Some(second) = &again {
        world.model.issue(None, second.id);
    }
    let before = save(&world.core);
    let late = world
        .core
        .verification_outcome(first.id, &report, world.now);
    assert!(late.is_empty(), "the cut-off dispatch's answer had effects");
    assert_eq!(
        before,
        save(&world.core),
        "the cut-off dispatch's answer changed the state"
    );
    if let Some(second) = again {
        effects.extend(
            world
                .core
                .verification_outcome(second.id, &report, world.now),
        );
    }
    effects
}

fn trial(world: &mut World, pass: bool) -> Vec<Effect> {
    let report = trial_report(pass);
    match world.core.begin_verification(world.now) {
        Some(dispatch) => {
            world.model.ahead = 0;
            world.model.issue(None, dispatch.id);
            world
                .core
                .verification_outcome(dispatch.id, &report, world.now)
        }
        None => Vec::new(),
    }
}

/// A verification dispatched whose answer is an infrastructure failure: it is retried after a
/// backoff, which stays open when the step ends.
fn stall_trial(world: &mut World) -> Vec<Effect> {
    let Some(dispatch) = world.core.begin_verification(world.now) else {
        return Vec::new();
    };
    world.model.ahead = 0;
    world.model.issue(None, dispatch.id);
    let report = TrialReport::stopped(TrialOutcome::ServiceUnavailable);
    world
        .core
        .verification_outcome(dispatch.id, &report, world.now)
}

/// One alarm per round under a continuous stream of tested submissions: a merge, then a
/// verification if the core offers one. The first round also leaves a verification queued.
fn storm(world: &mut World, rounds: usize) {
    for index in 0..AGENTS * 2 {
        apply(world, &Op::Release(index));
    }
    apply(
        world,
        &Op::Assume {
            who: 0,
            target: 3,
            on: 0,
        },
    );
    for round in 0..rounds {
        let who = 1 + round % (AGENTS - 1);
        for op in [
            Op::Claim {
                who,
                target: round % 3,
                wait: false,
                signature: false,
            },
            Op::Submit {
                who,
                evidence: true,
            },
            Op::Review {
                approve: true,
                pick: 0,
            },
        ] {
            apply(world, &op);
        }
        cycle(world, Answer::Merged);
        apply(world, &Op::Trial(true));
        check_after(world);
    }
}

fn pick_winner(world: &mut World, pick: usize) -> Vec<Effect> {
    let claim = world
        .core
        .state
        .claims
        .keys()
        .nth(pick)
        .map_or(ClaimId(1), |id| ClaimId(*id));
    let msg = ClientMsg::PickWinner {
        req: RequestId(8),
        race: latest_race(world),
        claim,
    };
    world.handle(&agent("felix"), msg)
}

fn target_claim(world: &World, target: usize) -> u64 {
    if target == 0 {
        return world
            .model
            .inflight
            .map_or(UNKNOWN_CLAIM, |flight| flight.claim);
    }
    let seen = &world.model.seen;
    seen.get((target - 1) % seen.len().max(1))
        .copied()
        .unwrap_or(UNKNOWN_CLAIM)
}

/// An open race from a clean slate: every agent releases its claims, the entrants join and
/// submit, the trials pass, the deadline passes, and the queue is answered `merged` twice.
fn run_race(world: &mut World, entrants: usize, evidence: bool) {
    for index in 0..AGENTS * 2 {
        apply(world, &Op::Release(index));
    }
    apply(world, &Op::OpenRace);
    for index in 0..entrants {
        apply(world, &Op::JoinRace(index));
        apply(
            world,
            &Op::Submit {
                who: index,
                evidence,
            },
        );
    }
    for _ in 0..entrants {
        apply(world, &Op::Trial(true));
    }
    apply(world, &Op::Tick(25_000));
    for _ in 0..2 {
        cycle(world, Answer::Merged);
    }
}

fn approve_then_begin(world: &mut World, pick: usize) {
    let effects = review(world, true, pick);
    emit(world, effects);
    begin_checked(world);
}

/// Claim two files, submit the first untested (held) and the second tested, dispatch the second,
/// then approve the first while the second is in flight.
fn run_overtake(world: &mut World, first: usize, offset: usize) {
    let second = (first + offset) % AGENTS;
    for (index, target) in [(first, 1), (second, 2)] {
        apply(
            world,
            &Op::Claim {
                who: index,
                target,
                wait: false,
                signature: false,
            },
        );
    }
    apply(
        world,
        &Op::Submit {
            who: first,
            evidence: false,
        },
    );
    apply(
        world,
        &Op::Submit {
            who: second,
            evidence: true,
        },
    );
    begin_checked(world);
    approve_then_begin(world, 0);
}

/// The claim a `Granted` reply names, if the request was granted.
fn granted(effects: &[Effect]) -> Option<u64> {
    replies(effects).into_iter().find_map(|msg| {
        let ServerMsg::Granted { claim, .. } = msg else {
            return None;
        };
        Some(claim.0)
    })
}

/// A landing answered `already_merged` at its first dispatch, with a submitted shadow claim
/// behind it: check 9 requires the shadow claim to be listed untried.
fn run_shadowed_landing(world: &mut World, who: usize, on: usize) {
    for _ in 0..AGENTS * 2 {
        if world.head_of_queue().is_none() {
            break;
        }
        cycle(world, Answer::Merged);
    }
    let claimed = claim(world, who, on, false, false);
    let blocker = granted(&claimed);
    emit(world, claimed);
    let shadowed = shadow(world, who + 1, on);
    emit(world, shadowed);
    for index in [who, who + 1] {
        let submitted = submit(world, &self::who(index), true);
        emit(world, submitted);
    }
    if blocker.is_some_and(|claim| world.live(claim) && !world.model.held.contains(&claim)) {
        cycle(world, Answer::AlreadyMerged);
    }
}

/// A challenging submission whose first dispatch is lost and whose retry is answered
/// `already_merged`: the verification it queues must start from main as it was at the lost
/// dispatch and end at the lander's main, and none may compare main with itself.
fn run_recovered_landing(
    world: &mut World,
    (who, on): (usize, usize),
    (restart, lost): (bool, Answer),
    moved_between: bool,
) {
    for _ in 0..AGENTS * 2 {
        if world.head_of_queue().is_none() {
            break;
        }
        cycle(world, Answer::Merged);
    }
    let target = (on + 1) % (TARGETS - 1);
    let assuming = assume(world, who, target, on);
    let assuming_claim = granted(&assuming);
    emit(world, assuming);
    let claimed = claim(world, who + 1, on, false, false);
    let challenger = granted(&claimed);
    emit(world, claimed);
    let submitted = submit(world, &self::who(who + 1), true);
    let challenges = kinds(&submitted)
        .iter()
        .any(|kind| matches!(kind, EventKind::AssumptionChallenged { .. }));
    emit(world, submitted);
    if let Some(claim) = challenger.filter(|claim| world.model.held.contains(claim)) {
        let approval = ClientMsg::Review {
            req: RequestId(7),
            claim: ClaimId(claim),
            approve: true,
            note: None,
        };
        let decided = world.handle(&agent("felix"), approval);
        emit(world, decided);
    }
    let first_main = world.core.state.head.clone();
    begin_checked(world);
    let Some(flight) = world.model.inflight else {
        return;
    };
    // Only a `Land` that is cut off or lost leaves the claim in doubt.
    advance_to(world, Stage::Land);
    if restart {
        let recovered = recover_checked(world);
        emit(world, recovered);
    } else {
        let answered = answer_checked(world, flight.claim, lost);
        emit(world, answered);
    }
    if moved_between {
        // The cut-off attempt went on to push: the retry finds main moved, and the baseline stays.
        cycle(world, Answer::MainMoved);
    }
    cycle(world, Answer::AlreadyMerged);
    let landed = challenger.is_some_and(|claim| world.model.merged.contains(&claim));
    // A baseline equal to the landing's main has no span: nothing is queued (check 7).
    let spans = first_main.as_ref().is_some_and(|main| main.0 != LANDED);
    let assuming_live = assuming_claim.is_some_and(|id| world.core.state.claims.contains_key(&id));
    if !(landed && challenges && spans && assuming_live && Some(flight.claim) == challenger) {
        return;
    }
    let queued = world.core.state.verifications.iter().find(|queued| {
        Some(queued.claim.0) == assuming_claim
            && matches!(queued.subject, Subject::Assumption { .. })
    });
    let queued = queued.expect("a landing with a baseline and a challenge queues its verification");
    assert_eq!(
        Some(&queued.before),
        first_main.as_ref(),
        "check 8: baseline is the first lost main"
    );
    assert_eq!(
        queued.main,
        CommitId(LANDED.into()),
        "check 8: ends at the lander's main"
    );
    CHECK_8_RUNS.fetch_add(1, Ordering::Relaxed);
}

/// How many times check 8 got past its early returns and ran, over the whole test.
static CHECK_8_RUNS: AtomicUsize = AtomicUsize::new(0);

fn run_pipeline(world: &mut World, answers: &[Answer]) {
    for index in 0..AGENTS {
        apply(
            world,
            &Op::Claim {
                who: index,
                target: index,
                wait: false,
                signature: false,
            },
        );
        apply(
            world,
            &Op::Submit {
                who: index,
                evidence: true,
            },
        );
    }
    for answer in answers {
        cycle(world, *answer);
    }
}

/// `begin_merge`, with the rules about what it may return checked.
fn begin_checked(world: &mut World) {
    // A submitted claim cannot be released or expire, so the claim in flight stays queued.
    let was_live = world.model.inflight;
    assert!(was_live.is_none_or(|flight| world.live(flight.claim)));
    let head = world.head_of_queue();
    let slot_due = world.slot_due();
    let ready = world.verification_ready();
    let dispatch = world.core.begin_merge(world.now);
    world.trace.push_str(&format!("{dispatch:?}"));
    if slot_due {
        assert!(
            dispatch.is_none(),
            "I12: a merge went ahead of a verification after {SLOT_AFTER} merges"
        );
    }
    if let Some(dispatch) = &dispatch {
        check_stage_dispatch(world, dispatch);
    }
    let Some(dispatch) = dispatch else {
        // Documented: `None` when nothing is ready. An in-flight merge is always returned.
        assert!(
            was_live.is_none(),
            "begin_merge dropped the merge in flight"
        );
        world.model.inflight = None;
        return;
    };
    if was_live.is_none() {
        world.model.ahead = if ready { world.model.ahead + 1 } else { 0 };
    }
    let id = dispatch.claim.0;
    assert!(world.core.has_merge_in_flight());
    let again = world.core.clone().begin_merge(world.now);
    assert_eq!(
        again.as_ref().map(|d| (d.claim, d.attempt)),
        Some((dispatch.claim, dispatch.attempt)),
        "check 3: a second begin_merge returned something other than the merge in flight"
    );
    if let Some(flight) = was_live {
        assert_eq!(
            id, flight.claim,
            "check 3: begin_merge started a second merge"
        );
        assert_eq!(
            dispatch.attempt, flight.attempt,
            "the in-flight attempt is unchanged"
        );
        assert_eq!(
            dispatch.id, flight.id,
            "the dispatch in flight keeps its id when it is returned again"
        );
        assert_eq!(
            Stage::of(&dispatch.stage),
            flight.stage,
            "the dispatch in flight keeps its stage when it is returned again"
        );
        return;
    }
    assert_eq!(
        dispatch.stage,
        MergeStage::Prepare,
        "a new merge starts at Prepare"
    );
    let work = world.core.state.claims[&id].work.as_ref().unwrap();
    assert!(
        !world.model.held.contains(&id) && !work.awaiting_review,
        "check 2: a submission held for review was dispatched"
    );
    assert_eq!(
        Some(id),
        head,
        "merges are taken in submission order, held claims skipped"
    );
    assert!(
        work.retry_at_ms
            .is_none_or(|due| due <= world.core.state.clock_ms),
        "a claim in backoff was dispatched"
    );
    let expected = world.model.moved.get(&id).copied().unwrap_or(0)
        + world.model.infra.get(&id).copied().unwrap_or(0)
        + 1;
    assert_eq!(
        dispatch.attempt, expected,
        "check 4: attempt counts every answer so far"
    );
    // Documented on `next_merge_ms`: the watchdog is `MERGE_WATCHDOG_MS` after the call starts.
    assert_eq!(
        world.core.next_merge_ms(true, world.now),
        Some(world.now + MERGE_WATCHDOG_MS)
    );
    assert_eq!(world.core.next_merge_ms(false, world.now), Some(world.now));
    world.model.issue(Some(id), dispatch.id);
    world.model.inflight = Some(Flight {
        id: dispatch.id,
        stage: Stage::Prepare,
        claim: id,
        attempt: dispatch.attempt,
        dispatched_ms: world.now,
    });
}

/// A `Test` or a `Land` names the pair the core journaled, and the stored marker holds it: no
/// call carries a head the core does not hold (rule 5).
fn check_stage_dispatch(world: &World, dispatch: &crate::coordinator::MergeDispatch) {
    let (MergeStage::Test { base, head } | MergeStage::Land { base, head }) = &dispatch.stage
    else {
        return;
    };
    let stored = serde_json::to_value(&world.core).unwrap();
    let stage = &stored["merge_in_flight"]["stage"];
    assert_eq!(
        (stage["base"].as_str(), stage["head"].as_str()),
        (Some(base.0.as_str()), Some(head.0.as_str())),
        "I3: a {:?} names a head the stored marker does not hold",
        dispatch.stage
    );
    assert_eq!(
        dispatch.id,
        world.core.state.merge_in_flight.as_ref().unwrap().dispatch
    );
}

/// Answer the calls of the merge in flight before `target` with `Prepared` and `Tested`: each
/// moves the merge to the next call under a new, larger dispatch id, with no effect for anyone,
/// the head journaled, and a repeat of the answer is stale.
fn advance_to(world: &mut World, target: Stage) {
    while let Some(flight) = world.model.inflight.filter(|flight| flight.stage < target) {
        let (outcome, next) = match flight.stage {
            Stage::Prepare => (
                MergeOutcome::Prepared {
                    base: CommitId("old".into()),
                    head: CommitId(MAIN.into()),
                },
                Stage::Test,
            ),
            Stage::Test | Stage::Land => (MergeOutcome::Tested {}, Stage::Land),
        };
        let effects = world.core.merge_outcome(flight.id, &outcome, world.now);
        assert!(
            effects.is_empty(),
            "a stage answer had effects: {effects:?}"
        );
        let stored = world
            .core
            .state
            .merge_in_flight
            .clone()
            .expect("the merge goes on to its next call");
        assert!(
            stored.dispatch > flight.id,
            "each call has a new dispatch id"
        );
        assert_eq!(stored.claim.0, flight.claim);
        assert_eq!(Stage::of(&stored.stage), next);
        let (MergeStage::Test { base, head } | MergeStage::Land { base, head }) = &stored.stage
        else {
            unreachable!("the merge moved past Prepare");
        };
        assert_eq!((base.0.as_str(), head.0.as_str()), ("old", MAIN));
        let before = save(&world.core);
        let again = world.core.merge_outcome(flight.id, &outcome, world.now);
        assert!(
            again.is_empty() && before == save(&world.core),
            "a repeat of a stage's answer changed the state"
        );
        world.model.issue(Some(flight.claim), stored.dispatch);
        world.model.inflight = Some(Flight {
            id: stored.dispatch,
            stage: next,
            ..flight
        });
        let dispatch = world
            .core
            .begin_merge(world.now)
            .expect("the next call is due");
        assert_eq!(dispatch.id, stored.dispatch);
        check_stage_dispatch(world, &dispatch);
    }
}

/// Apply the lander's answer for `claim` through `merge_outcome` and check what it did. The calls
/// before the one the answer comes from are answered first.
fn answer_checked(world: &mut World, claim: u64, answer: Answer) -> Vec<Effect> {
    let current = world.model.inflight.filter(|flight| flight.claim == claim);
    if current.is_none() {
        // Documented: an answer for any other claim is stale and changes nothing. It carries the
        // latest dispatch id issued for that claim, or an id never issued.
        let id = world
            .model
            .issued
            .iter()
            .rev()
            .find(|(of, _)| *of == Some(claim))
            .map_or(UNISSUED_DISPATCH, |(_, id)| *id);
        return stale_answer(world, id, answer);
    }
    assert!(world.live(claim), "the claim in flight is still queued");
    advance_to(world, answer.stage());
    let flight = world.model.inflight.expect("the merge is still in flight");
    let retry_before = world
        .core
        .state
        .claims
        .get(&claim)
        .and_then(|held| held.work.as_ref())
        .and_then(|work| work.retry_at_ms);
    let owed = owed_verifications(world, claim);
    let effects = world
        .core
        .merge_outcome(flight.id, &answer.outcome(), world.now);
    check_unverifiable(world, claim, &owed, &effects);
    world.model.inflight = None;
    assert!(
        !world.core.has_merge_in_flight(),
        "the answer left a marker behind"
    );
    let doubt = flight.stage == Stage::Land && !answer.candidate_missing();
    expect_class(
        world,
        claim,
        (answer.class(), doubt),
        &effects,
        retry_before,
    );
    effects
}

/// What a landing of `claim` owes verification, read before the answer is applied: the
/// (assuming claim, assumption) pairs it challenged whose assuming claim is live, and the
/// submitted shadow claims it blocks.
type Owed = (HashSet<(u64, Assumption)>, HashSet<u64>, HashSet<String>);

fn owed_verifications(world: &World, claim: u64) -> Owed {
    let state = &world.core.state;
    let mut pairs = HashSet::new();
    if let Some(work) = state.claims.get(&claim).and_then(|held| held.work.as_ref()) {
        for challenge in &work.challenged {
            if state.claims.contains_key(&challenge.claim.0) {
                pairs.insert((challenge.claim.0, challenge.assumption.clone()));
            }
        }
    }
    let mut shadows = HashSet::new();
    for (id, shadow) in &state.claims {
        let Some(denial) = &shadow.denial else {
            continue;
        };
        if denial.blocked_by.contains(&ClaimId(claim)) && denial.fork_commit.is_some() {
            shadows.insert(*id);
        }
    }
    let queue = state.verifications.iter().map(save_trial).collect();
    (pairs, shadows, queue)
}

/// A queued trial as stored. A trial the landing queued has an id the queue did not hold.
fn save_trial(trial: &Verification) -> String {
    serde_json::to_string(trial).unwrap()
}

/// Check 9: every live challenged assumption of a landing is either queued for verification
/// against the new main or listed as unverified on its `Merged` event, never both and never
/// neither; the same for the submitted shadow claims it blocks (queued or untried).
fn check_unverifiable(world: &World, claim: u64, owed: &Owed, effects: &[Effect]) {
    let mut landed = None;
    for kind in kinds(effects) {
        if let EventKind::Merged {
            claim: c,
            head,
            base: _,
            unverified,
            untried_shadows,
        } = kind
        {
            if c.0 == claim {
                landed = Some((head, unverified, untried_shadows));
            }
        }
    }
    let Some((head, unverified, untried)) = landed else {
        return;
    };
    let mut listed = HashSet::new();
    for item in unverified {
        assert!(
            listed.insert((item.claim.0, item.assumption.clone())),
            "check 9: an assumption is listed twice"
        );
    }
    let mut queued = HashSet::new();
    let mut tried = HashSet::new();
    for trial in &world.core.state.verifications {
        if trial.main != *head || owed.2.contains(&save_trial(trial)) {
            continue;
        }
        match &trial.subject {
            Subject::Assumption { assumption } => {
                queued.insert((trial.claim.0, assumption.clone()));
            }
            Subject::Shadow { blocking_claim } if blocking_claim.0 == claim => {
                tried.insert(trial.claim.0);
            }
            Subject::Shadow { .. } | Subject::RaceEntry { .. } => {}
        }
    }
    assert!(
        queued.is_disjoint(&listed),
        "check 9: an assumption is both queued and listed unverified"
    );
    let covered: HashSet<_> = queued.union(&listed).cloned().collect();
    assert_eq!(
        covered, owed.0,
        "check 9: queued plus unverified differs from live challenged"
    );
    let untried: HashSet<u64> = untried.iter().map(|id| id.0).collect();
    assert!(
        tried.is_disjoint(&untried),
        "check 9: a shadow is both tried and listed untried"
    );
    let covered: HashSet<u64> = tried.union(&untried).copied().collect();
    assert_eq!(
        covered, owed.1,
        "check 9: tried plus untried shadows differ from those owed"
    );
    if !unverified.is_empty() {
        CHECK_9_RUNS.fetch_add(1, Ordering::Relaxed);
    }
    if !untried.is_empty() {
        CHECK_9_SHADOW_RUNS.fetch_add(1, Ordering::Relaxed);
    }
}

/// How many landings listed an unverified assumption, over the whole test.
static CHECK_9_RUNS: AtomicUsize = AtomicUsize::new(0);

/// How many landings listed an untried shadow claim, over the whole test.
static CHECK_9_SHADOW_RUNS: AtomicUsize = AtomicUsize::new(0);

/// An answer to dispatch `id`, which is not the merge in flight: no effects and no state change.
fn stale_answer(world: &mut World, id: u64, answer: Answer) -> Vec<Effect> {
    assert!(
        world.model.inflight.is_none_or(|flight| flight.id != id),
        "the test sent a stale answer with the id in flight"
    );
    world.core.expire(world.now);
    let before = save(&world.core);
    let effects = world.core.merge_outcome(id, &answer.outcome(), world.now);
    assert!(
        effects.is_empty(),
        "a stale answer had effects: {effects:?}"
    );
    assert_eq!(
        before,
        save(&world.core),
        "a stale answer changed the state"
    );
    let report = TrialReport::stopped(TrialOutcome::Clean {});
    let verified = world.core.verification_outcome(id, &report, world.now);
    assert!(
        verified.is_empty() && before == save(&world.core),
        "a stale verification answer changed the state"
    );
    effects
}

/// An answer for an earlier dispatch: any id issued before, other than the merge in flight.
fn late_answer(world: &mut World, pick: usize, answer: Answer) -> Vec<Effect> {
    let in_flight = world.model.inflight.map(|flight| flight.id);
    let earlier: Vec<u64> = world
        .model
        .issued
        .iter()
        .map(|(_, id)| *id)
        .filter(|id| Some(*id) != in_flight)
        .collect();
    if earlier.is_empty() {
        return Vec::new();
    }
    stale_answer(world, earlier[pick % earlier.len()], answer)
}

/// What `class` must have done to a live claim.
fn expect_class(
    world: &mut World,
    claim: u64,
    (class, doubt): (Class, bool),
    effects: &[Effect],
    retry_before: Option<u64>,
) {
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
            world.model.doubt.remove(&claim);
            assert_eq!(merged_events, 1, "check 1: one Merged for a landed claim");
            assert!(
                !world.core.state.claims.contains_key(&claim),
                "a merged claim is released"
            );
        }
        Class::Rejected => {
            world.model.doubt.remove(&claim);
            assert_eq!(rejected_events, 1);
            assert_reopened(world, claim);
        }
        Class::Moved => {
            let moved = world.model.moved.entry(claim).or_insert(0);
            *moved += 1;
            let moved = *moved;
            assert_eq!(merged_events, 0);
            // `MAX_MAIN_MOVED` answers reject the claim, never fewer.
            if moved >= MAX_MAIN_MOVED {
                world.model.doubt.remove(&claim);
                assert_eq!(
                    rejected_events, 1,
                    "check 4: rejected at {moved} main-moved"
                );
                assert_reopened(world, claim);
            } else {
                assert_eq!(rejected_events, 0, "check 4: rejected after only {moved}");
                assert!(
                    world.live(claim),
                    "check 4: still queued after {moved} main-moved"
                );
                assert_redispatched_at_once(world, claim, retry_before);
            }
        }
        Class::Infra => expect_infra(world, claim, (merged_events, rejected_events), doubt),
    }
}

/// `src/merge.rs`: a `main_moved` answer "re-dispatches the claim at once", so it sets no backoff
/// and the next `begin_merge` returns the same claim.
fn assert_redispatched_at_once(world: &World, claim: u64, retry_before: Option<u64>) {
    let retry_after = world.core.state.claims[&claim]
        .work
        .as_ref()
        .and_then(|work| work.retry_at_ms);
    assert_eq!(
        retry_after, retry_before,
        "check 4: main_moved set a backoff"
    );
    let next = world.core.clone().begin_merge(world.now);
    let expected = if world.slot_due() {
        None
    } else {
        world.head_of_queue()
    };
    assert_eq!(
        next.map(|dispatch| dispatch.claim.0),
        expected,
        "check 4: the claim was not dispatched again at once after main_moved"
    );
}

/// An infrastructure failure of a call that can have changed main (`doubt`: a `Land`) leaves the
/// claim in doubt; one of a `Prepare` or a `Test`, or a missing candidate, does not.
fn expect_infra(
    world: &mut World,
    claim: u64,
    (merged_events, rejected_events): (usize, usize),
    doubt: bool,
) {
    if doubt {
        world.model.doubt.insert(claim);
    }
    let failures = world.model.infra.entry(claim).or_insert(0);
    *failures += 1;
    let failures = *failures;
    assert_eq!(merged_events, 0);
    // `MAX_INFRA_RETRIES` retries are allowed: the failure after them rejects.
    if failures > MAX_INFRA_RETRIES {
        assert_eq!(
            rejected_events, 1,
            "check 4: rejected at failure {failures}"
        );
        assert_reopened(world, claim);
        world.model.doubt.remove(&claim);
        return;
    }
    assert_eq!(
        rejected_events, 0,
        "check 4: rejected at failure {failures}"
    );
    let due = world.core.state.claims[&claim]
        .work
        .as_ref()
        .unwrap()
        .retry_at_ms;
    assert_eq!(
        due,
        Some(world.core.state.clock_ms + infra_backoff_ms(failures)),
        "check 4: backoff after failure {failures}"
    );
}

/// A rejected claim is active again: unsubmitted, nothing to merge, still holding its scopes.
fn assert_reopened(world: &World, claim: u64) {
    let held = world
        .core
        .state
        .claims
        .get(&claim)
        .expect("a rejected claim stays");
    assert!(held.submitted.is_none() && held.work.is_none());
}

/// The process died with a merge in flight. `recover_merge` counts it as an infrastructure failure.
fn recover_checked(world: &mut World) -> Vec<Effect> {
    let Some(flight) = world.model.inflight else {
        let effects = world.core.recover_merge(world.now);
        assert!(effects.is_empty() && !world.core.has_merge_in_flight());
        return effects;
    };
    assert!(
        world.live(flight.claim),
        "the claim in flight is still queued"
    );
    let effects = world.core.recover_merge(world.now);
    world.model.inflight = None;
    assert!(!world.core.has_merge_in_flight());
    let rejected = kinds(&effects)
        .into_iter()
        .filter(|kind| matches!(kind, EventKind::SubmitRejected { claim: c, .. } if c.0 == flight.claim))
        .count();
    expect_infra(
        world,
        flight.claim,
        (0, rejected),
        flight.stage == Stage::Land,
    );
    effects
}

/// Wait out the head's backoff, dispatch, answer.
fn cycle(world: &mut World, answer: Answer) {
    if world.model.inflight.is_none() {
        if let Some(due) = world.core.next_merge_ms(false, world.now) {
            world.now = world.now.max(due);
            world.core.expire(world.now);
        }
    }
    begin_checked(world);
    if let Some(flight) = world.model.inflight {
        let effects = answer_checked(world, flight.claim, answer);
        emit(world, effects);
    }
}

/// Record what the core just did in the model and in the trace compared with the restored core.
fn emit(world: &mut World, effects: Vec<Effect>) {
    observe(world, &effects);
    world.trace.push_str(&format!("{effects:?}"));
}

/// Update the model from what the core just did, and check that no claim merges twice.
fn observe(world: &mut World, effects: &[Effect]) {
    for kind in kinds(effects) {
        if let EventKind::Merged { claim, .. } = kind {
            assert!(
                world.model.merged.insert(claim.0),
                "check 1: claim {} was merged twice",
                claim.0
            );
        }
        if let EventKind::ReviewDecided { claim, .. } | EventKind::SubmitRejected { claim, .. } =
            kind
        {
            world.model.held.remove(&claim.0);
        }
        // A race winner is told it is held by a notice, not a reply; the event is the record.
        if let EventKind::ReviewRequested { claim, .. } = kind {
            world.model.held.insert(claim.0);
        }
        if let EventKind::VerificationDropped { claim, .. } = kind {
            assert!(
                !world.core.state.claims.contains_key(&claim.0),
                "a verification was dropped for a claim that is still live"
            );
        }
    }
    for msg in replies(effects) {
        if let ServerMsg::ReviewRequired { claim, .. } = msg {
            world.model.held.insert(claim.0);
        }
        if let ServerMsg::Accepted { claim, .. } | ServerMsg::ReviewRequired { claim, .. } = msg {
            world.model.order.retain(|id| *id != claim.0);
            world.model.order.push(claim.0);
        }
    }
}

/// Checks that hold after every step, whatever the step was.
fn check_after(world: &mut World) {
    for id in world.core.state.claims.keys() {
        assert!(
            !world.model.merged.contains(id),
            "check 1: a merged claim is back"
        );
        if !world.model.seen.contains(id) {
            world.model.seen.push(*id);
        }
    }
    let ids: Vec<u64> = world.core.state.claims.keys().copied().collect();
    let live: Vec<u64> = ids.iter().copied().filter(|id| world.live(*id)).collect();
    for id in &live {
        assert!(
            world.model.order.contains(id),
            "claim {id} is queued but its submission was never accepted"
        );
    }
    world.model.held.retain(|id| ids.contains(id));
    world.model.doubt.retain(|id| ids.contains(id));
    let submitted: Vec<u64> = world
        .core
        .state
        .claims
        .iter()
        .filter(|(_, held)| held.submitted.is_some())
        .map(|(id, _)| *id)
        .collect();
    world.model.order.retain(|id| submitted.contains(id));
    world.model.moved.retain(|id, _| live.contains(id));
    world.model.infra.retain(|id, _| live.contains(id));
    for queued in &world.core.state.verifications {
        let compares_itself = match queued.subject {
            Subject::Assumption { .. } | Subject::Shadow { .. } => queued.before == queued.main,
            Subject::RaceEntry { .. } => false,
        };
        assert!(
            !compares_itself,
            "check 7: a trial compares main with itself (an already_merged landing)"
        );
    }
    assert_eq!(
        world.core.has_merge_in_flight(),
        world.model.inflight.is_some(),
        "check 3: the marker and the dispatched merge disagree"
    );
    assert!(
        world.model.ahead <= SLOT_AFTER,
        "I12: {} merges went ahead of a ready verification",
        world.model.ahead
    );
    check_no_wedge(world);
}

/// Check 6: if the queue has work, the core dispatches it or reports a wake no later than the
/// moment it becomes dispatchable. Wakes are never before `now` (`next_merge_ms`).
fn check_no_wedge(world: &World) {
    let core = &world.core;
    let now = world.now.max(core.state.clock_ms);
    if core.has_merge_in_flight() {
        // Not merging here: a stored marker means a restart, recovered at once.
        assert_eq!(
            core.next_wake_ms(false, false, now),
            Some(now),
            "check 6: marker"
        );
        // Merging here: the watchdog covers a hung instance.
        let wake = core.next_wake_ms(true, false, now);
        assert!(
            wake.is_some_and(|at| at <= now + MERGE_WATCHDOG_MS),
            "check 6: watchdog"
        );
        return;
    }
    if world.slot_due() {
        assert!(
            core.clone().begin_verification(now).is_some(),
            "I12: the verification owed the slot was not dispatchable"
        );
        assert_eq!(
            core.next_wake_ms(false, false, now),
            Some(now),
            "I12: the verification owed the slot has no wake"
        );
        return;
    }
    let Some(head) = world.head_of_queue() else {
        return;
    };
    let work = core.state.claims[&head].work.as_ref().unwrap();
    let eligible_at = work.retry_at_ms.unwrap_or(0).max(now);
    let wake = core.next_wake_ms(false, false, now);
    assert!(
        wake.is_some_and(|at| now <= at && at <= eligible_at),
        "check 6: wake {wake:?} is after claim {head} becomes eligible at {eligible_at}"
    );
    if eligible_at == now {
        assert!(
            core.clone().begin_merge(now).is_some(),
            "check 6: claim {head} is eligible now and begin_merge did not dispatch it"
        );
    }
}

fn run(script: &[Op]) {
    let mut world = new_world();
    for op in script {
        // Check 5: the state survives a save and load. The restored core and the original take
        // the same step and must end identical.
        let saved = save(&world.core);
        let mut restored = World {
            core: load(&saved),
            now: world.now,
            model: world.model.clone(),
            trace: String::new(),
        };
        assert_eq!(
            saved,
            save(&restored.core),
            "check 5: save then load changed the state"
        );
        world.trace.clear();
        apply(&mut world, op);
        apply(&mut restored, op);
        assert_eq!(
            world.trace, restored.trace,
            "check 5: a restored core behaved differently"
        );
        assert_eq!(
            save(&world.core),
            save(&restored.core),
            "check 5: restored state diverged"
        );
        check_after(&mut world);
    }
}

#[test]
fn random_merge_queue_scripts_keep_the_queue_rules() {
    let mut runner = TestRunner::default();
    let outcome = runner.run(&ops(), |script| {
        run(&script);
        Ok(())
    });
    if let Err(failure) = outcome {
        panic!("{failure}");
    }
    assert!(
        CHECK_8_RUNS.load(Ordering::Relaxed) > 0,
        "check 8 never ran: every case returned before reaching it"
    );
    assert!(
        CHECK_9_RUNS.load(Ordering::Relaxed) > 0,
        "check 9 never saw an unverified landing"
    );
    assert!(
        CHECK_9_SHADOW_RUNS.load(Ordering::Relaxed) > 0,
        "check 9 never saw an untried shadow"
    );
}

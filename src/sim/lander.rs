//! The simulated merge path: a git host with one `main`, and the lander in front of it that
//! answers the host's calls. The lander is honest (`docs/proposals/real-use.md`, C6 and C7): it
//! answers `Reconcile` and a repeated `Land` by whether the head is on main, and it never applies
//! a `Land` whose base is not main, which it refuses as a compare-and-swap (the answer is
//! `Unknown`, as GitHub's stale `beforeOid` is).
//!
//! With a seed (`with_faults`) the path is also asynchronous like CI: `plan` decides, for each
//! call, when the lander runs it, whether it runs at all, whether and when its reply arrives, and
//! whether the reply is sent twice. `execute` then does the call's work and gives the true answer.
//! Without a seed every call runs and is answered at once.

use std::collections::{BTreeMap, BTreeSet};

use crate::host::call::{Call, CallAnswer, Refusal, Request, TestFailure, TrialResult};
use crate::protocol::CommitId;
use crate::sim::rng::Rng;

/// Main before anything lands. It is not the head the agents declare in their `Hello`
/// (`driver::BASE`), so a dispatch built on the declared head names a commit main never held.
pub const MAIN0: &str = "1111111111111111111111111111111111111111";

/// How many merge calls in a row a rate limit refuses once it starts.
const RATE_LIMIT_MS: u64 = 15_000;

/// How a call for a poisoned commit is cut off, and for how many calls: all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Poison {
    AtPrepare,
    AtTest,
}

/// A commit on main's first-parent history after `MAIN0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    pub head: String,
    /// The main the head was made on: its first parent.
    pub parent: String,
    /// The fork commit the head was made from. `None` for a commit someone else pushed to main.
    pub fork: Option<String>,
}

#[derive(Debug, Clone)]
struct Candidate {
    fork: String,
    /// The main the head was prepared on.
    base: String,
}

/// What the lander does with one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// It runs and its answer is sent.
    Reply,
    /// It runs and the answer is `Unknown`.
    Unknown,
    /// It runs, and a push it made stands, but no answer is ever sent. The adapter's deadline
    /// answers instead. For a `Land` this is a push followed by a lost reply.
    Lost,
    /// It never runs and no answer is sent: a timeout.
    NeverRuns,
    /// The lander is rate limited: nothing runs and `Unknown` comes back at once.
    RateLimited,
    /// A commit that cuts the path off every time: nothing runs and `Unknown` comes back at once.
    CutOff,
}

/// When and how one call goes: all times are delays from now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// How long until the lander runs the call: a long delay is a call that arrives late.
    pub run_after_ms: u64,
    pub fate: Fate,
    /// How long after running the reply is sent.
    pub reply_after_ms: u64,
    /// When the reply is sent a second time, after the first.
    pub repeat_after_ms: Option<u64>,
}

impl Plan {
    /// Run at once, answer at once, once.
    pub const PROMPT: Plan = Plan {
        run_after_ms: 0,
        fate: Fate::Reply,
        reply_after_ms: 0,
        repeat_after_ms: None,
    };
}

/// The faults the lander has injected, by name, for the tests that the seeds reach every one.
pub type FaultCounts = BTreeMap<&'static str, u64>;

#[derive(Debug)]
struct Chaos {
    rng: Rng,
    seen_forks: BTreeSet<String>,
    limited_until_ms: u64,
    /// Reconcile answers still owed as `Unknown`.
    reconcile_unknowns: u32,
    /// Heads whose tests flip between failing and passing, and whether their next run passes.
    flaky: BTreeMap<String, bool>,
    judged: BTreeSet<String>,
    faults: FaultCounts,
}

impl Chaos {
    fn bump(&mut self, fault: &'static str) {
        *self.faults.entry(fault).or_insert(0) += 1;
    }
}

#[derive(Debug)]
pub struct Lander {
    main: String,
    history: Vec<Landing>,
    candidates: BTreeMap<String, Candidate>,
    /// The fork commit each head was made from, kept when the candidate ref is lost.
    head_fork: BTreeMap<String, String>,
    landed_forks: BTreeSet<String>,
    /// Calls cut off before they could finish, by fork commit: what an infrastructure failure
    /// the core counts against that submission is made of.
    cutoffs: BTreeMap<String, u32>,
    chaos: Option<Chaos>,
    /// How long main's history was when each call was issued, by call and dispatch.
    issued_at: BTreeMap<(u64, u64), usize>,
    /// Commits whose calls are cut off, and at which stage.
    poisoned: BTreeMap<String, Poison>,
    /// Every `Test` finds its candidate gone and is reported as passing all the same.
    stale_tests: bool,
    /// Every reply is sent this long after the call runs, with or without faults.
    slow_replies_ms: u64,
    /// Every reply is sent a second time this long after the first, with or without faults.
    repeat_replies_ms: Option<u64>,
}

impl Default for Lander {
    fn default() -> Self {
        Self::new()
    }
}

/// A head made from `fork` on `base`: the same for the same pair, as the deterministic rebase
/// makes it (requirement 7).
fn candidate_head(fork: &str, base: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in fork.bytes().chain([0]).chain(base.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}{:016x}{:08x}", hash.rotate_left(21), hash >> 32)
}

fn tests_failed() -> CallAnswer {
    CallAnswer::Refused {
        refusal: Refusal::TestsFailed {
            failure: TestFailure {
                exit_code: 1,
                stdout: String::new(),
                stderr: String::new(),
            },
        },
    }
}

impl Lander {
    /// An honest lander with no faults: every call runs and is answered at once.
    pub fn new() -> Self {
        Self {
            main: MAIN0.to_string(),
            history: Vec::new(),
            candidates: BTreeMap::new(),
            head_fork: BTreeMap::new(),
            landed_forks: BTreeSet::new(),
            cutoffs: BTreeMap::new(),
            chaos: None,
            issued_at: BTreeMap::new(),
            poisoned: BTreeMap::new(),
            stale_tests: false,
            slow_replies_ms: 0,
            repeat_replies_ms: None,
        }
    }

    /// Inject the faults of `seed`.
    pub fn with_faults(mut self, seed: u64) -> Self {
        self.chaos = Some(Chaos {
            rng: Rng::new(seed ^ 0x6c61_6e64_6572),
            seen_forks: BTreeSet::new(),
            limited_until_ms: 0,
            reconcile_unknowns: 0,
            flaky: BTreeMap::new(),
            judged: BTreeSet::new(),
            faults: FaultCounts::new(),
        });
        self
    }

    /// Send every reply `ms` after the call runs, for a script that needs a call to stay out.
    pub fn slow_replies(&mut self, ms: u64) {
        self.slow_replies_ms = ms;
    }

    /// Send every reply a second time `ms` after the first.
    pub fn repeat_replies(&mut self, ms: u64) {
        self.repeat_replies_ms = Some(ms);
    }

    pub fn main(&self) -> &str {
        &self.main
    }

    /// Main's first-parent history after `MAIN0`, oldest first.
    pub fn history(&self) -> &[Landing] {
        &self.history
    }

    /// Whether `commit` is main or on its first-parent history.
    pub fn on_main(&self, commit: &str) -> bool {
        commit == MAIN0 || self.history.iter().any(|landing| landing.head == commit)
    }

    /// How many calls were cut off before they finished, by fork commit.
    pub fn cutoffs(&self) -> &BTreeMap<String, u32> {
        &self.cutoffs
    }

    /// The fork commits every call for which is cut off at one stage.
    pub fn poisoned(&self) -> Vec<String> {
        self.poisoned.keys().cloned().collect()
    }

    /// Lose the candidate of every `Test` and report the head as tested.
    #[cfg(test)]
    pub(crate) fn report_lost_candidates_as_tested(&mut self) {
        self.stale_tests = true;
    }

    /// Cut off every call for `fork` at its `Prepare`, or at its `Test` when `at_test`.
    #[cfg(test)]
    pub(crate) fn poison(&mut self, fork: &str, at_test: bool) {
        let stage = if at_test {
            Poison::AtTest
        } else {
            Poison::AtPrepare
        };
        self.poisoned.insert(fork.to_string(), stage);
    }

    /// The faults injected so far, by name.
    pub fn faults(&self) -> FaultCounts {
        self.chaos
            .as_ref()
            .map(|chaos| chaos.faults.clone())
            .unwrap_or_default()
    }

    /// The fork commit a call is about, if it is about a submission.
    fn fork_of(&self, request: &Request) -> Option<String> {
        match request {
            Request::Prepare { fork_commit, .. } => Some(fork_commit.0.clone()),
            Request::Test { head, .. } | Request::Land { head, .. } => {
                self.head_fork.get(&head.0).cloned()
            }
            Request::Trial { .. } | Request::ReadHead { .. } | Request::Reconcile { .. } => None,
        }
    }

    /// Decide what happens to `call`, which the host issued at `now_ms`.
    pub fn plan(&mut self, call: &Call, now_ms: u64) -> Plan {
        self.maybe_poison(&call.request);
        if let Some(fork) = self.fork_of(&call.request) {
            if self.is_poisoned_at(&fork, &call.request) {
                if let Some(chaos) = self.chaos.as_mut() {
                    chaos.bump("cut_off_always");
                }
                return Plan {
                    fate: Fate::CutOff,
                    ..Plan::PROMPT
                };
            }
        }
        if self.chaos.is_none() {
            return Plan {
                reply_after_ms: self.slow_replies_ms,
                repeat_after_ms: self.repeat_replies_ms,
                ..Plan::PROMPT
            };
        }
        self.issued_at
            .insert((call.id.0, call.dispatch_id.0), self.history.len());
        self.maybe_push_to_main();
        self.maybe_lose_candidate(&call.request);
        let fate = self.fate_of(&call.request, now_ms);
        let Some(chaos) = self.chaos.as_mut() else {
            return Plan::PROMPT;
        };
        let (run_after_ms, reply_after_ms) = timings(&mut chaos.rng, fate);
        let repeat = chaos.rng.chance(0.08);
        let repeat_after_ms = repeat.then(|| 1 + chaos.rng.below(30_000) as u64);
        if run_after_ms > 0 {
            chaos.bump("delayed");
        }
        if reply_after_ms > 0 {
            chaos.bump("reply_delayed");
        }
        if repeat {
            chaos.bump("duplicated");
        }
        Plan {
            run_after_ms,
            fate,
            reply_after_ms,
            repeat_after_ms,
        }
    }

    /// The first time a fork commit is prepared, it may be one that cuts the path off every time.
    fn maybe_poison(&mut self, request: &Request) {
        let (Request::Prepare { fork_commit, .. }, Some(chaos)) = (request, self.chaos.as_mut())
        else {
            return;
        };
        if let Some(stage) = chaos.roll_poison(&fork_commit.0) {
            self.poisoned.insert(fork_commit.0.clone(), stage);
        }
    }

    /// Someone else pushes to main.
    fn maybe_push_to_main(&mut self) {
        let Some(chaos) = self.chaos.as_mut() else {
            return;
        };
        if !chaos.rng.chance(0.1) {
            return;
        }
        chaos.bump("pushed_to_main");
        let head = candidate_head("someone else", &self.main);
        let parent = std::mem::replace(&mut self.main, head.clone());
        self.history.push(Landing {
            head,
            parent,
            fork: None,
        });
    }

    /// The candidate ref a head is kept on goes missing.
    fn maybe_lose_candidate(&mut self, request: &Request) {
        let (Request::Test { head, .. } | Request::Land { head, .. }) = request else {
            return;
        };
        let Some(chaos) = self.chaos.as_mut() else {
            return;
        };
        if chaos.rng.chance(0.08) && self.candidates.remove(&head.0).is_some() {
            chaos.bump("candidate_lost");
        }
    }

    fn fate_of(&mut self, request: &Request, now_ms: u64) -> Fate {
        let Some(chaos) = self.chaos.as_mut() else {
            return Fate::Reply;
        };
        if now_ms < chaos.limited_until_ms {
            chaos.bump("rate_limited");
            return Fate::RateLimited;
        }
        let roll = chaos.rng.below(100);
        let land = is_land(request);
        match roll {
            0..=5 => chaos.counted("unknown", Fate::Unknown),
            6..=9 => chaos.counted("never_runs", Fate::NeverRuns),
            10..=12 => {
                chaos.limited_until_ms = now_ms + RATE_LIMIT_MS;
                chaos.counted("rate_limited", Fate::RateLimited)
            }
            13..=27 if land => chaos.counted("pushed_then_lost_reply", Fate::Lost),
            17..=18 if !land => chaos.counted("lost_reply", Fate::Lost),
            _ => Fate::Reply,
        }
    }

    /// Count `call` against its submission as a call cut off before it finished.
    pub fn charge(&mut self, call: &Call) {
        if let Some(fork) = self.fork_of(&call.request) {
            *self.cutoffs.entry(fork).or_insert(0) += 1;
        }
    }

    /// Run `call`: do its work on main and return the true answer.
    pub fn execute(&mut self, call: &Call) -> CallAnswer {
        match &call.request {
            Request::ReadHead { submitted } => self.read_head(submitted),
            Request::Prepare { fork_commit, .. } => self.prepare(&fork_commit.0),
            Request::Test { head, .. } => self.test(&head.0),
            Request::Land { base, head } => {
                self.note_late_land(call);
                self.land(&base.0, &head.0)
            }
            Request::Trial { .. } => self.trial(),
            Request::Reconcile { head } => self.reconcile(&head.0),
        }
    }

    fn read_head(&self, submitted: &[CommitId]) -> CallAnswer {
        CallAnswer::Head {
            main: Some(CommitId(self.main.clone())),
            landed: submitted
                .iter()
                .filter(|commit| self.landed_forks.contains(&commit.0))
                .cloned()
                .collect(),
        }
    }

    /// Rebase the fork commit onto main and keep the head. Nothing lands. A fork commit that is
    /// already on main has nothing left to add: the head is main.
    fn prepare(&mut self, fork: &str) -> CallAnswer {
        if self.landed_forks.contains(fork) {
            let main = CommitId(self.main.clone());
            return CallAnswer::Prepared {
                base: main.clone(),
                head: main,
            };
        }
        let head = candidate_head(fork, &self.main);
        self.head_fork.insert(head.clone(), fork.to_string());
        self.candidates.insert(
            head.clone(),
            Candidate {
                fork: fork.to_string(),
                base: self.main.clone(),
            },
        );
        CallAnswer::Prepared {
            base: CommitId(self.main.clone()),
            head: CommitId(head),
        }
    }

    fn test(&mut self, head: &str) -> CallAnswer {
        if self.stale_tests {
            self.candidates.remove(head);
            return CallAnswer::Tested;
        }
        let known = self.candidates.contains_key(head);
        let Some(chaos) = self.chaos.as_mut() else {
            return if known {
                CallAnswer::Tested
            } else {
                CallAnswer::CandidateMissing
            };
        };
        if !known {
            if chaos.rng.chance(0.8) {
                chaos.bump("stale_report");
                return CallAnswer::Tested;
            }
            return CallAnswer::CandidateMissing;
        }
        if chaos.judged.insert(head.to_string()) && chaos.rng.chance(0.12) {
            chaos.flaky.insert(head.to_string(), chaos.rng.chance(0.5));
        }
        let Some(next_passes) = chaos.flaky.get_mut(head) else {
            return CallAnswer::Tested;
        };
        let passes = *next_passes;
        *next_passes = !passes;
        chaos.bump("flipped_on_rerun");
        if passes {
            CallAnswer::Tested
        } else {
            tests_failed()
        }
    }

    /// A `Land` that reaches the lander after another landing happened since it was issued.
    fn note_late_land(&mut self, call: &Call) {
        let issued = self.issued_at.get(&(call.id.0, call.dispatch_id.0));
        let late = issued.is_some_and(|issued| *issued < self.history.len());
        if let (true, Some(chaos)) = (late, self.chaos.as_mut()) {
            chaos.bump("late_land_after_landing");
        }
    }

    /// Compare-and-swap main from `base` to `head`. A head already on main is landed already; a
    /// base that is not main is refused and nothing moves.
    fn land(&mut self, base: &str, head: &str) -> CallAnswer {
        if self.on_main(head) {
            return CallAnswer::Landed {
                head: CommitId(head.to_string()),
            };
        }
        let Some(candidate) = self.candidates.get(head) else {
            return CallAnswer::CandidateMissing;
        };
        if base != self.main || candidate.base != base {
            if let Some(chaos) = self.chaos.as_mut() {
                chaos.bump("compare_and_swap_refused");
            }
            return CallAnswer::Unknown;
        }
        let fork = candidate.fork.clone();
        let parent = candidate.base.clone();
        self.main = head.to_string();
        self.landed_forks.insert(fork.clone());
        self.history.push(Landing {
            head: head.to_string(),
            parent,
            fork: Some(fork),
        });
        CallAnswer::Landed {
            head: CommitId(head.to_string()),
        }
    }

    fn trial(&mut self) -> CallAnswer {
        let Some(chaos) = self.chaos.as_mut() else {
            return clean_trial();
        };
        if chaos.rng.chance(0.2) {
            chaos.bump("trial_flipped");
            return CallAnswer::Trialled {
                before: Some(TrialResult::Clean),
                after: Some(TrialResult::Conflict),
            };
        }
        clean_trial()
    }

    /// Whether `head` is on main, and where main is.
    fn reconcile(&mut self, head: &str) -> CallAnswer {
        if let Some(chaos) = self.chaos.as_mut() {
            if chaos.reconcile_unknowns == 0 && chaos.rng.chance(0.3) {
                chaos.reconcile_unknowns = 2 + chaos.rng.below(5) as u32;
            }
            if chaos.reconcile_unknowns > 0 {
                chaos.reconcile_unknowns -= 1;
                chaos.bump("reconcile_unknown");
                return CallAnswer::Unknown;
            }
        }
        CallAnswer::Reconciled {
            main: CommitId(self.main.clone()),
            landed: self.on_main(head),
        }
    }
}

/// Whether `request` is a `Land`.
fn is_land(request: &Request) -> bool {
    match request {
        Request::Land { .. } => true,
        Request::Prepare { .. }
        | Request::Test { .. }
        | Request::Trial { .. }
        | Request::ReadHead { .. }
        | Request::Reconcile { .. } => false,
    }
}

fn clean_trial() -> CallAnswer {
    CallAnswer::Trialled {
        before: Some(TrialResult::Clean),
        after: Some(TrialResult::Clean),
    }
}

impl Chaos {
    /// The first time a fork commit is seen, it may be one that cuts the path off every time.
    fn roll_poison(&mut self, fork: &str) -> Option<Poison> {
        if !self.seen_forks.insert(fork.to_string()) || !self.rng.chance(0.06) {
            return None;
        }
        self.bump("poisoned_commit");
        Some(if self.rng.chance(0.5) {
            Poison::AtPrepare
        } else {
            Poison::AtTest
        })
    }

    fn counted(&mut self, fault: &'static str, fate: Fate) -> Fate {
        self.bump(fault);
        fate
    }
}

impl Lander {
    /// Whether `request` is the stage at which `fork` is cut off.
    fn is_poisoned_at(&self, fork: &str, request: &Request) -> bool {
        match (self.poisoned.get(fork), request) {
            (Some(Poison::AtPrepare), Request::Prepare { .. })
            | (Some(Poison::AtTest), Request::Test { .. }) => true,
            (
                Some(Poison::AtPrepare | Poison::AtTest) | None,
                Request::Prepare { .. }
                | Request::Test { .. }
                | Request::Land { .. }
                | Request::Trial { .. }
                | Request::ReadHead { .. }
                | Request::Reconcile { .. },
            ) => false,
        }
    }
}

/// How long until the lander runs a call, and until it replies after that.
fn timings(rng: &mut Rng, fate: Fate) -> (u64, u64) {
    let prompt = match fate {
        Fate::RateLimited | Fate::CutOff => true,
        Fate::Reply | Fate::Unknown | Fate::Lost | Fate::NeverRuns => false,
    };
    let run_after_ms = if prompt {
        0
    } else {
        match rng.below(100) {
            0..=69 => 0,
            70..=89 => 1_000 + rng.below(4_000) as u64,
            90..=96 => 5_000 + rng.below(35_000) as u64,
            _ => 60_000 + rng.below(340_000) as u64,
        }
    };
    let reply_after_ms = match rng.below(100) {
        0..=74 => 0,
        75..=94 => 500 + rng.below(7_500) as u64,
        _ => 20_000 + rng.below(100_000) as u64,
    };
    (run_after_ms, reply_after_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::call::{CallId, DispatchId};
    use crate::protocol::{AgentId, ClaimId};

    fn call(id: u64, request: Request) -> Call {
        Call {
            id: CallId(id),
            dispatch_id: DispatchId(id),
            request,
            deadline_ms: u64::MAX,
        }
    }

    fn prepare(fork: &str) -> Request {
        Request::Prepare {
            claim: ClaimId(1),
            agent: AgentId("a".to_string()),
            fork_commit: CommitId(fork.to_string()),
            scopes: Vec::new(),
        }
    }

    fn land(base: &str, head: &str) -> Request {
        Request::Land {
            base: CommitId(base.to_string()),
            head: CommitId(head.to_string()),
        }
    }

    fn prepared_head(lander: &mut Lander, fork: &str) -> (String, String) {
        let CallAnswer::Prepared { base, head } = lander.execute(&call(1, prepare(fork))) else {
            panic!("a prepare was not prepared");
        };
        (base.0, head.0)
    }

    #[test]
    fn a_repeated_prepare_of_one_commit_on_one_main_makes_the_same_head() {
        let mut lander = Lander::new();
        assert_eq!(
            prepared_head(&mut lander, "f1"),
            prepared_head(&mut lander, "f1")
        );
    }

    #[test]
    fn a_land_moves_main_once_and_a_repeat_finds_the_head_landed() {
        let mut lander = Lander::new();
        let (base, head) = prepared_head(&mut lander, "f1");
        for _ in 0..2 {
            let answer = lander.execute(&call(2, land(&base, &head)));
            assert_eq!(
                answer,
                CallAnswer::Landed {
                    head: CommitId(head.clone())
                }
            );
        }
        assert_eq!(lander.main(), head);
        assert_eq!(lander.history().len(), 1);
    }

    #[test]
    fn a_head_prepared_on_an_older_main_is_refused_on_a_fresh_base() {
        let mut lander = Lander::new();
        let (base, stale) = prepared_head(&mut lander, "f1");
        let (_, newer) = prepared_head(&mut lander, "f2");
        lander.execute(&call(2, land(&base, &newer)));
        let answer = lander.execute(&call(3, land(&newer, &stale)));
        assert_eq!(answer, CallAnswer::Unknown);
        assert_eq!(lander.main(), newer, "a non-fast-forward moved main");
        assert!(!lander.on_main(&stale));
    }

    #[test]
    fn a_land_whose_base_is_not_main_is_refused_and_moves_nothing() {
        let mut lander = Lander::new();
        let (base, first) = prepared_head(&mut lander, "f1");
        let (_, second) = prepared_head(&mut lander, "f2");
        lander.execute(&call(2, land(&base, &first)));
        let answer = lander.execute(&call(3, land(&base, &second)));
        assert_eq!(answer, CallAnswer::Unknown);
        assert_eq!(lander.main(), first);
        assert!(!lander.on_main(&second));
    }

    #[test]
    fn reconcile_says_landed_only_for_a_head_on_main() {
        let mut lander = Lander::new();
        let (base, head) = prepared_head(&mut lander, "f1");
        let reconcile = |lander: &mut Lander| {
            lander.execute(&call(
                9,
                Request::Reconcile {
                    head: CommitId(head.clone()),
                },
            ))
        };
        assert_eq!(
            reconcile(&mut lander),
            CallAnswer::Reconciled {
                main: CommitId(base.clone()),
                landed: false
            },
            "a prepared head is not a landed one"
        );
        lander.execute(&call(2, land(&base, &head)));
        assert_eq!(
            reconcile(&mut lander),
            CallAnswer::Reconciled {
                main: CommitId(head.clone()),
                landed: true
            }
        );
    }

    #[test]
    fn a_fork_commit_already_on_main_prepares_to_main_itself() {
        let mut lander = Lander::new();
        let (base, head) = prepared_head(&mut lander, "f1");
        lander.execute(&call(2, land(&base, &head)));
        let (new_base, new_head) = prepared_head(&mut lander, "f1");
        assert_eq!((new_base, new_head), (head.clone(), head));
    }

    #[test]
    fn a_land_that_arrives_after_a_later_landing_is_refused_and_counted_as_late() {
        let mut counted = false;
        for seed in 0..200 {
            let mut lander = Lander::new().with_faults(seed);
            let (base, first) = prepared_head(&mut lander, "f1");
            let (_, second) = prepared_head(&mut lander, "f2");
            let late = call(5, land(&base, &first));
            lander.plan(&late, 0);
            let main = lander.main().to_string();
            if main != base {
                continue;
            }
            lander.execute(&call(6, land(&base, &second)));
            let answer = lander.execute(&late);
            if answer == CallAnswer::CandidateMissing {
                continue;
            }
            assert_eq!(answer, CallAnswer::Unknown);
            assert_eq!(lander.main(), second, "the late land moved main");
            assert!(!lander.on_main(&first));
            counted |= lander.faults().contains_key("late_land_after_landing");
        }
        assert!(counted, "no late land was counted");
    }

    #[test]
    fn reconcile_answers_unknown_several_times_in_a_row_for_some_seed() {
        let mut longest = 0;
        for seed in 0..20 {
            let mut lander = Lander::new().with_faults(seed);
            let mut run = 0;
            for n in 0..60 {
                let reconcile = Request::Reconcile {
                    head: CommitId("h".to_string()),
                };
                if lander.execute(&call(n, reconcile)) == CallAnswer::Unknown {
                    run += 1;
                } else {
                    run = 0;
                }
                longest = longest.max(run);
            }
        }
        assert!(longest >= 2, "the longest run of Unknown was {longest}");
    }

    #[test]
    fn a_test_of_a_head_whose_candidate_is_gone_may_be_reported_as_passing() {
        let mut stale = false;
        for seed in 0..50 {
            let mut lander = Lander::new().with_faults(seed);
            let (_, head) = prepared_head(&mut lander, "f1");
            lander.candidates.remove(&head);
            let test = Request::Test {
                base: CommitId(MAIN0.to_string()),
                head: CommitId(head),
            };
            let answer = lander.execute(&call(2, test));
            assert!(
                answer == CallAnswer::Tested || answer == CallAnswer::CandidateMissing,
                "a gone candidate answered {answer:?}"
            );
            stale |= answer == CallAnswer::Tested;
        }
        assert!(stale, "no seed reported a stale commit as tested");
    }

    #[test]
    fn a_lander_without_a_seed_runs_every_call_at_once() {
        let mut lander = Lander::new();
        assert_eq!(lander.plan(&call(1, prepare("f")), 0), Plan::PROMPT);
    }

    #[test]
    fn plans_with_a_seed_repeat_for_the_same_seed_and_cover_every_fate() {
        let run = |seed: u64| {
            let mut lander = Lander::new().with_faults(seed);
            (0..400)
                .map(|n| lander.plan(&call(n, prepare(&format!("f{n}"))), n * 20_000))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(5), run(5));
        assert_ne!(run(5), run(6));
        let plans = run(5);
        for fate in [
            Fate::Reply,
            Fate::Unknown,
            Fate::Lost,
            Fate::NeverRuns,
            Fate::RateLimited,
            Fate::CutOff,
        ] {
            assert!(
                plans.iter().any(|plan| plan.fate == fate),
                "no plan has fate {fate:?}"
            );
        }
        assert!(plans.iter().any(|plan| plan.run_after_ms > 60_000));
        assert!(plans.iter().any(|plan| plan.repeat_after_ms.is_some()));
    }
}

//! The driver's merge path: how a call the host issues reaches the simulated lander, and how its
//! answer, or none, comes back. A call is planned by the lander (`Lander::plan`): it may run late,
//! never run, run and lose its reply, or have its reply delayed, repeated or answered by the
//! adapter's deadline instead. Everything that happens later waits on the timeline, in time order,
//! until `quiet` finds it due.
//!
//! An answer sent before a restart is never received: the lander does not outlive the call's
//! connection to the host, though what the lander did, such as a push, stays done.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::host::call::{Call, CallAnswer, CallId, Request};
use crate::host::Input;
use crate::shell::{EVENT_PREFIX, STATE_KEY};
use crate::sim::check::Waits;
use crate::sim::driver::{Answer, Dispatch, Sim};
use crate::sim::lander::{Fate, Plan};

/// Something that happens to the merge path at a set time.
#[derive(Debug)]
pub(super) enum Due {
    /// The lander reaches a call.
    Run { call: Call, plan: Plan, epoch: u64 },
    /// An answer reaches the host.
    Reply {
        call: CallId,
        answer: CallAnswer,
        epoch: u64,
    },
    /// The adapter's deadline for a call passes: it answers `Unknown` if nothing has.
    Deadline { call: CallId, epoch: u64 },
}

/// What the host did with an answer for a dispatch that was not in flight (invariant I7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleAnswer {
    pub call: u64,
    pub dispatch: u64,
    /// What the host stored in answer, if it stored anything beyond the time it had seen.
    pub changed: Option<String>,
}

/// Which call a dispatch was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotCall {
    /// The first call of a merge.
    MergeStart,
    /// A trial of a verification.
    Trial,
    /// Any other call.
    Other,
}

/// What the stored state said when a call was dispatched, for invariant I12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotNote {
    pub events_len: usize,
    pub call: SlotCall,
    /// The next verification was queued, out of backoff and not running.
    pub verification_ready: bool,
}

/// The stored state before a stale answer, to compare with what the host staged after it.
pub(super) struct Probe {
    call: u64,
    dispatch: u64,
    state: Value,
}

/// The state without the time the host has seen, which any answer may move.
fn without_clock(state: &Value) -> Value {
    let mut state = state.clone();
    if let Some(object) = state.as_object_mut() {
        object.remove("clock_ms");
    }
    state
}

impl Sim {
    /// The host issued `call`: note it and send it down the merge path.
    pub(super) fn answer_call(&mut self, call: &Call) {
        let now_ms = self.now_ms();
        self.note_dispatch(call);
        self.open_calls.insert(call.id.0, call.clone());
        let epoch = self.crashes;
        let deadline = Due::Deadline {
            call: call.id,
            epoch,
        };
        self.schedule(call.deadline_ms, deadline);
        let plan = self.lander.plan(call, now_ms);
        let run = Due::Run {
            call: call.clone(),
            plan,
            epoch,
        };
        if plan.run_after_ms == 0 {
            self.process(run);
        } else {
            self.schedule(now_ms.saturating_add(plan.run_after_ms), run);
        }
    }

    /// What the host waits on: the calls it has out and no answer for.
    pub fn waits(&self) -> Waits {
        let mut waits = Waits::default();
        for call in self.open_calls.values() {
            match call.request {
                Request::Prepare { .. } | Request::Test { .. } | Request::Land { .. } => {
                    waits.merging = true;
                }
                Request::Trial { .. } => waits.verifying = true,
                Request::ReadHead { .. } | Request::Reconcile { .. } => waits.reading = true,
            }
        }
        waits
    }

    fn note_dispatch(&mut self, call: &Call) {
        let events_len = self.stored_events();
        self.dispatches.push(Dispatch {
            request: call.request.clone(),
            events_len,
        });
        let kind = match &call.request {
            Request::Prepare { .. } => SlotCall::MergeStart,
            Request::Trial { .. } => SlotCall::Trial,
            Request::Test { .. }
            | Request::Land { .. }
            | Request::ReadHead { .. }
            | Request::Reconcile { .. } => SlotCall::Other,
        };
        let verification_ready = self.verification_ready();
        self.slot_notes.push(SlotNote {
            events_len,
            call: kind,
            verification_ready,
        });
    }

    fn stored_state_value(&self) -> Option<Value> {
        serde_json::from_str(self.store.state()?).ok()
    }

    /// Whether the next verification is queued, out of backoff, and none is running.
    fn verification_ready(&self) -> bool {
        let Some(state) = self.stored_state_value() else {
            return false;
        };
        if !state
            .get("verification_in_flight")
            .is_none_or(Value::is_null)
        {
            return false;
        }
        let Some(next) = state
            .get("verifications")
            .and_then(Value::as_array)
            .and_then(|queue| queue.first())
        else {
            return false;
        };
        let retry = next.get("retry_at_ms").and_then(Value::as_u64);
        retry.is_none_or(|at_ms| at_ms <= self.now_ms())
    }

    fn schedule(&mut self, at_ms: u64, due: Due) {
        self.timeline_seq += 1;
        self.timeline.insert((at_ms, self.timeline_seq), due);
    }

    /// When the merge path next has something to do: the earliest item still live. A deadline of
    /// a call that was answered does nothing, so it is not a reason to move the clock.
    pub(super) fn next_due_ms(&self) -> Option<u64> {
        self.timeline
            .iter()
            .find(|(_, due)| self.is_live(due))
            .map(|((at_ms, _), _)| *at_ms)
    }

    fn is_live(&self, due: &Due) -> bool {
        match due {
            Due::Deadline { call, epoch } => {
                *epoch == self.crashes && self.open_calls.contains_key(&call.0)
            }
            Due::Run { .. } | Due::Reply { .. } => true,
        }
    }

    /// Carry out the earliest item that is due. False when none is.
    pub(super) fn deliver_due(&mut self) -> bool {
        let now_ms = self.now_ms();
        let Some(&key) = self.timeline.keys().next() else {
            return false;
        };
        if key.0 > now_ms {
            return false;
        }
        let Some(due) = self.timeline.remove(&key) else {
            return false;
        };
        if self.is_live(&due) {
            self.log.push(format!("due {} {due:?}", key.0));
            self.process(due);
        }
        true
    }

    fn process(&mut self, due: Due) {
        match due {
            Due::Run { call, plan, epoch } => self.run_call(&call, plan, epoch),
            Due::Reply {
                call,
                answer,
                epoch,
            } => self.send_reply(call, answer, 0, epoch),
            Due::Deadline { call, epoch } => self.expire_call(call, epoch),
        }
    }

    /// The lander runs `call`, as `plan` says, and sends the reply it says.
    fn run_call(&mut self, call: &Call, plan: Plan, epoch: u64) {
        let runs = !matches!(
            plan.fate,
            Fate::NeverRuns | Fate::RateLimited | Fate::CutOff
        );
        let truth = runs.then(|| self.lander.execute(call));
        let answer = match (plan.fate, truth) {
            (Fate::Reply, Some(answer)) => answer,
            (Fate::Unknown | Fate::RateLimited | Fate::CutOff, _) => CallAnswer::Unknown,
            (Fate::Lost | Fate::NeverRuns | Fate::Reply, _) => return,
        };
        self.send_reply(call.id, answer.clone(), plan.reply_after_ms, epoch);
        if let Some(again) = plan.repeat_after_ms {
            let after = plan.reply_after_ms.saturating_add(again);
            self.send_reply(call.id, answer, after, epoch);
        }
    }

    /// An answer leaves the lander and reaches the host `after_ms` later.
    fn send_reply(&mut self, call: CallId, answer: CallAnswer, after_ms: u64, epoch: u64) {
        if epoch != self.crashes {
            return;
        }
        if after_ms > 0 {
            let at_ms = self.now_ms().saturating_add(after_ms);
            let reply = Due::Reply {
                call,
                answer,
                epoch,
            };
            self.schedule(at_ms, reply);
            return;
        }
        let now_ms = self.now_ms();
        let replied = Input::Replied {
            now_ms,
            call,
            answer,
        };
        self.answers.push_back(Answer::Reply {
            call,
            input: replied,
        });
    }

    /// The adapter's deadline for `call` passed with no answer: it answers `Unknown`, or tells the
    /// host the call was lost, which the host reads the same.
    fn expire_call(&mut self, call: CallId, epoch: u64) {
        if epoch != self.crashes || !self.open_calls.contains_key(&call.0) {
            return;
        }
        let now_ms = self.now_ms();
        let input = if call.0.is_multiple_of(2) {
            Input::Replied {
                now_ms,
                call,
                answer: CallAnswer::Unknown,
            }
        } else {
            Input::CallLost { now_ms, call }
        };
        self.answers.push_back(Answer::Reply { call, input });
    }

    /// The host receives an answer, or the news that the call was lost. If its dispatch is not the
    /// one in flight, the host must change nothing (invariant I7): what it staged is compared.
    pub(super) fn reply_to_host(&mut self, call: CallId, input: Input) {
        if self.store.staged().is_some() {
            let fault = format!("call {} was answered with a write outstanding", call.0);
            self.faults.push(fault);
        }
        let open = self.open_calls.remove(&call.0);
        if let Some(open) = &open {
            if is_failure(&input) {
                self.lander.charge(open);
            }
            self.answered_calls.insert(call.0, open.clone());
        }
        let known = open.or_else(|| self.answered_calls.get(&call.0).cloned());
        let probe = known.and_then(|known| self.probe_for(&known));
        self.input(input);
        if let Some(probe) = probe {
            self.settle(probe);
        }
    }

    /// The state to compare with, if `call` is not the dispatch the stored state has in flight.
    fn probe_for(&self, call: &Call) -> Option<Probe> {
        let state = self.stored_state_value()?;
        let pointer = match call.request {
            Request::Prepare { .. }
            | Request::Test { .. }
            | Request::Land { .. }
            | Request::Reconcile { .. } => "/merge_in_flight/dispatch",
            Request::Trial { .. } => "/verification_in_flight/dispatch",
            Request::ReadHead { .. } => return None,
        };
        let in_flight = state.pointer(pointer).and_then(Value::as_u64);
        if in_flight == Some(call.dispatch_id.0) {
            return None;
        }
        Some(Probe {
            call: call.id.0,
            dispatch: call.dispatch_id.0,
            state: without_clock(&state),
        })
    }

    fn settle(&mut self, probe: Probe) {
        let changed = self.store.staged().and_then(|entries| {
            entries
                .iter()
                .find_map(|(key, json)| stored_difference(key, json, &probe.state))
        });
        self.stale_answers.push(StaleAnswer {
            call: probe.call,
            dispatch: probe.dispatch,
            changed,
        });
    }

    /// A restart or a failed commit struck: the core may count it against the merge in flight, so
    /// it is charged to that merge's fork commit.
    pub(super) fn strike_merge_in_flight(&mut self) {
        let Some(state) = self.stored_state_value() else {
            return;
        };
        let Some(claim) = state
            .pointer("/merge_in_flight/claim")
            .and_then(Value::as_u64)
        else {
            return;
        };
        let pointer = format!("/claims/{claim}/work/fork_commit");
        if let Some(fork) = state.pointer(&pointer).and_then(Value::as_str) {
            *self.fork_strikes.entry(fork.to_string()).or_insert(0) += 1;
        }
    }

    /// Restarts and failed commits that struck during a merge of each fork commit.
    pub fn fork_strikes(&self) -> &BTreeMap<String, u64> {
        &self.fork_strikes
    }

    /// Feed the stored log to the counters the host keeps, from where they left off.
    pub(super) fn count_events(&mut self) {
        let from = self.tally.next_seq();
        let (events, _) = self.store.events();
        let page: Vec<_> = events
            .iter()
            .filter(|event| event.seq >= from)
            .cloned()
            .collect();
        self.tally.add_page(&page);
    }
}

/// How a staged entry differs from the state before it, if it does.
fn stored_difference(key: &str, json: &str, before: &Value) -> Option<String> {
    if key.starts_with(EVENT_PREFIX) {
        return Some(format!("wrote the event {key}"));
    }
    if key != STATE_KEY {
        return Some(format!("wrote {key}"));
    }
    let after: Value = serde_json::from_str(json).ok()?;
    (without_clock(&after) != *before).then(|| "changed the stored state".to_string())
}

/// Whether the host takes `input` as an attempt that did not finish: an answer of `Unknown`, a
/// candidate that is gone, or a call lost. The core counts each against the submission.
fn is_failure(input: &Input) -> bool {
    match input {
        Input::Replied { answer, .. } => match answer {
            CallAnswer::Unknown | CallAnswer::CandidateMissing => true,
            CallAnswer::Prepared { .. }
            | CallAnswer::Tested
            | CallAnswer::Landed { .. }
            | CallAnswer::Trialled { .. }
            | CallAnswer::Head { .. }
            | CallAnswer::Reconciled { .. }
            | CallAnswer::Refused { .. } => false,
        },
        Input::CallLost { .. } => true,
        Input::Opened { .. }
        | Input::Frame { .. }
        | Input::Binary { .. }
        | Input::Closed { .. }
        | Input::SendFailed { .. }
        | Input::Wake { .. }
        | Input::TrunkPoked { .. }
        | Input::Loaded { .. }
        | Input::ReplayPage { .. }
        | Input::Committed { .. }
        | Input::CommitFailed { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::host::HostConfig;
    use crate::protocol::{
        ClaimId, ClientMsg, CommitId, DecisionRecord, EventKind, Fence, Intent, Mode, OnConflict,
        RequestId, Scope, ScopeClaim,
    };
    use crate::sim::check::{self, Checker};
    use crate::sim::driver::{RequestKind, Sim};
    use crate::sim::variant::Variant;

    const SLOW_MS: u64 = 2_000;

    fn file(path: &str) -> ScopeClaim {
        ScopeClaim {
            scope: Scope::File {
                path: path.to_string(),
            },
            mode: Mode::EditBody,
        }
    }

    fn claim(req: u64, path: &str) -> ClientMsg {
        ClientMsg::Claim {
            req: RequestId(req),
            intent: Intent {
                summary: "work".to_string(),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes: vec![file(path)],
            on_conflict: OnConflict::Fail,
        }
    }

    fn submit(req: u64, claim: ClaimId, fence: Fence, path: &str) -> ClientMsg {
        ClientMsg::Submit {
            req: RequestId(req),
            claim,
            fence,
            fork_commit: CommitId(format!("{req:040x}")),
            touched: vec![file(path)],
            decisions: DecisionRecord {
                rejected: Vec::new(),
                evidence: vec!["tests passed".to_string()],
                transcript: None,
            },
        }
    }

    fn merged(sim: &Sim) -> bool {
        sim.store()
            .events()
            .0
            .iter()
            .any(|event| matches!(event.kind, EventKind::Merged { .. }))
    }

    /// One agent claims a file and has it merged, every reply late and then repeated.
    fn merged_with_repeated_replies() -> Sim {
        let config = HostConfig {
            repo: "sim".to_string(),
            run: Some("sim".to_string()),
            ..HostConfig::default()
        };
        let mut sim = Sim::new(config, Variant::Correct);
        let agent = sim.connect("a");
        assert!(sim.quiet());
        sim.slow_replies(SLOW_MS);
        sim.repeat_replies(SLOW_MS / 2);
        sim.send(agent, &claim(1, "src/a.rs"), RequestKind::Claim, None);
        assert!(sim.quiet());
        let (claim, fence) = {
            let view = sim.view("a").expect("the agent connected");
            let (claim, fence) = view.held.iter().next().expect("the agent holds a claim");
            (ClaimId(*claim), *fence)
        };
        let work = submit(2, claim, fence, "src/a.rs");
        sim.send(agent, &work, RequestKind::Submit, Some(claim));
        for _ in 0..60 {
            sim.advance(SLOW_MS / 2 + 1);
            assert!(sim.quiet());
        }
        sim
    }

    /// One agent submits a commit that the merge path cuts off every time, at its `Prepare` or its
    /// `Test`, and the clock runs past every backoff.
    fn poisoned_commit_run(at_test: bool) -> Sim {
        let config = HostConfig {
            repo: "sim".to_string(),
            run: Some("sim".to_string()),
            ..HostConfig::default()
        };
        let mut sim = Sim::new(config, Variant::Correct);
        let agent = sim.connect("a");
        assert!(sim.quiet());
        sim.send(agent, &claim(1, "src/a.rs"), RequestKind::Claim, None);
        assert!(sim.quiet());
        let (claim, fence) = {
            let view = sim.view("a").expect("the agent connected");
            let (claim, fence) = view.held.iter().next().expect("the agent holds a claim");
            (ClaimId(*claim), *fence)
        };
        sim.poison_fork(&format!("{:040x}", 2), at_test);
        let work = submit(2, claim, fence, "src/a.rs");
        sim.send(agent, &work, RequestKind::Submit, Some(claim));
        for _ in 0..12 {
            sim.advance(100_000);
            assert!(sim.quiet());
        }
        sim
    }

    #[test]
    fn a_poisoned_commit_uses_up_its_tries_and_is_rejected_with_the_cores_reason() {
        for at_test in [false, true] {
            let sim = poisoned_commit_run(at_test);
            let fork = format!("{:040x}", 2);
            assert_eq!(
                sim.lander().cutoffs().get(&fork),
                Some(&4),
                "at test: {at_test}"
            );
            let events = sim.store().events().0.clone();
            let reasons: Vec<&String> = events
                .iter()
                .filter_map(|event| {
                    if let EventKind::SubmitRejected { reason, .. } = &event.kind {
                        return Some(reason);
                    }
                    None
                })
                .collect();
            assert_eq!(reasons.len(), 1, "{reasons:?}");
            assert!(crate::sim::merge_check::is_infra_rejection(reasons[0]));
            assert!(!merged(&sim));
            let found = check::run(&sim, &Checker::ALL);
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    fn a_repeated_reply_is_recorded_as_an_answer_for_a_dispatch_not_in_flight() {
        let sim = merged_with_repeated_replies();
        assert!(merged(&sim), "the script merged nothing");
        let stale = sim.stale_answers();
        assert!(stale.len() >= 3, "{stale:?}");
        assert!(
            stale.iter().all(|answer| answer.changed.is_none()),
            "{stale:?}"
        );
    }

    fn only(sim: &Sim, checker: Checker) -> Vec<String> {
        check::run(sim, &[checker])
            .into_iter()
            .map(|violation| violation.detail)
            .collect()
    }

    #[test]
    fn the_stale_answer_agent_base_and_counter_checkers_read_what_the_driver_records() {
        use crate::host::call::Request;
        use crate::protocol::Event;
        use crate::sim::driver::{Dispatch, StaleAnswer, BASE};

        let mut sim = merged_with_repeated_replies();
        sim.stale_answers.push(StaleAnswer {
            call: 1,
            dispatch: 1,
            changed: Some("changed the stored state".to_string()),
        });
        assert_eq!(only(&sim, Checker::I7).len(), 1);
        sim.dispatches.push(Dispatch {
            request: Request::Land {
                base: CommitId(BASE.to_string()),
                head: CommitId("h".to_string()),
            },
            events_len: 0,
        });
        assert_eq!(only(&sim, Checker::AgentBase).len(), 1);
        assert!(only(&sim, Checker::I11).is_empty());
        let merged = Event {
            seq: 1_000,
            at_ms: 0,
            run: crate::protocol::RunId("r".to_string()),
            kind: EventKind::Merged {
                claim: ClaimId(99),
                head: CommitId("h".to_string()),
                base: None,
                unverified: Vec::new(),
                untried_shadows: Vec::new(),
            },
        };
        sim.tally.add_page(&[merged]);
        assert_eq!(only(&sim, Checker::I11).len(), 1);
    }

    #[test]
    fn a_merge_with_late_and_repeated_replies_keeps_every_checker_silent() {
        let sim = merged_with_repeated_replies();
        let found = check::run(&sim, &Checker::ALL);
        assert!(found.is_empty(), "{found:?}");
    }
}

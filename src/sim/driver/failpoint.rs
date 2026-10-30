//! Failpoints: the moments of a call at which the process can die. A landing is three calls
//! (`Prepare`, `Test`, `Land`) with a marker persisted before each goes out and a write that
//! records each answer, so a restart can find the merge cut off at any of four moments of any
//! step. A failpoint names one, and the driver crashes there once, or every time it comes round.

use crate::host::call::Request;

/// Which call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Prepare,
    Test,
    Land,
    Reconcile,
    Trial,
    ReadHead,
}

impl Step {
    pub(super) fn of(request: &Request) -> Self {
        match request {
            Request::Prepare { .. } => Step::Prepare,
            Request::Test { .. } => Step::Test,
            Request::Land { .. } => Step::Land,
            Request::Reconcile { .. } => Step::Reconcile,
            Request::Trial { .. } => Step::Trial,
            Request::ReadHead { .. } => Step::ReadHead,
        }
    }
}

/// How far a call got when the process died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moment {
    /// The marker that starts the call is durable and the call has not left the host.
    Persisted,
    /// The call is out. The lander may have run it, and any push it made stands, but the host
    /// never hears the answer.
    Running,
    /// The host has the answer and staged the write that records it. The write is lost.
    Answered,
    /// The write that records the answer is durable and nothing after it has run: no notice
    /// reached a client, no next call went out, no wake was set.
    Recorded,
}

/// A step and the moment of it at which to crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failpoint {
    pub step: Step,
    pub moment: Moment,
}

impl Failpoint {
    pub fn new(step: Step, moment: Moment) -> Self {
        Self { step, moment }
    }
}

#[cfg(test)]
mod tests {
    use super::{Failpoint, Moment, Step};
    use crate::host::call::Request;
    use crate::host::HostConfig;
    use crate::merge::MAX_INFRA_RETRIES;
    use crate::protocol::EventKind;
    use crate::protocol::{Assumption, ClaimId, ClientMsg, RequestId, Scope};
    use crate::sim::check::{self, Checker};
    use crate::sim::driver::merge_path::tests::{
        claim, fork, race_run, rejection_reasons, submit, submitted_commit,
    };
    use crate::sim::driver::{RequestKind, Sim};
    use crate::sim::merge_check;
    use crate::sim::{self, Variant};

    const MOMENTS: [Moment; 4] = [
        Moment::Persisted,
        Moment::Running,
        Moment::Answered,
        Moment::Recorded,
    ];

    fn at(step: Step, moment: Moment) -> Failpoint {
        Failpoint::new(step, moment)
    }

    /// The clock runs past every backoff, a round at a time.
    fn run_to_rest(sim: &mut Sim) {
        for _ in 0..12 {
            sim.advance(100_000);
            assert!(sim.quiet());
        }
    }

    fn merged_events(sim: &Sim) -> usize {
        let (events, _) = sim.store().events();
        events
            .iter()
            .filter(|event| merge_check::merged_claim(&event.kind).is_some())
            .count()
    }

    /// The commit landed once: one `Merged`, one commit of it on main, nothing rejected, and
    /// every checker is clean.
    fn assert_landed_once(sim: &Sim, point: Failpoint) {
        assert_eq!(sim.crashes(), 1, "{point:?} did not strike once");
        assert_landed_after_crashes(sim, point);
    }

    /// `assert_landed_once`, whatever number of times the process died.
    fn assert_landed_after_crashes(sim: &Sim, point: Failpoint) {
        assert_eq!(merged_events(sim), 1, "{point:?}: not merged exactly once");
        let landings: Vec<_> = sim
            .lander()
            .history()
            .iter()
            .filter(|landing| landing.fork.as_deref() == Some(fork().as_str()))
            .collect();
        assert_eq!(landings.len(), 1, "{point:?}: {landings:?}");
        let kept = sim.lander().candidate_count();
        assert!(
            kept <= 1,
            "{point:?}: the lander keeps {kept} candidate refs"
        );
        let reasons = rejection_reasons(sim);
        assert!(reasons.is_empty(), "{point:?}: {reasons:?}");
        let found = check::run(sim, &Checker::ALL);
        assert!(found.is_empty(), "{point:?}: {found:?}");
    }

    /// One submission, a crash at `point`, a restart, and the clock run out.
    fn crash_and_land(point: Failpoint, arm: impl FnOnce(&mut Sim)) -> Sim {
        let mut sim = submitted_commit(|sim| {
            arm(sim);
            sim.crash_at(point);
        });
        run_to_rest(&mut sim);
        assert_landed_once(&sim, point);
        sim
    }

    fn crash_and_land_plainly(point: Failpoint) -> Sim {
        crash_and_land(point, |_| {})
    }

    /// How many `Prepare`, `Test`, `Land` and `Reconcile` calls went out: how recovery went on.
    fn calls_sent(sim: &Sim) -> [usize; 4] {
        let mut sent = [0; 4];
        for dispatch in sim.dispatches() {
            match dispatch.request {
                Request::Prepare { .. } => sent[0] += 1,
                Request::Test { .. } => sent[1] += 1,
                Request::Land { .. } => sent[2] += 1,
                Request::Reconcile { .. } => sent[3] += 1,
                Request::Trial { .. } | Request::ReadHead { .. } => {}
            }
        }
        sent
    }

    /// A crash at `point` after which the calls that went out are `sent`: `[Prepare, Test, Land,
    /// Reconcile]`. A recovery that took another road, such as a second `Prepare` where the
    /// journaled head should have been tested again, fails here.
    fn recovers_by(point: Failpoint, sent: [usize; 4]) {
        let sim = crash_and_land_plainly(point);
        assert_eq!(calls_sent(&sim), sent, "{point:?}");
    }

    /// The call at `point` arrives at the lander 5 s after it is issued, so the process dies
    /// with the lander having run `runs` calls; the call is run after the restart, and the claim
    /// lands once.
    fn late_call_lands_once(point: Failpoint, runs: usize) -> Sim {
        let mut sim = submitted_commit(|sim| {
            sim.late_runs(5_000);
            sim.crash_at(point);
        });
        for _ in 0..30 {
            if sim.crashes() > 0 {
                break;
            }
            sim.advance(1_000);
            assert!(sim.quiet());
        }
        assert_eq!(sim.crashes(), 1, "{point:?} did not strike");
        assert_eq!(
            sim.lander().runs(),
            runs,
            "{point:?}: the call ran before the crash"
        );
        run_to_rest(&mut sim);
        assert_landed_once(&sim, point);
        assert!(
            sim.lander().runs() > runs,
            "{point:?}: the call never arrived"
        );
        sim
    }

    #[test]
    fn window_dispatch_persisted_a_restart_before_prepare_goes_out_lands_once() {
        recovers_by(at(Step::Prepare, Moment::Persisted), [1, 1, 1, 0]);
    }

    #[test]
    fn window_prepare_running_a_restart_with_the_head_pushed_lands_once() {
        recovers_by(at(Step::Prepare, Moment::Running), [2, 1, 1, 0]);
    }

    #[test]
    fn window_prepare_running_a_restart_before_the_call_arrives_lands_once() {
        let sim = late_call_lands_once(at(Step::Prepare, Moment::Running), 0);
        assert_eq!(calls_sent(&sim), [2, 1, 1, 0], "the Prepare is sent again");
    }

    #[test]
    fn window_ref_pushed_head_not_journaled_the_answer_lost_lands_once() {
        recovers_by(at(Step::Prepare, Moment::Answered), [2, 1, 1, 0]);
    }

    #[test]
    fn window_head_journaled_a_restart_before_test_goes_out_lands_once() {
        recovers_by(at(Step::Prepare, Moment::Recorded), [1, 1, 1, 0]);
        recovers_by(at(Step::Test, Moment::Persisted), [1, 1, 1, 0]);
    }

    #[test]
    fn window_test_in_flight_a_restart_lands_once() {
        recovers_by(at(Step::Test, Moment::Running), [1, 2, 1, 0]);
        recovers_by(at(Step::Test, Moment::Answered), [1, 2, 1, 0]);
        let sim = late_call_lands_once(at(Step::Test, Moment::Running), 1);
        assert_eq!(
            calls_sent(&sim)[0..2],
            [1, 2],
            "the Test is sent again, not a Prepare"
        );
    }

    #[test]
    fn window_tested_not_landed_a_restart_lands_once() {
        recovers_by(at(Step::Test, Moment::Recorded), [1, 1, 1, 1]);
        recovers_by(at(Step::Land, Moment::Persisted), [1, 1, 1, 1]);
    }

    #[test]
    fn window_land_sent_outcome_not_persisted_a_restart_lands_once() {
        recovers_by(at(Step::Land, Moment::Running), [1, 1, 1, 1]);
        recovers_by(at(Step::Land, Moment::Answered), [1, 1, 1, 1]);
        let sim = late_call_lands_once(at(Step::Land, Moment::Running), 2);
        assert_eq!(
            calls_sent(&sim)[2..],
            [1, 1],
            "a Reconcile settles it, no second Land"
        );
    }

    #[test]
    fn window_outcome_persisted_not_delivered_a_restart_keeps_the_landing_once() {
        recovers_by(at(Step::Land, Moment::Recorded), [1, 1, 1, 0]);
    }

    /// The restart loses the candidate ref: a sweep or an operator took it while the host was down.
    fn crash_and_land_without_refs(point: Failpoint, sent: [usize; 4]) {
        let mut sim = submitted_commit(|sim| {
            sim.lose_candidates_in_restarts();
            sim.crash_at(point);
        });
        run_to_rest(&mut sim);
        assert_landed_once(&sim, point);
        assert_eq!(calls_sent(&sim), sent, "{point:?}");
    }

    #[test]
    fn window_ref_lost_between_prepare_and_test_main_at_base_lands_once() {
        crash_and_land_without_refs(at(Step::Test, Moment::Persisted), [2, 2, 1, 0]);
    }

    #[test]
    fn window_ref_lost_between_test_and_land_main_at_base_lands_once() {
        crash_and_land_without_refs(at(Step::Land, Moment::Persisted), [2, 2, 2, 1]);
    }

    #[test]
    fn window_ref_lost_with_main_at_the_head_is_landed_not_prepared_again() {
        crash_and_land_without_refs(at(Step::Land, Moment::Running), [1, 1, 1, 1]);
    }

    /// The submission is cut off every time it reaches `point`; it is rejected for infrastructure
    /// once, after exactly the bound, and nothing lands.
    fn cut_off_every_time(point: Failpoint) -> Sim {
        let mut sim = submitted_commit(|sim| sim.crash_at_every(point));
        run_to_rest(&mut sim);
        let reasons = rejection_reasons(&sim);
        assert_eq!(reasons.len(), 1, "{point:?}: {reasons:?}");
        assert!(merge_check::is_infra_rejection(&reasons[0]), "{reasons:?}");
        assert_eq!(merged_events(&sim), 0, "{point:?}: merged");
        assert!(sim.lander().history().is_empty(), "{point:?}: landed");
        let found = check::run(&sim, &Checker::ALL);
        assert!(found.is_empty(), "{point:?}: {found:?}");
        sim
    }

    #[test]
    fn i6_a_restart_during_every_prepare_is_rejected_after_the_bound() {
        let sim = cut_off_every_time(at(Step::Prepare, Moment::Running));
        assert_eq!(sim.crashes(), u64::from(MAX_INFRA_RETRIES) + 1);
    }

    #[test]
    fn i6_a_restart_during_every_test_is_rejected_after_the_bound() {
        let sim = cut_off_every_time(at(Step::Test, Moment::Running));
        assert_eq!(sim.crashes(), u64::from(MAX_INFRA_RETRIES) + 1);
    }

    #[test]
    fn i6_a_restart_after_every_recorded_test_is_not_a_cut_off_and_the_claim_lands() {
        let point = at(Step::Test, Moment::Recorded);
        let mut sim = submitted_commit(|sim| sim.crash_at_every(point));
        run_to_rest(&mut sim);
        assert_landed_once(&sim, point);
    }

    /// A cut-off at the `Land` stage is outside I6: nothing after `Land` counts toward the
    /// bound. Each restart finds the marker and reconciles at once, the `Land` is due again and
    /// the process dies before it goes out. The claim stays in flight, never rejected, and lands
    /// once the process survives it (an operator ends the wait with `resolve_doubt`).
    #[test]
    fn a_land_marker_cut_off_every_time_is_never_rejected() {
        let point = at(Step::Land, Moment::Persisted);
        let restarts = u64::from(MAX_INFRA_RETRIES) + 3;
        let mut sim = submitted_commit(|sim| sim.crash_at_every(point));
        for _ in 0..30 {
            if sim.crashes() >= restarts {
                break;
            }
            sim.advance(100_000);
            sim.quiet();
        }
        assert!(sim.crashes() >= restarts, "only {} restarts", sim.crashes());
        assert!(
            rejection_reasons(&sim).is_empty(),
            "a doubt rejected the claim"
        );
        assert_eq!(
            merged_events(&sim),
            0,
            "landed while the marker kept cutting it off"
        );
        assert_eq!(calls_sent(&sim)[2], 0, "a Land went out past its marker");
        sim.disarm();
        run_to_rest(&mut sim);
        assert_landed_after_crashes(&sim, point);
    }

    #[test]
    fn window_rescheduling_a_restart_after_a_cut_off_prepare_is_recorded_rejects_once() {
        let point = at(Step::Prepare, Moment::Recorded);
        let mut sim = submitted_commit(|sim| {
            sim.poison_fork(&fork(), false);
            sim.crash_at(point);
        });
        run_to_rest(&mut sim);
        assert_eq!(sim.crashes(), 1);
        let attempts = usize::try_from(MAX_INFRA_RETRIES).expect("a small bound") + 1;
        assert_eq!(calls_sent(&sim), [attempts, 0, 0, 0]);
        let reasons = rejection_reasons(&sim);
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        assert!(merge_check::is_infra_rejection(&reasons[0]), "{reasons:?}");
        assert_eq!(merged_events(&sim), 0);
        let found = check::run(&sim, &Checker::ALL);
        assert!(found.is_empty(), "{found:?}");
    }

    /// The process dies at each of `points` in turn, the later ones while it recovers from the
    /// earlier ones, and the claim still lands once. The calls that went out are `sent`:
    /// `[Prepare, Test, Land, Reconcile]`.
    fn recover_through(points: &[Failpoint], sent: [usize; 4]) {
        let mut sim = submitted_commit(|sim| sim.crash_at_each(points));
        run_to_rest(&mut sim);
        assert_eq!(sim.crashes(), points.len() as u64, "{points:?}");
        assert_landed_after_crashes(&sim, points[points.len() - 1]);
        assert_eq!(calls_sent(&sim), sent, "{points:?}");
    }

    #[test]
    fn window_reconcile_in_flight_a_restart_lands_once() {
        for (moment, after_running, after_persisted) in [
            (Moment::Persisted, [1, 1, 1, 1], [1, 1, 1, 1]),
            (Moment::Running, [1, 1, 1, 2], [1, 1, 1, 2]),
            (Moment::Answered, [1, 1, 1, 2], [1, 1, 1, 2]),
            (Moment::Recorded, [1, 1, 1, 1], [1, 1, 1, 2]),
        ] {
            let reconcile = at(Step::Reconcile, moment);
            recover_through(&[at(Step::Land, Moment::Running), reconcile], after_running);
            recover_through(
                &[at(Step::Land, Moment::Persisted), reconcile],
                after_persisted,
            );
        }
    }

    #[test]
    fn a_second_restart_while_recovering_still_lands_once() {
        recover_through(
            &[
                at(Step::Prepare, Moment::Running),
                at(Step::Prepare, Moment::Persisted),
            ],
            [2, 1, 1, 0],
        );
        recover_through(
            &[
                at(Step::Test, Moment::Running),
                at(Step::Test, Moment::Running),
            ],
            [1, 3, 1, 0],
        );
    }

    /// A race whose entrant submits, with the process dying at `point` after it did: the race is
    /// decided once and the winner lands once.
    fn race_through(point: Failpoint) -> Sim {
        let sim = race_run(0, |sim| sim.crash_at(point));
        assert_eq!(sim.crashes(), 1, "{point:?} did not strike once");
        let (events, _) = sim.store().events();
        let decided = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::RaceDecided { .. }))
            .count();
        assert_eq!(decided, 1, "{point:?}: race decided {decided} times");
        assert_eq!(merged_events(&sim), 1, "{point:?}: not merged exactly once");
        let found = check::run(&sim, &Checker::ALL);
        assert!(found.is_empty(), "{point:?}: {found:?}");
        sim
    }

    #[test]
    fn window_trial_in_flight_a_restart_decides_the_race_once() {
        for moment in [
            Moment::Persisted,
            Moment::Running,
            Moment::Answered,
            Moment::Recorded,
        ] {
            race_through(at(Step::Trial, moment));
        }
    }

    #[test]
    fn window_read_head_in_flight_a_restart_decides_the_race_once_and_reads_again() {
        let calm = read_heads_sent(&race_run(0, |_| {}));
        for (moment, resent) in [
            (Moment::Persisted, 0),
            (Moment::Running, 1),
            (Moment::Answered, 1),
            (Moment::Recorded, 0),
        ] {
            let point = at(Step::ReadHead, moment);
            let sim = race_through(point);
            assert_eq!(read_heads_sent(&sim), calm + resent, "{point:?}");
        }
    }

    /// Agent `b` claims `src/b.rs` assuming something of `src/a.rs`, then agent `a` claims
    /// `src/a.rs` and submits: the landing queues a verification of `b`'s assumption, and a trial
    /// of it goes to the lander. `arm` runs just before `a` submits.
    fn verified_run(arm: impl FnOnce(&mut Sim)) -> Sim {
        let config = HostConfig {
            repo: "sim".to_string(),
            run: Some("sim".to_string()),
            reviewers: Some("reviewer".to_string()),
            ..HostConfig::default()
        };
        let mut sim = Sim::new(config, Variant::Correct);
        let (a, b) = (sim.connect("a"), sim.connect("b"));
        assert!(sim.quiet());
        let ClientMsg::Claim {
            req,
            mut intent,
            scopes,
            on_conflict,
        } = claim(1, "src/b.rs")
        else {
            unreachable!("claim builds a Claim");
        };
        intent.assumptions = vec![Assumption {
            scope: Scope::File {
                path: "src/a.rs".to_string(),
            },
            statement: "a.rs keeps its behaviour".to_string(),
        }];
        let assuming = ClientMsg::Claim {
            req,
            intent,
            scopes,
            on_conflict,
        };
        sim.send(b, &assuming, RequestKind::Claim, None);
        sim.send(a, &claim(2, "src/a.rs"), RequestKind::Claim, None);
        assert!(sim.quiet());
        let (held, fence) = {
            let view = sim.view("a").expect("the agent connected");
            let (held, fence) = view.held.iter().next().expect("a holds a claim");
            (ClaimId(*held), *fence)
        };
        arm(&mut sim);
        sim.send(
            a,
            &submit(3, held, fence, "src/a.rs"),
            RequestKind::Submit,
            Some(held),
        );
        assert!(sim.quiet());
        let reviewer = sim.connect("reviewer");
        let approve = ClientMsg::Review {
            req: RequestId(4),
            claim: held,
            approve: true,
            note: None,
        };
        sim.send(reviewer, &approve, RequestKind::Other, None);
        for _ in 0..12 {
            sim.advance(20_000);
            assert!(sim.quiet());
            keep_alive(&mut sim, &["a", "b"]);
        }
        sim
    }

    /// Each of `agents` sends a heartbeat, on a new socket if a restart closed theirs, so the
    /// claims the run depends on do not lapse while the clock runs.
    fn keep_alive(sim: &mut Sim, agents: &[&str]) {
        for agent in agents {
            if sim.view(agent).and_then(|view| view.sock).is_none() {
                sim.connect(agent);
                assert!(sim.quiet());
            }
            if let Some(sock) = sim.view(agent).and_then(|view| view.sock) {
                sim.send(sock, &ClientMsg::Heartbeat, RequestKind::Other, None);
            }
        }
        assert!(sim.quiet());
    }

    fn trials_sent(sim: &Sim) -> usize {
        let mut sent = 0;
        for dispatch in sim.dispatches() {
            if let Request::Trial { .. } = dispatch.request {
                sent += 1;
            }
        }
        sent
    }

    #[test]
    fn window_verification_trial_in_flight_a_restart_tries_it_again_once() {
        let calm = verified_run(|_| {});
        let tried = trials_sent(&calm);
        assert!(tried >= 1, "no trial went out: {:?}", calm.dispatches());
        for (moment, resent) in [
            (Moment::Persisted, 0),
            (Moment::Running, 1),
            (Moment::Answered, 1),
            (Moment::Recorded, 0),
        ] {
            let point = at(Step::Trial, moment);
            let sim = verified_run(|sim| sim.crash_at(point));
            assert_eq!(sim.crashes(), 1, "{point:?} did not strike once");
            assert_eq!(merged_events(&sim), 1, "{point:?}: not merged exactly once");
            assert_eq!(trials_sent(&sim), tried + resent, "{point:?}");
            let found = check::run(&sim, &Checker::ALL);
            assert!(found.is_empty(), "{point:?}: {found:?}");
        }
    }

    fn verification_queue_is_empty(sim: &Sim) -> bool {
        let state: serde_json::Value = serde_json::from_str(sim.store().state().unwrap_or("{}"))
            .expect("the stored state is JSON");
        let queued = state.get("verifications").and_then(|v| v.as_array());
        let running = state.get("verification_in_flight");
        queued.is_some_and(Vec::is_empty) && running.is_none_or(serde_json::Value::is_null)
    }

    #[test]
    fn i6_a_restart_during_every_verification_trial_ends_it_inconclusive_after_the_bound() {
        let point = at(Step::Trial, Moment::Running);
        let sim = verified_run(|sim| sim.crash_at_every(point));
        assert_eq!(sim.crashes(), u64::from(MAX_INFRA_RETRIES) + 1);
        assert_eq!(merged_events(&sim), 1, "the merge itself landed once");
        assert!(
            verification_queue_is_empty(&sim),
            "the verification never ended"
        );
        let found = check::run(&sim, &Checker::ALL);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn i6_a_restart_during_every_race_trial_decides_the_race_once_after_the_bound() {
        let point = at(Step::Trial, Moment::Running);
        let sim = race_run(0, |sim| sim.crash_at_every(point));
        assert_eq!(sim.crashes(), u64::from(MAX_INFRA_RETRIES) + 1);
        let (events, _) = sim.store().events();
        let decided = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::RaceDecided { .. }))
            .count();
        assert_eq!(decided, 1, "the race was decided {decided} times");
        let found = check::run(&sim, &Checker::ALL);
        assert!(found.is_empty(), "{found:?}");
    }

    fn read_heads_sent(sim: &Sim) -> usize {
        let mut sent = 0;
        for dispatch in sim.dispatches() {
            if let Request::ReadHead { .. } = dispatch.request {
                sent += 1;
            }
        }
        sent
    }

    #[test]
    fn the_seeds_crash_at_every_moment_of_prepare_test_land_and_reconcile() {
        let mut reached = Vec::new();
        for seed in 0..180 {
            let run = sim::run_seed(seed, Variant::Correct, &[]);
            reached.extend(run.failpoints);
        }
        for step in [Step::Prepare, Step::Test, Step::Land, Step::Reconcile] {
            for moment in MOMENTS {
                let point = at(step, moment);
                assert!(reached.contains(&point), "no seed crashed at {point:?}");
            }
        }
    }
}

//! Port of `tessel-steward/src/trial-report.test.ts` (`reportTrial`): 7 `it`, no `it.each`. The
//! title of each test is the TS title in snake case, so its original is recognisable. The runner
//! is a closure standing in for "one trial in a sandbox of its own".
//!
//! The last test is not a port: the TS single-run case only ever ran a clean outcome.
//!
//! The two runs are threads, so the order of the calls is not fixed: the first test compares the
//! calls as a set where the TS test compared them as a list.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use tessel_lander::merge::MergeError;
use tessel_lander::outcome::{PackageStep, StepOutcome, TrialOutcome, TrialReport};
use tessel_lander::trial::{report_trial, TrialPlan};
use tessel_lander::types::Sha;

const WAIT: Duration = Duration::from_secs(5);

fn sha(character: char) -> Sha {
    Sha::parse(&character.to_string().repeat(40)).expect("test sha")
}

fn before() -> Sha {
    sha('1')
}
fn main_sha() -> Sha {
    sha('2')
}
fn commit() -> Sha {
    sha('3')
}

fn plan() -> TrialPlan {
    TrialPlan {
        before: before(),
        main: main_sha(),
        commit: commit(),
    }
}

fn clean(base: &Sha) -> TrialOutcome {
    TrialOutcome::Clean {
        base: base.clone(),
        head: sha('9'),
        commit: commit(),
    }
}

fn container_died() -> MergeError {
    MergeError::ReadAccessNotRevoked
}

#[test]
fn runs_both_sides_with_the_one_commit_it_was_given() {
    let calls = Mutex::new(Vec::new());
    let runner = |main: &Sha, tried: &Sha| {
        calls
            .lock()
            .expect("lock")
            .push((main.clone(), tried.clone()));
        Ok(clean(main))
    };

    let report = report_trial(&plan(), &runner).expect("the report");

    let mut calls = calls.into_inner().expect("lock");
    calls.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    assert_eq!(
        calls,
        [(before(), commit()), (main_sha(), commit())],
        "each side tries the one commit"
    );
    assert_eq!(
        report,
        TrialReport {
            before: clean(&before()),
            after: Some(clean(&main_sha())),
        }
    );
}

/// A runner that says when a side started and waits to be let go.
struct Gated {
    started: Sender<Sha>,
    release: Mutex<Receiver<()>>,
}

impl Gated {
    fn run(&self, main: &Sha) -> TrialOutcome {
        self.started.send(main.clone()).expect("the test listens");
        let _ = self.release.lock().expect("lock").recv_timeout(WAIT * 2);
        clean(main)
    }
}

#[test]
fn starts_both_sides_before_either_has_finished() {
    let (started_tx, started_rx) = mpsc::channel();
    let (before_release, before_gate) = mpsc::channel();
    let (main_release, main_gate) = mpsc::channel();
    let gated = |tx: &Sender<Sha>, rx| Gated {
        started: tx.clone(),
        release: Mutex::new(rx),
    };
    let (before_run, main_run) = (
        gated(&started_tx, before_gate),
        gated(&started_tx, main_gate),
    );
    let runner = |main: &Sha, _commit: &Sha| {
        Ok(if *main == before() {
            before_run.run(main)
        } else {
            main_run.run(main)
        })
    };

    let report = thread::scope(|scope| {
        let pending = scope.spawn(|| report_trial(&plan(), &runner));
        let mut started = vec![
            started_rx.recv_timeout(WAIT).expect("a side started"),
            started_rx
                .recv_timeout(WAIT)
                .expect("the other side started while the first was still running"),
        ];
        started.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(started, [before(), main_sha()]);
        main_release.send(()).expect("release main");
        before_release.send(()).expect("release before");
        pending.join().expect("the report thread")
    })
    .expect("the report");

    assert_eq!(
        report,
        TrialReport {
            before: clean(&before()),
            after: Some(clean(&main_sha())),
        }
    );
}

fn failing_baselines() -> Vec<TrialOutcome> {
    vec![
        TrialOutcome::CommitNotInFork,
        TrialOutcome::MainUnreachable { main: before() },
        TrialOutcome::NothingToTest {
            base: before(),
            commit: commit(),
        },
        TrialOutcome::Conflict {
            base: before(),
            commit: commit(),
            files: vec!["a".to_owned()],
        },
    ]
}

#[test]
fn discards_mains_result_when_the_baseline_is_not_clean_even_if_main_passed() {
    for failing in failing_baselines() {
        let calls = Mutex::new(0);
        let runner = |main: &Sha, _commit: &Sha| {
            *calls.lock().expect("lock") += 1;
            Ok(if *main == before() {
                failing.clone()
            } else {
                clean(main)
            })
        };

        let report = report_trial(&plan(), &runner).expect("the report");

        assert_eq!(
            report,
            TrialReport {
                before: failing.clone(),
                after: None,
            }
        );
        assert_eq!(*calls.lock().expect("lock"), 2);
    }
}

#[test]
fn is_inconclusive_without_rethrowing_when_the_baseline_is_not_clean_and_mains_run_throws() {
    let failed = TrialOutcome::TestsFailed {
        base: before(),
        head: sha('9'),
        commit: commit(),
        result: StepOutcome::finished(PackageStep::Test, 1, "", ""),
    };
    let runner = |main: &Sha, _commit: &Sha| {
        if *main == before() {
            Ok(failed.clone())
        } else {
            Err(container_died())
        }
    };

    let report = report_trial(&plan(), &runner).expect("the report");

    assert_eq!(
        report,
        TrialReport {
            before: failed.clone(),
            after: None,
        }
    );
}

#[test]
fn makes_one_run_which_is_both_sides_when_the_baseline_is_main_itself() {
    let calls = Mutex::new(0);
    let runner = |main: &Sha, _commit: &Sha| {
        *calls.lock().expect("lock") += 1;
        Ok(clean(main))
    };
    let same = TrialPlan {
        before: main_sha(),
        ..plan()
    };

    let report = report_trial(&same, &runner).expect("the report");

    assert_eq!(*calls.lock().expect("lock"), 1);
    assert_eq!(report.after.as_ref(), Some(&report.before));
}

#[test]
fn reports_what_mains_run_says_whatever_it_is() {
    let after = TrialOutcome::CommitNotInFork;
    let runner = |main: &Sha, _commit: &Sha| {
        Ok(if *main == before() {
            clean(main)
        } else {
            after.clone()
        })
    };

    let report = report_trial(&plan(), &runner).expect("the report");

    assert_eq!(
        report,
        TrialReport {
            before: clean(&before()),
            after: Some(after.clone()),
        }
    );
}

#[test]
fn throws_the_first_error_only_after_both_runs_have_finished() {
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let main_finished = AtomicBool::new(false);
    let runner = |main: &Sha, _commit: &Sha| {
        if *main == before() {
            return Err(container_died());
        }
        let _ = gate.lock().expect("lock").recv_timeout(WAIT);
        main_finished.store(true, Ordering::SeqCst);
        Ok(clean(main))
    };
    let settled = AtomicBool::new(false);

    thread::scope(|scope| {
        let pending = scope.spawn(|| {
            let result = report_trial(&plan(), &runner);
            settled.store(true, Ordering::SeqCst);
            result
        });
        thread::sleep(Duration::from_millis(50));
        assert!(
            !settled.load(Ordering::SeqCst),
            "the error came out while main's run was still going"
        );
        release.send(()).expect("release main");
        let error = pending
            .join()
            .expect("the report thread")
            .expect_err("the baseline's error");
        assert!(matches!(error, MergeError::ReadAccessNotRevoked), "{error}");
    });
    assert!(main_finished.load(Ordering::SeqCst));
}

// ---- not in the TS suite ----

#[test]
fn keeps_no_main_side_when_the_one_run_of_the_baseline_that_is_main_itself_is_not_clean() {
    let runner = |_main: &Sha, _commit: &Sha| Ok(TrialOutcome::CommitNotInFork);
    let same = TrialPlan {
        before: main_sha(),
        ..plan()
    };

    let report = report_trial(&same, &runner).expect("the report");

    assert_eq!(
        report,
        TrialReport {
            before: TrialOutcome::CommitNotInFork,
            after: None,
        }
    );
}

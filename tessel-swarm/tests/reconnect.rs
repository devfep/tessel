//! Connection resets against the local target: every socket ends at once, with no close frame, as
//! the 19:39 live run's did. Agents and the scripted reviewer reopen their connections, learn from
//! the coordinator's log where they stand, and the run finishes with every task accounted for.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tessel_coordinator::protocol::{ClaimId, Event, EventKind, ReleaseReason};
use tessel_swarm::conn::{Reconnect, MAX_RESETS_PER_TASK};
use tessel_swarm::demo;
use tessel_swarm::guard::ScratchRepo;
use tessel_swarm::local::{self, LocalServer, LocalSetup};
use tessel_swarm::on::{self, OnConfig, OnResult, Policy, Resolution, REVIEWER};
use tessel_swarm::tasks::{Kind, Task};

/// Short pauses, so that a test does not wait out the live schedule (0.5 s doubling to 8 s).
const QUICK: Reconnect = Reconnect {
    tries: 5,
    first_delay: Duration::from_millis(20),
};

/// For tests that keep the agent out until something happens in the log. The pauses stop growing
/// at 8 s, so 40 tries outlast a refusal of about two minutes on average: a machine many times
/// slower than a laptop does not run them out before the refusal is lifted. A test that passes
/// never waits them out, and one that hangs is stopped by `BOUND`.
const PATIENT: Reconnect = Reconnect {
    tries: 40,
    first_delay: Duration::from_millis(200),
};

/// No test may take longer than this, whatever goes wrong.
const BOUND: Duration = Duration::from_secs(90);

fn body(id: usize, func: &str) -> Task {
    Task {
        id,
        func: func.into(),
        kind: Kind::Body,
    }
}

fn signature(id: usize, func: &str) -> Task {
    Task {
        id,
        func: func.into(),
        kind: Kind::Signature,
    }
}

fn config(agents: usize, work_ms: u64, reconnect: Reconnect) -> OnConfig {
    OnConfig {
        agents,
        policy: Policy::Wait,
        work_ms,
        task_timeout: Duration::from_secs(60),
        trial_wait: Duration::from_secs(60),
        heartbeat_every: Duration::from_millis(100),
        max_denials: 400,
        scripted_reviewer: true,
        reconnect,
    }
}

async fn start(config: &OnConfig, scratch: &std::path::Path, lease_ms: u64) -> LocalServer {
    let repo = ScratchRepo::parse("swarm-test").unwrap();
    let names = on::principals(config.agents, config.scripted_reviewer);
    let reviewers: Vec<String> = if config.scripted_reviewer {
        vec![REVIEWER.to_string()]
    } else {
        Vec::new()
    };
    let base = demo::base_tree();
    LocalServer::start(LocalSetup {
        repo: &repo,
        scratch: &scratch.join("server"),
        base: &base,
        names: &names,
        reviewers: &reviewers,
        shadow_enabled: config.policy == Policy::Shadow,
        lease_ms,
    })
    .await
    .unwrap()
}

/// Awaits `run`, which must finish within `BOUND`: a hang fails the test instead of the suite.
async fn bounded<T>(run: impl std::future::Future<Output = T>) -> T {
    let Ok(done) = tokio::time::timeout(BOUND, run).await else {
        unreachable!("the run did not finish within {BOUND:?}");
    };
    done
}

/// What a run on a server left behind.
struct Outcome {
    server: LocalServer,
    result: OnResult,
    _scratch: tempfile::TempDir,
}

/// Starts a server, lets `arm` set up the cut, runs the tasks, and bounds the whole run.
async fn run_with(
    tasks: &[Task],
    config: &OnConfig,
    lease_ms: u64,
    arm: impl FnOnce(&LocalServer),
) -> Outcome {
    let scratch = tempfile::tempdir().unwrap();
    let server = start(config, scratch.path(), lease_ms).await;
    arm(&server);
    let agents = scratch.path().join("agents");
    let run = Box::pin(on::run_on(&server.endpoint, tasks, &agents, config));
    let result = bounded(run).await.unwrap();
    Outcome {
        server,
        result,
        _scratch: scratch,
    }
}

fn count(events: &[Event], wanted: impl Fn(&EventKind) -> bool) -> usize {
    events.iter().filter(|e| wanted(&e.kind)).count()
}

fn connections_of(events: &[Event], who: &str) -> usize {
    count(
        events,
        |k| matches!(k, EventKind::AgentConnected { agent } if agent.0 == who),
    )
}

/// Every task is accounted for exactly once, and each outcome is one of the four.
fn assert_accounted(result: &OnResult, tasks: usize) {
    assert_eq!(result.results.len(), tasks, "{:?}", result.results);
    for r in &result.results {
        assert!(
            matches!(
                r.result,
                Resolution::Merged
                    | Resolution::Rejected
                    | Resolution::Lapsed
                    | Resolution::Disconnected
            ),
            "never silently lost: {r:?}"
        );
    }
}

#[test]
fn the_longest_pause_before_each_try_doubles_from_half_a_second_and_stops_at_eight() {
    let ceilings: Vec<u128> = (0..7)
        .map(|attempt| Reconnect::STANDARD.ceiling(attempt).as_millis())
        .collect();
    assert_eq!(ceilings, [500, 1000, 2000, 4000, 8000, 8000, 8000]);
    assert_eq!(Reconnect::STANDARD.tries, 5);
}

#[test]
fn each_pause_is_a_random_share_of_its_ceiling() {
    let mut seen = std::collections::HashSet::new();
    for attempt in 0..7 {
        let ceiling = Reconnect::STANDARD.ceiling(attempt);
        for _ in 0..50 {
            let pause = Reconnect::STANDARD.delay(attempt);
            assert!(pause <= ceiling, "{pause:?} over {ceiling:?}");
            seen.insert(pause);
        }
    }
    assert!(seen.len() > 100, "the pauses vary: {} distinct", seen.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_socket_reset_while_agents_queue_and_one_works_the_run_completes() {
    // a01 holds unitPrice and works; a02 and a03 are queued behind it when everything is cut.
    let tasks = [
        body(1, "unitPrice"),
        body(2, "unitPrice"),
        body(3, "unitPrice"),
    ];
    let queued = Arc::new(AtomicUsize::new(0));
    let second_wait = move |e: &Event| {
        matches!(e.kind, EventKind::WaitQueued { .. }) && queued.fetch_add(1, Ordering::SeqCst) == 1
    };
    let cfg = config(3, 1000, QUICK);
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        server.cutter().reset_on(second_wait, Vec::new());
    })
    .await;
    let result = &run.result;
    assert_accounted(result, 3);
    let outcomes: Vec<Resolution> = result.results.iter().map(|r| r.result).collect();
    assert_eq!(outcomes, [Resolution::Merged; 3], "{:?}", result.results);
    assert_eq!(result.summary.merges, 3);
    let withdrawn = count(&result.events, |k| {
        matches!(k, EventKind::WaitWithdrawn { .. })
    });
    assert_eq!(
        withdrawn, 2,
        "both queued requests were withdrawn by the cut"
    );
    for r in &result.results {
        assert!(
            r.reconnects >= 1,
            "every agent was cut and came back: {r:?}"
        );
    }
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reviewer_reconnects_and_decides_a_held_submission_exactly_once() {
    // The cut lands in the step that logs the review request, before the reviewer can answer.
    let tasks = [signature(1, "unitPrice"), body(2, "restock")];
    let cfg = config(2, 200, QUICK);
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        let held = |e: &Event| matches!(e.kind, EventKind::ReviewRequested { .. });
        server.cutter().reset_on(held, Vec::new());
    })
    .await;
    let result = &run.result;
    assert_accounted(result, 2);
    let outcomes: Vec<Resolution> = result.results.iter().map(|r| r.result).collect();
    assert_eq!(outcomes, [Resolution::Merged; 2], "{:?}", result.results);
    let requested = count(&result.events, |k| {
        matches!(k, EventKind::ReviewRequested { .. })
    });
    let decided = count(&result.events, |k| {
        matches!(k, EventKind::ReviewDecided { .. })
    });
    assert_eq!((requested, decided), (1, 1), "decided once, not missed");
    assert_eq!(result.reviews_held, 0);
    assert!(
        connections_of(&result.events, REVIEWER) >= 2,
        "the reviewer reconnected: {:?}",
        result.events
    );
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_review_requested_while_the_reviewer_was_gone_is_found_when_it_returns() {
    // The cut lands in the step that logs the submission, so the review request that follows is
    // written when no reviewer is connected: only the log, read from where it stopped, has it.
    let tasks = [signature(1, "unitPrice")];
    let cfg = config(1, 200, QUICK);
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        let submitted = |e: &Event| matches!(e.kind, EventKind::Submitted { .. });
        server.cutter().reset_on(submitted, Vec::new());
    })
    .await;
    let result = &run.result;
    assert_accounted(result, 1);
    assert_eq!(result.results[0].result, Resolution::Merged);
    let decided = count(&result.events, |k| {
        matches!(k, EventKind::ReviewDecided { .. })
    });
    assert_eq!(decided, 1, "decided once");
    assert_eq!(result.reviews_held, 0);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_merge_that_lands_while_the_submitter_is_gone_is_found_in_the_log() {
    // The agent cannot reconnect until the merge is in the log, so the `Merged` message it would
    // have been sent is lost and only the log can tell it.
    let tasks = [body(1, "unitPrice")];
    let cfg = config(1, 0, PATIENT);
    let mut lifted = None;
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        let submitted = |e: &Event| matches!(e.kind, EventKind::Submitted { .. });
        let cutter = server.cutter();
        cutter.reset_on(submitted, vec!["a01".to_string()]);
        let log = server.reader();
        lifted = Some(tokio::spawn(async move {
            while count(&log.events(), |k| matches!(k, EventKind::Merged { .. })) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            cutter.refuse_new(Vec::new());
        }));
    })
    .await;
    lifted.unwrap().await.unwrap();
    let r = &run.result.results[0];
    assert_eq!(r.result, Resolution::Merged, "{r:?}");
    assert_eq!(r.reconnects, 1, "{r:?}");
    let note = r.note.as_deref().unwrap_or_default();
    assert!(
        note.contains("merged while the connection was down"),
        "{note}"
    );
    assert_eq!(run.result.summary.merges, 1);
    assert_accounted(&run.result, 1);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_that_lapsed_while_the_agent_was_gone_is_that_tasks_lapsed_outcome() {
    let tasks = [body(1, "unitPrice")];
    let cfg = config(1, 1500, PATIENT);
    let mut lifted = None;
    let run = run_with(&tasks, &cfg, 400, |server| {
        let granted = |e: &Event| matches!(e.kind, EventKind::ClaimGranted { .. });
        let cutter = server.cutter();
        cutter.reset_on(granted, vec!["a01".to_string()]);
        let log = server.reader();
        lifted = Some(tokio::spawn(async move {
            while !log.events().iter().any(|e| {
                matches!(
                    e.kind,
                    EventKind::ClaimReleased {
                        reason: ReleaseReason::LeaseExpired,
                        ..
                    }
                )
            }) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            cutter.refuse_new(Vec::new());
        }));
    })
    .await;
    lifted.unwrap().await.unwrap();
    let r = &run.result.results[0];
    assert_eq!(r.result, Resolution::Lapsed, "{r:?}");
    assert_eq!(r.reconnects, 1, "{r:?}");
    assert_eq!(run.result.summary.merges, 0);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_that_cannot_reconnect_end_as_disconnected_after_their_tries() {
    let tasks = [body(1, "unitPrice"), body(2, "restock")];
    let mut cfg = config(
        2,
        1000,
        Reconnect {
            tries: 3,
            first_delay: Duration::from_millis(10),
        },
    );
    cfg.scripted_reviewer = false;
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        let granted = |e: &Event| matches!(e.kind, EventKind::ClaimGranted { .. });
        let gone = vec!["a01".to_string(), "a02".to_string()];
        server.cutter().reset_on(granted, gone);
    })
    .await;
    assert_accounted(&run.result, 2);
    for r in &run.result.results {
        assert_eq!(r.result, Resolution::Disconnected, "{r:?}");
        assert_eq!(r.reconnects, 0, "{r:?}");
        let note = r.note.as_deref().unwrap_or_default();
        assert!(note.contains("3 reconnect tries"), "{note}");
    }
    assert_eq!(
        run.server.cutter().turned_away(),
        6,
        "three tries for each of the two agents, and no more"
    );
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reviewer_that_cannot_reconnect_fails_the_run_naming_its_tries() {
    let tasks = [body(1, "unitPrice")];
    let cfg = config(
        1,
        500,
        Reconnect {
            tries: 3,
            first_delay: Duration::from_millis(10),
        },
    );
    let scratch = tempfile::tempdir().unwrap();
    let server = start(&cfg, scratch.path(), local::LEASE_MS).await;
    let granted = |e: &Event| matches!(e.kind, EventKind::ClaimGranted { .. });
    let gone = vec![REVIEWER.to_string()];
    server.cutter().reset_on(granted, gone);
    let agents = scratch.path().join("agents");
    let run = Box::pin(on::run_on(&server.endpoint, &tasks, &agents, &cfg));
    let Err(error) = bounded(run).await else {
        unreachable!("a reviewer that is gone fails the run");
    };
    let message = format!("{error:#}");
    assert!(message.contains("3 reconnect tries"), "{message}");
    server.shutdown().await;
}

fn queued_agent(kind: &EventKind) -> Option<&str> {
    let EventKind::WaitQueued { agent, .. } = kind else {
        return None;
    };
    Some(agent.0.as_str())
}

/// The agent of the first `WaitQueued` in the log, once there is one.
fn first_waiter(log: &local::LogReader) -> Option<String> {
    log.events().into_iter().find_map(|e| {
        let EventKind::WaitQueued { agent, .. } = e.kind else {
            return None;
        };
        Some(agent.0)
    })
}

/// The log's `AgentConnected` count for `who`.
fn connected_count(log: &local::LogReader, who: &str) -> usize {
    connections_of(&log.events(), who)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_submission_still_undecided_when_the_agent_returns_is_waited_for_not_sent_again() {
    // The agent and the reviewer are both gone. The agent is let back in first, while the
    // submission is held, so it has to wait for the outcome; then the reviewer is let in.
    let tasks = [signature(1, "unitPrice")];
    let cfg = config(1, 0, PATIENT);
    let mut lifted = None;
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        let submitted = |e: &Event| matches!(e.kind, EventKind::Submitted { .. });
        let cutter = server.cutter();
        let gone = vec!["a01".to_string(), REVIEWER.to_string()];
        cutter.reset_on(submitted, gone);
        let log = server.reader();
        lifted = Some(tokio::spawn(async move {
            while connected_count(&log, "a01") < 1
                || count(&log.events(), |k| {
                    matches!(k, EventKind::ReviewRequested { .. })
                }) == 0
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            cutter.refuse_new(vec![REVIEWER.to_string()]);
            while connected_count(&log, "a01") < 3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            cutter.refuse_new(Vec::new());
        }));
    })
    .await;
    lifted.unwrap().await.unwrap();
    let r = &run.result.results[0];
    assert_eq!(r.result, Resolution::Merged, "{r:?}");
    assert_eq!(r.reconnects, 1, "{r:?}");
    assert_eq!(
        r.note, None,
        "it was told live, not found in the log: {r:?}"
    );
    let submitted = count(&run.result.events, |k| {
        matches!(k, EventKind::Submitted { .. })
    });
    assert_eq!(submitted, 1, "the submission was not sent again");
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_grant_made_while_the_agent_was_gone_is_found_in_the_log_and_the_work_goes_on() {
    // The coordinator closes the waiter's socket and keeps its request (a close the Durable
    // Object makes itself). The waiter cannot return until the holder has gone and the grant
    // was made to nobody.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let cfg = config(2, 500, PATIENT);
    let scratch = tempfile::tempdir().unwrap();
    let server = start(&cfg, scratch.path(), local::LEASE_MS).await;
    let (log, cutter) = (server.reader(), server.cutter());
    let cut = tokio::spawn(async move {
        let waiter = loop {
            if let Some(agent) = first_waiter(&log) {
                break agent;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        cutter.refuse_new(vec![waiter.clone()]);
        assert!(cutter.cut(&waiter), "{waiter} had an open socket");
        let granted_to_waiter = |log: &local::LogReader| {
            log.events().iter().any(
                |e| matches!(&e.kind, EventKind::ClaimGranted { agent, .. } if agent.0 == waiter),
            )
        };
        while !granted_to_waiter(&log) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        cutter.refuse_new(Vec::new());
        waiter
    });
    let agents = scratch.path().join("agents");
    let run = Box::pin(on::run_on(&server.endpoint, &tasks, &agents, &cfg));
    let result = bounded(run).await.unwrap();
    let waiter = cut.await.unwrap();
    assert_accounted(&result, 2);
    let outcomes: Vec<Resolution> = result.results.iter().map(|r| r.result).collect();
    assert_eq!(outcomes, [Resolution::Merged; 2], "{:?}", result.results);
    let mine = result.results.iter().find(|r| r.agent == waiter).unwrap();
    assert_eq!(mine.reconnects, 1, "{mine:?}");
    assert!(mine.work_ms >= 400, "it did the work once back: {mine:?}");
    let granted = count(&result.events, |k| {
        matches!(k, EventKind::ClaimGranted { .. })
    });
    assert_eq!(granted, 2, "no second claim was made for the same task");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_the_coordinator_kept_queued_is_waited_for_on_the_new_connection() {
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let cfg = config(2, 500, QUICK);
    let scratch = tempfile::tempdir().unwrap();
    let server = start(&cfg, scratch.path(), local::LEASE_MS).await;
    let (log, cutter) = (server.reader(), server.cutter());
    let cut = tokio::spawn(async move {
        let waiter = loop {
            if let Some(agent) = first_waiter(&log) {
                break agent;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert!(cutter.cut(&waiter), "{waiter} had an open socket");
        waiter
    });
    let agents = scratch.path().join("agents");
    let run = Box::pin(on::run_on(&server.endpoint, &tasks, &agents, &cfg));
    let result = bounded(run).await.unwrap();
    let waiter = cut.await.unwrap();
    let outcomes: Vec<Resolution> = result.results.iter().map(|r| r.result).collect();
    assert_eq!(outcomes, [Resolution::Merged; 2], "{:?}", result.results);
    let queued = count(&result.events, |k| {
        matches!(k, EventKind::WaitQueued { .. })
    });
    assert_eq!(queued, 1, "the claim was not sent a second time");
    let mine = result.results.iter().find(|r| r.agent == waiter).unwrap();
    assert_eq!(mine.reconnects, 1, "{mine:?}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_network_that_keeps_failing_ends_the_task_after_a_bounded_number_of_resets() {
    let tasks = [body(1, "unitPrice")];
    let mut cfg = config(1, 1000, QUICK);
    cfg.scripted_reviewer = false;
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        // The first cut is the grant; after that every return of a01 is cut again.
        let hellos = Arc::new(AtomicUsize::new(0));
        let again = move |e: &Event| {
            if matches!(e.kind, EventKind::ClaimGranted { .. }) {
                return true;
            }
            let EventKind::AgentConnected { agent } = &e.kind else {
                return false;
            };
            agent.0 == "a01" && hellos.fetch_add(1, Ordering::SeqCst) >= 1
        };
        server.cutter().reset_on_each(again);
    })
    .await;
    let r = &run.result.results[0];
    assert_eq!(r.result, Resolution::Disconnected, "{r:?}");
    assert_eq!(r.reconnects, MAX_RESETS_PER_TASK, "{r:?}");
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wait_that_a_reset_cut_short_is_counted_whole() {
    // Two agents queue behind a holder that works for 3 s; everything is cut the moment the holder
    // submits. The waiters return to a holder that is only a merge away from done, so their second
    // wait is short: the 3 s before the cut are part of their wait, not lost with the withdrawn
    // request.
    let tasks = [
        body(1, "unitPrice"),
        body(2, "unitPrice"),
        body(3, "unitPrice"),
    ];
    let cfg = config(3, 3000, QUICK);
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        let submitted = |e: &Event| matches!(e.kind, EventKind::Submitted { .. });
        server.cutter().reset_on(submitted, Vec::new());
    })
    .await;
    let result = &run.result;
    let waiters: Vec<_> = result
        .results
        .iter()
        .filter(|r| {
            let queued = |e: &Event| queued_agent(&e.kind) == Some(r.agent.as_str());
            result.events.iter().any(queued)
        })
        .collect();
    assert_eq!(waiters.len(), 2, "{:?}", result.results);
    for r in waiters {
        assert!(
            r.waited_ms >= 3000,
            "the wait before the reset is kept: {r:?}"
        );
    }
    let outcomes: Vec<Resolution> = result.results.iter().map(|r| r.result).collect();
    assert_eq!(outcomes, [Resolution::Merged; 3], "{:?}", result.results);
    run.server.shutdown().await;
}

/// How long after the log shows the claim shadowed the agent is cut. Nothing the agent does after
/// reading its answer is visible to the cutter, so this is a delay: reading takes milliseconds and
/// the submit path takes about a second (`work_ms`, checks, push), so 250 ms leaves a margin on
/// both sides on a loaded machine.
const CUT_DELAY: Duration = Duration::from_millis(250);

/// Heartbeats too far apart to fire during a test: the shadow agent's first use of its connection
/// after a cut is then its own request, which is what sends it through the log.
const NO_BEAT: Duration = Duration::from_secs(30);

fn shadow_config(work_ms: u64, heartbeat_every: Duration) -> OnConfig {
    let mut cfg = config(2, work_ms, PATIENT);
    cfg.policy = Policy::Shadow;
    cfg.heartbeat_every = heartbeat_every;
    cfg
}

fn shadow_claims(events: &[Event]) -> Vec<(String, ClaimId)> {
    let mut found = Vec::new();
    for event in events {
        if let EventKind::ClaimShadowed { agent, claim, .. } = &event.kind {
            found.push((agent.0.clone(), *claim));
        }
    }
    found
}

/// Closes the shadow agent's sockets `CUT_DELAY` after the log shows its claim shadowed: the answer was
/// read, the work is under way and nothing is submitted. With `until_lapse` the agent is also
/// turned away until the lease of its claim has run out. Returns the agent's name.
fn cut_the_shadow_agent(
    server: &LocalServer,
    until_lapse: bool,
) -> tokio::task::JoinHandle<String> {
    let (log, cutter) = (server.reader(), server.cutter());
    tokio::spawn(async move {
        let agent = loop {
            if let Some((agent, _)) = shadow_claims(&log.events()).into_iter().next() {
                break agent;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        };
        tokio::time::sleep(CUT_DELAY).await;
        if until_lapse {
            cutter.refuse_new(vec![agent.clone()]);
        }
        assert!(cutter.cut(&agent), "{agent} had an open socket");
        if until_lapse {
            let lapsed = |log: &local::LogReader| {
                count(&log.events(), |k| {
                    matches!(
                        k,
                        EventKind::ClaimReleased {
                            reason: ReleaseReason::LeaseExpired,
                            ..
                        }
                    )
                }) > 0
            };
            while !lapsed(&log) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            cutter.refuse_new(Vec::new());
        }
        agent
    })
}

fn submissions_of(events: &[Event], claim: ClaimId) -> usize {
    count(
        events,
        |k| matches!(k, EventKind::Submitted { claim: c, .. } if *c == claim),
    )
}

fn outcomes(result: &OnResult) -> Vec<String> {
    let mut found: Vec<String> = result
        .results
        .iter()
        .map(|r| format!("{:?}", r.result))
        .collect();
    found.sort();
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shadow_agent_cut_before_it_submitted_goes_on_with_the_same_claim() {
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let cfg = shadow_config(1000, NO_BEAT);
    let mut cut = None;
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        cut = Some(cut_the_shadow_agent(server, false));
    })
    .await;
    let agent = cut.unwrap().await.unwrap();
    let events = &run.result.events;
    let shadows = shadow_claims(events);
    assert_eq!(
        shadows.len(),
        1,
        "no second claim for the task: {shadows:?}"
    );
    assert_eq!(submissions_of(events, shadows[0].1), 1, "submitted once");
    assert_eq!(outcomes(&run.result), ["Merged", "Shadowed"]);
    let mine = run
        .result
        .results
        .iter()
        .find(|r| r.agent == agent)
        .unwrap();
    assert_eq!(mine.result, Resolution::Shadowed, "{mine:?}");
    assert!(mine.reconnects >= 1, "{mine:?}");
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mass_reset_as_a_claim_is_shadowed_does_not_end_the_shadow_agent() {
    // Every socket is reset in the step that logs the shadow claim, so the agent never hears the
    // answer: it cannot know the claim's fence and claims the task again.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let cfg = shadow_config(1000, Duration::from_millis(100));
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        let shadowed = |e: &Event| matches!(e.kind, EventKind::ClaimShadowed { .. });
        server.cutter().reset_on(shadowed, Vec::new());
    })
    .await;
    assert_eq!(outcomes(&run.result), ["Merged", "Shadowed"]);
    let events = &run.result.events;
    let (agent, _) = shadow_claims(events).pop().unwrap();
    let mine = run
        .result
        .results
        .iter()
        .find(|r| r.agent == agent)
        .unwrap();
    assert!(mine.reconnects >= 1, "{mine:?}");
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shadow_claim_that_lapsed_while_its_agent_was_gone_is_that_tasks_lapsed_outcome() {
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let cfg = shadow_config(1000, Duration::from_millis(100));
    let mut cut = None;
    let run = run_with(&tasks, &cfg, 5000, |server| {
        cut = Some(cut_the_shadow_agent(server, true));
    })
    .await;
    let agent = cut.unwrap().await.unwrap();
    let events = &run.result.events;
    let shadows = shadow_claims(events);
    assert_eq!(
        shadows.len(),
        1,
        "no second claim for the task: {shadows:?}"
    );
    assert_eq!(
        submissions_of(events, shadows[0].1),
        0,
        "it never submitted"
    );
    assert_eq!(outcomes(&run.result), ["Lapsed", "Merged"]);
    let mine = run
        .result
        .results
        .iter()
        .find(|r| r.agent == agent)
        .unwrap();
    assert_eq!(mine.result, Resolution::Lapsed, "{mine:?}");
    assert_eq!(mine.reconnects, 1, "{mine:?}");
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejection_that_lands_while_the_submitter_is_gone_is_found_and_its_claim_released() {
    // Main changes the claimed line behind the agent, so the steward rejects the submission; the
    // agent cannot return until that is in the log.
    let tasks = [body(1, "unitPrice")];
    let cfg = config(1, 800, PATIENT);
    let mut lifted = None;
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        let submitted = |e: &Event| matches!(e.kind, EventKind::Submitted { .. });
        let cutter = server.cutter();
        cutter.reset_on(submitted, vec!["a01".to_string()]);
        let log = server.reader();
        let tessel_swarm::endpoint::Remote::Local { trunk, .. } = server.endpoint.remote.clone()
        else {
            unreachable!("the local target has a local remote");
        };
        lifted = Some(tokio::spawn(async move {
            while count(&log.events(), |k| {
                matches!(k, EventKind::ClaimGranted { .. })
            }) == 0
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            tokio::task::spawn_blocking(move || poison(&trunk))
                .await
                .unwrap();
            while count(&log.events(), |k| {
                matches!(k, EventKind::SubmitRejected { .. })
            }) == 0
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            cutter.refuse_new(Vec::new());
        }));
    })
    .await;
    lifted.unwrap().await.unwrap();
    let r = &run.result.results[0];
    assert_eq!(r.result, Resolution::Rejected, "{r:?}");
    assert_eq!(r.reconnects, 1, "{r:?}");
    let released_by_agent = count(&run.result.events, |k| {
        matches!(
            k,
            EventKind::ClaimReleased {
                reason: ReleaseReason::Agent,
                ..
            }
        )
    });
    assert_eq!(released_by_agent, 1, "the rejected claim was released");
    run.server.shutdown().await;
}

/// Changes the line the task edits on the trunk, outside any claim.
fn poison(trunk: &std::path::Path) {
    let file = trunk.join("src/pricing.ts");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace("return base * qty;", "return qty * base;"),
    )
    .unwrap();
    tessel_swarm::git::Git::new(trunk)
        .commit_all("Change main behind the agents")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_granted_to_nobody_that_lapsed_before_the_agent_returned_is_lapsed() {
    // The coordinator closes the waiter's socket and keeps its request; the grant goes to nobody
    // and expires a lease later. The log says so when the waiter returns.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let cfg = config(2, 1000, PATIENT);
    let scratch = tempfile::tempdir().unwrap();
    let server = start(&cfg, scratch.path(), 600).await;
    let (log, cutter) = (server.reader(), server.cutter());
    let cut = tokio::spawn(async move {
        let waiter = loop {
            if let Some(agent) = first_waiter(&log) {
                break agent;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        cutter.refuse_new(vec![waiter.clone()]);
        assert!(cutter.cut(&waiter), "{waiter} had an open socket");
        while count(&log.events(), |k| {
            matches!(
                k,
                EventKind::ClaimReleased {
                    reason: ReleaseReason::LeaseExpired,
                    ..
                }
            )
        }) == 0
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        cutter.refuse_new(Vec::new());
        waiter
    });
    let agents = scratch.path().join("agents");
    let run = Box::pin(on::run_on(&server.endpoint, &tasks, &agents, &cfg));
    let result = bounded(run).await.unwrap();
    let waiter = cut.await.unwrap();
    let mine = result.results.iter().find(|r| r.agent == waiter).unwrap();
    assert_eq!(mine.result, Resolution::Lapsed, "{mine:?}");
    assert_eq!(mine.reconnects, 1, "{mine:?}");
    assert_accounted(&result, 2);
    server.shutdown().await;
}

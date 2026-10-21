//! Connection resets against the local target: every socket ends at once, with no close frame, as
//! the 19:39 live run's did. Agents and the scripted reviewer reopen their connections, learn from
//! the coordinator's log where they stand, and the run finishes with every task accounted for.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tessel_coordinator::protocol::{AgentId, ClaimId, Event, EventKind, ReleaseReason, RunId};
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
const BOUND: Duration = Duration::from_secs(180);

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

/// How often a wait looks at the log again.
const POLL: Duration = Duration::from_millis(5);

/// The longest any wait on the log may last. It is shorter than `BOUND`, so that a wait that never
/// ends names itself before the run's own bound does.
const WAIT: Duration = Duration::from_secs(150);

/// The last few events of a log, as `seq:kind`, for the message of a wait that gave up.
fn log_tail(events: &[Event]) -> String {
    let start = events.len().saturating_sub(10);
    let mut tail = Vec::new();
    for event in &events[start..] {
        let text = format!("{:?}", event.kind);
        let name = text.split([' ', '(', '{']).next().unwrap_or_default();
        tail.push(format!("{}:{name}", event.seq));
    }
    tail.join(", ")
}

/// Waits until `find` returns something for the log, and returns it. Panics after `WAIT`, naming
/// what was waited for and the last events the log held.
async fn wait_for<T>(
    what: &str,
    log: &local::LogReader,
    find: impl Fn(&[Event]) -> Option<T>,
) -> T {
    let started = tokio::time::Instant::now();
    loop {
        let events = log.events();
        if let Some(found) = find(&events) {
            return found;
        }
        assert!(
            started.elapsed() < WAIT,
            "gave up after {WAIT:?} waiting for {what}; the log held {} events, the last ones: {}",
            events.len(),
            log_tail(&events)
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Waits until `ready` is true of the log, within `WAIT`.
async fn wait_until(what: &str, log: &local::LogReader, ready: impl Fn(&[Event]) -> bool) {
    wait_for(what, log, |events| ready(events).then_some(())).await;
}

/// Like `wait_until`, but ends with the reason when a claim is released first: the agent gave up
/// before the thing waited for could happen, and waiting out `WAIT` would only hide why.
async fn wait_until_unless_released(
    what: &str,
    log: &local::LogReader,
    ready: impl Fn(&[Event]) -> bool,
) -> Result<(), String> {
    let started = tokio::time::Instant::now();
    loop {
        let events = log.events();
        if ready(&events) {
            return Ok(());
        }
        let released = events.iter().find_map(|e| {
            let EventKind::ClaimReleased { claim, reason } = &e.kind else {
                return None;
            };
            Some((claim.0, format!("{reason:?}")))
        });
        if let Some((claim, reason)) = released {
            return Err(format!(
                "claim {claim} was released ({reason}) before {what}; the log held {} events, the \
                 last ones: {}",
                events.len(),
                log_tail(&events)
            ));
        }
        assert!(
            started.elapsed() < WAIT,
            "gave up after {WAIT:?} waiting for {what}; the last events: {}",
            log_tail(&events)
        );
        tokio::time::sleep(POLL).await;
    }
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

/// Whether `claim` ended because its lease ran out. Another agent's lapse does not count.
fn claim_lapsed(events: &[Event], claim: ClaimId) -> bool {
    events.iter().any(|e| {
        matches!(
            &e.kind,
            EventKind::ClaimReleased { claim: c, reason: ReleaseReason::LeaseExpired }
                if *c == claim
        )
    })
}

/// Whether a claim granted to `agent` ended because its lease ran out.
fn grant_lapsed(events: &[Event], agent: &str) -> bool {
    events.iter().any(|e| {
        let EventKind::ClaimGranted {
            agent: a, claim, ..
        } = &e.kind
        else {
            return false;
        };
        a.0 == agent && claim_lapsed(events, *claim)
    })
}

fn connections_of(events: &[Event], who: &str) -> usize {
    count(
        events,
        |k| matches!(k, EventKind::AgentConnected { agent } if agent.0 == who),
    )
}

/// Wraps a tripwire condition so that it cannot be true until every agent in `agents` has
/// connected at least once; events that satisfy `then` before that are let through.
///
/// A cut that refuses agents also turns away their first connection, and `run_on` treats a failed
/// first connection as an error of the whole run, not as a reconnect case. On 2026-10-09 a fresh
/// clone of the trunk under load failed the test of two agents that cannot reconnect, twice, with
/// `401 Unauthorized`: a01's grant cut the sockets before a02 had connected for the first time.
fn after_connected(
    agents: &[&str],
    then: impl Fn(&Event) -> bool + Send + 'static,
) -> impl Fn(&Event) -> bool + Send + 'static {
    let waiting = Mutex::new(agents.iter().map(|a| (*a).to_string()).collect::<Vec<_>>());
    move |event| {
        let mut waiting = waiting.lock().unwrap();
        if let EventKind::AgentConnected { agent } = &event.kind {
            waiting.retain(|name| *name != agent.0);
        }
        waiting.is_empty() && then(event)
    }
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
fn a_cut_waits_for_every_listed_agent_to_have_connected() {
    let connected = |seq: u64, who: &str| Event {
        seq,
        at_ms: 0,
        run: RunId("test".to_string()),
        kind: EventKind::AgentConnected {
            agent: AgentId(who.to_string()),
        },
    };
    let odd = |e: &Event| e.seq % 2 == 1;
    let cut = after_connected(&["a01", "a02"], odd);
    assert!(!cut(&connected(0, "a01")), "a02 has not connected");
    assert!(
        !cut(&connected(1, "a01")),
        "the trigger came before a02 connected"
    );
    assert!(
        !cut(&connected(2, "a02")),
        "both connected, but this event is no trigger"
    );
    assert!(
        cut(&connected(3, "a01")),
        "the first trigger after both connected fires"
    );
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
            let merged =
                |events: &[Event]| count(events, |k| matches!(k, EventKind::Merged { .. }));
            wait_until("the merge", &log, |events| merged(events) > 0).await;
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
            let lapsed = |events: &[Event]| grant_lapsed(events, "a01");
            wait_until("a01's lease to run out", &log, lapsed).await;
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
        server
            .cutter()
            .reset_on(after_connected(&["a01", "a02"], granted), gone);
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
fn first_waiter(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| {
        let EventKind::WaitQueued { agent, .. } = &e.kind else {
            return None;
        };
        Some(agent.0.clone())
    })
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
            let requested = |events: &[Event]| {
                count(events, |k| matches!(k, EventKind::ReviewRequested { .. }))
            };
            let both =
                |events: &[Event]| connections_of(events, "a01") >= 1 && requested(events) > 0;
            let held = "a01 connected and the submission held for review";
            if let Err(why) = wait_until_unless_released(held, &log, both).await {
                cutter.refuse_new(Vec::new());
                return Err(why);
            }
            cutter.refuse_new(vec![REVIEWER.to_string()]);
            let back = |events: &[Event]| connections_of(events, "a01") >= 3;
            wait_until("a01 connected a third time", &log, back).await;
            // The third connection is the one that reads the log, and the read ends at its own
            // `AgentConnected`: nothing the reviewer logs from here on can be in what a01 read, so
            // a01 hears the outcome live on its second connection however long it takes to look.
            cutter.refuse_new(Vec::new());
            Ok(())
        }));
    })
    .await;
    let lifted = lifted.unwrap().await.unwrap();
    assert!(
        lifted.is_ok(),
        "{lifted:?}; the agent's result: {:?}",
        run.result.results
    );
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
        let waiter = wait_for("a queued request", &log, first_waiter).await;
        cutter.refuse_new(vec![waiter.clone()]);
        assert!(cutter.cut(&waiter), "{waiter} had an open socket");
        let granted_to_waiter = |events: &[Event]| {
            events.iter().any(
                |e| matches!(&e.kind, EventKind::ClaimGranted { agent, .. } if agent.0 == waiter),
            )
        };
        wait_until("the grant made to the waiter", &log, granted_to_waiter).await;
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
        let waiter = wait_for("a queued request", &log, first_waiter).await;
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
    let cut = result
        .events
        .iter()
        .find(|e| matches!(e.kind, EventKind::Submitted { .. }))
        .unwrap();
    // The waits the cut ended: queued before the cut, and not granted before it.
    let mut cut_short = Vec::new();
    for queued in result.events.iter().filter(|e| e.seq < cut.seq) {
        let Some(agent) = queued_agent(&queued.kind) else {
            continue;
        };
        let granted = |e: &Event| {
            matches!(&e.kind, EventKind::ClaimGranted { agent: a, .. } if a.0 == agent)
                && e.seq > queued.seq
                && e.seq < cut.seq
        };
        if !result.events.iter().any(granted) {
            cut_short.push((agent, cut.at_ms - queued.at_ms));
        }
    }
    assert_eq!(cut_short.len(), 2, "{cut_short:?} {:?}", result.results);
    for (agent, before_cut) in cut_short {
        // The coordinator's own timestamps give the wait before the cut, however long the holder
        // took to get there; an agent's tasks add up.
        let waited: u64 = result
            .results
            .iter()
            .filter(|r| r.agent == agent)
            .map(|r| r.waited_ms)
            .sum();
        assert!(
            waited >= before_cut,
            "{agent}'s wait before the reset ({before_cut} ms) is kept: {:?}",
            result.results
        );
    }
    let outcomes: Vec<Resolution> = result.results.iter().map(|r| r.result).collect();
    assert_eq!(outcomes, [Resolution::Merged; 3], "{:?}", result.results);
    run.server.shutdown().await;
}

/// Holds every push to an agent's fork, so that a test can cut an agent at a known point: after
/// it has read its answer, edited and committed, and before it has submitted. A fork's
/// `pre-receive` hook records that a push arrived, then waits for the gate to open. The wait is on
/// the file that opening removes, not on a duration.
struct PushGate {
    dir: tempfile::TempDir,
}

impl PushGate {
    fn install(server: &LocalServer) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("closed"), "").unwrap();
        let forks = server.endpoint.remote.forks.clone();
        for fork in std::fs::read_dir(forks).unwrap() {
            let fork = fork.unwrap().path();
            let name = fork.file_stem().unwrap().to_string_lossy().into_owned();
            let gate = dir.path().display();
            // The hook gives up when the gate has been closed for `WAIT`, so a test binary that
            // is killed leaves no hook waiting for ever.
            let polls = WAIT.as_millis() / 20;
            let script = format!(
                "#!/bin/sh\ncat >/dev/null\n: > '{gate}/arrived-{name}'\nn=0\n\
                 while [ -e '{gate}/closed' ] && [ \"$n\" -lt {polls} ]; do\n\
                 sleep 0.02; n=$((n + 1)); done\n"
            );
            let hook = fork.join("hooks/pre-receive");
            std::fs::write(&hook, script).unwrap();
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { dir }
    }

    /// Waits until `agent` has pushed, which is when it can no longer be about to read its answer
    /// and has not yet been able to submit.
    async fn push_arrived(&self, agent: &str) {
        let arrived = self.dir.path().join(format!("arrived-{agent}"));
        let started = tokio::time::Instant::now();
        while !arrived.exists() {
            assert!(
                started.elapsed() < WAIT,
                "gave up after {WAIT:?} waiting for {agent} to push"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    fn open(&self) {
        std::fs::remove_file(self.dir.path().join("closed")).unwrap();
    }
}

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

/// Closes the shadow agent's sockets once the log shows its claim shadowed and it has pushed its
/// work: the answer was read, the work is under way and nothing is submitted. The push is held
/// until the sockets are closed. With `until_lapse` the agent is also turned away until the lease
/// of its claim has run out. Returns the agent's name, and the gate, which has to outlive the run.
fn cut_the_shadow_agent(
    server: &LocalServer,
    until_lapse: bool,
) -> tokio::task::JoinHandle<(String, PushGate)> {
    let (log, cutter) = (server.reader(), server.cutter());
    let gate = PushGate::install(server);
    tokio::spawn(async move {
        let first_shadow = |events: &[Event]| shadow_claims(events).into_iter().next();
        let (agent, claim) = wait_for("a shadowed claim", &log, first_shadow).await;
        gate.push_arrived(&agent).await;
        if until_lapse {
            cutter.refuse_new(vec![agent.clone()]);
        }
        assert!(cutter.cut(&agent), "{agent} had an open socket");
        gate.open();
        if until_lapse {
            let lapsed = |events: &[Event]| claim_lapsed(events, claim);
            wait_until("the shadow claim's lease to run out", &log, lapsed).await;
            cutter.refuse_new(Vec::new());
        }
        (agent, gate)
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
    let (agent, _gate) = cut.unwrap().await.unwrap();
    let events = &run.result.events;
    let shadows = shadow_claims(events);
    assert_eq!(
        shadows.len(),
        1,
        "no second claim for the task: {shadows:?}"
    );
    assert_eq!(
        submissions_of(events, shadows[0].1),
        1,
        "submitted once; the agents' results: {:?}",
        run.result.results
    );
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
    let (agent, _gate) = cut.unwrap().await.unwrap();
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
    // The agent edits main as it stands when it is granted, works, pushes and submits. Main
    // changes in the step that logs the submission: after the agent's edit and push, and before
    // the steward can start the merge, which then conflicts at the claimed line. The same step
    // cuts the agent, which cannot return until the rejection is in the log. Nothing here
    // depends on how long the agent works.
    let tasks = [body(1, "unitPrice")];
    let cfg = config(1, 0, PATIENT);
    let mut lifted = None;
    let run = run_with(&tasks, &cfg, local::LEASE_MS, |server| {
        let trunk = server.endpoint.remote.trunk.clone();
        let change_main_then_cut = move |e: &Event| {
            let submitted = matches!(e.kind, EventKind::Submitted { .. });
            if submitted {
                poison(&trunk);
            }
            submitted
        };
        let cutter = server.cutter();
        cutter.reset_on(change_main_then_cut, vec!["a01".to_string()]);
        let log = server.reader();
        lifted = Some(tokio::spawn(async move {
            let rejected = |events: &[Event]| {
                count(events, |k| matches!(k, EventKind::SubmitRejected { .. })) > 0
            };
            wait_until("the rejection", &log, rejected).await;
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

/// The lease in the test below. The holder of the task's scopes works for a second and renews its
/// claim every `heartbeat_every` (100 ms); a lease only a few heartbeats long lapses under it when
/// the machine stalls the agent for that long, and the holder's task would then end lapsed. The
/// test waits for the lapse of the waiter's own grant (`grant_lapsed`), so the holder's lapse can
/// no longer satisfy the wait; a longer lease only keeps the holder's task from ending that way.
/// The wait is on the log, so it costs only the seconds a passing run spends waiting.
const UNHEARD_LEASE_MS: u64 = 5_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_granted_to_nobody_that_lapsed_before_the_agent_returned_is_lapsed() {
    // The coordinator closes the waiter's socket and keeps its request; the grant goes to nobody
    // and expires a lease later. The log says so when the waiter returns.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let cfg = config(2, 1000, PATIENT);
    let scratch = tempfile::tempdir().unwrap();
    let server = start(&cfg, scratch.path(), UNHEARD_LEASE_MS).await;
    let (log, cutter) = (server.reader(), server.cutter());
    let cut = tokio::spawn(async move {
        let waiter = wait_for("a queued request", &log, first_waiter).await;
        cutter.refuse_new(vec![waiter.clone()]);
        assert!(cutter.cut(&waiter), "{waiter} had an open socket");
        let lapsed = |events: &[Event]| grant_lapsed(events, &waiter);
        wait_until("the unheard grant's lease to run out", &log, lapsed).await;
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

//! Mode `on` against the local target: the real coordinator core behind a WebSocket server, with
//! a steward that lands work with real git and the demo repository's real tests.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::path::PathBuf;

use std::time::Duration;

use tessel_coordinator::protocol::{ClaimId, Event, EventKind, Outcome, Summary};
use tessel_swarm::demo;
use tessel_swarm::git::{self, Checks, Git};
use tessel_swarm::guard::ScratchRepo;
use tessel_swarm::local::{LocalServer, LocalSetup};
use tessel_swarm::off::{self, OffConfig};
use tessel_swarm::on::{self, OnConfig, OnResult, Policy, Resolution, REVIEWER};
use tessel_swarm::tasks::{Kind, Task};
use tokio::io::AsyncReadExt;

type Hook = Box<dyn FnOnce(PathBuf) + Send>;

struct Run {
    server: LocalServer,
    result: OnResult,
    _scratch: tempfile::TempDir,
}

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

fn add(id: usize, func: &str, host: &str) -> Task {
    Task {
        id,
        func: func.into(),
        kind: Kind::Add {
            host: host.into(),
            constant: 4,
        },
    }
}

fn config(agents: usize, policy: Policy, work_ms: u64) -> OnConfig {
    OnConfig {
        agents,
        policy,
        work_ms,
        task_timeout: Duration::from_secs(60),
        trial_wait: Duration::from_secs(60),
        max_denials: 400,
        scripted_reviewer: true,
    }
}

/// Runs `tasks` on a fresh local target. `hook` runs on a blocking thread `delay` after the first
/// claim is granted and gets the trunk directory, so a test can change main while an agent works.
async fn start_server(config: &OnConfig, scratch: &std::path::Path) -> LocalServer {
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
    })
    .await
    .unwrap()
}

async fn run_with_hook(tasks: &[Task], config: OnConfig, hook: Option<(Duration, Hook)>) -> Run {
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path()).await;
    if let Some((after, hook)) = hook {
        let tessel_swarm::endpoint::Remote::Local { trunk, .. } = server.endpoint.remote.clone()
        else {
            unreachable!("the local target has a local remote");
        };
        let log = server.reader();
        tokio::spawn(async move {
            while !log
                .events()
                .iter()
                .any(|e| matches!(e.kind, EventKind::ClaimGranted { .. }))
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(after).await;
            tokio::task::spawn_blocking(move || hook(trunk))
                .await
                .unwrap();
        });
    }
    let result = on::run_on(
        &server.endpoint,
        tasks,
        &scratch.path().join("agents"),
        &config,
    )
    .await
    .unwrap();
    Run {
        server,
        result,
        _scratch: scratch,
    }
}

async fn run(tasks: &[Task], config: OnConfig) -> Run {
    run_with_hook(tasks, config, None).await
}

fn trunk_of(run: &Run) -> PathBuf {
    let tessel_swarm::endpoint::Remote::Local { trunk, .. } = run.server.endpoint.remote.clone()
    else {
        unreachable!("the local target has a local remote");
    };
    trunk
}

fn merged_claims(run: &Run) -> usize {
    run.result
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Merged { .. }))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_conflicting_pair_waits_and_then_both_merge() {
    // Both rewrite unitPrice's body: the claims conflict, so the second agent queues.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let run = run(&tasks, config(2, Policy::Wait, 300)).await;
    let outcomes: Vec<Resolution> = run.result.results.iter().map(|r| r.result).collect();
    assert_eq!(outcomes, [Resolution::Merged, Resolution::Merged]);
    assert_eq!(run.result.waits_in_log, 1, "exactly one claim had to queue");
    assert_eq!(run.result.summary.denials, 0, "waiting is not a denial");
    assert_eq!(run.result.summary.merges, 2);
    assert!(
        run.result.results.iter().any(|r| r.waited_ms >= 250),
        "{:?}",
        run.result.results
    );
    let trunk = trunk_of(&run);
    let text = std::fs::read_to_string(trunk.join("src/pricing.ts")).unwrap();
    assert!(
        text.contains("const out1 =") || text.contains("const out2 ="),
        "{text}"
    );
    assert!(matches!(
        git::run_checks(&trunk).unwrap(),
        Checks::Pass { .. }
    ));
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_denied_agent_picks_other_work_and_comes_back() {
    let tasks = [
        body(1, "unitPrice"),
        body(2, "unitPrice"),
        body(3, "restock"),
    ];
    let run = run(&tasks, config(2, Policy::Skip, 500)).await;
    assert!(
        run.result
            .results
            .iter()
            .all(|r| r.result == Resolution::Merged),
        "{:?}",
        run.result.results
    );
    let denials: u32 = run.result.results.iter().map(|r| r.denials).sum();
    assert!(
        denials >= 1,
        "the second unitPrice task must have been denied at least once"
    );
    assert_eq!(
        run.result.summary.denials,
        u64::from(denials),
        "the log counts every denial"
    );
    assert_eq!(run.result.waits_in_log, 0, "the skip policy never queues");
    assert_eq!(run.result.summary.merges, 3);
    let order: Vec<String> = run
        .result
        .events
        .iter()
        .filter_map(|e| {
            let EventKind::ClaimGranted { intent, .. } = &e.kind else {
                return None;
            };
            Some(intent.summary.clone())
        })
        .collect();
    // Which agent claimed first depends on timing; the later of the pair waited on the holder.
    let at = |label: &str| order.iter().position(|s| s.starts_with(label)).unwrap();
    assert!(
        at("t03") < at("t01").max(at("t02")),
        "restock was done while unitPrice was held: {order:?}"
    );
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claims_serialize_a_signature_change_and_a_new_caller_that_break_each_other_in_git() {
    let tasks = [
        signature(1, "unitPrice"),
        add(2, "unitPrice", "src/pricing.ts"),
    ];

    let uncoordinated = {
        let scratch = tempfile::tempdir().unwrap();
        off::run_off(
            &tasks,
            scratch.path(),
            OffConfig {
                agents: 2,
                work_ms: 0,
            },
        )
        .await
        .unwrap()
    };
    assert_eq!(
        uncoordinated.counts.tests_failed, 1,
        "plain git lands a red trunk's worth of work"
    );

    let coordinated = run(&tasks, config(2, Policy::Wait, 200)).await;
    assert_eq!(coordinated.result.summary.merges, 2);
    assert_eq!(coordinated.result.rejected_in_log, 0);
    assert_eq!(
        coordinated.result.waits_in_log, 1,
        "Depend on unitPrice waits for its signature change"
    );
    let trunk = trunk_of(&coordinated);
    assert!(matches!(
        git::run_checks(&trunk).unwrap(),
        Checks::Pass { tests: 13 }
    ));
    let pricing = std::fs::read_to_string(trunk.join("src/pricing.ts")).unwrap();
    assert!(
        pricing.contains("unitPrice(p0, p1, 1) + 4"),
        "the late caller read the new signature: {pricing}"
    );
    coordinated.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_submission_releases_its_claim_for_the_next_agent() {
    let tasks = [
        body(1, "unitPrice"),
        body(2, "unitPrice"),
        body(3, "restock"),
    ];
    // While agent one works, main changes the same line outside any claim: its commit will not
    // apply, the steward rejects it, and agent two (queued behind it) must still get its turn.
    let poison: Hook = Box::new(|trunk| {
        let file = trunk.join("src/pricing.ts");
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(
            &file,
            text.replace("return base * qty;", "return qty * base;"),
        )
        .unwrap();
        Git::new(&trunk)
            .commit_all("Change main behind the agents")
            .unwrap();
    });
    let run = run_with_hook(
        &tasks,
        config(2, Policy::Wait, 1500),
        Some((Duration::from_millis(500), poison)),
    )
    .await;
    // Which agent claimed first depends on timing; the one that did is the one rejected.
    let mut outcomes: Vec<String> = run
        .result
        .results
        .iter()
        .map(|r| format!("{:?}", r.result))
        .collect();
    outcomes.sort();
    assert_eq!(outcomes, ["Merged", "Merged", "Rejected"]);
    assert_eq!(run.result.rejected_in_log, 1);
    assert_eq!(run.result.summary.merges, 2);
    // The rejected agent goes on to restock and stays connected. Its rejected claim must be
    // released at once, not when it disconnects: the queued agent is granted before restock merges.
    let events = &run.result.events;
    let granted = |label: &str| {
        events
            .iter()
            .position(|e| {
                let EventKind::ClaimGranted { intent, .. } = &e.kind else {
                    return false;
                };
                intent.summary.starts_with(label)
            })
            .unwrap()
    };
    let restock_claim = events.iter().find_map(|e| {
        let EventKind::ClaimGranted { claim, intent, .. } = &e.kind else {
            return None;
        };
        intent.summary.starts_with("t03").then_some(*claim)
    });
    let restock_merged = events
        .iter()
        .position(
            |e| matches!(&e.kind, EventKind::Merged { claim, .. } if Some(*claim) == restock_claim),
        )
        .unwrap();
    assert!(
        granted("t01").max(granted("t02")) < restock_merged,
        "the queued agent waited for the rejected agent to disconnect"
    );
    assert!(
        run.result.wasted_ms >= 1400,
        "the rejected agent's work time is counted: {}",
        run.result.wasted_ms
    );
    assert_eq!(merged_claims(&run), 2);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_number_comes_from_summary_over_the_coordinators_own_log() {
    let tasks = [
        body(1, "unitPrice"),
        body(2, "unitPrice"),
        body(3, "restock"),
    ];
    let run = run(&tasks, config(3, Policy::Wait, 100)).await;
    let coordinator_log = run.server.log();
    assert_eq!(
        serde_json::to_value(&run.result.events).unwrap(),
        serde_json::to_value(&coordinator_log).unwrap(),
        "the watched log is the coordinator's log, gap-free from seq 0"
    );
    assert_eq!(run.result.summary, Summary::from_events(&coordinator_log));
    assert_eq!(run.result.summary.claims_granted, 3);
    assert_eq!(run.result.summary.merges, 3);
    run.server.shutdown().await;
}

fn shadow_claims(run: &Run) -> Vec<ClaimId> {
    run.result
        .events
        .iter()
        .filter_map(|e| {
            let EventKind::ClaimShadowed { claim, .. } = &e.kind else {
                return None;
            };
            Some(*claim)
        })
        .collect()
}

fn outcomes_of(run: &Run) -> Vec<String> {
    let mut outcomes: Vec<String> = run
        .result
        .results
        .iter()
        .map(|r| format!("{:?}", r.result))
        .collect();
    outcomes.sort();
    outcomes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shadowed_agent_submits_for_verification_and_the_conflict_is_counted_from_the_log() {
    // Both rewrite unitPrice's body on one line: whoever claims second is shadowed, and its work,
    // tried on the trunk after the first one merged, conflicts.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let run = run(&tasks, config(2, Policy::Shadow, 1000)).await;
    assert_eq!(outcomes_of(&run), ["Merged", "Shadowed"]);
    let shadows = shadow_claims(&run);
    assert_eq!(shadows.len(), 1, "{:?}", run.result.events);
    let shadow = shadows[0];
    let events = &run.result.events;
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::Submitted { claim, .. } if *claim == shadow)),
        "the shadow work was pushed and submitted like a granted claim's"
    );
    assert_eq!(
        merged_claims(&run),
        1,
        "a shadow submission is never merged"
    );
    let verdicts: Vec<Outcome> = events
        .iter()
        .filter_map(|e| {
            let EventKind::DenialVerified {
                shadow_claim,
                outcome,
                ..
            } = &e.kind
            else {
                return None;
            };
            (*shadow_claim == shadow).then_some(*outcome)
        })
        .collect();
    assert_eq!(verdicts, [Outcome::TextualConflict]);
    assert_eq!(run.result.summary, Summary::from_events(events));
    assert_eq!(run.result.summary.conflicts_prevented_verified, 1);
    assert_eq!(run.result.summary.false_alarms, 0);
    assert_eq!(run.result.summary.denials, 1);
    let trials = run.result.shadow_trials;
    assert_eq!(
        (trials.claims, trials.inconclusive, trials.never_verified),
        (1, 0, 0)
    );
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocker_that_never_merges_leaves_the_denial_unverified_and_prevents_nothing() {
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    // Main changes the same line behind the blocker: its commit will not apply and is rejected.
    let poison: Hook = Box::new(|trunk| {
        let file = trunk.join("src/pricing.ts");
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(
            &file,
            text.replace("return base * qty;", "return qty * base;"),
        )
        .unwrap();
        Git::new(&trunk)
            .commit_all("Change main behind the agents")
            .unwrap();
    });
    let run = run_with_hook(
        &tasks,
        config(2, Policy::Shadow, 1500),
        Some((Duration::from_millis(500), poison)),
    )
    .await;
    assert_eq!(outcomes_of(&run), ["Rejected", "Shadowed"]);
    assert_eq!(merged_claims(&run), 0);
    let summary = &run.result.summary;
    assert_eq!(summary.denials, 1, "the shadow claim is a denial");
    assert_eq!(summary.conflicts_prevented_verified, 0);
    assert_eq!(summary.false_alarms, 0);
    let trials = run.result.shadow_trials;
    assert_eq!(
        (trials.claims, trials.inconclusive, trials.never_verified),
        (1, 0, 1)
    );
    assert!(
        !run.result
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::DenialVerified { .. })),
        "no work landed to conflict with, so nothing was tried"
    );
    run.server.shutdown().await;
}

/// Every shadow claim that was tried: its agent's next claim is logged after the trial. A fork's
/// `main` is replaced by each push, so an agent that went on before the trial would remove the
/// commit the trial needs. Returns how many claims had a next claim to check.
fn trials_precede_the_next_claim(events: &[Event]) -> usize {
    let mut checked = 0;
    for (at, event) in events.iter().enumerate() {
        let EventKind::ClaimShadowed { agent, claim, .. } = &event.kind else {
            continue;
        };
        let verified = |e: &Event| {
            let EventKind::DenialVerified { shadow_claim, .. } = &e.kind else {
                return false;
            };
            shadow_claim == claim
        };
        let tried = events.iter().rposition(verified);
        let next = events[at + 1..].iter().position(|e| {
            let (EventKind::ClaimGranted { agent: a, .. }
            | EventKind::ClaimShadowed { agent: a, .. }) = &e.kind
            else {
                return false;
            };
            a == agent
        });
        if let (Some(tried), Some(next)) = (tried, next) {
            assert!(
                tried < at + 1 + next,
                "{agent:?} claimed again before {claim:?} was tried"
            );
            checked += 1;
        }
    }
    checked
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shadow_agent_does_not_overwrite_its_fork_before_its_trial_has_run() {
    // Four rewrites of one function on two agents: the agent shadowed first goes on to the next
    // task and, once its trial is logged, is shadowed again.
    let tasks = [
        body(1, "unitPrice"),
        body(2, "unitPrice"),
        body(3, "unitPrice"),
        body(4, "unitPrice"),
    ];
    let limit = Duration::from_secs(20);
    let config = OnConfig {
        task_timeout: limit,
        trial_wait: limit,
        ..config(2, Policy::Shadow, 1500)
    };
    let run = run(&tasks, config).await;
    let summary = &run.result.summary;
    let trials = run.result.shadow_trials;
    // A shadow claim submitted after its blocker merged is never tried, so it is never verified;
    // every other one conflicts with its blocker and was tried.
    assert_eq!(trials.inconclusive, 0, "{:?}", run.result.results);
    assert_eq!(
        summary.conflicts_prevented_verified + trials.never_verified,
        trials.claims,
        "{:?}",
        run.result.results
    );
    assert!(
        summary.conflicts_prevented_verified >= 1,
        "{:?}",
        run.result.results
    );
    assert_eq!(summary.false_alarms, 0);
    assert_eq!(summary.denials, trials.claims);
    assert_eq!(*summary, Summary::from_events(&run.result.events));
    // However long the agents wait, the log holds only the observer's own connects: the run's
    // single watcher and the final read.
    let observers = run
        .result
        .events
        .iter()
        .filter(
            |e| matches!(&e.kind, EventKind::AgentConnected { agent } if agent.0 == on::OBSERVER),
        )
        .count();
    assert!(observers <= 2, "{observers} observer connects in the log");
    assert!(trials_precede_the_next_claim(&run.result.events) >= 1);
    // A second wait that never saw its trial would run to the limit.
    for shadowed in run
        .result
        .results
        .iter()
        .filter(|r| r.result == Resolution::Shadowed)
    {
        assert!(shadowed.waited_ms < 10_000, "{shadowed:?}");
    }
    run.server.shutdown().await;
}

/// A shadow run, and when it ended in unix milliseconds.
struct ShadowRun {
    run: Run,
    ended_ms: u64,
}

fn unix_ms() -> u64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    u64::try_from(since.as_millis()).unwrap()
}

/// `seq` of the first event `pick` accepts.
fn first_seq(events: &[Event], pick: impl Fn(&EventKind) -> bool) -> Option<u64> {
    events.iter().find(|e| pick(&e.kind)).map(|e| e.seq)
}

/// Runs two conflicting tasks under the shadow policy until the shadow work was submitted before
/// the blocker merged. The shadow agent skips its work time to get ahead of that merge, but how far
/// ahead it gets is a race against the machine: on a loaded host its git steps can outlast the
/// blocker's work time. The coordinator then rightly tries nothing, because work submitted after
/// the merge has no baseline from before it, so that run says nothing about shadow trials. `None`
/// when no attempt got ahead.
async fn shadow_run_that_got_ahead(work_ms: u64) -> Option<ShadowRun> {
    const ATTEMPTS: usize = 5;
    for _ in 0..ATTEMPTS {
        let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
        let run = run(&tasks, config(2, Policy::Shadow, work_ms)).await;
        let ended_ms = unix_ms();
        let events = &run.result.events;
        let shadows = shadow_claims(&run);
        assert_eq!(shadows.len(), 1, "{:?}", run.result.results);
        let submitted = first_seq(
            events,
            |k| matches!(k, EventKind::Submitted { claim, .. } if *claim == shadows[0]),
        );
        let merged = first_seq(events, |k| matches!(k, EventKind::Merged { .. }));
        if submitted.unwrap() < merged.unwrap() {
            return Some(ShadowRun { run, ended_ms });
        }
        run.server.shutdown().await;
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shadow_submission_is_on_record_early_and_its_work_time_is_still_counted() {
    let work_ms = 2500;
    let Some(ShadowRun { run, ended_ms }) = shadow_run_that_got_ahead(work_ms).await else {
        unreachable!("the shadow work was submitted after the blocker merged in every attempt");
    };
    let events = &run.result.events;
    let shadow = shadow_claims(&run)[0];
    let submitted = events
        .iter()
        .find(|e| matches!(&e.kind, EventKind::Submitted { claim, .. } if *claim == shadow))
        .unwrap();
    // The agent submits first and then spends its work time, so the run cannot have ended before
    // that time has passed since the submission.
    assert!(
        ended_ms - submitted.at_ms >= work_ms,
        "the shadow work was submitted {} ms before the run ended, not {work_ms} ms",
        ended_ms - submitted.at_ms
    );
    let shadowed = run
        .result
        .results
        .iter()
        .find(|r| r.result == Resolution::Shadowed)
        .unwrap();
    assert!(shadowed.work_ms >= work_ms, "{shadowed:?}");
    assert!(
        run.result.work_ms_total >= 2 * work_ms,
        "{}",
        run.result.work_ms_total
    );
    assert_eq!(run.result.summary.conflicts_prevented_verified, 1);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trial_that_lands_after_the_agents_stopped_waiting_is_still_counted() {
    // The shadow agent does not wait for its trial at all, and the run ends when the blocker has
    // merged, before the steward has tried the shadow work: the run's own wait covers it.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let config = OnConfig {
        trial_wait: Duration::ZERO,
        ..config(2, Policy::Shadow, 1000)
    };
    let run = run(&tasks, config).await;
    let trials = run.result.shadow_trials;
    assert_eq!(
        (trials.claims, trials.inconclusive, trials.never_verified),
        (1, 0, 0)
    );
    assert_eq!(run.result.summary.conflicts_prevented_verified, 1);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_open_connections() {
    let scratch = tempfile::tempdir().unwrap();
    let repo = ScratchRepo::parse("swarm-test").unwrap();
    let names = on::principals(1, true);
    let server = LocalServer::start(LocalSetup {
        repo: &repo,
        scratch: scratch.path(),
        base: &demo::base_tree(),
        names: &names,
        reviewers: &[REVIEWER.to_string()],
        shadow_enabled: false,
    })
    .await
    .unwrap();
    let addr = server.addr().unwrap();
    // A client that connected and never finished the handshake must not outlive the server.
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    server.shutdown().await;
    let mut buf = [0u8; 8];
    let closed = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf)).await;
    assert!(
        matches!(closed, Ok(Ok(0) | Err(_))),
        "the connection was left open: {closed:?}"
    );
}

/// Every task is accounted for: merged, rejected or not finished, once each.
fn assert_accounted(run: &Run, tasks: usize) {
    let results = &run.result.results;
    assert_eq!(results.len(), tasks, "{results:?}");
    let count = |wanted: Resolution| results.iter().filter(|r| r.result == wanted).count();
    let unfinished = tasks - count(Resolution::Merged) - count(Resolution::Rejected);
    assert_eq!(count(Resolution::Merged) as u64, run.result.summary.merges);
    assert_eq!(
        count(Resolution::Rejected) as u64,
        run.result.rejected_in_log
    );
    assert_eq!(
        run.result.summary.merges + run.result.rejected_in_log + unfinished as u64,
        tasks as u64
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tasks_left_in_the_queue_when_every_agent_has_stopped_are_still_counted() {
    let tasks: Vec<Task> = [
        "unitPrice",
        "restock",
        "taxFor",
        "available",
        "loyaltyPoints",
    ]
    .iter()
    .enumerate()
    .map(|(i, f)| body(i + 1, f))
    .collect();
    let mut cfg = config(2, Policy::Wait, 0);
    cfg.task_timeout = Duration::from_millis(30);
    let run = run(&tasks, cfg).await;
    let unrun = run
        .result
        .results
        .iter()
        .filter(|r| r.result == Resolution::NotRun)
        .count();
    assert!(
        unrun >= 1,
        "both agents stop at their first timeout: {:?}",
        run.result.results
    );
    assert_accounted(&run, 5);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_scripted_reviewer_a_held_submission_stays_held() {
    let tasks = [signature(1, "unitPrice")];
    let mut cfg = config(1, Policy::Wait, 0);
    cfg.scripted_reviewer = false;
    cfg.task_timeout = Duration::from_secs(2);
    let held = run(&tasks, cfg).await;
    assert_eq!(held.result.reviews_held, 1);
    assert_eq!(held.result.reviews_approved, 0);
    assert_eq!(held.result.summary.merges, 0);
    assert_eq!(held.result.results[0].result, Resolution::TimedOut);
    assert_accounted(&held, 1);
    held.server.shutdown().await;

    let approved = run(&tasks, config(1, Policy::Wait, 0)).await;
    assert_eq!(approved.result.reviews_held, 0);
    assert_eq!(approved.result.reviews_approved, 1);
    assert_eq!(approved.result.summary.merges, 1);
    assert_accounted(&approved, 1);
    approved.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_is_the_time_from_the_claim_to_its_answer_only() {
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let run = run(&tasks, config(2, Policy::Wait, 600)).await;
    let mut waits: Vec<u64> = run.result.results.iter().map(|r| r.waited_ms).collect();
    waits.sort_unstable();
    assert!(waits[0] < 400, "the holder was granted at once: {waits:?}");
    assert!(
        waits[1] >= 500,
        "the second agent queued for the first one's work: {waits:?}"
    );
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_whose_log_watcher_cannot_connect_fails_with_the_watchers_error() {
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    // The first claim is held for a full second of work, so the second is always shadowed and
    // the final wait has an accepted shadow submit to wait for.
    let config = config(2, Policy::Shadow, 1000);
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path()).await;
    let mut endpoint = server.endpoint.clone();
    endpoint.tokens.insert(
        on::OBSERVER.to_string(),
        tessel_swarm::endpoint::Token::new("not-a-valid-token".into()),
    );
    let Err(error) = on::run_on(&endpoint, &tasks, &scratch.path().join("agents"), &config).await
    else {
        unreachable!("a run whose watcher failed must not succeed");
    };
    assert!(
        server
            .log()
            .iter()
            .any(|e| matches!(e.kind, EventKind::ClaimShadowed { .. })),
        "the run had a shadow claim to wait for"
    );
    let message = format!("{error:#}");
    assert!(
        message.contains("the log watch could not reconnect"),
        "the watcher's error leads: {message}"
    );
    assert!(
        message.contains("the log watch stopped before"),
        "the failure it caused is kept as context: {message}"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reviewer_error_does_not_hide_the_watchers_error() {
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let config = config(2, Policy::Shadow, 1000);
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path()).await;
    let mut endpoint = server.endpoint.clone();
    endpoint.tokens.insert(
        on::OBSERVER.to_string(),
        tessel_swarm::endpoint::Token::new("not-a-valid-token".into()),
    );
    endpoint.tokens.remove(REVIEWER);
    let Err(error) = on::run_on(&endpoint, &tasks, &scratch.path().join("agents"), &config).await
    else {
        unreachable!("a run whose watcher and reviewer failed must not succeed");
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("the log watch could not reconnect"),
        "the watcher's error is reported: {message}"
    );
    assert!(
        message.contains("no identity token for agent swarm-reviewer"),
        "the reviewer's error is reported: {message}"
    );
    server.shutdown().await;
}

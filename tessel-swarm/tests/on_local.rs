//! Mode `on` against the local target: the real coordinator core behind a WebSocket server, with
//! a steward that lands work with real git and the demo repository's real tests.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::path::PathBuf;

use std::time::Duration;

use tessel_coordinator::protocol::{ClaimId, Event, EventKind, Outcome, ReleaseReason, Summary};
use tessel_swarm::conn::{Reconnect, HEARTBEAT_EVERY};
use tessel_swarm::demo;
use tessel_swarm::git::{self, Checks, Git};
use tessel_swarm::guard::ScratchRepo;
use tessel_swarm::local::{self, LocalServer, LocalSetup};
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
        heartbeat_every: HEARTBEAT_EVERY,
        max_denials: 400,
        scripted_reviewer: true,
        reconnect: Reconnect::OFF,
    }
}

/// Runs `tasks` on a fresh local target. `hook` runs on a blocking thread `delay` after the first
/// claim is granted and gets the trunk directory, so a test can change main while an agent works.
async fn start_server(config: &OnConfig, scratch: &std::path::Path, lease_ms: u64) -> LocalServer {
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

async fn run_with_hook(tasks: &[Task], config: OnConfig, hook: Option<(Duration, Hook)>) -> Run {
    run_leased(tasks, config, hook, local::LEASE_MS).await
}

async fn run_with_lease(tasks: &[Task], config: OnConfig, lease_ms: u64) -> Run {
    run_leased(tasks, config, None, lease_ms).await
}

async fn run_leased(
    tasks: &[Task],
    config: OnConfig,
    hook: Option<(Duration, Hook)>,
    lease_ms: u64,
) -> Run {
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path(), lease_ms).await;
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
    let limit_ms = u64::try_from(limit.as_millis()).unwrap();
    for shadowed in run
        .result
        .results
        .iter()
        .filter(|r| r.result == Resolution::Shadowed)
    {
        assert!(shadowed.waited_ms < limit_ms, "{shadowed:?}");
    }
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shadow_submission_is_on_record_early_and_its_work_time_is_still_counted() {
    // The shadow agent submits before it spends its work time. The blocker works for the whole of
    // it first, so checkout, commit, push and submit would have to take more than `work_ms` for
    // the shadow to be late.
    let work_ms = 6000;
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let run = run(&tasks, config(2, Policy::Shadow, work_ms)).await;
    let events = &run.result.events;
    let shadows = shadow_claims(&run);
    assert_eq!(shadows.len(), 1, "{:?}", run.result.results);
    let at_ms =
        |pick: &dyn Fn(&EventKind) -> bool| events.iter().find(|e| pick(&e.kind)).unwrap().at_ms;
    let answered =
        at_ms(&|k| matches!(k, EventKind::ClaimShadowed { claim, .. } if *claim == shadows[0]));
    let submitted =
        at_ms(&|k| matches!(k, EventKind::Submitted { claim, .. } if *claim == shadows[0]));
    assert!(
        submitted - answered < work_ms,
        "the shadow work was submitted {} ms after its claim was answered, not before {work_ms} ms",
        submitted - answered
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
        lease_ms: local::LEASE_MS,
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
    assert!(
        waits[0] + 300 <= waits[1],
        "the holder was granted long before the second agent: {waits:?}"
    );
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
    let server = start_server(&config, scratch.path(), local::LEASE_MS).await;
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
    let server = start_server(&config, scratch.path(), local::LEASE_MS).await;
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

/// One agent whose simulated work outlasts the lease, with a heartbeat of `heartbeat_every`.
fn lease_config(heartbeat_every: Duration, work_ms: u64) -> OnConfig {
    OnConfig {
        heartbeat_every,
        ..config(1, Policy::Wait, work_ms)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_is_kept_alive_while_its_agent_works() {
    // The work takes twice the lease; the heartbeat is fifteen times inside it.
    let run = run_with_lease(
        &[body(1, "unitPrice")],
        lease_config(Duration::from_millis(200), 6000),
        3000,
    )
    .await;
    assert_eq!(
        run.result.results[0].result,
        Resolution::Merged,
        "{:?}",
        run.result.results
    );
    assert_eq!(run.result.summary.merges, 1);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lapsed_claim_is_that_tasks_outcome_and_the_run_goes_on() {
    let tasks = [body(1, "unitPrice"), body(2, "restock")];
    let never = Duration::from_secs(3600);
    let run = run_with_lease(&tasks, lease_config(never, 1500), 600).await;
    let results = &run.result.results;
    assert_eq!(results.len(), 2, "{results:?}");
    for r in results {
        assert_eq!(r.result, Resolution::Lapsed, "{r:?}");
        let note = r.note.as_deref().unwrap_or_default();
        assert!(note.starts_with("claim lapsed (lease expired)"), "{note}");
    }
    assert_eq!(run.result.summary.merges, 0);
    assert_accounted(&run, 2);
    run.server.shutdown().await;
}

fn lease_expired(log: &[Event]) -> Vec<ClaimId> {
    log.iter()
        .filter_map(|e| {
            let EventKind::ClaimReleased { claim, reason } = e.kind else {
                return None;
            };
            (reason == ReleaseReason::LeaseExpired).then_some(claim)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_granted_after_a_queue_wait_of_many_leases_is_kept_alive() {
    // The holder works for five leases while the other agent waits in the queue, so the waiter's
    // grant arrives long after its claim was sent. Its claim must still be renewed.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let config = OnConfig {
        heartbeat_every: Duration::from_millis(80),
        ..config(2, Policy::Wait, 2000)
    };
    let run = run_with_lease(&tasks, config, 400).await;
    let outcomes: Vec<Resolution> = run.result.results.iter().map(|r| r.result).collect();
    assert_eq!(outcomes, [Resolution::Merged, Resolution::Merged]);
    assert_eq!(run.result.waits_in_log, 1);
    assert!(
        run.result.results.iter().any(|r| r.waited_ms >= 1600),
        "one agent waited for several leases: {:?}",
        run.result.results
    );
    assert!(lease_expired(&run.result.events).is_empty());
    run.server.shutdown().await;
}

const QUEUED_FOR: Duration = Duration::from_millis(300);

/// Waits until the log shows an agent queued and then `QUEUED_FOR` more, then closes that agent's
/// socket from the coordinator's side without withdrawing its queued request. Returns the agent.
async fn cut_the_first_waiter(log: local::LogReader, cutter: local::Cutter) -> String {
    loop {
        let queued = log.events().into_iter().find_map(|e| {
            let EventKind::WaitQueued { agent, .. } = e.kind else {
                return None;
            };
            Some(agent.0)
        });
        if let Some(agent) = queued {
            tokio::time::sleep(QUEUED_FOR).await;
            assert!(cutter.cut(&agent), "{agent} had an open socket");
            return agent;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_whose_connection_the_coordinator_closes_while_it_waits_ends_only_its_task() {
    // What the live log showed: a queued claim granted long after it was sent lapses exactly one
    // lease later, because the coordinator closed the waiter's socket but kept its request.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let config = OnConfig {
        heartbeat_every: Duration::from_millis(100),
        ..config(2, Policy::Wait, 1500)
    };
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path(), 600).await;
    let cut = tokio::spawn(cut_the_first_waiter(server.reader(), server.cutter()));
    let agents = scratch.path().join("agents");
    let result = on::run_on(&server.endpoint, &tasks, &agents, &config)
        .await
        .unwrap();
    let waiter = cut.await.unwrap();
    let mine = |r: &&on::TaskResult| r.agent == waiter;
    let cut_off = result.results.iter().find(mine).unwrap();
    assert_eq!(cut_off.result, Resolution::Disconnected, "{cut_off:?}");
    let note = cut_off.note.as_deref().unwrap_or_default();
    assert!(
        note.contains("1011") && note.contains("coordinator error"),
        "{note}"
    );
    let queued_for = u64::try_from(QUEUED_FOR.as_millis()).unwrap();
    assert!(
        cut_off.waited_ms >= queued_for,
        "its queue wait is counted: {cut_off:?}"
    );
    assert_eq!(cut_off.work_ms, 0, "it never got to work: {cut_off:?}");
    let other = result.results.iter().find(|r| r.agent != waiter).unwrap();
    assert_eq!(other.result, Resolution::Merged, "{other:?}");
    assert_eq!(result.summary.merges, 1);
    // The coordinator still granted the request of the agent it had cut off, to no socket, and
    // that claim lapses one lease later: the live log's signature.
    let granted_to_waiter = result.events.iter().find_map(|e| {
        let EventKind::ClaimGranted { agent, claim, .. } = &e.kind else {
            return None;
        };
        (agent.0 == waiter).then_some(*claim)
    });
    let granted_to_waiter = granted_to_waiter.expect("the cut-off waiter's request was granted");
    let mut lapsed = Vec::new();
    for _ in 0..100 {
        lapsed = lease_expired(&server.log());
        if !lapsed.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(lapsed, [granted_to_waiter]);
    let run = Run {
        server,
        result,
        _scratch: scratch,
    };
    assert_accounted(&run, 2);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tasks_a_cut_off_agent_never_took_are_done_by_the_others() {
    let tasks = [
        body(1, "unitPrice"),
        body(2, "unitPrice"),
        body(3, "unitPrice"),
        body(4, "unitPrice"),
    ];
    let config = OnConfig {
        heartbeat_every: Duration::from_millis(100),
        ..config(2, Policy::Wait, 800)
    };
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path(), 3000).await;
    let cut = tokio::spawn(cut_the_first_waiter(server.reader(), server.cutter()));
    let agents = scratch.path().join("agents");
    let result = on::run_on(&server.endpoint, &tasks, &agents, &config)
        .await
        .unwrap();
    let waiter = cut.await.unwrap();
    let outcomes: Vec<(&str, Resolution)> = result
        .results
        .iter()
        .map(|r| (r.agent.as_str(), r.result))
        .collect();
    let disconnected: Vec<&(&str, Resolution)> = outcomes
        .iter()
        .filter(|(_, r)| *r == Resolution::Disconnected)
        .collect();
    assert_eq!(disconnected, [&(waiter.as_str(), Resolution::Disconnected)]);
    let merged = outcomes
        .iter()
        .filter(|(agent, r)| *r == Resolution::Merged && *agent != waiter)
        .count();
    assert_eq!(merged, 3, "{outcomes:?}");
    let run = Run {
        server,
        result,
        _scratch: scratch,
    };
    assert_accounted(&run, 4);
    run.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_cut_off_while_it_works_has_its_work_time_counted_as_work() {
    let config = OnConfig {
        heartbeat_every: Duration::from_millis(100),
        ..config(1, Policy::Wait, 1500)
    };
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path(), 600).await;
    let (log, cutter) = (server.reader(), server.cutter());
    let cut = tokio::spawn(async move {
        while !log
            .events()
            .iter()
            .any(|e| matches!(e.kind, EventKind::ClaimGranted { .. }))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(QUEUED_FOR).await;
        assert!(cutter.cut("a01"), "a01 had an open socket");
    });
    let agents = scratch.path().join("agents");
    let result = on::run_on(&server.endpoint, &[body(1, "unitPrice")], &agents, &config)
        .await
        .unwrap();
    cut.await.unwrap();
    let r = &result.results[0];
    assert_eq!(r.result, Resolution::Disconnected, "{r:?}");
    assert!(
        r.work_ms >= 1500,
        "the work it did is counted as work: {r:?}"
    );
    assert!(r.waited_ms < r.work_ms, "and not as waiting: {r:?}");
    assert_eq!(result.summary.merges, 0);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn time_a_submission_spends_held_for_review_is_not_work_when_the_agent_is_cut_off() {
    // Nobody reviews, so the submission stays held; the agent is cut off `HELD_FOR` later.
    const HELD_FOR: Duration = Duration::from_secs(8);
    let held_ms = u64::try_from(HELD_FOR.as_millis()).unwrap();
    let mut config = config(1, Policy::Wait, 0);
    config.scripted_reviewer = false;
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path(), local::LEASE_MS).await;
    let (log, cutter) = (server.reader(), server.cutter());
    let cut = tokio::spawn(async move {
        while !log
            .events()
            .iter()
            .any(|e| matches!(e.kind, EventKind::ReviewRequested { .. }))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(HELD_FOR).await;
        assert!(cutter.cut("a01"), "a01 had an open socket");
    });
    let agents = scratch.path().join("agents");
    let tasks = [signature(1, "unitPrice")];
    let result = on::run_on(&server.endpoint, &tasks, &agents, &config)
        .await
        .unwrap();
    cut.await.unwrap();
    let r = &result.results[0];
    assert_eq!(r.result, Resolution::Disconnected, "{r:?}");
    assert!(
        r.work_ms < held_ms,
        "work ends at the push, not at the close: {r:?}"
    );
    assert!(r.waited_ms < held_ms, "{r:?}");
    assert_eq!(result.summary.merges, 0);
    server.shutdown().await;
}

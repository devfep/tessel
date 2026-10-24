//! Mode `on` against the local target: the real coordinator core behind a WebSocket server, with
//! a steward that lands work with real git and the demo repository's real tests.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt;
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
        let trunk = server.endpoint.remote.trunk.clone();
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
    run.server.endpoint.remote.trunk.clone()
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
    let limit = Duration::from_secs(60);
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
    // The shadow agent submits before it spends its work time; the blocker spends it first. What
    // is asserted is the order in the event log: the shadow's `Submitted` comes before the
    // blocker's. A claim-to-submit span is not compared, because the two agents' checkout, commit
    // and push are not equally slow on a loaded machine, so a bound on the difference of the spans
    // fails without the shadow having waited. Events can share a millisecond, so the log position
    // decides, not `at_ms`. The shadow's result still counts its work time, and so does the total.
    let work_ms = 6000;
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let run = run(&tasks, config(2, Policy::Shadow, work_ms)).await;
    let events = &run.result.events;
    let shadows = shadow_claims(&run);
    assert_eq!(shadows.len(), 1, "{:?}", run.result.results);
    let Some(EventKind::ClaimGranted { claim: holder, .. }) = events
        .iter()
        .map(|e| &e.kind)
        .find(|k| matches!(k, EventKind::ClaimGranted { .. }))
    else {
        unreachable!("the holder was granted its claim");
    };
    let submitted_at = |wanted: &ClaimId| {
        events
            .iter()
            .position(|e| matches!(&e.kind, EventKind::Submitted { claim, .. } if claim == wanted))
    };
    let (shadow_submitted, holder_submitted) = (submitted_at(&shadows[0]), submitted_at(holder));
    assert!(
        shadow_submitted.is_some() && shadow_submitted < holder_submitted,
        "the shadow's Submitted is at log position {shadow_submitted:?}, the blocker's at \
         {holder_submitted:?}: the shadow did not submit first"
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

/// How long a wait on the log or on a push may last before the test says what it waited for. Only
/// a task that died reaches it, so it is far above anything a loaded machine takes.
const WAIT: Duration = Duration::from_secs(150);

/// How often a wait looks at the log, a push or a heartbeat count again.
const POLL: Duration = Duration::from_millis(5);

/// Waits until `find` returns something for the log, and returns it.
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
            "gave up after {WAIT:?} waiting for {what}; the log held {} events",
            events.len()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Holds each agent's push to its fork until the test lets that agent's push through, so that a
/// test can keep an agent's claim open and unsubmitted for as long as it needs: a submitted claim
/// does not lapse. A fork's `pre-receive` hook records that a push arrived, then waits for the
/// file that `open` removes, not for a duration.
struct PushGate {
    dir: tempfile::TempDir,
}

impl PushGate {
    fn install(server: &LocalServer) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let forks = server.endpoint.remote.forks.clone();
        for fork in std::fs::read_dir(forks).unwrap() {
            let fork = fork.unwrap().path();
            let name = fork.file_stem().unwrap().to_string_lossy().into_owned();
            let gate = dir.path().display();
            std::fs::write(dir.path().join(format!("closed-{name}")), "").unwrap();
            // The hook gives up when its gate has been closed for `WAIT` by the clock, not by
            // a count of polls, which a loaded machine stretches: a test binary that is killed
            // leaves no hook waiting for ever.
            let secs = WAIT.as_secs();
            let script = format!(
                "#!/bin/sh\ncat >/dev/null\n: > '{gate}/arrived-{name}'\n\
                 end=$(( $(date +%s) + {secs} ))\n\
                 while [ -e '{gate}/closed-{name}' ] && [ \"$(date +%s)\" -lt \"$end\" ]; do\n\
                 sleep 0.02; done\n\
                 [ ! -e '{gate}/closed-{name}' ] || exit 1\n"
            );
            let hook = fork.join("hooks/pre-receive");
            std::fs::write(&hook, script).unwrap();
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { dir }
    }

    /// Waits until `agent` has pushed: its answer is read, its work is done, and it cannot submit
    /// before `open`.
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

    fn open(&self, agent: &str) {
        std::fs::remove_file(self.dir.path().join(format!("closed-{agent}"))).unwrap();
    }
}

/// Moves the coordinator's clock in steps, each taken only after the agents it names have sent a
/// heartbeat since the previous step, so that a step shorter than a lease never lapses a claim
/// whose agent keeps renewing it, however slowly the machine runs.
struct Beats<'a> {
    server: &'a LocalServer,
    agents: Vec<String>,
    seen: Vec<usize>,
}

impl<'a> Beats<'a> {
    fn new(server: &'a LocalServer, agents: &[&str]) -> Self {
        let agents: Vec<String> = agents.iter().map(|a| (*a).to_string()).collect();
        let seen = agents.iter().map(|a| server.heartbeats(a)).collect();
        Self {
            server,
            agents,
            seen,
        }
    }

    /// The agents that have sent no heartbeat since the last step.
    fn silent(&self) -> Vec<&str> {
        let owed = |(agent, seen): &(&String, &usize)| self.server.heartbeats(agent) <= **seen;
        let pairs = self.agents.iter().zip(&self.seen);
        pairs
            .filter(owed)
            .map(|(agent, _)| agent.as_str())
            .collect()
    }

    /// Waits for a heartbeat from each agent, then moves the clock by `ms` and waits for an expiry
    /// pass that saw the new time.
    async fn step(&mut self, ms: u64) {
        let started = tokio::time::Instant::now();
        while self
            .agents
            .iter()
            .zip(&self.seen)
            .any(|(agent, seen)| self.server.heartbeats(agent) <= *seen)
        {
            assert!(
                started.elapsed() < WAIT,
                "gave up after {WAIT:?} waiting for a heartbeat from {:?}",
                self.silent()
            );
            tokio::time::sleep(POLL).await;
        }
        self.advance(ms).await;
    }

    /// Moves the clock by `ms` without waiting for a heartbeat: for the first step after a grant,
    /// which is shorter than the lease the grant just started.
    async fn advance(&mut self, ms: u64) {
        self.server.advance_clock(ms);
        // Counted after the advance: a heartbeat handled before it has not renewed at the new time.
        self.seen = self
            .agents
            .iter()
            .map(|a| self.server.heartbeats(a))
            .collect();
        self.server.settle_expiry().await;
    }
}

/// The coordinator's time, in milliseconds, of the first event `pick` accepts.
fn logged_at(events: &[Event], pick: impl Fn(&EventKind) -> bool) -> u64 {
    events.iter().find(|e| pick(&e.kind)).unwrap().at_ms
}

const LEASE_MS: u64 = 400;

/// A step of the coordinator's clock: less than the lease, so that one heartbeat between two
/// steps is enough to keep a claim, however long the real time between the heartbeats is.
const STEP_MS: u64 = LEASE_MS * 3 / 4;

/// The agent that got the first grant.
fn first_granted(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| {
        let EventKind::ClaimGranted { agent, .. } = &e.kind else {
            return None;
        };
        Some(agent.0.clone())
    })
}

/// The agent whose request was queued first.
fn first_queued(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| {
        let EventKind::WaitQueued { agent, .. } = &e.kind else {
            return None;
        };
        Some(agent.0.clone())
    })
}

/// `Some` when the log holds a grant to `waiter`.
fn granted_to(events: &[Event], waiter: &str) -> Option<()> {
    events.iter().find_map(|e| {
        let EventKind::ClaimGranted { agent, .. } = &e.kind else {
            return None;
        };
        (agent.0 == waiter).then_some(())
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_is_kept_alive_while_its_agent_works() {
    // The coordinator's clock runs past two leases while the agent works, and only its heartbeats
    // renew the claim: between two steps the clock has moved three quarters of a lease.
    let lease_ms = 3000;
    let step_ms = lease_ms * 3 / 4;
    let config = lease_config(Duration::from_millis(200), 0);
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path(), lease_ms).await;
    server.freeze_clock();
    let gate = PushGate::install(&server);
    let log = server.reader();
    let drive = async {
        let agent = wait_for("a grant", &log, first_granted).await;
        gate.push_arrived(&agent).await;
        let mut beats = Beats::new(&server, &[&agent]);
        for _ in 0..3 {
            beats.step(step_ms).await;
        }
        gate.open(&agent);
    };
    let agents = scratch.path().join("agents");
    let tasks = [body(1, "unitPrice")];
    let (result, ()) = tokio::join!(
        on::run_on(&server.endpoint, &tasks, &agents, &config),
        drive
    );
    let result = result.unwrap();
    assert_eq!(
        result.results[0].result,
        Resolution::Merged,
        "{:?}",
        result.results
    );
    assert_eq!(result.summary.merges, 1);
    assert_eq!(lease_expired(&result.events), []);
    let granted = logged_at(&result.events, |k| {
        matches!(k, EventKind::ClaimGranted { .. })
    });
    let merged = logged_at(&result.events, |k| matches!(k, EventKind::Merged { .. }));
    assert_eq!(
        merged - granted,
        3 * step_ms,
        "the claim lived through three steps, two and a quarter leases, on the coordinator's clock"
    );
    server.shutdown().await;
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

/// Steps of the coordinator's clock the waiter queues through: six leases.
const QUEUE_STEPS: u64 = 8;

/// Steps after the waiter's grant: two and a quarter leases.
const GRANT_STEPS: u64 = 3;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_granted_after_a_queue_wait_of_many_leases_is_kept_alive() {
    // The coordinator's clock runs six leases while one agent holds the scope and the other waits
    // in the queue, so the waiter's grant arrives long after its claim was sent. Its expiry counts
    // from the grant, and its heartbeats renew it for two more leases. Both agents' pushes are
    // held, so neither claim is submitted, and the clock moves only after a heartbeat.
    let tasks = [body(1, "unitPrice"), body(2, "unitPrice")];
    let config = OnConfig {
        heartbeat_every: Duration::from_millis(80),
        ..config(2, Policy::Wait, 0)
    };
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&config, scratch.path(), LEASE_MS).await;
    server.freeze_clock();
    let gate = PushGate::install(&server);
    let log = server.reader();
    let drive = async {
        let holder = wait_for("the first grant", &log, first_granted).await;
        let waiter = wait_for("a queued claim", &log, first_queued).await;
        gate.push_arrived(&holder).await;
        let mut beats = Beats::new(&server, &[&holder, &waiter]);
        for _ in 0..QUEUE_STEPS {
            beats.step(STEP_MS).await;
        }
        gate.open(&holder);
        wait_for("the waiter's grant", &log, |events| {
            granted_to(events, &waiter)
        })
        .await;
        gate.push_arrived(&waiter).await;
        let mut beats = Beats::new(&server, &[&waiter]);
        beats.advance(STEP_MS).await;
        for _ in 1..GRANT_STEPS {
            beats.step(STEP_MS).await;
        }
        gate.open(&waiter);
    };
    let agents = scratch.path().join("agents");
    let (result, ()) = tokio::join!(
        on::run_on(&server.endpoint, &tasks, &agents, &config),
        drive
    );
    let result = result.unwrap();
    let outcomes: Vec<Resolution> = result.results.iter().map(|r| r.result).collect();
    assert_eq!(outcomes, [Resolution::Merged, Resolution::Merged]);
    assert_eq!(result.waits_in_log, 1);
    assert_eq!(lease_expired(&result.events), []);
    let events = &result.events;
    let queued = logged_at(events, |k| matches!(k, EventKind::WaitQueued { .. }));
    let waiter_granted = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ClaimGranted { .. }))
        .nth(1)
        .unwrap()
        .at_ms;
    let last_merged = events
        .iter()
        .rfind(|e| matches!(e.kind, EventKind::Merged { .. }))
        .unwrap()
        .at_ms;
    assert_eq!(
        waiter_granted - queued,
        QUEUE_STEPS * STEP_MS,
        "the waiter queued for six leases on the coordinator's clock"
    );
    assert_eq!(
        last_merged - waiter_granted,
        GRANT_STEPS * STEP_MS,
        "its claim lived {GRANT_STEPS} steps, two and a quarter leases, past the grant"
    );
    server.shutdown().await;
}

/// How long the waiter stays queued, in real time, before it is cut. It is a minimum that the
/// test then reads back as the lower bound of the agent's own measured wait, so a stall on a
/// loaded machine only lengthens it and never breaks the bound. The holder's push is held until
/// the cut has returned, so the order of the cut and the holder's release does not depend on it.
const QUEUED_FOR: Duration = Duration::from_millis(300);

/// Waits until the log shows an agent queued and then `QUEUED_FOR` more, then closes that agent's
/// socket from the coordinator's side without withdrawing its queued request, and then lets the
/// holder's push through. Returns the agent that was cut.
async fn cut_the_first_waiter(
    log: &local::LogReader,
    cutter: &local::Cutter,
    gate: &PushGate,
) -> String {
    let waiter = wait_for("a queued claim", log, first_queued).await;
    let holder = wait_for("the first grant", log, first_granted).await;
    tokio::time::sleep(QUEUED_FOR).await;
    assert!(cutter.cut(&waiter), "{waiter} had an open socket");
    gate.open(&holder);
    waiter
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
    let lease_ms = 600;
    let server = start_server(&config, scratch.path(), lease_ms).await;
    server.freeze_clock();
    let gate = PushGate::install(&server);
    let (log, cutter) = (server.reader(), server.cutter());
    let agents = scratch.path().join("agents");
    let (result, waiter) = tokio::join!(
        on::run_on(&server.endpoint, &tasks, &agents, &config),
        cut_the_first_waiter(&log, &cutter, &gate)
    );
    let result = result.unwrap();
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
    // that claim lapses one lease later on the coordinator's clock: the live log's signature. The
    // other agent's claim is gone by now, so no heartbeat is owed to anything that is still open.
    let granted_to_waiter = result.events.iter().find_map(|e| {
        let EventKind::ClaimGranted { agent, claim, .. } = &e.kind else {
            return None;
        };
        (agent.0 == waiter).then_some(*claim)
    });
    let granted_to_waiter = granted_to_waiter.expect("the cut-off waiter's request was granted");
    server.advance_clock(lease_ms - 1);
    server.settle_expiry().await;
    assert_eq!(
        lease_expired(&server.log()),
        [],
        "the claim lives until a full lease has passed since its grant"
    );
    server.advance_clock(1);
    server.settle_expiry().await;
    assert_eq!(lease_expired(&server.log()), [granted_to_waiter]);
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
    let lease_ms = 3000;
    let server = start_server(&config, scratch.path(), lease_ms).await;
    server.freeze_clock();
    let gate = PushGate::install(&server);
    let (log, cutter) = (server.reader(), server.cutter());
    // The cut waiter's request is still queued, so it is granted when the holder releases, to a
    // socket nobody hears. The others can take the rest only once that claim lapses, and the
    // clock moves a lease past its grant, a step after each heartbeat of the agent still queued.
    let drive = async {
        let waiter = cut_the_first_waiter(&log, &cutter, &gate).await;
        let holder = wait_for("the first grant", &log, first_granted).await;
        wait_for("the cut waiter's grant", &log, |events| {
            granted_to(events, &waiter)
        })
        .await;
        let mut beats = Beats::new(&server, &[&holder]);
        for _ in 0..2 {
            beats.step(lease_ms * 3 / 4).await;
        }
        waiter
    };
    let agents = scratch.path().join("agents");
    let (result, waiter) = tokio::join!(
        on::run_on(&server.endpoint, &tasks, &agents, &config),
        drive
    );
    let result = result.unwrap();
    let ghost = result.events.iter().find_map(|e| {
        let EventKind::ClaimGranted { agent, claim, .. } = &e.kind else {
            return None;
        };
        (agent.0 == waiter).then_some((*claim, e.at_ms))
    });
    let (ghost, granted) = ghost.expect("the cut-off waiter's request was granted");
    assert_eq!(
        lease_expired(&result.events),
        [ghost],
        "the cut waiter's claim lapsed, and nothing else did"
    );
    let lapsed = logged_at(&result.events, |k| {
        matches!(
            k,
            EventKind::ClaimReleased {
                reason: ReleaseReason::LeaseExpired,
                ..
            }
        )
    });
    assert_eq!(
        lapsed - granted,
        2 * (lease_ms * 3 / 4),
        "it lapsed at the second step after its grant, on the coordinator's clock"
    );
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
    let lease_ms = 600;
    let server = start_server(&config, scratch.path(), lease_ms).await;
    server.freeze_clock();
    let gate = PushGate::install(&server);
    let (log, cutter) = (server.reader(), server.cutter());
    // The agent's push is held until the cut has returned, so the cut comes before its submission
    // however slowly the machine runs, and the work time it has by then is all its own.
    let cut = async {
        let agent = wait_for("a grant", &log, first_granted).await;
        gate.push_arrived(&agent).await;
        assert!(cutter.cut(&agent), "{agent} had an open socket");
        gate.open(&agent);
    };
    let agents = scratch.path().join("agents");
    let tasks = [body(1, "unitPrice")];
    let (result, ()) = tokio::join!(on::run_on(&server.endpoint, &tasks, &agents, &config), cut);
    let result = result.unwrap();
    let r = &result.results[0];
    assert_eq!(r.result, Resolution::Disconnected, "{r:?}");
    assert!(
        r.work_ms >= 1500,
        "the work it did is counted as work: {r:?}"
    );
    assert!(r.waited_ms < r.work_ms, "and not as waiting: {r:?}");
    assert_eq!(result.summary.merges, 0);
    // The cut agent's claim lapses a lease after its grant on the coordinator's clock, which has
    // not moved since it was frozen.
    server.settle_expiry().await;
    assert_eq!(
        lease_expired(&server.log()),
        [],
        "the frozen clock lapses nothing"
    );
    server.advance_clock(lease_ms - 1);
    server.settle_expiry().await;
    assert_eq!(
        lease_expired(&server.log()),
        [],
        "a lease has not passed yet"
    );
    server.advance_clock(1);
    server.settle_expiry().await;
    assert_eq!(
        lease_expired(&server.log()).len(),
        1,
        "a full lease has passed"
    );
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
    let at_ms = |pick: &dyn Fn(&EventKind) -> bool| {
        result.events.iter().find(|e| pick(&e.kind)).unwrap().at_ms
    };
    let granted = at_ms(&|k| matches!(k, EventKind::ClaimGranted { .. }));
    let requested = at_ms(&|k| matches!(k, EventKind::ReviewRequested { .. }));
    assert!(
        r.work_ms < requested - granted + held_ms / 2,
        "work ends at the push, not at the close: {r:?}, {} ms to the submission",
        requested - granted
    );
    assert!(r.waited_ms < held_ms, "{r:?}");
    assert_eq!(result.summary.merges, 0);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_whose_own_checks_fail_reports_the_failure_not_the_noise_around_it() {
    let tasks = [body(1, "unitPrice")];
    let cfg = config(1, Policy::Wait, 0);
    let scratch = tempfile::tempdir().unwrap();
    let server = start_server(&cfg, scratch.path(), local::LEASE_MS).await;
    let trunk = server.endpoint.remote.trunk.clone();
    // The red test logs far more than the note may hold before it throws, and a passing test is
    // named with as much after it, so a cap that is too large lets noise in and neither the head
    // nor the tail of the output holds the failure.
    std::fs::write(
        trunk.join("test/red.test.ts"),
        "import { test } from \"node:test\";\n\
         console.log(\"x\".repeat(4096));\n\
         test(\"red\", () => { throw new Error(\"the trunk is red\"); });\n\
         test(\"y\".repeat(4096), () => {});\n",
    )
    .unwrap();
    Git::new(&trunk).commit_all("Make main red").unwrap();
    let agents = scratch.path().join("agents");
    let result = on::run_on(&server.endpoint, &tasks, &agents, &cfg)
        .await
        .unwrap();
    let r = &result.results[0];
    assert_eq!(r.result, Resolution::Failed, "{r:?}");
    let note = r.note.as_deref().unwrap();
    let (head, quoted) = note.split_once("quoted as data: ").unwrap();
    assert!(
        head.starts_with(
            "the agent's own checks failed on the checkout it edited; exit status: 1;"
        ),
        "{note}"
    );
    let kept: String = serde_json::from_str(quoted).unwrap();
    assert!(kept.len() <= 1500, "{} bytes", kept.len());
    assert!(kept.starts_with("not ok"), "{kept}");
    assert!(kept.contains("the trunk is red"), "{kept}");
    assert!(kept.contains("# fail 1"), "{kept}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_is_kept_alive_while_its_agents_own_checks_run() {
    // A test in main waits for the driver while the agent's checks run, so they outlast the lease
    // with no simulated work around them and nothing but the heartbeat renews it. The coordinator's
    // clock runs past two leases meanwhile, a step after each heartbeat; the test in main reads
    // the files the driver writes, so no timer decides how long the checks last.
    let cfg = lease_config(Duration::from_millis(200), 0);
    let scratch = tempfile::tempdir().unwrap();
    let lease_ms = 3000;
    let step_ms = lease_ms * 3 / 4;
    let server = start_server(&cfg, scratch.path(), lease_ms).await;
    server.freeze_clock();
    let trunk = server.endpoint.remote.trunk.clone();
    let signals = tempfile::tempdir().unwrap();
    let (started, release) = (
        signals.path().join("started"),
        signals.path().join("release"),
    );
    std::fs::write(
        trunk.join("test/slow.test.ts"),
        format!(
            "import {{ existsSync, writeFileSync }} from \"node:fs\";\n\
             import {{ test }} from \"node:test\";\n\
             import {{ setTimeout as pause }} from \"node:timers/promises\";\n\
             test(\"slow\", async () => {{\n\
             writeFileSync(\"{}\", \"\");\n\
             const end = Date.now() + {};\n\
             while (Date.now() < end && !existsSync(\"{}\")) await pause(20);\n}});\n",
            started.display(),
            WAIT.as_millis(),
            release.display()
        ),
    )
    .unwrap();
    Git::new(&trunk).commit_all("Add a slow test").unwrap();
    let log = server.reader();
    let drive = async {
        let agent = wait_for("a grant", &log, first_granted).await;
        let waiting = tokio::time::Instant::now();
        while !started.exists() {
            assert!(
                waiting.elapsed() < WAIT,
                "gave up waiting for the checks to start"
            );
            tokio::time::sleep(POLL).await;
        }
        let mut beats = Beats::new(&server, &[&agent]);
        for _ in 0..3 {
            beats.step(step_ms).await;
        }
        std::fs::write(&release, "").unwrap();
    };
    let agents = scratch.path().join("agents");
    let tasks = [body(1, "unitPrice")];
    let (result, ()) = tokio::join!(on::run_on(&server.endpoint, &tasks, &agents, &cfg), drive);
    let result = result.unwrap();
    let r = &result.results[0];
    assert_eq!(r.result, Resolution::Merged, "{r:?}");
    assert_eq!(lease_expired(&result.events), []);
    let granted = logged_at(&result.events, |k| {
        matches!(k, EventKind::ClaimGranted { .. })
    });
    let merged = logged_at(&result.events, |k| matches!(k, EventKind::Merged { .. }));
    assert_eq!(
        merged - granted,
        3 * step_ms,
        "the checks outlasted two leases on the coordinator's clock"
    );
    server.shutdown().await;
}

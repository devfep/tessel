//! Mode `on` against the local target: the real coordinator core behind a WebSocket server, with
//! a steward that lands work with real git and the demo repository's real tests.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::path::PathBuf;

use std::time::Duration;

use tessel_coordinator::protocol::{EventKind, Summary};
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
        max_denials: 400,
    }
}

/// Runs `tasks` on a fresh local target. `hook` runs on a blocking thread `delay` after the first
/// claim is granted and gets the trunk directory, so a test can change main while an agent works.
async fn run_with_hook(tasks: &[Task], config: OnConfig, hook: Option<(Duration, Hook)>) -> Run {
    let scratch = tempfile::tempdir().unwrap();
    let repo = ScratchRepo::parse("swarm-test").unwrap();
    let names = on::principals(config.agents);
    let base = demo::base_tree();
    let server = LocalServer::start(LocalSetup {
        repo: &repo,
        scratch: &scratch.path().join("server"),
        base: &base,
        names: &names,
        reviewers: &[REVIEWER.to_string()],
    })
    .await
    .unwrap();
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
        .filter_map(|e| match &e.kind {
            EventKind::ClaimGranted { intent, .. } => Some(intent.summary.clone()),
            _ => None,
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
            .position(|e| matches!(&e.kind, EventKind::ClaimGranted { intent, .. } if intent.summary.starts_with(label)))
            .unwrap()
    };
    let restock_claim = events.iter().find_map(|e| match &e.kind {
        EventKind::ClaimGranted { claim, intent, .. } if intent.summary.starts_with("t03") => {
            Some(*claim)
        }
        _ => None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_open_connections() {
    let scratch = tempfile::tempdir().unwrap();
    let repo = ScratchRepo::parse("swarm-test").unwrap();
    let names = on::principals(1);
    let server = LocalServer::start(LocalSetup {
        repo: &repo,
        scratch: scratch.path(),
        base: &demo::base_tree(),
        names: &names,
        reviewers: &[REVIEWER.to_string()],
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

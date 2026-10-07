//! A claim stays alive through a slow checkout and a slow push. Both talk to the remote while the
//! claim is held, which makes them the steps most likely to be slow against a live remote, so this
//! test slows them down.
//!
//! This file holds one test because it puts a `git` wrapper first on `PATH` for its whole process.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use tessel_swarm::conn::HEARTBEAT_EVERY;
use tessel_swarm::demo;
use tessel_swarm::guard::ScratchRepo;
use tessel_swarm::local::{LocalServer, LocalSetup};
use tessel_swarm::on::{self, OnConfig, Policy, Resolution, REVIEWER};
use tessel_swarm::tasks::{Kind, Task};

const LEASE_MS: u64 = 1000;
const SLOW_SYNC: Duration = Duration::from_millis(2500);

/// Puts a `git` first on `PATH` that sleeps in the third `fetch` it runs in an agent's checkout
/// and in its `push`. The first fetch is the agent's checkout, the second the sync that plans its
/// claim, and the third the sync after the grant, which happens while the claim is held.
fn slow_remote(dir: &std::path::Path) {
    let real = std::process::Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap();
    let counter = dir.join("fetches");
    let script = format!(
        "#!/bin/sh\n\
         case \"$*\" in *push*) case \"$PWD\" in */agents/*) sleep {secs};; esac;; esac\n\
         case \"$*\" in *fetch*) case \"$PWD\" in */agents/*)\n\
         echo x >> '{counter}'\n\
         if [ \"$(wc -l < '{counter}')\" -eq 3 ]; then sleep {secs}; fi;;\n\
         esac;; esac\n\
         exec '{real}' \"$@\"\n",
        counter = counter.display(),
        real = real.trim(),
        secs = SLOW_SYNC.as_secs_f64(),
    );
    let wrapper = dir.join("git");
    std::fs::write(&wrapper, script).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var("PATH").unwrap();
    std::env::set_var("PATH", format!("{}:{path}", dir.display()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_survives_a_checkout_and_a_push_slower_than_its_lease() {
    let scratch = tempfile::tempdir().unwrap();
    slow_remote(scratch.path());
    let config = OnConfig {
        agents: 1,
        policy: Policy::Wait,
        work_ms: 0,
        task_timeout: Duration::from_secs(60),
        trial_wait: Duration::from_secs(60),
        heartbeat_every: HEARTBEAT_EVERY.min(Duration::from_millis(100)),
        max_denials: 400,
        scripted_reviewer: true,
    };
    let repo = ScratchRepo::parse("swarm-test").unwrap();
    let names = on::principals(1, true);
    let server = LocalServer::start(LocalSetup {
        repo: &repo,
        scratch: &scratch.path().join("server"),
        base: &demo::base_tree(),
        names: &names,
        reviewers: &[REVIEWER.to_string()],
        shadow_enabled: false,
        lease_ms: LEASE_MS,
    })
    .await
    .unwrap();
    let task = Task {
        id: 1,
        func: "unitPrice".into(),
        kind: Kind::Body,
    };
    let result = on::run_on(
        &server.endpoint,
        &[task],
        &scratch.path().join("agents"),
        &config,
    )
    .await
    .unwrap();
    assert_eq!(
        result.results[0].result,
        Resolution::Merged,
        "{:?}",
        result.results
    );
    server.shutdown().await;
}

//! `SudoRunner` through the real sudoers rule, with the real accounts and the real `tessel-runner`.
//!
//! These tests are ignored: they need `scripts/ci/own-users-linux.sh`'s layout (the accounts
//! `_tessel` and `_tessel-runner`, `/opt/tessel/bin/tessel-runner`, the rule, the runner's
//! configuration and a stand-in `cargo` that does what a marker file in the tree says). The Linux
//! job builds this test binary, copies it where `_tessel` can run it, and runs it as `_tessel` with
//! `--ignored --test-threads=1`: each test builds its own runner, and a second one would kill the
//! first's processes. On a machine without that layout they fail, which is why they are ignored.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tessel_runner::{RequestTest, RunRequest, RunnerError, MAX_REQUEST_BYTES, MAX_TAIL_BYTES};
use tessel_server::landing::{SudoRunner, SudoRunnerSettings, TestRunner};

const HANDOFF: &str = "/var/tessel/handoff";

fn runner() -> SudoRunner {
    SudoRunner::new(SudoRunnerSettings {
        user: "_tessel-runner".to_owned(),
        sudo: PathBuf::from("/usr/bin/sudo"),
        runner: PathBuf::from("/opt/tessel/bin/tessel-runner"),
        runner_config: PathBuf::from("/var/tessel-runner/tessel-runner.toml"),
        kill: PathBuf::from("/bin/kill"),
        ps: PathBuf::from("/bin/ps"),
        handoff_dir: PathBuf::from(HANDOFF),
        settle: Duration::from_secs(10),
    })
    .expect("a runner through the real rule")
}

/// A call directory in the handoff directory with a tree in it whose `marker` file tells the
/// stand-in `cargo` what to do. Private, as the lander makes it.
fn tree(name: &str, marker: &str) -> RunRequest {
    let call = PathBuf::from(HANDOFF).join(name);
    let tree = call.join("test");
    fs::create_dir_all(&tree).expect("a tree");
    fs::write(tree.join("marker"), marker).expect("the marker");
    fs::set_permissions(&call, fs::Permissions::from_mode(0o700)).expect("chmod");
    request_for(tree.to_str().expect("utf-8"))
}

fn request_for(tree: &str) -> RunRequest {
    RunRequest {
        tree: tree.to_owned(),
        tests: vec![RequestTest {
            dir: String::new(),
            argv: ["cargo", "test", "--workspace"].map(str::to_owned).to_vec(),
        }],
        timeout_secs: 60,
    }
}

fn account_has_no_process() -> bool {
    let output = Command::new("/bin/ps")
        .args(["-U", "_tessel-runner", "-o", "pid="])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout).trim().is_empty()
}

#[test]
#[ignore = "needs the accounts of scripts/ci/own-users-linux.sh; run as _tessel"]
fn a_run_goes_through_the_real_rule_and_the_tests_run_in_a_copy() {
    let runner = runner();
    let result = runner
        .run(&tree("real-plain", "plain"))
        .expect("a result through the rule");
    assert!(result.passed, "{result:?}");
    assert!(result.stdout.contains("ran"), "{}", result.stdout);
}

#[test]
#[ignore = "needs the accounts of scripts/ci/own-users-linux.sh; run as _tessel"]
fn the_command_sees_the_configurations_environment_whatever_the_servers_is() {
    let runner = runner();
    std::env::set_var("LD_PRELOAD", "/nonexistent/evil.so");
    std::env::set_var("PATH", "/tmp/evil");
    let result = runner.run(&tree("real-env", "env")).expect("a result");
    let seen = result.stdout;
    assert!(!seen.contains("LD_PRELOAD"), "{seen}");
    assert!(!seen.contains("/tmp/evil"), "{seen}");
    assert!(seen.contains("HOME=/var/empty"), "{seen}");
    assert!(
        seen.contains("PATH=/var/tessel/toolchain/bin:/usr/bin:/bin"),
        "{seen}"
    );
}

#[test]
#[ignore = "needs the accounts of scripts/ci/own-users-linux.sh; run as _tessel"]
fn a_refusal_and_a_run_that_cannot_start_are_errors_and_output_past_the_tail_limit_is_cut() {
    let runner = runner();
    let over = "x".repeat(MAX_REQUEST_BYTES + 1);
    let refused = runner.run(&request_for(&over)).unwrap_err();
    assert!(
        matches!(
            refused,
            RunnerError::Unsupervised {
                what: "refused",
                ..
            }
        ),
        "{refused}"
    );
    let mut no_npm = tree("real-nonpm", "plain");
    no_npm.tests[0].argv = ["npm", "test"].map(str::to_owned).to_vec();
    let unsupervised = runner.run(&no_npm).unwrap_err();
    assert!(
        matches!(
            unsupervised,
            RunnerError::Unsupervised {
                what: "unsupervised",
                ..
            }
        ),
        "{unsupervised}"
    );
    let big = runner.run(&tree("real-big", "big")).expect("a result");
    assert!(big.stdout.len() <= MAX_TAIL_BYTES && big.stdout_truncated);
}

#[test]
#[ignore = "needs the accounts of scripts/ci/own-users-linux.sh; run as _tessel"]
fn nothing_one_run_leaves_in_the_temporary_or_work_directory_reaches_the_next() {
    let runner = runner();
    runner
        .run(&tree("real-tmp-1", "tmp-write"))
        .expect("a result");
    let listing = runner
        .run(&tree("real-tmp-2", "tmp-list"))
        .expect("a result");
    assert_eq!(
        listing.stdout.trim(),
        "",
        "the next run saw {}",
        listing.stdout
    );
    runner.run(&tree("real-mess-1", "mess")).expect("a result");
    let after = runner
        .run(&tree("real-mess-2", "plain"))
        .expect("the mess did not wedge it");
    assert!(after.passed);
    assert!(
        PathBuf::from("/var/tessel/keys").exists(),
        "the link's target was left alone"
    );
}

#[test]
#[ignore = "needs the accounts of scripts/ci/own-users-linux.sh; run as _tessel"]
fn two_runs_from_two_threads_do_not_overlap() {
    let runner = Arc::new(runner());
    let started = Instant::now();
    let threads: Vec<_> = ["real-slow-1", "real-slow-2"]
        .into_iter()
        .map(|name| {
            let (runner, request) = (Arc::clone(&runner), tree(name, "slow"));
            std::thread::spawn(move || runner.run(&request))
        })
        .collect();
    for thread in threads {
        assert!(thread.join().expect("thread").is_ok());
    }
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "two one-second runs took {:?}",
        started.elapsed()
    );
}

#[test]
#[ignore = "needs the accounts of scripts/ci/own-users-linux.sh; run as _tessel"]
fn a_call_given_up_mid_run_kills_a_child_that_left_the_process_group_and_frees_the_gate() {
    let runner = runner();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let timer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        flag.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let error = runner
        .run_cancellable(&tree("real-sleep", "sleep"), &|| {
            stop.load(Ordering::SeqCst)
        })
        .unwrap_err();
    timer.join().expect("timer");
    assert!(matches!(error, RunnerError::Unsupervised { .. }), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    assert!(account_has_no_process(), "the account still has a process");
    assert!(
        runner.run(&tree("real-after", "plain")).is_ok(),
        "the gate is free again"
    );
}

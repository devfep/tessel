//! The `tessel-swarm` binary: the target guard, what a run writes, and that it cleans up and
//! keeps credentials out of its output.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::path::Path;
use std::process::{Command, Output};

fn swarm(args: &[&str], tmp: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tessel-swarm"))
        .args(args)
        .env("TMPDIR", tmp)
        .env_remove("STEWARD_ADMIN_TOKEN")
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn protected_and_foreign_repositories_are_refused_before_anything_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    for repo in ["tessel-dogfood", "demo", "my-repo", "swarm-"] {
        for target in ["local", "live"] {
            let args = [
                "run",
                "--repo",
                repo,
                "--target",
                target,
                "--out",
                out.to_str().unwrap(),
            ];
            let output = swarm(&args, tmp.path());
            assert!(!output.status.success(), "{repo} / {target}");
            let shown = text(&output);
            assert!(
                shown.contains("protected")
                    || shown.contains("scratch")
                    || shown.contains("suffix"),
                "{shown}"
            );
        }
    }
    assert!(!out.exists(), "a refused run writes nothing");
}

#[test]
fn a_live_run_without_its_endpoints_says_what_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let output = swarm(
        &[
            "run", "--repo", "swarm-x", "--target", "live", "--mode", "on",
        ],
        tmp.path(),
    );
    assert!(!output.status.success());
    assert!(text(&output).contains("--coordinator"), "{}", text(&output));
}

#[test]
fn tasks_are_the_same_for_the_same_seed() {
    let tmp = tempfile::tempdir().unwrap();
    let first = swarm(&["tasks", "--seed", "3", "--tasks", "5"], tmp.path());
    let second = swarm(&["tasks", "--seed", "3", "--tasks", "5"], tmp.path());
    let other = swarm(&["tasks", "--seed", "4", "--tasks", "5"], tmp.path());
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    assert_ne!(first.stdout, other.stdout);
    let tasks: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(tasks.as_array().map(Vec::len), Some(5));
}

#[test]
fn a_local_run_writes_its_evidence_and_leaves_nothing_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let scratch = tmp.path().join("tmp");
    std::fs::create_dir(&scratch).unwrap();
    let out = tmp.path().join("results");
    let args = [
        "run",
        "--seed",
        "7",
        "--tasks",
        "3",
        "--agents",
        "2",
        "--work-ms",
        "0",
        "--out",
        out.to_str().unwrap(),
    ];
    let output = swarm(&args, &scratch);
    let shown = text(&output);
    assert!(output.status.success(), "{shown}");
    for secret in ["local-a01", "local-a02", "local-swarm-reviewer", "Bearer"] {
        assert!(!shown.contains(secret), "{secret} leaked into the output");
    }
    for name in [
        "seed7-off.json",
        "seed7-on.json",
        "seed7-on-events.json",
        "seed7-ab.md",
    ] {
        assert!(out.join(name).is_file(), "{name} was not written");
    }
    let on: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("seed7-on.json")).unwrap()).unwrap();
    assert_eq!(on["mode"], "on");
    assert_eq!(on["target"], "local");
    assert_eq!(on["summary_from_event_log"]["merges"], 3);
    let events = std::fs::read_to_string(out.join("seed7-on-events.json")).unwrap();
    assert!(!events.contains("local-a01"));
    assert_eq!(
        std::fs::read_dir(&scratch).unwrap().count(),
        0,
        "temp directories were removed"
    );
}

#[test]
fn a_live_run_refuses_the_production_coordinator_and_any_other_host() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    for host in [
        "wss://tessel-coordinator.devfep.workers.dev",
        "wss://example.com",
    ] {
        let args = [
            "run",
            "--repo",
            "swarm-x",
            "--target",
            "live",
            "--coordinator",
            host,
            "--steward",
            "https://s.invalid",
            "--out",
            out.to_str().unwrap(),
        ];
        let output = swarm(&args, tmp.path());
        assert!(!output.status.success(), "{host}");
        let shown = text(&output);
        assert!(
            shown.contains("production coordinator") || shown.contains("not the swarm"),
            "{shown}"
        );
    }
    assert!(!out.exists(), "a refused live run writes nothing");
}

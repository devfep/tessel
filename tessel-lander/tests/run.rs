//! The runner: exact environment, timeout, exit codes and bounded output.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::{Duration, Instant};

use tessel_lander::git::GitCommand;
use tessel_lander::run::{run, RunError, MAX_OUTPUT_BYTES};

fn command(argv: &[&str], env: &[(&str, &str)], timeout: Duration) -> GitCommand {
    GitCommand {
        argv: argv.iter().map(OsString::from).collect(),
        env: env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect::<BTreeMap<_, _>>(),
        timeout,
    }
}

fn sh(script: &str) -> GitCommand {
    let path = std::env::var("PATH").expect("PATH");
    command(
        &["/bin/sh", "-c", script],
        &[("PATH", &path)],
        Duration::from_secs(30),
    )
}

#[test]
fn the_process_sees_exactly_the_command_environment() {
    let output = run(&command(
        &["/usr/bin/env"],
        &[("ONLY", "this")],
        Duration::from_secs(10),
    ))
    .expect("env runs");
    assert!(output.success());
    assert_eq!(output.stdout, "ONLY=this\n");
}

#[test]
fn reports_the_exit_code_and_both_streams() {
    let output = run(&sh("printf out; printf err >&2; exit 3")).expect("sh runs");
    assert_eq!(output.exit_code, Some(3));
    assert!(!output.success());
    assert_eq!(output.stdout, "out");
    assert_eq!(output.stderr, "err");
}

#[test]
fn a_signal_has_no_exit_code_and_is_not_success() {
    let output = run(&sh("kill -9 $$")).expect("sh runs");
    assert_eq!(output.exit_code, None);
    assert!(!output.success());
}

#[test]
fn kills_a_command_that_outlives_its_timeout() {
    let started = Instant::now();
    let error = run(&command(
        &["/bin/sleep", "20"],
        &[],
        Duration::from_millis(200),
    ))
    .expect_err("times out");
    assert!(
        matches!(error, RunError::TimedOut { seconds: 0, .. }),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_command_that_finishes_within_its_timeout_is_not_killed() {
    let output = run(&command(
        &["/bin/sleep", "0.3"],
        &[],
        Duration::from_secs(20),
    ))
    .expect("done");
    assert!(output.success());
}

#[test]
fn output_of_exactly_the_limit_is_kept() {
    let output = run(&sh(&format!("head -c {MAX_OUTPUT_BYTES} /dev/zero"))).expect("at the limit");
    assert_eq!(output.stdout.len(), MAX_OUTPUT_BYTES);
}

#[test]
fn output_past_the_limit_is_an_error_not_a_truncated_result() {
    let error = run(&sh(&format!("head -c {} /dev/zero", MAX_OUTPUT_BYTES + 1)))
        .expect_err("over the limit");
    assert!(
        matches!(
            error,
            RunError::OutputTooLarge {
                stream: "stdout",
                limit: MAX_OUTPUT_BYTES,
                ..
            }
        ),
        "{error}"
    );
    let error = run(&sh(&format!(
        "head -c {} /dev/zero >&2",
        MAX_OUTPUT_BYTES + 1
    )))
    .expect_err("over the limit");
    assert!(
        matches!(
            error,
            RunError::OutputTooLarge {
                stream: "stderr",
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn bytes_that_are_not_utf8_are_replaced_not_dropped() {
    let output = run(&sh("printf 'a\\377b'")).expect("sh runs");
    assert_eq!(output.stdout, "a\u{fffd}b");
}

#[test]
fn a_program_that_does_not_exist_is_a_spawn_error_naming_the_command() {
    let error = run(&command(
        &["/nonexistent/git", "status"],
        &[],
        Duration::from_secs(5),
    ))
    .expect_err("no such program");
    assert!(matches!(error, RunError::Spawn { .. }));
    assert!(
        error.to_string().contains("/nonexistent/git status"),
        "{error}"
    );
}

#[test]
fn an_empty_argv_is_a_spawn_error() {
    let error = run(&command(&[], &[], Duration::from_secs(5))).expect_err("nothing to run");
    assert!(matches!(error, RunError::Spawn { .. }));
}

//! The runner: exact environment, timeout, exit codes and bounded output.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tessel_lander::git::GitCommand;
use tessel_lander::run::{run, RunError};

/// 16 MiB, written out so a change to the limit is a change to this test.
const LIMIT: usize = 16_777_216;

/// How long the processes that a timeout must cut off would otherwise run, and the timeout tests'
/// only wall-clock bound: a run that returns in under half of it did not wait for them. The bound
/// separates "returned at the deadline" from "waited for the process to end on its own", a gap of
/// tens of seconds, so no scheduler delay on a loaded machine closes it. A passing run takes about
/// its timeout, which is well under a second.
const OUTLASTS: Duration = Duration::from_secs(60);

/// The timeout of the tests that read a pid the command wrote: the shell must be scheduled and
/// write it before the deadline, or the run kills it first and the test panics with "pid written".
/// A passing test takes this long, since its command never ends on its own; the value is seconds
/// past the milliseconds a shell needs, and only a machine that cannot start a shell for this long
/// fails it.
const STARTS_WITHIN: Duration = Duration::from_secs(5);

fn returned_without_waiting(elapsed: Duration) -> bool {
    elapsed < OUTLASTS / 2
}

fn command(argv: &[&str], env: &[(&str, &str)], timeout: Duration) -> GitCommand {
    GitCommand {
        dir: PathBuf::from("/"),
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
fn the_process_starts_in_the_command_directory() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut command = sh("pwd -P");
    command.dir = dir.path().canonicalize().expect("canonical path");
    let output = run(&command).expect("sh runs");
    assert!(output.success(), "{}", output.stderr);
    assert_eq!(output.stdout.trim_end(), command.dir.display().to_string());
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
        &["/bin/sleep", &OUTLASTS.as_secs().to_string()],
        &[],
        Duration::from_millis(200),
    ))
    .expect_err("times out");
    assert!(
        matches!(error, RunError::TimedOut { seconds: 0, .. }),
        "{error}"
    );
    assert!(returned_without_waiting(started.elapsed()));
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
    let output = run(&sh(&format!("head -c {LIMIT} /dev/zero"))).expect("at the limit");
    assert_eq!(output.stdout.len(), LIMIT);
}

#[test]
fn output_past_the_limit_is_an_error_not_a_truncated_result() {
    let error = run(&sh(&format!("head -c {} /dev/zero", LIMIT + 1))).expect_err("over the limit");
    assert!(
        matches!(
            error,
            RunError::OutputTooLarge {
                stream: "stdout",
                limit: LIMIT,
                ..
            }
        ),
        "{error}"
    );
    let error =
        run(&sh(&format!("head -c {} /dev/zero >&2", LIMIT + 1))).expect_err("over the limit");
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

#[test]
fn a_grandchild_holding_the_pipe_cannot_outlast_the_timeout() {
    let mut probe = sh(&format!("sleep {} & exit 0", OUTLASTS.as_secs()));
    probe.timeout = Duration::from_secs(1);
    let started = Instant::now();
    let error = run(&probe).expect_err("the pipe stays open");
    assert!(matches!(error, RunError::TimedOut { .. }), "{error}");
    assert!(
        returned_without_waiting(started.elapsed()),
        "{:?}",
        started.elapsed()
    );
}

/// Kills the process whose pid is in the file when dropped during a panic, so a failing test
/// leaves no `sleep`.
struct KillOnDrop(std::path::PathBuf);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // Only a failing test can leave the process running; after a pass its pid may be reused.
        if !std::thread::panicking() {
            return;
        }
        if let Ok(pid) = std::fs::read_to_string(&self.0) {
            std::process::Command::new("kill")
                .args(["-9", pid.trim()])
                .output()
                .ok();
        }
    }
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .output()
        .expect("kill runs")
        .status
        .success()
}

/// How long `assert_gone` polls before it calls a process stuck. The wait ends the moment the
/// process is gone, so a passing test never pays it: it only decides how long a failing test
/// takes to say so. It is long because the thing waited for is the kernel and the runner's reaper
/// thread getting scheduled, and a loaded machine can delay both by seconds. It stays under
/// `OUTLASTS`, so a process that was never killed cannot pass by ending on its own.
const GONE_GUARD: Duration = Duration::from_secs(30);

/// Waits until the process is gone (a zombie still answers `kill -0`), or `GONE_GUARD` passes.
fn assert_gone(pid_file: &std::path::Path) {
    let pid = std::fs::read_to_string(pid_file).expect("pid written");
    let deadline = Instant::now() + GONE_GUARD;
    while alive(pid.trim()) {
        assert!(
            Instant::now() < deadline,
            "process {} still exists",
            pid.trim()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_timeout_kills_the_whole_process_group_not_only_the_command() {
    let scratch = tempfile::TempDir::new().expect("tempdir");
    let pid_file = scratch.path().join("grandchild.pid");
    let _cleanup = KillOnDrop(pid_file.clone());
    let mut probe = sh(&format!(
        "sleep {0} & echo $! > '{1}'; sleep {0}",
        OUTLASTS.as_secs(),
        pid_file.display()
    ));
    probe.timeout = STARTS_WITHIN;
    run(&probe).expect_err("times out");
    assert_gone(&pid_file);
}

#[test]
fn a_background_process_is_killed_when_the_command_finishes_normally() {
    let scratch = tempfile::TempDir::new().expect("tempdir");
    let pid_file = scratch.path().join("grandchild.pid");
    let _cleanup = KillOnDrop(pid_file.clone());
    let script = format!(
        "sleep {} >/dev/null 2>&1 & echo $! > '{}'",
        OUTLASTS.as_secs(),
        pid_file.display()
    );
    let output = run(&sh(&script)).expect("finishes");
    assert!(output.success());
    assert_gone(&pid_file);
}

#[test]
fn a_timed_out_command_is_reaped_not_left_a_zombie() {
    let scratch = tempfile::TempDir::new().expect("tempdir");
    let pid_file = scratch.path().join("command.pid");
    let _cleanup = KillOnDrop(pid_file.clone());
    let mut probe = sh(&format!(
        "echo $$ > '{}'; exec sleep {}",
        pid_file.display(),
        OUTLASTS.as_secs()
    ));
    probe.timeout = STARTS_WITHIN;
    let started = Instant::now();
    let error = run(&probe).expect_err("times out");
    assert!(matches!(error, RunError::TimedOut { .. }), "{error}");
    assert!(
        returned_without_waiting(started.elapsed()),
        "{:?}",
        started.elapsed()
    );
    assert_gone(&pid_file);
}

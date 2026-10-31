//! Runs with a stand-in `cargo` first on the command's `PATH`.

use std::process::Command;
use std::time::{Duration, Instant};

use tessel_lander::outcome::{is_timed_out, PackageStep};
use tessel_runner::{run, RunResult, RunnerError, MAX_FIRST_FAILURE_BYTES, MAX_TAIL_BYTES};

use crate::common::{request, Rig};

fn no_process_matches(pattern: &str) -> bool {
    for _ in 0..30 {
        let found = Command::new("pgrep")
            .args(["-f", pattern])
            .output()
            .expect("pgrep runs");
        if found.status.code() == Some(1) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn a_passing_run_reports_exit_zero_and_passed() {
    let rig = Rig::with_cargo("echo all good; echo a warning >&2");
    let result = run(&rig.config(), &rig.request()).unwrap();
    assert!(result.passed);
    assert!(!result.timed_out);
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "all good\n");
    assert_eq!(result.stderr, "a warning\n");
    assert!(!result.stdout_truncated && !result.stderr_truncated);
    assert_eq!(result.first_failure, None);
}

#[test]
fn a_failing_run_quotes_the_first_failing_tests_block_and_not_the_second() {
    let rig = Rig::with_cargo("cat output.txt; exit 101");
    // Captured once from `cargo test` 1.99.1 on a crate with two failing tests.
    rig.write_in_tree(
        "output.txt",
        include_str!("../fixtures/cargo-1.99.1-test-failures.stdout"),
    );
    let result = run(&rig.config(), &rig.request()).unwrap();
    assert!(!result.passed);
    assert_eq!(result.exit_code, 101);
    let quoted = result.first_failure.unwrap();
    assert!(
        quoted.starts_with("---- first_fails stdout ----\n\nthread 'first_fails'"),
        "{quoted}"
    );
    assert!(quoted.ends_with("right: 3"), "{quoted}");
    assert!(!quoted.contains("second_fails"));
    assert!(
        result.stdout.contains("second boom"),
        "the output itself is whole"
    );
}

#[test]
fn a_long_first_failure_is_cut_at_1500_bytes() {
    let rig = Rig::with_cargo("cat output.txt; exit 101");
    let long_line = "x".repeat(200);
    let block = vec![long_line; 30].join("\n");
    rig.write_in_tree(
        "output.txt",
        &format!("---- a stdout ----\n{block}\n\n---- b stdout ----\n"),
    );
    let result = run(&rig.config(), &rig.request()).unwrap();
    let quoted = result.first_failure.unwrap();
    assert_eq!(quoted.len(), MAX_FIRST_FAILURE_BYTES);
    assert!(quoted.starts_with("---- a stdout ----\nxxx"));
    assert!(!quoted.contains("b stdout"));
}

#[test]
fn a_run_stops_at_the_first_failing_command() {
    let rig = Rig::with_cargo(
        "case \"$*\" in *--lib*) touch second-ran;; *) echo first failed; exit 1;; esac",
    );
    let mut asked = rig.request();
    let mut second = asked.tests[0].clone();
    second.argv = crate::common::argv(&["cargo", "test", "--lib"]);
    asked.tests.push(second);
    let result = run(&rig.config(), &asked).unwrap();
    assert_eq!(result.exit_code, 1);
    assert!(!result.passed);
    assert!(!rig.tree.path().join("second-ran").exists());
}

#[test]
fn every_command_of_a_passing_run_runs_in_its_dir() {
    let rig = Rig::with_cargo("pwd");
    std::fs::create_dir(rig.tree.path().join("sub")).unwrap();
    let mut asked = rig.request();
    asked.tests.push(
        request(rig.tree.path(), "sub", &["cargo", "test"])
            .tests
            .remove(0),
    );
    let result = run(&rig.config(), &asked).unwrap();
    assert!(result.passed);
    let lines: Vec<&str> = result.stdout.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(!lines[0].ends_with("/sub"));
    assert!(lines[1].ends_with("/sub"), "{lines:?}");
}

#[test]
fn a_run_past_its_timeout_is_killed_with_its_whole_process_group() {
    let rig = Rig::with_cargo("sleep 4242 &\nwait");
    let mut asked = rig.request();
    asked.timeout_secs = 1;
    let started = Instant::now();
    let result = run(&rig.config(), &asked).unwrap();
    let elapsed = started.elapsed();
    assert!(result.timed_out);
    assert!(!result.passed);
    assert_eq!(result.exit_code, 137);
    assert!(
        elapsed < Duration::from_secs(30),
        "replied after {elapsed:?}"
    );
    assert!(no_process_matches("sleep 4242"), "a sleep outlived the run");
}

#[test]
fn a_process_a_passing_run_leaves_behind_is_killed() {
    let rig = Rig::with_cargo("sleep 4343 &\necho done");
    let started = Instant::now();
    let result = run(&rig.config(), &rig.request()).unwrap();
    assert!(result.passed);
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(no_process_matches("sleep 4343"), "a sleep outlived the run");
}

#[test]
fn output_tails_keep_the_last_256_kib_and_say_they_were_cut() {
    let rig = Rig::with_cargo(
        "head -c 600000 /dev/zero | tr '\\0' a\necho END\n\
         head -c 300000 /dev/zero | tr '\\0' b >&2\necho ERR >&2",
    );
    let result = run(&rig.config(), &rig.request()).unwrap();
    assert!(result.passed);
    assert_eq!(result.stdout.len(), MAX_TAIL_BYTES);
    assert!(result.stdout_truncated);
    assert!(result.stdout.ends_with("aaaEND\n"));
    assert_eq!(result.stderr.len(), MAX_TAIL_BYTES);
    assert!(result.stderr_truncated);
    assert!(result.stderr.ends_with("bbbERR\n"));
}

#[test]
fn the_command_sees_only_the_configured_environment() {
    let rig = Rig::with_cargo("env");
    let result = run(&rig.config(), &rig.request()).unwrap();
    let mut names = Vec::new();
    for line in result.stdout.lines() {
        let (name, value) = line.split_once('=').unwrap();
        names.push(name.to_owned());
        if name == "HOME" {
            assert_eq!(value, "/home-for-test");
        }
        if name == "PATH" {
            assert_eq!(value, rig.path_value());
        }
    }
    for name in &names {
        assert!(
            ["PATH", "HOME", "PWD", "SHLVL", "_", "OLDPWD"].contains(&name.as_str()),
            "unexpected variable {name}"
        );
    }
    assert!(names.contains(&"PATH".to_owned()) && names.contains(&"HOME".to_owned()));
}

#[test]
fn the_command_reads_a_closed_stdin() {
    let rig = Rig::with_cargo("cat\necho stdin-ended");
    let mut asked = rig.request();
    asked.timeout_secs = 60;
    let result = run(&rig.config(), &asked).unwrap();
    assert!(!result.timed_out, "cat waited on an open stdin: {result:?}");
    assert!(result.passed, "{result:?}");
    assert_eq!(result.stdout, "stdin-ended\n");
}

#[test]
fn the_requests_timeout_cannot_exceed_the_configured_one() {
    let rig = Rig::with_cargo("sleep 4444 &\nwait");
    let config = rig.config_with_timeout(Some(1));
    let mut asked = rig.request();
    asked.timeout_secs = 3600;
    let started = Instant::now();
    let result = run(&config, &asked).unwrap();
    assert!(result.timed_out);
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(no_process_matches("sleep 4444"));
}

#[test]
fn a_command_that_cannot_start_is_a_supervision_error_that_quotes_nothing() {
    let rig = Rig::with_cargo("true");
    let config = rig
        .config()
        .with_env(vec![("PATH".to_owned(), String::new())]);
    let error = run(&config, &rig.request()).unwrap_err();
    assert!(matches!(error, RunnerError::Unsupervised { .. }), "{error}");
    assert_eq!(error.exit_code(), 3);
    assert!(!error
        .to_string()
        .contains(&rig.tree.path().display().to_string()));
}

#[test]
fn a_refused_request_runs_nothing() {
    let rig = Rig::with_cargo("touch ran");
    let mut asked = rig.request();
    asked.tests[0].argv = crate::common::argv(&["cargo", "test", "--manifest-path", "x"]);
    assert!(run(&rig.config(), &asked).is_err());
    let mut asked = rig.request();
    asked.tests[0].dir = "missing".to_owned();
    assert!(run(&rig.config(), &asked).is_err());
    assert!(!rig.tree.path().join("ran").exists());
}

#[test]
fn a_passing_result_converts_to_a_passed_step_outcome() {
    let rig = Rig::with_cargo("echo fine");
    let outcome = run(&rig.config(), &rig.request())
        .unwrap()
        .into_step_outcome();
    assert_eq!(outcome.step, PackageStep::Test);
    assert!(outcome.passed);
    assert!(!outcome.timed_out);
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.stdout, "fine\n");
    assert_eq!(outcome.reason, None);
    assert!(outcome.wall_ms.is_some());
}

#[test]
fn a_failing_result_converts_to_a_step_outcome_that_did_not_pass() {
    let rig = Rig::with_cargo("exit 3");
    let outcome = run(&rig.config(), &rig.request())
        .unwrap()
        .into_step_outcome();
    assert!(!outcome.passed);
    assert!(!outcome.timed_out);
    assert_eq!(outcome.exit_code, 3);
}

#[test]
fn a_timed_out_result_converts_to_a_timed_out_step_outcome_that_did_not_pass() {
    let result = RunResult {
        step: tessel_runner::ResultStep::Test,
        exit_code: 137,
        passed: true,
        timed_out: true,
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        first_failure: None,
        wall_ms: 7,
    };
    assert!(is_timed_out(result.exit_code));
    let outcome = result.into_step_outcome();
    assert!(outcome.timed_out);
    assert!(!outcome.passed);
    assert_eq!(outcome.wall_ms, Some(7));
}

#[test]
fn a_leader_that_leaves_its_process_group_cannot_hold_the_reply_past_the_timeout() {
    let rig =
        Rig::with_cargo("exec perl -e 'setpgrp(0, getpgrp(getppid())); exec \"sleep\", \"4545\"'");
    let mut asked = rig.request();
    asked.timeout_secs = 1;
    let started = Instant::now();
    let result = run(&rig.config(), &asked).unwrap();
    assert!(result.timed_out);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert!(no_process_matches("sleep 4545"));
}

#[test]
fn one_deadline_covers_all_the_commands() {
    let rig = Rig::with_cargo("sleep 0.7");
    let mut asked = rig.request();
    asked.tests.push(asked.tests[0].clone());
    asked.timeout_secs = 1;
    let started = Instant::now();
    let result = run(&rig.config(), &asked).unwrap();
    assert!(
        result.timed_out,
        "two 0.7 s commands fit a 1 s timeout one by one"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn a_command_that_kills_itself_is_exit_137_without_a_timeout() {
    let rig = Rig::with_cargo("kill -9 $$");
    let result = run(&rig.config(), &rig.request()).unwrap();
    assert_eq!(result.exit_code, 137);
    assert!(!result.timed_out);
    assert!(!result.passed);
}

#[test]
fn a_tail_cut_inside_a_character_stays_within_the_limit_and_has_no_broken_character() {
    let rig = Rig::with_cargo("perl -e 'print \"\\xc3\\xa9\" x 200000; print \"a\"'");
    let result = run(&rig.config(), &rig.request()).unwrap();
    assert!(result.stdout_truncated);
    assert!(
        result.stdout.len() <= MAX_TAIL_BYTES,
        "{}",
        result.stdout.len()
    );
    assert!(!result.stdout.contains('\u{FFFD}'));
    assert!(result.stdout.ends_with("éa"));
}

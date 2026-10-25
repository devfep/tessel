//! The runner run through `sudo -u <account>`, with stand-ins for sudo, the runner, `kill` and
//! `ps`: the logic of the gate, the kill, the poll and the deadline, without root or the accounts.
//! What needs the real accounts is the Linux CI job's (docs/proposals/own-users.md, section 8).

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tessel_runner::{RequestTest, ResultStep, RunRequest, RunResult, RunnerError};
use tessel_server::landing::{SudoRunner, SudoRunnerSettings, TestRunner};

fn result(exit_code: i32, first_failure: Option<&str>) -> RunResult {
    RunResult {
        step: ResultStep::Test,
        exit_code,
        passed: exit_code == 0,
        timed_out: false,
        stdout: "out".to_owned(),
        stderr: "err".to_owned(),
        stdout_truncated: false,
        stderr_truncated: false,
        first_failure: first_failure.map(str::to_owned),
        wall_ms: 1,
    }
}

/// A directory of stand-in programs and the state they share. The stand-ins have the state
/// directory written into them: `SudoRunner` clears the environment.
struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Rig {
        let rig = Rig {
            dir: tempfile::tempdir().expect("rig dir"),
        };
        let state = rig.state();
        fs::create_dir(&state).expect("state dir");
        fs::write(state.join("procs"), "").expect("procs");
        fs::write(state.join("mode"), "ok").expect("mode");
        let result = serde_json::to_string(&result(0, None)).expect("serialize the result");
        fs::write(state.join("result.json"), result).expect("result");
        rig.script(
            "sudo",
            r#"if [ -f STATE/refuse ]; then echo "sudo: a password is required" >&2; exit 1; fi
[ "$1" = -n ] && [ "$2" = -u ] && [ "$4" = -- ] || { echo "sudo: unexpected arguments: $*" >&2; exit 1; }
echo "$3 $5 $6 $7" >> STATE/sudo.log
shift 4
exec "$@""#,
        );
        rig.script(
            "tessel-runner",
            r#"cat > STATE/request.json
echo "$$" >> STATE/procs
echo "start $(date +%s%N)" >> STATE/timeline
case "$(cat STATE/mode)" in
ok) cat STATE/result.json ;;
slow) sleep 0.4; cat STATE/result.json ;;
sleep) sleep 30 >/dev/null 2>&1 & echo "$!" >> STATE/procs; wait ;;
leaves-a-child) sleep 30 >/dev/null 2>&1 & echo "$!" >> STATE/procs; cat STATE/result.json ;;
refuse) exit 2 ;;
unsupervised) exit 3 ;;
flood) head -c 6000000 /dev/zero | tr '\0' x ;;
esac
echo "end $(date +%s%N)" >> STATE/timeline"#,
        );
        rig.script(
            "kill",
            r#"echo "$*" >> STATE/kill.log
[ -f STATE/stubborn ] && exit 0
while read -r pid; do kill -9 "$pid" 2>/dev/null; done < STATE/procs
: > STATE/procs"#,
        );
        rig.script(
            "ps",
            r#"while read -r pid; do kill -0 "$pid" 2>/dev/null && echo "$pid"; done < STATE/procs
exit 0"#,
        );
        rig
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn script(&self, name: &str, body: &str) {
        let body = body.replace("STATE", self.state().to_str().expect("utf-8"));
        let path = self.dir.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write a stand-in");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    fn set_mode(&self, mode: &str) {
        fs::write(self.state().join("mode"), mode).expect("mode");
    }

    fn touch(&self, name: &str) {
        fs::write(self.state().join(name), "").expect("touch");
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.state().join(name)).unwrap_or_default()
    }

    fn settings(&self) -> SudoRunnerSettings {
        SudoRunnerSettings {
            user: "_tessel-runner".to_owned(),
            sudo: self.dir.path().join("sudo"),
            runner: self.dir.path().join("tessel-runner"),
            runner_config: PathBuf::from("/var/tessel-runner/tessel-runner.toml"),
            kill: self.dir.path().join("kill"),
            ps: self.dir.path().join("ps"),
            settle: Duration::from_secs(3),
        }
    }

    fn runner(&self) -> SudoRunner {
        SudoRunner::new(self.settings()).expect("a runner")
    }
}

fn request() -> RunRequest {
    RunRequest {
        tree: "/var/tessel/handoff/acme-demo-call-1".to_owned(),
        tests: vec![RequestTest {
            dir: String::new(),
            argv: ["cargo", "test", "--workspace"].map(str::to_owned).to_vec(),
        }],
        timeout_secs: 60,
    }
}

fn never() -> bool {
    false
}

/// A process that is nobody's child of this test, so that init reaps it when it is killed.
fn stray_process() -> String {
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
        .output()
        .expect("start a stray process");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn a_run_goes_through_sudo_as_the_account_with_the_request_on_stdin_and_the_result_on_stdout() {
    let rig = Rig::new();
    let runner = rig.runner();
    let got = runner
        .run_cancellable(&request(), &never)
        .expect("a result");
    assert_eq!(got, result(0, None));
    assert_eq!(
        rig.read("request.json"),
        serde_json::to_string(&request()).expect("serialize")
    );
    let log = rig.read("sudo.log");
    let line = log
        .lines()
        .find(|line| line.contains("tessel-runner --config"))
        .expect("sudo started the runner");
    assert!(
        line.starts_with("_tessel-runner ")
            && line.ends_with("--config /var/tessel-runner/tessel-runner.toml"),
        "{log}"
    );
}

#[test]
fn everything_the_account_owns_is_killed_after_every_run_and_at_start() {
    let rig = Rig::new();
    rig.set_mode("leaves-a-child");
    let runner = rig.runner();
    assert_eq!(rig.read("kill.log").lines().count(), 1, "one kill at start");
    runner
        .run_cancellable(&request(), &never)
        .expect("a result");
    assert!(
        rig.read("kill.log").lines().count() >= 2,
        "{}",
        rig.read("kill.log")
    );
    assert!(
        rig.read("kill.log").lines().all(|line| line == "-KILL -1"),
        "the kill is exactly kill -KILL -1: {}",
        rig.read("kill.log")
    );
    for pid in rig.read("procs").lines() {
        assert!(!alive(pid), "{pid} survived");
    }
}

#[test]
fn a_process_left_by_an_earlier_server_is_killed_at_start() {
    let rig = Rig::new();
    let pid = stray_process();
    fs::write(rig.state().join("procs"), format!("{pid}\n")).expect("record it");
    let _runner = rig.runner();
    assert!(!alive(&pid), "the stray process was killed");
}

#[test]
fn a_survivor_closes_the_gate_and_no_later_run_starts() {
    let rig = Rig::new();
    let runner = rig.runner();
    rig.set_mode("leaves-a-child");
    rig.touch("stubborn");
    let first = runner.run_cancellable(&request(), &never);
    assert!(
        first.is_ok(),
        "the result of the run that left the survivor stands"
    );
    let started = rig.read("timeline").matches("start").count();
    let second = runner.run_cancellable(&request(), &never).unwrap_err();
    assert!(
        matches!(second, RunnerError::Unsupervised { .. }),
        "{second}"
    );
    assert_eq!(
        rig.read("timeline").matches("start").count(),
        started,
        "the runner did not start"
    );
    for pid in rig.read("procs").lines() {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-9", pid])
            .status();
    }
}

#[test]
fn a_survivor_at_start_stops_the_server_from_starting() {
    let rig = Rig::new();
    let pid = stray_process();
    fs::write(rig.state().join("procs"), format!("{pid}\n")).expect("record it");
    rig.touch("stubborn");
    assert!(SudoRunner::new(rig.settings()).is_err());
    let _ = std::process::Command::new("/bin/kill")
        .args(["-9", &pid])
        .status();
}

#[test]
fn a_sudo_that_refuses_stops_the_server_from_starting() {
    let rig = Rig::new();
    rig.touch("refuse");
    let error = SudoRunner::new(rig.settings()).err().expect("an error");
    assert!(error.to_string().contains("sudo"), "{error}");
}

#[test]
fn a_call_given_up_mid_run_kills_the_account_itself_and_frees_the_gate() {
    let rig = Rig::new();
    let runner = Arc::new(rig.runner());
    rig.set_mode("sleep");
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let timer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let error = runner
        .run_cancellable(&request(), &|| stop.load(Ordering::SeqCst))
        .unwrap_err();
    timer.join().expect("timer");
    assert!(matches!(error, RunnerError::Unsupervised { .. }), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "no deadlock: {:?}",
        started.elapsed()
    );
    for pid in rig.read("procs").lines() {
        assert!(!alive(pid), "{pid} survived the kill");
    }
    rig.set_mode("ok");
    assert!(
        runner.run_cancellable(&request(), &never).is_ok(),
        "the gate is free again"
    );
}

#[test]
fn two_runs_never_overlap_even_from_two_threads() {
    let rig = Rig::new();
    rig.set_mode("slow");
    let runner = Arc::new(rig.runner());
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let runner = Arc::clone(&runner);
            std::thread::spawn(move || runner.run_cancellable(&request(), &never))
        })
        .collect();
    for thread in threads {
        assert!(thread.join().expect("thread").is_ok());
    }
    let events: Vec<String> = rig
        .read("timeline")
        .lines()
        .map(|line| line.split(' ').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        events,
        ["start", "end", "start", "end"],
        "{}",
        rig.read("timeline")
    );
}

#[test]
fn a_runner_that_refuses_or_cannot_supervise_is_an_error_and_a_flood_is_cut() {
    for mode in ["refuse", "unsupervised", "flood"] {
        let rig = Rig::new();
        let runner = rig.runner();
        rig.set_mode(mode);
        let error = runner.run_cancellable(&request(), &never).unwrap_err();
        assert!(
            matches!(error, RunnerError::Unsupervised { .. }),
            "{mode}: {error}"
        );
    }
}

#[test]
fn run_without_a_cancel_is_the_same_run() {
    let rig = Rig::new();
    let runner = rig.runner();
    assert_eq!(runner.run(&request()).expect("a result"), result(0, None));
    let _: &Path = rig.dir.path();
}

fn lander_config(rig: &Rig) -> tessel_server::config::LanderConfig {
    let settings = rig.settings();
    tessel_server::config::LanderConfig {
        work_dir: rig.dir.path().join("work"),
        runner_config: settings.runner_config.clone(),
        token_file: None,
        sudo_runner: Some(tessel_server::config::SudoRunnerConfig {
            user: settings.user,
            sudo: settings.sudo,
            runner: settings.runner,
            handoff_dir: rig.dir.path().join("handoff"),
            kill: settings.kill,
            ps: settings.ps,
            settle_secs: 3,
        }),
    }
}

#[test]
fn the_server_builds_the_sudo_runner_from_the_lander_table_and_kills_at_start() {
    let rig = Rig::new();
    let runner = tessel_server::server::runner_of(&lander_config(&rig)).expect("a runner");
    assert_eq!(
        rig.read("kill.log").lines().count(),
        1,
        "the start-up kill ran"
    );
    assert_eq!(runner.run(&request()).expect("a result"), result(0, None));
}

#[test]
fn a_sudo_that_refuses_is_a_start_error_naming_the_table() {
    let rig = Rig::new();
    rig.touch("refuse");
    let error = tessel_server::server::runner_of(&lander_config(&rig))
        .err()
        .expect("an error");
    assert!(
        matches!(error, tessel_server::server::StartError::Sudo(_)),
        "{error}"
    );
    assert!(
        error.to_string().contains("[lander.sudo_runner]"),
        "{error}"
    );
}

#[test]
fn without_a_sudo_runner_table_the_in_process_runner_is_built_as_before() {
    let rig = Rig::new();
    let mut lander = lander_config(&rig);
    lander.sudo_runner = None;
    lander.runner_config = rig.dir.path().join("missing-runner.toml");
    let error = tessel_server::server::runner_of(&lander)
        .err()
        .expect("an error");
    assert!(
        matches!(error, tessel_server::server::StartError::Runner(_)),
        "{error}"
    );
}

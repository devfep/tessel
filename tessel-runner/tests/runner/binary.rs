//! The real `tessel-runner` binary over standard input and standard output.

use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::{argv, own_config_text, write_config, Rig};

const SECRET_NAMES: [&str; 2] = ["TESSEL_TOKEN", "STEWARD_ADMIN_TOKEN"];

fn spawn_binary(rig: &Rig, config: &Path, path_env: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_tessel-runner"))
        .arg("--config")
        .arg(config)
        .env_clear()
        .env("PATH", path_env)
        .env("HOME", "/home-for-test")
        .env("TESSEL_TOKEN", "token-value-1")
        .env("STEWARD_ADMIN_TOKEN", "admin-value-2")
        .env("UNRELATED", "unrelated-value-3")
        .current_dir(rig.tree.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the runner")
}

fn send_request(child: &mut Child, input: &str) {
    let mut stdin = child.stdin.take().expect("stdin");
    match stdin.write_all(input.as_bytes()) {
        Ok(()) => {}
        // The runner refuses a bad config before it reads stdin, so it may have exited already.
        Err(error) if error.kind() == ErrorKind::BrokenPipe => {}
        Err(error) => panic!("send the request: {error}"),
    }
}

fn run_binary(rig: &Rig, config: &Path, path_env: &str, input: &str) -> Output {
    let mut child = spawn_binary(rig, config, path_env);
    send_request(&mut child, input);
    child.wait_with_output().expect("wait for the runner")
}

fn wait_until_exited(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().expect("poll the runner").is_none() {
        assert!(Instant::now() < deadline, "the runner did not exit");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn own_config(rig: &Rig) -> std::path::PathBuf {
    write_config(rig.bin.path(), &own_config_text(), 0o600)
}

fn request_json(rig: &Rig) -> String {
    serde_json::to_string(&rig.request()).expect("serialize the request")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn the_binary_runs_a_request_from_stdin_and_replies_on_stdout() {
    let rig = Rig::with_cargo("echo hello");
    let config = own_config(&rig);
    let output = run_binary(&rig, &config, &rig.path_value(), &request_json(&rig));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "");
    let stdout = text(&output.stdout);
    assert_eq!(stdout.lines().count(), 1, "one object, one line");
    let result: Value = serde_json::from_str(&stdout).unwrap();
    let mut keys: Vec<&str> = result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "exit_code",
            "first_failure",
            "passed",
            "stderr",
            "stderr_truncated",
            "stdout",
            "stdout_truncated",
            "step",
            "timed_out",
            "wall_ms"
        ]
    );
    assert_eq!(result["step"], "test");
    assert_eq!(result["passed"], true);
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["stdout"], "hello\n");
    assert_eq!(result["first_failure"], Value::Null);
}

#[test]
fn the_binary_exits_0_when_the_tests_fail_and_says_so_in_the_result() {
    let rig = Rig::with_cargo("echo '---- t stdout ----'; echo oops; exit 1");
    let config = own_config(&rig);
    let output = run_binary(&rig, &config, &rig.path_value(), &request_json(&rig));
    assert_eq!(output.status.code(), Some(0));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["passed"], false);
    assert_eq!(result["exit_code"], 1);
    assert_eq!(result["first_failure"], "---- t stdout ----\noops");
}

#[test]
fn the_binary_does_not_pass_its_environment_to_the_command() {
    let rig = Rig::with_cargo("env");
    let config = own_config(&rig);
    let output = run_binary(&rig, &config, &rig.path_value(), &request_json(&rig));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let seen = result["stdout"].as_str().unwrap();
    for name in SECRET_NAMES {
        assert!(!seen.contains(name), "{name} reached the command");
    }
    for value in [
        "token-value-1",
        "admin-value-2",
        "UNRELATED",
        "unrelated-value-3",
    ] {
        assert!(!seen.contains(value), "{value} reached the command");
    }
    assert!(seen.contains("HOME=/home-for-test"));
    assert!(seen.contains("PATH="));
}

#[test]
fn the_binary_exits_2_with_a_fixed_message_when_the_config_is_missing() {
    let rig = Rig::with_cargo("touch ran");
    let missing = rig.bin.path().join("tessel-runner.toml");
    let output = run_binary(&rig, &missing, &rig.path_value(), &request_json(&rig));
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(text(&output.stdout), "");
    let message = text(&output.stderr);
    assert!(message.contains("tessel-runner.toml"), "{message}");
    assert!(message.contains("does not exist"), "{message}");
    assert!(!rig.tree.path().join("ran").exists());
}

#[test]
fn the_binary_exits_2_even_when_it_exited_before_the_request_was_sent() {
    let rig = Rig::with_cargo("touch ran");
    let missing = rig.bin.path().join("tessel-runner.toml");
    let mut child = spawn_binary(&rig, &missing, &rig.path_value());
    wait_until_exited(&mut child);
    send_request(&mut child, &request_json(&rig));
    let output = child.wait_with_output().expect("wait for the runner");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(text(&output.stdout), "");
    assert!(text(&output.stderr).contains("does not exist"));
}

#[test]
fn the_binary_exits_2_when_the_config_names_another_user() {
    let rig = Rig::with_cargo("touch ran");
    let config = write_config(
        rig.bin.path(),
        "[runner]\nuser = \"somebody-else\"\n",
        0o600,
    );
    let output = run_binary(&rig, &config, &rig.path_value(), &request_json(&rig));
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(text(&output.stdout), "");
    assert!(text(&output.stderr).contains("[runner] user"));
    assert!(!rig.tree.path().join("ran").exists());
}

#[test]
fn the_binary_exits_2_for_a_command_outside_the_grammar_and_runs_nothing() {
    let rig = Rig::with_cargo("touch ran");
    let config = own_config(&rig);
    let mut asked = rig.request();
    asked.tests[0].argv = argv(&["cargo", "test", "--manifest-path", "x"]);
    let input = serde_json::to_string(&asked).unwrap();
    let output = run_binary(&rig, &config, &rig.path_value(), &input);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(text(&output.stdout), "");
    let message = text(&output.stderr);
    assert!(message.contains("not allowed"), "{message}");
    assert!(!message.contains(&rig.tree.path().display().to_string()));
    assert!(!rig.tree.path().join("ran").exists());
}

#[test]
fn the_binary_exits_2_for_a_malformed_request() {
    let rig = Rig::with_cargo("touch ran");
    let config = own_config(&rig);
    for input in ["", "not json", "{\"tree\": \"/\"}", "{\"extra\": 1}"] {
        let output = run_binary(&rig, &config, &rig.path_value(), input);
        assert_eq!(output.status.code(), Some(2), "{input}");
        assert_eq!(text(&output.stdout), "");
    }
    assert!(!rig.tree.path().join("ran").exists());
}

#[test]
fn the_binary_exits_3_when_the_command_cannot_start() {
    let rig = Rig::with_cargo("true");
    let config = own_config(&rig);
    let output = run_binary(&rig, &config, "", &request_json(&rig));
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(text(&output.stdout), "");
    assert!(text(&output.stderr).contains("cannot supervise"));
}

#[test]
fn the_binary_exits_2_for_an_argument_it_does_not_know() {
    let output = Command::new(env!("CARGO_BIN_EXE_tessel-runner"))
        .arg("--sandbox")
        .stdin(Stdio::null())
        .output()
        .expect("start the runner");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(text(&output.stdout), "");
    assert!(text(&output.stderr).contains("usage"));
}

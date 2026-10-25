//! The sandboxed runner: trees arrive in a handoff directory, are copied into the runner's own
//! work directory, and run there with an environment taken from the configuration file.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tessel_runner::{run, RequestRefusal, RunnerConfig, RunnerError};

use crate::common::{own_config_text, request, write_config, Rig};

/// A handoff directory with a tree in it, a work directory, a temporary directory and a directory
/// to sweep, around a rig whose stand-in `cargo` is `body`.
struct Sandbox {
    rig: Rig,
    base: tempfile::TempDir,
}

impl Sandbox {
    fn new(cargo: &str) -> Sandbox {
        let sandbox = Sandbox {
            rig: Rig::with_cargo(cargo),
            base: tempfile::tempdir().expect("base dir"),
        };
        for dir in ["handoff/call-1", "work", "tmp", "shared"] {
            fs::create_dir_all(sandbox.base.path().join(dir)).expect("make a directory");
        }
        sandbox
    }

    fn handoff_tree(&self) -> PathBuf {
        self.base.path().join("handoff/call-1")
    }

    fn work(&self) -> PathBuf {
        self.base.path().join("work")
    }

    fn config_text(&self, extra: &str) -> String {
        format!(
            "{}path = \"{}\"\ntmpdir = \"{}\"\nwork_dir = \"{}\"\nhandoff_dir = \"{}\"\n\
             sweep_dirs = [\"{}\"]\n{extra}",
            own_config_text(),
            self.rig.path_value(),
            self.base.path().join("tmp").display(),
            self.work().display(),
            self.base.path().join("handoff").display(),
            self.base.path().join("shared").display(),
        )
    }

    fn config(&self) -> RunnerConfig {
        self.config_with(&self.config_text(""))
    }

    fn config_with(&self, text: &str) -> RunnerConfig {
        let path = write_config(self.base.path(), text, 0o644);
        RunnerConfig::load(&path).expect("a sandbox configuration loads")
    }
}

fn refusal(error: RunnerError) -> RequestRefusal {
    match error {
        RunnerError::Request(refusal) => refusal,
        other @ (RunnerError::Config { .. } | RunnerError::Unsupervised { .. }) => {
            panic!("expected a refusal, got {other}")
        }
    }
}

#[test]
fn the_tests_run_in_a_copy_in_the_work_directory_and_the_handoff_tree_is_left_alone() {
    let sandbox = Sandbox::new("pwd; cat marker.txt; echo changed > marker.txt");
    fs::write(sandbox.handoff_tree().join("marker.txt"), "original\n").expect("write");
    let result = run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .expect("the run completes");
    let work = fs::canonicalize(sandbox.work()).expect("work exists");
    assert!(
        result.stdout.starts_with(work.to_str().expect("utf-8")),
        "{}",
        result.stdout
    );
    assert!(result.stdout.ends_with("original\n"), "{}", result.stdout);
    assert_eq!(
        fs::read_to_string(sandbox.handoff_tree().join("marker.txt")).expect("read"),
        "original\n",
        "the tests wrote to the copy, not to the handoff tree"
    );
}

#[test]
fn links_in_the_tree_are_copied_as_links_and_never_followed() {
    let sandbox = Sandbox::new("true");
    let outside = sandbox.base.path().join("outside.txt");
    fs::write(&outside, "secret").expect("write");
    symlink(&outside, sandbox.handoff_tree().join("link")).expect("link");
    fs::create_dir(sandbox.handoff_tree().join("sub")).expect("dir");
    symlink("../missing", sandbox.handoff_tree().join("sub/dangling")).expect("link");
    let script = sandbox.handoff_tree().join("run.sh");
    fs::write(&script, "#!/bin/sh\n").expect("write");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
    run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .expect("the run completes");
    let copy = sandbox.work().join("tree");
    assert_eq!(
        fs::read_link(copy.join("link")).expect("copied as a link"),
        outside
    );
    assert_eq!(
        fs::read_link(copy.join("sub/dangling")).expect("copied as a link"),
        Path::new("../missing")
    );
    let mode = fs::metadata(copy.join("run.sh"))
        .expect("copied")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "an executable stays executable");
}

#[test]
fn a_tree_outside_the_handoff_and_work_directories_is_refused() {
    let sandbox = Sandbox::new("true");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let refused = run(
        &sandbox.config(),
        &request(elsewhere.path(), "", &["cargo", "test", "--workspace"]),
    )
    .unwrap_err();
    assert_eq!(refused.exit_code(), 2);
    assert_eq!(refusal(refused), RequestRefusal::TreeNotInSandbox);
}

#[test]
fn a_link_inside_the_handoff_directory_to_a_tree_outside_it_is_refused() {
    let sandbox = Sandbox::new("true");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let link = sandbox.base.path().join("handoff/call-2");
    symlink(elsewhere.path(), &link).expect("link");
    let refused = run(
        &sandbox.config(),
        &request(&link, "", &["cargo", "test", "--workspace"]),
    )
    .unwrap_err();
    assert!(
        matches!(
            refusal(refused),
            RequestRefusal::TreeNotDirectory | RequestRefusal::TreeNotInSandbox
        ),
        "a link is not a tree"
    );
}

#[test]
fn the_command_sees_only_the_environment_of_the_file() {
    let sandbox = Sandbox::new("env | sort");
    let result = run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .expect("the run completes");
    let names: Vec<&str> = result
        .stdout
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .filter(|name| !matches!(*name, "PWD" | "SHLVL" | "_" | "OLDPWD"))
        .collect();
    assert_eq!(names, ["HOME", "PATH", "TMPDIR"], "{}", result.stdout);
    assert!(
        result.stdout.contains("HOME=/var/empty\n"),
        "{}",
        result.stdout
    );
}

/// Makes `path` hard to remove: an ACL that denies deleting it, then user-immutable (which blocks
/// any later change of the ACL).
fn make_stubborn(path: &Path) {
    let acl = Command::new("/bin/chmod")
        .args(["+a", "everyone deny delete"])
        .arg(path)
        .status()
        .expect("chmod runs");
    assert!(acl.success());
    let flagged = Command::new("/usr/bin/chflags")
        .arg("uchg")
        .arg(path)
        .status()
        .expect("chflags runs");
    assert!(flagged.success());
}

#[test]
#[cfg(target_os = "macos")]
fn each_run_starts_with_an_empty_work_directory_even_after_flags_acls_and_read_only_dirs() {
    let sandbox = Sandbox::new("ls -A");
    let leftover = sandbox.work().join("old/deep");
    fs::create_dir_all(&leftover).expect("make leftovers");
    let stubborn = leftover.join("stubborn.txt");
    fs::write(&stubborn, "x").expect("write");
    make_stubborn(&stubborn);
    fs::set_permissions(&leftover, fs::Permissions::from_mode(0o500)).expect("read-only dir");
    fs::write(sandbox.base.path().join("tmp/scratch"), "x").expect("write");
    let result = run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .expect("the run completes");
    assert!(
        !sandbox.work().join("old").exists(),
        "the leftovers were removed"
    );
    assert!(
        !sandbox.base.path().join("tmp/scratch").exists(),
        "the temporary directory too"
    );
    assert!(!result.stdout.contains("old"), "{}", result.stdout);
}

#[test]
fn files_the_account_left_in_the_shared_directories_are_removed_and_others_stay() {
    let sandbox = Sandbox::new("true");
    let shared = sandbox.base.path().join("shared");
    fs::write(shared.join("left-behind"), "x").expect("write");
    fs::create_dir_all(shared.join("dir/inner")).expect("dirs");
    fs::write(shared.join("dir/inner/file"), "x").expect("write");
    let untouched = sandbox.base.path().join("untouched.txt");
    fs::write(&untouched, "keep").expect("write");
    run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .expect("the run completes");
    assert!(!shared.join("left-behind").exists());
    assert!(!shared.join("dir").exists());
    assert!(shared.exists(), "the shared directory itself stays");
    assert!(
        untouched.exists(),
        "a file outside the listed directories stays"
    );
}

#[test]
fn the_sweep_never_removes_the_work_or_handoff_directories_it_runs_in() {
    let sandbox = Sandbox::new("true");
    let base = sandbox.base.path();
    let text = sandbox.config_text("").replace(
        &format!("sweep_dirs = [\"{}\"]", base.join("shared").display()),
        &format!("sweep_dirs = [\"{}\"]", base.display()),
    );
    fs::write(base.join("stray.txt"), "x").expect("write");
    run(
        &sandbox.config_with(&text),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .expect("the run completes");
    assert!(
        sandbox.handoff_tree().exists(),
        "the handoff directory survived the sweep"
    );
    assert!(
        sandbox.work().exists(),
        "the work directory survived the sweep"
    );
    assert!(
        !base.join("stray.txt").exists(),
        "a stray file of the account was removed"
    );
}

fn stand_in_rustc(rig: &Rig, version: &str) {
    rig.add_program(
        "rustc",
        &format!("echo 'rustc {version} (abcdef 2026-01-01)'"),
    );
}

#[test]
fn a_tree_whose_channel_differs_from_the_compiler_is_refused_and_nothing_runs() {
    let sandbox = Sandbox::new("echo ran > ran.txt");
    stand_in_rustc(&sandbox.rig, "1.98.0");
    fs::write(
        sandbox.handoff_tree().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.99.1\"\n",
    )
    .expect("write");
    let refused = run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .unwrap_err();
    assert_eq!(refused.exit_code(), 2);
    assert_eq!(refusal(refused), RequestRefusal::ToolchainMismatch);
    assert!(!sandbox.work().join("tree/ran.txt").exists());
}

#[test]
fn a_tree_whose_channel_matches_the_compiler_runs() {
    let sandbox = Sandbox::new("echo ran");
    stand_in_rustc(&sandbox.rig, "1.99.1");
    fs::write(
        sandbox.handoff_tree().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.99.1\"\ncomponents = [\"clippy\"]\n",
    )
    .expect("write");
    let result = run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .expect("the run completes");
    assert_eq!(result.stdout, "ran\n");
}

#[test]
fn a_channel_that_is_not_a_version_is_not_compared() {
    let sandbox = Sandbox::new("echo ran");
    stand_in_rustc(&sandbox.rig, "1.98.0");
    fs::write(
        sandbox.handoff_tree().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"stable\"\n",
    )
    .expect("write");
    let result = run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .expect("the run completes");
    assert_eq!(result.stdout, "ran\n");
}

#[test]
fn a_tree_without_a_toolchain_file_runs_without_the_compiler_check() {
    let sandbox = Sandbox::new("echo ran");
    let result = run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .expect("the run completes");
    assert_eq!(result.stdout, "ran\n");
}

/// The soft and hard limits of the sandbox's processes, as the stand-in `cargo` reads them.
#[test]
fn the_binary_limits_the_processes_of_the_account_before_it_runs_anything() {
    let sandbox = Sandbox::new("echo soft=$(ulimit -Su) hard=$(ulimit -Hu) core=$(ulimit -Sc)");
    // The hard limit a process may set without privilege is the current one, so the test lowers
    // nothing below it and the numbers are portable.
    let current = Command::new("/bin/sh")
        .args(["-c", "ulimit -Hu"])
        .output()
        .expect("sh runs");
    let hard: u64 = String::from_utf8_lossy(&current.stdout)
        .trim()
        .parse()
        .expect("a numeric limit");
    // Fork fails once the account has as many processes as the limit, and the system clamps it to
    // its own per-user maximum, so the test sets a limit that is high and below both.
    let limit = hard.min(8000);
    let text = sandbox.config_text(&format!("max_processes = {limit}\n"));
    let config = write_config(sandbox.base.path(), &text, 0o644);
    let mut child = Command::new(env!("CARGO_BIN_EXE_tessel-runner"))
        .arg("--config")
        .arg(&config)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the runner");
    let request = serde_json::to_string(&request(
        &sandbox.handoff_tree(),
        "",
        &["cargo", "test", "--workspace"],
    ))
    .expect("serialize");
    std::io::Write::write_all(&mut child.stdin.take().expect("stdin"), request.as_bytes())
        .expect("send");
    let output = child.wait_with_output().expect("wait");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    let seen = result["stdout"].as_str().expect("stdout");
    assert_eq!(seen, format!("soft={limit} hard={limit} core=0\n"));
}

/// A chain of `depth` directories below `top`, made by a shell that `cd`s one level at a time, so
/// that the path is longer than the system allows in one call.
fn deep_chain(top: &Path, depth: usize) {
    let name = "d".repeat(100);
    let script = format!(
        "i=0; while [ $i -lt {depth} ]; do mkdir {name} && cd {name} || exit 1; \
         i=$((i+1)); done; echo x > leaf"
    );
    let made = Command::new("/bin/sh")
        .args(["-c", &script])
        .current_dir(top)
        .status()
        .expect("sh runs");
    assert!(made.success());
}

#[test]
fn a_chain_of_directories_longer_than_a_path_may_be_cannot_wedge_later_runs() {
    let sandbox = Sandbox::new("true");
    let base = sandbox.base.path();
    fs::create_dir_all(sandbox.work().join("chain")).expect("work chain");
    deep_chain(&sandbox.work().join("chain"), 60);
    fs::create_dir_all(base.join("shared/chain")).expect("shared chain");
    deep_chain(&base.join("shared/chain"), 60);
    let locked = sandbox.work().join("locked");
    fs::create_dir_all(locked.join("inner")).expect("locked dir");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    for _ in 0..2 {
        let result = run(
            &sandbox.config(),
            &request(
                &sandbox.handoff_tree(),
                "",
                &["cargo", "test", "--workspace"],
            ),
        )
        .expect("a later run is not wedged by the chain");
        assert!(result.passed);
    }
    assert!(!sandbox.work().join("chain").exists());
    assert!(!sandbox.work().join("locked").exists());
    assert!(!base.join("shared/chain").exists());
}

#[test]
fn a_tree_inside_the_work_directory_is_refused_because_the_server_never_sends_one() {
    let sandbox = Sandbox::new("true");
    fs::create_dir_all(sandbox.work().join("tree")).expect("a tree in work");
    let refused = run(
        &sandbox.config(),
        &request(
            &sandbox.work().join("tree"),
            "",
            &["cargo", "test", "--workspace"],
        ),
    )
    .unwrap_err();
    assert_eq!(refusal(refused), RequestRefusal::TreeNotInSandbox);
}

#[test]
fn a_limit_above_the_inherited_hard_limit_is_lowered_to_it_and_the_run_still_starts() {
    let sandbox = Sandbox::new("echo hard=$(ulimit -Hu)");
    let inherited = Command::new("/bin/sh")
        .args(["-c", "ulimit -Hu"])
        .output()
        .expect("sh runs");
    let text = sandbox.config_text("max_processes = 5000000\n");
    let config = write_config(sandbox.base.path(), &text, 0o644);
    let mut child = Command::new(env!("CARGO_BIN_EXE_tessel-runner"))
        .arg("--config")
        .arg(&config)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the runner");
    let asked = serde_json::to_string(&request(
        &sandbox.handoff_tree(),
        "",
        &["cargo", "test", "--workspace"],
    ))
    .expect("serialize");
    std::io::Write::write_all(&mut child.stdin.take().expect("stdin"), asked.as_bytes())
        .expect("send");
    let output = child.wait_with_output().expect("wait");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    let seen = result["stdout"].as_str().expect("stdout").trim().to_owned();
    let hard: u64 = String::from_utf8_lossy(&inherited.stdout)
        .trim()
        .parse()
        .unwrap_or(u64::MAX);
    let got: u64 = seen.trim_start_matches("hard=").parse().expect("a number");
    assert!(got <= hard, "{got} is within the inherited {hard}");
}

#[test]
fn an_entry_the_account_cannot_remove_from_a_shared_directory_does_not_wedge_the_run() {
    let sandbox = Sandbox::new("true");
    let shared = sandbox.base.path().join("shared");
    fs::write(shared.join("stuck"), "x").expect("write");
    // A directory the account cannot delete from, as a 0777 directory of someone else's would be.
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o500)).expect("chmod");
    let outcome = run(
        &sandbox.config(),
        &request(
            &sandbox.handoff_tree(),
            "",
            &["cargo", "test", "--workspace"],
        ),
    );
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o700)).expect("restore");
    let result = outcome.expect("an entry that stays is not a failed run");
    assert!(result.passed);
}

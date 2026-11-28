//! `create_in` says why `git --version` failed: the exit code and stderr of a git that exits
//! nonzero, a git killed by a signal, a git that prints nothing, and a git whose output is huge.
//! This file changes `PATH` for the process, so it holds one test and nothing else runs beside it.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]
#![expect(clippy::panic, reason = "a wrong variant is a failed test")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tempfile::TempDir;
use tessel_lander::git::{CommandError, Git};

fn fake_git(dir: &Path, script: &str) {
    let path = dir.join("git");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write fake git");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn assert_version_failed(error: &CommandError, expected: Option<i32>) {
    let CommandError::VersionFailed { exit_code, .. } = error else {
        panic!("expected VersionFailed, got {error:?}");
    };
    assert_eq!(*exit_code, expected, "{error:?}");
}

fn refusal(scratch: &Path, bin: &Path, script: &str) -> CommandError {
    fake_git(bin, script);
    Git::create_in(&scratch.join("work")).expect_err("the fake git is refused")
}

#[test]
fn a_git_whose_version_check_fails_is_refused_with_why() {
    let scratch = TempDir::new().expect("tempdir");
    let bin = scratch.path().join("bin");
    fs::create_dir(&bin).expect("bin dir");
    std::env::set_var("PATH", &bin);

    let error = refusal(
        scratch.path(),
        &bin,
        "echo 'xcrun: error: no developer directory' >&2\nexit 7",
    );
    assert_version_failed(&error, Some(7));
    let text = error.to_string();
    assert!(text.contains("exit code 7"), "{text}");
    assert!(text.contains("no developer directory"), "{text}");

    let error = refusal(scratch.path(), &bin, "kill -9 $$");
    assert_version_failed(&error, None);
    assert!(error.to_string().contains("killed by signal 9"), "{error}");

    let error = refusal(scratch.path(), &bin, "kill -SEGV $$");
    assert_version_failed(&error, None);
    assert!(error.to_string().contains("killed by signal 11"), "{error}");

    let error = refusal(
        scratch.path(),
        &bin,
        "echo 'dyld: missing library' >&2\nexit 0",
    );
    assert_version_failed(&error, Some(0));
    let text = error.to_string();
    assert!(text.contains("printed no version"), "{text}");
    assert!(text.contains("missing library"), "{text}");

    let error = refusal(scratch.path(), &bin, "echo 'git version 2.50.1'\nexit 9");
    assert_version_failed(&error, Some(9));
    assert!(error.to_string().contains("git version 2.50.1"), "{error}");

    let error = refusal(
        scratch.path(),
        &bin,
        "/usr/bin/head -c 300000 /dev/zero | /usr/bin/tr '\\0' x >&2\nexit 7",
    );
    let text = error.to_string();
    assert!(text.contains("...[cut]"), "stderr is cut");
    assert!(text.len() < 2_000, "message is {} bytes", text.len());

    let error = refusal(
        scratch.path(),
        &bin,
        "/usr/bin/head -c 300000 /dev/zero | /usr/bin/tr '\\0' x",
    );
    assert!(
        matches!(error, CommandError::UnsupportedGit { .. }),
        "{error:?}"
    );
    let text = error.to_string();
    assert!(
        text.contains("...[cut]") && text.len() < 2_000,
        "{} bytes",
        text.len()
    );

    let error = refusal(
        scratch.path(),
        &bin,
        "/usr/bin/head -c 300000 /dev/zero | /usr/bin/tr '\\0' x\nexit 5",
    );
    assert_version_failed(&error, Some(5));
    let text = error.to_string();
    assert!(
        text.contains("...[cut]") && text.len() < 2_000,
        "stdout: {} bytes",
        text.len()
    );
}

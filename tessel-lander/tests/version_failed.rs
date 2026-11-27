//! `create_in` says why `git --version` failed: the exit code and stderr of a git that exits
//! nonzero, and a git killed by a signal. This file changes `PATH` for the process, so it holds
//! one test and nothing else runs beside it.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helper is not a #[test] function"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tempfile::TempDir;
use tessel_lander::git::Git;

fn fake_git(dir: &Path, script: &str) {
    let path = dir.join("git");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write fake git");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn a_git_whose_version_check_fails_is_refused_with_its_exit_code_or_signal_and_stderr() {
    let scratch = TempDir::new().expect("tempdir");
    let bin = scratch.path().join("bin");
    fs::create_dir(&bin).expect("bin dir");
    std::env::set_var("PATH", &bin);

    fake_git(
        &bin,
        "echo 'xcrun: error: no developer directory' >&2\nexit 1",
    );
    let error = Git::create_in(&scratch.path().join("exit")).expect_err("git exits 1");
    let text = error.to_string();
    assert!(text.contains("exit code 1"), "{text}");
    assert!(text.contains("no developer directory"), "{text}");

    fake_git(&bin, "kill -9 $$");
    let error = Git::create_in(&scratch.path().join("signal")).expect_err("git is killed");
    let text = error.to_string();
    assert!(text.contains("killed by a signal"), "{text}");
}

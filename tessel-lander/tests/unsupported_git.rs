//! `create_in` refuses a `git` older than 2.31 or with an unreadable version. This file changes
//! `PATH` for the process, so it holds one test and nothing else runs beside it.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helper is not a #[test] function"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tempfile::TempDir;
use tessel_lander::git::{CommandError, Git};

fn fake_git(dir: &Path, version_line: &str) {
    let path = dir.join("git");
    fs::write(&path, format!("#!/bin/sh\necho '{version_line}'\n")).expect("write fake git");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn an_old_or_unreadable_git_is_refused_by_create_in() {
    let scratch = TempDir::new().expect("tempdir");
    let bin = scratch.path().join("bin");
    fs::create_dir(&bin).expect("bin dir");
    std::env::set_var("PATH", &bin);

    fake_git(&bin, "git version 2.30.2");
    let error = Git::create_in(&scratch.path().join("old")).expect_err("2.30.2 is too old");
    assert!(
        matches!(error, CommandError::UnsupportedGit { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("git version 2.30.2"), "{error}");

    fake_git(&bin, "git version banana");
    let error = Git::create_in(&scratch.path().join("odd")).expect_err("not a version");
    assert!(
        matches!(error, CommandError::UnsupportedGit { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("banana"), "{error}");

    fake_git(&bin, "git version 2.31.0");
    Git::create_in(&scratch.path().join("new")).expect("2.31.0 is enough");
}

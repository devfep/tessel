//! The clone step is the only way to get a [`Workspace`]: a directory git itself created, whose
//! config the source repository did not write. (That none can be built any other way is the
//! `compile_fail` doctests on `Workspace` and on `Git`.)

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

mod support;

use std::fs;

use tempfile::TempDir;
use tessel_lander::git::{Git, GitCommand};
use tessel_lander::run::{GitOutput, GitRunner, RunError, SystemRunner};
use tessel_lander::workspace::{clone_main, CloneError, CloneOutcome};

use support::{cloned, commit_file, failed_clone, fixture_git, sources_for};

struct Exits(Option<i32>);

impl GitRunner for Exits {
    fn run(&self, _command: &GitCommand) -> Result<GitOutput, RunError> {
        Ok(GitOutput {
            exit_code: self.0,
            stdout: String::new(),
            stderr: "fatal: no".to_owned(),
        })
    }
}

fn source_repo(scratch: &TempDir) -> String {
    let source = scratch.path().join("source");
    fs::create_dir(&source).expect("source dir");
    fixture_git(&source, &["init", "--quiet", "--initial-branch=main"]);
    commit_file(&source, "a.txt", "a\n");
    source.display().to_string()
}

#[test]
fn a_clone_into_a_missing_directory_is_a_workspace_at_that_path() {
    let scratch = TempDir::new().expect("tempdir");
    let remote = source_repo(&scratch);
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    let destination = scratch.path().join("workspace");

    let outcome = clone_main(
        &git,
        &SystemRunner,
        &sources_for(&remote, &remote, "main"),
        &destination,
    )
    .expect("clone runs");

    let workspace = cloned(outcome);
    assert_eq!(workspace.path(), destination);
    assert!(destination.join("a.txt").is_file());
}

#[test]
fn a_clone_into_an_existing_empty_directory_is_a_workspace() {
    let scratch = TempDir::new().expect("tempdir");
    let remote = source_repo(&scratch);
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    let destination = scratch.path().join("workspace");
    fs::create_dir(&destination).expect("empty dir");

    let outcome = clone_main(
        &git,
        &SystemRunner,
        &sources_for(&remote, &remote, "main"),
        &destination,
    )
    .expect("clone runs");

    assert!(matches!(outcome, CloneOutcome::Cloned(_)));
}

#[test]
fn a_repository_that_already_holds_its_own_config_is_refused_with_the_real_runner_too() {
    let scratch = TempDir::new().expect("tempdir");
    let remote = source_repo(&scratch);
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    let planted = scratch.path().join("planted");
    fs::create_dir(&planted).expect("planted dir");
    fixture_git(&planted, &["init", "--quiet", "--initial-branch=main"]);
    fixture_git(
        &planted,
        &["config", "merge.evil.driver", "touch ran; exit 1"],
    );

    let error = clone_main(
        &git,
        &SystemRunner,
        &sources_for(&remote, &remote, "main"),
        &planted,
    )
    .expect_err("refused");

    assert!(
        matches!(error, CloneError::DestinationInUse { .. }),
        "{error}"
    );
}

#[test]
fn a_clone_git_did_not_complete_is_not_a_workspace() {
    let git = Git::new("/lander/home".into(), "/lander/hooks".into());
    let sources = sources_for("https://git.example/demo.git", "", "main");
    for exit_code in [Some(128), Some(1), None] {
        let outcome =
            clone_main(&git, &Exits(exit_code), &sources, "/w".as_ref()).expect("runner answers");
        let output = failed_clone(outcome);
        assert_eq!(output.exit_code, exit_code);
        assert_eq!(output.stderr, "fatal: no");
    }
}

#[test]
fn an_existing_repository_is_refused_even_to_a_runner_that_runs_nothing() {
    let scratch = TempDir::new().expect("tempdir");
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    let planted = scratch.path().join("planted");
    fs::create_dir(&planted).expect("planted dir");
    fixture_git(&planted, &["init", "--quiet", "--initial-branch=main"]);
    fixture_git(
        &planted,
        &["config", "merge.evil.driver", "touch ran; exit 1"],
    );

    let error = clone_main(
        &git,
        &support::AlwaysOk,
        &sources_for("https://git.example/demo.git", "", "main"),
        &planted,
    )
    .expect_err("a repository is not a destination");

    assert!(
        matches!(error, CloneError::DestinationInUse { ref path } if *path == planted),
        "{error}"
    );
}

#[test]
fn a_file_a_non_empty_directory_and_a_symlink_are_refused_before_anything_runs() {
    let scratch = TempDir::new().expect("tempdir");
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    let sources = sources_for("https://git.example/demo.git", "", "main");
    let file = scratch.path().join("file");
    fs::write(&file, "x").expect("file");
    let full = scratch.path().join("full");
    fs::create_dir(&full).expect("dir");
    fs::write(full.join("x"), "x").expect("file in dir");
    let empty = scratch.path().join("empty");
    fs::create_dir(&empty).expect("empty dir");
    let link = scratch.path().join("link");
    std::os::unix::fs::symlink(&empty, &link).expect("symlink");
    for destination in [file, full, link] {
        let error =
            clone_main(&git, &support::AlwaysOk, &sources, &destination).expect_err("refused");
        assert!(
            matches!(error, CloneError::DestinationInUse { .. }),
            "{destination:?}: {error}"
        );
    }
}

#[test]
fn an_empty_directory_and_a_missing_path_are_accepted_by_a_runner_that_runs_nothing() {
    let scratch = TempDir::new().expect("tempdir");
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    let sources = sources_for("https://git.example/demo.git", "", "main");
    let empty = scratch.path().join("empty");
    fs::create_dir(&empty).expect("empty dir");
    for destination in [empty, scratch.path().join("missing")] {
        let outcome =
            clone_main(&git, &support::AlwaysOk, &sources, &destination).expect("accepted");
        assert!(matches!(outcome, CloneOutcome::Cloned(_)));
    }
}

#[test]
fn a_destination_that_cannot_be_inspected_is_refused_before_anything_runs() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = TempDir::new().expect("tempdir");
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    let sources = sources_for("https://git.example/demo.git", "", "main");
    let closed = scratch.path().join("closed");
    fs::create_dir(&closed).expect("dir");
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).expect("close dir");

    let result = clone_main(
        &git,
        &support::AlwaysOk,
        &sources,
        &closed.join("workspace"),
    );

    fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).expect("reopen dir");
    let error = result.expect_err("cannot inspect");
    assert!(
        matches!(error, CloneError::DestinationUnreadable { .. }),
        "{error}"
    );
}

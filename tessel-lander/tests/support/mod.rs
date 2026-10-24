//! Fixture repositories built with plain git under a fixed author and committer, whatever the
//! test process holds.

#![expect(
    dead_code,
    reason = "each test crate compiles all of support and uses only some of it"
)]
#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

pub mod repos;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tessel_lander::git::{Git, GitCommand, MergeSources};
use tessel_lander::run::{GitOutput, GitRunner, RunError};
use tessel_lander::types::Sha;
use tessel_lander::workspace::{clone_main, CloneOutcome, Workspace};

/// A runner for which every command succeeds without running anything.
pub struct AlwaysOk;

impl GitRunner for AlwaysOk {
    fn run(&self, _command: &GitCommand) -> Result<GitOutput, RunError> {
        Ok(GitOutput {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

pub fn sources_for(main_remote: &str, fork_remote: &str) -> MergeSources {
    MergeSources {
        main_remote: main_remote.to_owned(),
        fork_remote: fork_remote.to_owned(),
    }
}

/// A workspace at `path` through the clone step with a runner that runs nothing, for tests of
/// the command builders. The directory does not exist.
pub fn workspace_at(path: &str) -> Workspace {
    let git = Git::new("/lander/home".into(), "/lander/hooks".into());
    let sources = sources_for("https://git.example/demo.git", "");
    cloned(clone_main(&git, &AlwaysOk, &sources, Path::new(path)).expect("clone step runs"))
}

pub fn cloned(outcome: CloneOutcome) -> Workspace {
    match outcome {
        CloneOutcome::Cloned(workspace) => workspace,
        CloneOutcome::Failed(output) => unreachable!("the clone failed: {}", output.stderr),
    }
}

pub fn failed_clone(outcome: CloneOutcome) -> GitOutput {
    match outcome {
        CloneOutcome::Failed(output) => output,
        CloneOutcome::Cloned(workspace) => {
            unreachable!("the clone made {}", workspace.path().display())
        }
    }
}

/// Runs git in `repo` with its own cleared environment and returns trimmed stdout.
pub fn fixture_git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.invalid")
        .env("GIT_AUTHOR_DATE", "2025-03-04T05:06:07+02:00")
        .env("GIT_COMMITTER_NAME", "Fixture Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.invalid")
        .env("GIT_COMMITTER_DATE", "2025-03-04T05:06:07+02:00")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_owned()
}

pub fn commit_file(repo: &Path, name: &str, contents: &str) -> Sha {
    fs::write(repo.join(name), contents).expect("write file");
    fixture_git(repo, &["add", "--", name]);
    fixture_git(repo, &["commit", "--quiet", "-m", &format!("add {name}")]);
    Sha::parse(&fixture_git(repo, &["rev-parse", "HEAD"])).expect("sha")
}

/// `main` is A then M; `feature` is A then B. Returns (M, A, B).
pub fn fixture(repo: &Path) -> (Sha, Sha, Sha) {
    fixture_git(repo, &["init", "--quiet", "--initial-branch=main"]);
    let a = commit_file(repo, "base.txt", "base\n");
    fixture_git(repo, &["checkout", "--quiet", "-b", "feature"]);
    let b = commit_file(repo, "feature.txt", "feature\n");
    fixture_git(repo, &["checkout", "--quiet", "main"]);
    let m = commit_file(repo, "other.txt", "other\n");
    (m, a, b)
}

/// Like [`fixture`], but M and B both change `conflict.txt`, so replaying B onto M conflicts and
/// a merge driver named for that file would run. A also commits `.gitattributes` naming the
/// filter, merge driver and diff driver `evil` for every file.
pub fn conflicting_fixture(repo: &Path) -> (Sha, Sha, Sha) {
    fixture_git(repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(
        repo.join(".gitattributes"),
        "* filter=evil merge=evil diff=evil\n",
    )
    .expect("write attributes");
    fixture_git(repo, &["add", "--", ".gitattributes"]);
    commit_file(repo, "conflict.txt", "base\n");
    let a = Sha::parse(&fixture_git(repo, &["rev-parse", "HEAD"])).expect("sha");
    fixture_git(repo, &["checkout", "--quiet", "-b", "feature"]);
    let b = commit_file(repo, "conflict.txt", "feature\n");
    fixture_git(repo, &["checkout", "--quiet", "main"]);
    let m = commit_file(repo, "conflict.txt", "main\n");
    (m, a, b)
}

/// Writes an executable `sh` script, making its directory if needed.
pub fn write_script(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("script dir");
    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// The sorted names in `dir`: where planted code leaves a marker file when it runs.
pub fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read markers")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

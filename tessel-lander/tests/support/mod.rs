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

use std::fs;
use std::path::Path;
use std::process::Command;

use tessel_lander::types::Sha;

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

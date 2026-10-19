//! C7: a rebase gives the same commit ids on every machine and for every caller environment.
//!
//! The first test sets process-wide environment variables. The other test, the fixture and the
//! runner read only `PATH` from the process, so they do not observe them.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use tessel_lander::git::Git;
use tessel_lander::run::run;
use tessel_lander::types::Sha;

/// The head after replaying the fixture's feature commit onto its advanced main, written down so
/// that a committer name, email or date taken from anywhere else changes it.
const REBASED_HEAD: &str = "459880f548df0118ce7b5620e9c051b352ee6729";

/// Runs git for the fixture with its own fixed author and committer, whatever the process holds.
fn fixture_git(repo: &Path, args: &[&str]) -> String {
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

fn commit_file(repo: &Path, name: &str, contents: &str) -> Sha {
    fs::write(repo.join(name), contents).expect("write file");
    fixture_git(repo, &["add", "--", name]);
    fixture_git(repo, &["commit", "--quiet", "-m", &format!("add {name}")]);
    Sha::parse(&fixture_git(repo, &["rev-parse", "HEAD"])).expect("sha")
}

/// `main` is A then M; `feature` is A then B. Returns (M, A, B).
fn fixture(repo: &Path) -> (Sha, Sha, Sha) {
    fixture_git(repo, &["init", "--quiet", "--initial-branch=main"]);
    let a = commit_file(repo, "base.txt", "base\n");
    fixture_git(repo, &["checkout", "--quiet", "-b", "feature"]);
    let b = commit_file(repo, "feature.txt", "feature\n");
    fixture_git(repo, &["checkout", "--quiet", "main"]);
    let m = commit_file(repo, "other.txt", "other\n");
    (m, a, b)
}

fn rebase_head(root: &Path, repo: &Path, (m, a, b): &(Sha, Sha, Sha)) -> String {
    let git = Git::create_in(root).expect("environment directories");
    let rebase = run(&git.rebase_command(repo, m, a, b)).expect("rebase runs");
    assert!(rebase.success(), "rebase: {}", rebase.stderr);
    let head = run(&git.head_command(repo)).expect("head runs");
    assert!(head.success(), "head: {}", head.stderr);
    head.stdout.trim().to_owned()
}

fn plant_traps(repo: &Path, marker: &Path) {
    let hooks = repo.join(".git").join("hooks");
    let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
    for hook in [
        "pre-rebase",
        "post-checkout",
        "post-commit",
        "post-rewrite",
        "reference-transaction",
    ] {
        let path = hooks.join(hook);
        fs::write(&path, &script).expect("write hook");
        make_executable(&path);
    }
    for alias in ["rebase", "diff", "rev-parse", "merge-base"] {
        let value = format!("!touch '{}'", marker.display());
        fixture_git(repo, &["config", &format!("alias.{alias}"), &value]);
    }
    fs::write(
        repo.join(".gitattributes"),
        "* filter=evil merge=evil diff=evil\n",
    )
    .expect("write attributes");
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn the_rebased_head_is_a_known_id_whatever_the_callers_environment_says() {
    let scratch = TempDir::new().expect("tempdir");
    let repo = scratch.path().join("repo");
    fs::create_dir(&repo).expect("repo dir");
    let ids = fixture(&repo);

    let hostile_config = scratch.path().join("hostile.gitconfig");
    fs::write(
        &hostile_config,
        "[user]\n\tname = Ambient Config\n\temail = ambient-config@example.invalid\n",
    )
    .expect("write config");
    for (key, value) in [
        ("GIT_COMMITTER_NAME", "Ambient Committer"),
        ("GIT_COMMITTER_EMAIL", "ambient-committer@example.invalid"),
        ("GIT_COMMITTER_DATE", "2031-12-31T23:59:59+05:00"),
        ("GIT_AUTHOR_NAME", "Ambient Author"),
        ("GIT_AUTHOR_EMAIL", "ambient-author@example.invalid"),
        ("GIT_AUTHOR_DATE", "2032-01-01T00:00:00+00:00"),
        ("EMAIL", "ambient-email@example.invalid"),
        (
            "GIT_CONFIG_GLOBAL",
            hostile_config.to_str().expect("utf-8 path"),
        ),
        ("HOME", scratch.path().to_str().expect("utf-8 path")),
    ] {
        std::env::set_var(key, value);
    }

    let head = rebase_head(&scratch.path().join("lander"), &repo, &ids);
    assert_eq!(head, REBASED_HEAD);

    let (an, ae, ad, cn, ce, cd) = {
        let line = fixture_git(
            &repo,
            &["log", "-1", "--format=%an|%ae|%aI|%cn|%ce|%cI", "HEAD"],
        );
        let fields: Vec<&str> = line.split('|').collect();
        (
            fields[0].to_owned(),
            fields[1].to_owned(),
            fields[2].to_owned(),
            fields[3].to_owned(),
            fields[4].to_owned(),
            fields[5].to_owned(),
        )
    };
    assert_eq!(
        (an.as_str(), ae.as_str()),
        ("Fixture Author", "author@example.invalid")
    );
    assert_eq!(ad, "2025-03-04T05:06:07+02:00");
    assert_eq!(
        (cn.as_str(), ce.as_str()),
        ("Tessel Lander", "lander@tessel.invalid")
    );
    assert_eq!(cd, "2026-01-01T00:00:00Z");
}

#[test]
fn repo_hooks_aliases_and_attribute_drivers_do_not_run() {
    let scratch = TempDir::new().expect("tempdir");
    let repo = scratch.path().join("repo");
    fs::create_dir(&repo).expect("repo dir");
    let ids = fixture(&repo);
    let marker = scratch.path().join("ran");
    plant_traps(&repo, &marker);
    fixture_git(&repo, &["checkout", "--quiet", "feature"]);
    assert!(marker.exists(), "control: plain git runs the trap");
    fs::remove_file(&marker).expect("remove marker");

    let head = rebase_head(&scratch.path().join("lander"), &repo, &ids);

    assert!(!marker.exists(), "repo code ran");
    assert_eq!(head, REBASED_HEAD, "no trap changed the result");
}

//! C7: a rebase gives the same commit ids on every machine and for every caller environment.
//!
//! This file holds one test on purpose: it sets process-wide environment variables, which no
//! other test in the same process may observe.

mod support;

use std::fs;

use tempfile::TempDir;
use tessel_lander::git::Git;
use tessel_lander::run::{run, SystemRunner};
use tessel_lander::workspace::clone_main;

use support::{cloned, fixture, fixture_git, sources_for};

/// The head after replaying the fixture's feature commit onto its advanced main, written down so
/// that a committer name, email or date taken from anywhere else changes it.
const REBASED_HEAD: &str = "459880f548df0118ce7b5620e9c051b352ee6729";

#[test]
fn the_rebased_head_is_a_known_id_whatever_the_callers_environment_says() {
    let scratch = TempDir::new().expect("tempdir");
    let repo = scratch.path().join("repo");
    fs::create_dir(&repo).expect("repo dir");
    let (m, a, b) = fixture(&repo);

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
        // These would redirect or break git if the environment were not cleared.
        ("GIT_DIR", "/nonexistent/ambient.git"),
        ("GIT_OBJECT_DIRECTORY", "/nonexistent/objects"),
        ("GIT_CONFIG_PARAMETERS", "'core.hooksPath'='/nonexistent'"),
    ] {
        std::env::set_var(key, value);
    }

    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    let remote = repo.display().to_string();
    let sources = sources_for(&remote, &remote, "feature");
    let workspace = cloned(
        clone_main(
            &git,
            &SystemRunner,
            &sources,
            &scratch.path().join("workspace"),
        )
        .expect("clone runs"),
    );
    let rebase = run(&git.rebase_command(&workspace, &m, &a, &b)).expect("rebase runs");
    assert!(rebase.success(), "rebase: {}", rebase.stderr);
    let head = run(&git.head_command(&workspace)).expect("head runs");
    assert_eq!(head.stdout.trim(), REBASED_HEAD);

    let line = fixture_git(
        workspace.path(),
        &["log", "-1", "--format=%an|%ae|%aI|%cn|%ce|%cI", "HEAD"],
    );
    assert_eq!(
        line,
        "Fixture Author|author@example.invalid|2025-03-04T05:06:07+02:00|\
         Tessel Lander|lander@tessel.invalid|2026-01-01T00:00:00Z"
    );
}

//! Rule 4: the lander runs git but never repo code. A source repository plants a filter, a merge
//! driver, a hook, `core.fsmonitor`, `core.sshCommand` and `diff.external` in its config and
//! commits attributes that name them. Nothing may run in the workspace the lander clones.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tessel_lander::git::Git;
use tessel_lander::run::{run, SystemRunner};
use tessel_lander::workspace::{clone_main, Workspace};

use support::{cloned, conflicting_fixture, failed_clone, fixture_git, sources_for};

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Plants every repo-code hook in `source`'s own config. Markers are files in `markers`.
fn plant(source: &Path, markers: &Path) {
    let touch = |name: &str| format!("touch '{}'", markers.join(name).display());
    let hook = source.join(".git").join("hooks").join("post-checkout");
    script(&hook, &touch("hook"));
    let fsmonitor = markers.join("fsmonitor.sh");
    script(&fsmonitor, &touch("fsmonitor"));
    for (key, value) in [
        (
            "filter.evil.clean",
            format!("{}; cat", touch("filter-clean")),
        ),
        (
            "filter.evil.smudge",
            format!("{}; cat", touch("filter-smudge")),
        ),
        (
            "merge.evil.driver",
            format!("{}; exit 1", touch("merge-driver")),
        ),
        ("diff.evil.textconv", touch("textconv")),
        ("diff.external", touch("diff-external")),
        ("core.sshCommand", format!("{}; false", touch("ssh"))),
        ("core.fsmonitor", fsmonitor.display().to_string()),
        (
            "core.hooksPath",
            source.join(".git").join("hooks").display().to_string(),
        ),
    ] {
        fixture_git(source, &["config", key, &value]);
    }
}

fn markers(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read markers")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n != "fsmonitor.sh")
        .collect();
    names.sort();
    names
}

fn clone_source(git: &Git, source: &Path, destination: &Path) -> Workspace {
    let remote = source.display().to_string();
    let sources = sources_for(&remote, &remote, "feature");
    cloned(clone_main(git, &SystemRunner, &sources, destination).expect("clone runs"))
}

fn setup() -> (
    TempDir,
    PathBuf,
    PathBuf,
    Git,
    (
        tessel_lander::types::Sha,
        tessel_lander::types::Sha,
        tessel_lander::types::Sha,
    ),
) {
    let scratch = TempDir::new().expect("tempdir");
    let source = scratch.path().join("source");
    let markers = scratch.path().join("markers");
    fs::create_dir(&source).expect("source dir");
    fs::create_dir(&markers).expect("markers dir");
    let ids = conflicting_fixture(&source);
    plant(&source, &markers);
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    (scratch, source, markers, git, ids)
}

#[test]
fn nothing_the_source_repos_config_names_runs_in_the_cloned_workspace() {
    let (scratch, source, markers_dir, git, (m, a, b)) = setup();
    let workspace = clone_source(&git, &source, &scratch.path().join("workspace"));

    let rebase = run(&git.rebase_command(&workspace, &m, &a, &b)).expect("rebase runs");
    assert!(
        !rebase.success(),
        "the fixture conflicts, so the merge driver would run"
    );
    let conflicts = run(&git.conflicts_command(&workspace)).expect("conflicts runs");
    assert_eq!(conflicts.stdout, "conflict.txt\0");
    for command in [
        git.changed_files_command(&workspace, &a, &b),
        git.diff_command(&workspace, &a, &b),
        git.head_command(&workspace),
    ] {
        run(&command).expect("command runs");
    }

    assert_eq!(markers(&markers_dir), Vec::<String>::new());
}

#[test]
fn control_the_same_config_does_run_a_filter_and_a_merge_driver_in_the_source_repo() {
    let (scratch, source, markers_dir, git, (m, a, b)) = setup();
    let workspace = clone_source(&git, &source, &scratch.path().join("workspace"));
    let mut command = git.rebase_command(&workspace, &m, &a, &b);
    assert_eq!(command.argv[1], "-C");
    command.argv[2] = source.clone().into_os_string();
    // Same bytes, new modification time: git re-reads the file to see that it is unchanged, which
    // runs the clean filter. Without this the filter runs only while the index is racily clean.
    fs::write(source.join("conflict.txt"), "main\n").expect("touch");

    let rebase = run(&command).expect("rebase runs");

    assert!(!rebase.success());
    let ran = markers(&markers_dir);
    assert!(ran.contains(&"merge-driver".to_owned()), "{ran:?}");
    assert!(
        ran.iter().any(|name| name.starts_with("filter-")),
        "{ran:?}"
    );
    for neutralised in ["hook", "fsmonitor", "diff-external", "ssh", "textconv"] {
        assert!(
            !ran.contains(&neutralised.to_owned()),
            "{neutralised} ran: {ran:?}"
        );
    }
}

#[test]
fn a_remote_that_names_a_command_or_a_plain_transport_is_refused() {
    let scratch = TempDir::new().expect("tempdir");
    let marker = scratch.path().join("ext-ran");
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    for (remote, expected) in [
        (format!("ext::sh -c 'touch {}'", marker.display()), "ext"),
        ("http://127.0.0.1:9/repo.git".to_owned(), "http"),
        ("git://127.0.0.1:9/repo.git".to_owned(), "git"),
    ] {
        let sources = sources_for(&remote, "", "main");
        let destination = scratch.path().join(format!("workspace-{expected}"));
        let clone = failed_clone(
            clone_main(&git, &SystemRunner, &sources, &destination).expect("clone runs"),
        );
        assert!(
            clone
                .stderr
                .contains(&format!("transport '{expected}' not allowed")),
            "{expected}: {}",
            clone.stderr
        );
    }
    assert!(!marker.exists());
}

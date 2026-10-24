//! Rule 4: the lander runs git but never repo code. A source repository plants a filter, a merge
//! driver, a hook, `core.fsmonitor`, `core.sshCommand` and `diff.external` in its config and
//! commits attributes that name them. Nothing may run in the workspace the lander clones.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

mod support;

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;
use tessel_lander::git::{Git, GitCommand};
use tessel_lander::run::{run, SystemRunner};
use tessel_lander::workspace::{clone_main, Workspace};

use support::{
    cloned, conflicting_fixture, failed_clone, file_names, fixture_git, sources_for, write_script,
};

/// Plants every repo-code hook in `source`'s own config. Markers are files in `markers`.
fn plant(source: &Path, markers: &Path) {
    let touch = |name: &str| format!("touch '{}'", markers.join(name).display());
    let hook = source.join(".git").join("hooks").join("post-checkout");
    write_script(&hook, &touch("hook"));
    let fsmonitor = markers.join("fsmonitor.sh");
    write_script(&fsmonitor, &touch("fsmonitor"));
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
    let mut names = file_names(dir);
    names.retain(|name| name != "fsmonitor.sh");
    names
}

fn clone_source(git: &Git, source: &Path, destination: &Path) -> Workspace {
    let remote = source.display().to_string();
    let sources = sources_for(&remote, &remote);
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
    // Same bytes, new modification time, as in the control: a clean filter in the workspace
    // would run, so no marker proves there is none.
    fs::write(workspace.path().join("conflict.txt"), "main\n").expect("touch");

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
fn a_workspace_cloned_to_a_relative_destination_runs_commands() {
    let (_scratch, source, _markers, git, _ids) = setup();
    let cwd = std::env::current_dir().expect("cwd");
    let inside_cwd = TempDir::new_in(&cwd).expect("tempdir in the working directory");
    let relative = PathBuf::from(inside_cwd.path().file_name().expect("name")).join("workspace");
    assert!(relative.is_relative());

    let workspace = clone_source(&git, &source, &relative);

    assert!(workspace.path().is_absolute());
    let head = run(&git.head_command(&workspace)).expect("head runs");
    assert!(head.success(), "{}", head.stderr);
}

#[test]
fn an_empty_directory_inside_another_repository_does_not_run_that_repositorys_config() {
    let scratch = TempDir::new().expect("tempdir");
    let repo = scratch.path().join("planted");
    fs::create_dir(&repo).expect("planted dir");
    let (m, a, b) = conflicting_fixture(&repo);
    let marker = scratch.path().join("driver-ran");
    let driver = format!("touch '{}'; exit 1", marker.display());
    fixture_git(&repo, &["config", "merge.evil.driver", &driver]);
    let inside = repo.join("inside");
    fs::create_dir(&inside).expect("empty dir inside the repository");
    let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
    let sources = sources_for("https://git.example/demo.git", "");
    let workspace = cloned(clone_main(&git, &support::AlwaysOk, &sources, &inside).expect("clone"));

    let rebase = run(&git.rebase_command(&workspace, &m, &a, &b)).expect("rebase runs");

    assert!(!rebase.success(), "git must not find the parent repository");
    assert!(
        rebase.stderr.contains("not a git repository"),
        "{}",
        rebase.stderr
    );
    assert!(!marker.exists(), "the parent repository's merge driver ran");
}

#[test]
fn control_the_same_config_does_run_a_filter_and_a_merge_driver_in_the_source_repo() {
    let (scratch, source, markers_dir, git, (m, a, b)) = setup();
    let workspace = clone_source(&git, &source, &scratch.path().join("workspace"));
    let mut command = git.rebase_command(&workspace, &m, &a, &b);
    assert_eq!(command.argv[1], "-C");
    command.argv[2] = source.clone().into_os_string();
    command
        .env
        .insert("GIT_DIR".to_owned(), source.join(".git").into());
    command
        .env
        .insert("GIT_WORK_TREE".to_owned(), source.clone().into());
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

/// A command of the lander's fixed environment in `workspace`, which is not one of the merge's
/// commands: it exists to see what git does with config planted in the workspace.
fn probe(git: &Git, workspace: &Workspace, args: &[&str]) -> GitCommand {
    let mut argv = vec![OsString::from("git"), "-C".into(), workspace.path().into()];
    argv.extend(args.iter().map(OsString::from));
    GitCommand {
        argv,
        env: git.env(),
        timeout: Duration::from_secs(10),
    }
}

fn plant_in_workspace(workspace: &Workspace, key: &str, value: &str) {
    fixture_git(workspace.path(), &["config", key, value]);
}

#[test]
fn an_ssh_command_planted_in_the_workspace_config_is_not_run_by_a_fetch() {
    let (scratch, source, markers_dir, git, _ids) = setup();
    let workspace = clone_source(&git, &source, &scratch.path().join("workspace"));
    let marker = markers_dir.join("ssh-in-workspace");
    plant_in_workspace(
        &workspace,
        "core.sshCommand",
        &format!("touch '{}'; false", marker.display()),
    );

    let fetch = run(&probe(
        &git,
        &workspace,
        &["ls-remote", "--", "ssh://127.0.0.1:9/repository.git"],
    ))
    .expect("ls-remote runs");

    assert!(!fetch.success(), "nothing listens on port 9");
    assert!(!marker.exists(), "the planted ssh command ran");
}

#[test]
fn a_credential_helper_planted_in_the_workspace_config_is_not_asked_for_a_credential() {
    let (scratch, source, markers_dir, git, _ids) = setup();
    let workspace = clone_source(&git, &source, &scratch.path().join("workspace"));
    let marker = markers_dir.join("credential-helper");
    plant_in_workspace(
        &workspace,
        "credential.helper",
        &format!("!touch '{}'; cat >/dev/null", marker.display()),
    );
    let ask = probe(&git, &workspace, &["credential", "fill"]);
    let mut child = Command::new(&ask.argv[0])
        .args(&ask.argv[1..])
        .env_clear()
        .envs(&ask.env)
        .env("GIT_ASKPASS", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("git credential runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"protocol=https\nhost=git.example.invalid\n\n")
        .expect("request");
    child.wait().expect("git credential ends");

    assert!(!marker.exists(), "the planted credential helper ran");
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
        let sources = sources_for(&remote, "");
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

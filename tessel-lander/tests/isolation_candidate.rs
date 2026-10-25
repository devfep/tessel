//! Requirement S3 (`docs/proposals/real-use.md`) for the candidate ref: what a head carries
//! (committed hooks, `.gitattributes` filters, a `.git` directory in its tree) is inert when
//! `fetch_candidate_tree` makes the test tree and when `land_candidate` reads it. The control
//! strips the one setting that makes the filter inert and shows the planted code then runs.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

mod support;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tessel_lander::candidate::candidate_ref;
use tessel_lander::git::{Git, GitCommand};
use tessel_lander::merge::{MergeContext, PushUpdate};
use tessel_lander::outcome::MergeOutcome;
use tessel_lander::run::{run, GitOutput, GitRunner, RunError};
use tessel_lander::types::Sha;
use tessel_lander::workspace::{fetch_candidate_tree, CandidateTreeOutcome};

use support::repos::{LocalPorts, Repos};
use support::{file_names, fixture_git, write_script};

/// Runs every command with `GIT_CONFIG_GLOBAL` removed, so git reads the home config.
struct ReadsHomeConfig;

impl GitRunner for ReadsHomeConfig {
    fn run(&self, command: &GitCommand) -> Result<GitOutput, RunError> {
        let mut command = command.clone();
        command.env.remove("GIT_CONFIG_GLOBAL");
        run(&command)
    }
}

struct Scene {
    repos: Repos,
    git: Git,
    markers: PathBuf,
}

impl Scene {
    /// A lander environment whose home config defines the filter `evil`, a bare main, and a
    /// directory for the markers planted code leaves.
    fn new() -> Scene {
        let repos = Repos::new();
        let git = Git::create_in(&repos.scratch.path().join("isolated")).expect("environment");
        let markers = repos.scratch.path().join("markers");
        fs::create_dir(&markers).expect("markers dir");
        let file = repos.scratch.path().join("isolated/home/.gitconfig");
        let file = file.display().to_string();
        let touch = |name: &str| format!("touch '{}'", markers.join(name).display());
        for (key, name) in [
            ("filter.evil.smudge", "smudge"),
            ("filter.evil.clean", "clean"),
        ] {
            let value = format!("{}; cat", touch(name));
            fixture_git(
                repos.scratch.path(),
                &["config", "--file", &file, key, &value],
            );
        }
        Scene {
            repos,
            git,
            markers,
        }
    }

    fn ran(&self) -> Vec<String> {
        file_names(&self.markers)
    }

    /// A head on main whose tree names the filter for every file and holds hook-shaped files.
    fn hostile_head(&self) -> Sha {
        let repos = &self.repos;
        repos.commit(".gitattributes", "* filter=evil\n");
        for path in [
            ".githooks/post-checkout",
            "hooks/post-checkout",
            ".husky/pre-commit",
        ] {
            let marker = self
                .markers
                .join(format!("committed-{}", path.replace('/', "-")));
            write_script(
                &repos.work.join(path),
                &format!("touch '{}'", marker.display()),
            );
            fixture_git(&repos.work, &["add", "--", path]);
        }
        fixture_git(&repos.work, &["commit", "--quiet", "-m", "hostile"]);
        let head = repos.head();
        repos.set_candidate(head.as_str(), &head);
        head
    }

    fn tree(&self, runner: &dyn GitRunner, head: &Sha, name: &str) -> CandidateTreeOutcome {
        let remote = self.repos.main.display().to_string();
        let destination = self.repos.scratch.path().join(name);
        fetch_candidate_tree(&self.git, runner, &remote, head, &destination).expect("tree step")
    }
}

#[test]
fn a_head_with_filters_and_committed_hooks_makes_an_inert_test_tree() {
    let scene = Scene::new();
    let head = scene.hostile_head();

    let outcome = scene.tree(&tessel_lander::run::SystemRunner, &head, "tree");

    assert!(
        matches!(outcome, CandidateTreeOutcome::Fetched(_)),
        "{outcome:?}"
    );
    assert_eq!(scene.ran(), Vec::<String>::new());
}

#[test]
fn control_with_the_home_config_readable_the_filter_runs_in_the_checkout() {
    let scene = Scene::new();
    let head = scene.hostile_head();

    scene.tree(&ReadsHomeConfig, &head, "tree");

    let ran = scene.ran();
    assert!(ran.contains(&"smudge".to_owned()), "{ran:?}");
    assert!(
        !ran.iter().any(|name| name.starts_with("committed-")),
        "a committed file ran: {ran:?}"
    );
}

#[test]
fn a_hostile_head_lands_without_anything_it_carries_running() {
    let scene = Scene::new();
    let head = scene.hostile_head();
    let sources = scene.repos.sources();
    let base = scene.repos.main_head();
    let (clone_dir, test_dir) = (
        scene.repos.scratch.path().join("main-clone"),
        scene.repos.scratch.path().join("unused"),
    );
    let ctx = MergeContext {
        git: &scene.git,
        runner: &tessel_lander::run::SystemRunner,
        sources: &sources,
        clone_dir: &clone_dir,
        test_dir: &test_dir,
        pinned_main: &base,
    };
    let update = PushUpdate {
        base: base.clone(),
        head: head.clone(),
    };

    let outcome =
        tessel_lander::candidate::land_candidate(&ctx, &LocalPorts::new(&scene.repos), &update)
            .expect("land");

    assert!(
        matches!(outcome, MergeOutcome::Merged { .. }),
        "{outcome:?}"
    );
    assert_eq!(scene.repos.main_head(), head);
    assert_eq!(scene.ran(), Vec::<String>::new());
}

/// Makes a commit whose tree holds `.git/config` with a `core.fsmonitor` that would run a
/// program, puts it on the candidate ref and returns it. Git itself will not write the tree.
fn head_with_a_git_directory(scene: &Scene) -> Sha {
    let work = &scene.repos.work;
    let marker = scene.markers.join("fsmonitor");
    let config = format!("[core]\n\tfsmonitor = touch '{}'\n", marker.display());
    let file = scene.repos.scratch.path().join("evil-config");
    fs::write(&file, config).expect("config");
    let blob = fixture_git(work, &["hash-object", "-w", &file.display().to_string()]);
    let inner = mktree(work, &format!("100644 blob {blob}\tconfig\n"));
    let top = mktree(work, &format!("040000 tree {inner}\t.git\n"));
    let commit = fixture_git(work, &["commit-tree", &top, "-m", "a .git directory"]);
    let head = Sha::parse(&commit).expect("sha");
    scene.repos.set_candidate(head.as_str(), &head);
    head
}

fn mktree(repo: &Path, listing: &str) -> String {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("mktree")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("git mktree");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(listing.as_bytes())
        .expect("listing");
    let output = child.wait_with_output().expect("mktree");
    assert!(output.status.success(), "mktree refused the listing");
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_owned()
}

#[test]
fn a_head_with_a_git_directory_in_its_tree_makes_no_test_tree_and_leaves_the_config_alone() {
    let scene = Scene::new();
    let head = head_with_a_git_directory(&scene);
    assert_eq!(
        candidate_ref(&head),
        format!("refs/tessel/candidate/{head}")
    );

    let outcome = scene.tree(&tessel_lander::run::SystemRunner, &head, "tree");

    assert!(
        matches!(outcome, CandidateTreeOutcome::Failed(_)),
        "{outcome:?}"
    );
    let config =
        fs::read_to_string(scene.repos.scratch.path().join("tree/.git/config")).expect("config");
    assert!(!config.contains("fsmonitor"), "{config}");
    assert_eq!(scene.ran(), Vec::<String>::new());
}

#[test]
fn a_head_with_a_git_directory_is_never_pushed_to_main() {
    let scene = Scene::new();
    let head = head_with_a_git_directory(&scene);
    let sources = scene.repos.sources();
    let base = scene.repos.main_head();
    let (clone_dir, test_dir) = (
        scene.repos.scratch.path().join("main-clone"),
        scene.repos.scratch.path().join("unused"),
    );
    let ctx = MergeContext {
        git: &scene.git,
        runner: &tessel_lander::run::SystemRunner,
        sources: &sources,
        clone_dir: &clone_dir,
        test_dir: &test_dir,
        pinned_main: &base,
    };
    let update = PushUpdate {
        base: base.clone(),
        head,
    };

    let outcome =
        tessel_lander::candidate::land_candidate(&ctx, &LocalPorts::new(&scene.repos), &update)
            .expect("land");

    assert!(
        !matches!(outcome, MergeOutcome::Merged { .. }),
        "{outcome:?}"
    );
    assert_eq!(scene.repos.main_head(), base);
    assert_eq!(scene.ran(), Vec::<String>::new());
}

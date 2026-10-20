//! The repo's install and test steps run in a tree of their own, never in the workspace that
//! `land` pushes from (`docs/proposals/real-use.md`, security requirement 3). Repo code in the
//! workspace could rewrite `.git/config` (`url.*.pushInsteadOf`, `include.path`,
//! `remote.<url>.pushurl`, `remote.<url>.receivepack`), refs, objects or `info/attributes`, and
//! the push would follow. Main is a `file://` URL here, as a real remote is a URL: git reads
//! `remote.<url>.*` for a push to a URL, but ignores it (with a warning) for a plain path.
//!
//! Each effect that can change what `land` does has a control: the same plant made in the
//! workspace itself, where `land` is subverted. A control that stopped showing that would mean
//! the test beside it no longer proves anything. A moved ref and rewritten attributes change
//! nothing `land` does today, so they are planted in the hostile run but have no control.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

mod support;

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tessel_lander::git::{Git, GitCommand, MergeSources};
use tessel_lander::merge::{
    land, prepare, run_merge, BoundaryError, MergeContext, MergePolicy, MergePorts, MergeRequest,
    Prepared, PushUpdate, Ready,
};
use tessel_lander::outcome::{
    EvidenceResult, LandedMerge, MergeOutcome, PackageStep, StepOutcome, TrialOutcome,
};
use tessel_lander::run::{GitOutput, GitRunner, RunError, SystemRunner};
use tessel_lander::trial::{run_trial, TrialContext, TrialPorts};
use tessel_lander::types::Sha;
use tessel_lander::workspace::{clone_test_tree, CloneError, TestTree, TestTreeOutcome};

use support::repos::{whole_repo, Repos};
use support::{fixture_git, workspace_at, write_script};

/// A merge that has work to do (the fork has `b.txt` on top of main) and the things a hostile
/// package step would aim at: a second repository that looks like main, and a program to run.
struct Scene {
    repos: Repos,
    git: Git,
    base: Sha,
    commit: Sha,
    evil: PathBuf,
    marker: PathBuf,
    receive_pack: PathBuf,
    clone_dir: PathBuf,
    test_dir: PathBuf,
}

impl Scene {
    fn new() -> Scene {
        let repos = Repos::new();
        let base = repos.head();
        let commit = repos.commit("b.txt", "fork\n");
        repos.push(&repos.fork);
        repos.reset_to(base.as_str());
        let scratch = repos.scratch.path().to_path_buf();
        let git = Git::create_in(&scratch.join("isolated")).expect("environment");
        let evil = scratch.join("evil.git");
        let main = repos.main.display().to_string();
        let evil_arg = evil.display().to_string();
        fixture_git(&scratch, &["clone", "--quiet", "--bare", &main, &evil_arg]);
        let marker = scratch.join("receive-pack-ran");
        let receive_pack = scratch.join("receive-pack.sh");
        write_script(
            &receive_pack,
            &format!("touch '{}'\nexec git receive-pack \"$@\"", marker.display()),
        );
        Scene {
            repos,
            git,
            base,
            commit,
            evil,
            marker,
            receive_pack,
            clone_dir: scratch.join("workspace"),
            test_dir: scratch.join("test-tree"),
        }
    }

    fn main_remote(&self) -> String {
        format!("file://{}", self.repos.main.display())
    }

    fn sources(&self) -> MergeSources {
        MergeSources {
            main_remote: self.main_remote(),
            ..self.repos.sources()
        }
    }

    fn context<'a>(
        &'a self,
        runner: &'a dyn GitRunner,
        sources: &'a MergeSources,
    ) -> MergeContext<'a> {
        MergeContext {
            git: &self.git,
            runner,
            sources,
            clone_dir: &self.clone_dir,
            test_dir: &self.test_dir,
            pinned_main: &self.base,
        }
    }

    fn evil_main(&self) -> String {
        fixture_git(&self.evil, &["rev-parse", "main"])
    }

    fn receive_pack_ran(&self) -> bool {
        self.marker.exists()
    }
}

fn config(dir: &Path, key: &str, value: &str) {
    fixture_git(dir, &["config", "--local", key, value]);
}

fn redirect_pushes(scene: &Scene, dir: &Path) {
    let evil = scene.evil.display();
    config(
        dir,
        &format!("url.{evil}.pushInsteadOf"),
        &scene.main_remote(),
    );
}

fn include_a_redirect(scene: &Scene, dir: &Path) {
    let included = scene.repos.scratch.path().join("included.cfg");
    let key = format!("url.{}.pushInsteadOf", scene.evil.display());
    let file = included.display().to_string();
    fixture_git(
        scene.repos.scratch.path(),
        &["config", "--file", &file, &key, &scene.main_remote()],
    );
    config(dir, "include.path", &file);
}

fn name_a_receive_pack(scene: &Scene, dir: &Path) {
    let program = scene.receive_pack.display().to_string();
    config(
        dir,
        &format!("remote.{}.receivepack", scene.main_remote()),
        &program,
    );
}

fn set_a_push_url(scene: &Scene, dir: &Path) {
    let evil = scene.evil.display().to_string();
    config(
        dir,
        &format!("remote.{}.pushurl", scene.main_remote()),
        &evil,
    );
}

fn wipe_objects(_scene: &Scene, dir: &Path) {
    let objects = dir.join(".git/objects");
    fs::remove_dir_all(&objects).expect("remove objects");
    fs::create_dir(&objects).expect("recreate objects");
}

/// Points the tracking ref at a commit that is neither main nor the merged head.
fn move_a_ref(_scene: &Scene, dir: &Path) {
    let planted = fixture_git(dir, &["commit-tree", "HEAD^{tree}", "-m", "planted"]);
    fixture_git(dir, &["update-ref", "refs/remotes/origin/main", &planted]);
}

fn rewrite_attributes(_scene: &Scene, dir: &Path) {
    let info = dir.join(".git/info");
    fs::create_dir_all(&info).expect("info dir");
    fs::write(info.join("attributes"), "* merge=union\n").expect("write attributes");
}

fn plant_everything(scene: &Scene, dir: &Path) {
    redirect_pushes(scene, dir);
    include_a_redirect(scene, dir);
    name_a_receive_pack(scene, dir);
    set_a_push_url(scene, dir);
    move_a_ref(scene, dir);
    rewrite_attributes(scene, dir);
    wipe_objects(scene, dir);
}

/// Ports over the local repositories. With `hostile` set, the test step plants everything in
/// the tree it is given.
struct Ports<'a> {
    scene: &'a Scene,
    hostile: bool,
    log: RefCell<Vec<String>>,
    trees: RefCell<Vec<PathBuf>>,
}

impl<'a> Ports<'a> {
    fn new(scene: &'a Scene, hostile: bool) -> Ports<'a> {
        Ports {
            scene,
            hostile,
            log: RefCell::new(Vec::new()),
            trees: RefCell::new(Vec::new()),
        }
    }
}

impl MergePorts for Ports<'_> {
    fn revoke_read_access(&self) -> bool {
        true
    }

    fn run_package_step(
        &self,
        step: PackageStep,
        tree: &TestTree,
    ) -> Result<StepOutcome, BoundaryError> {
        self.log.borrow_mut().push(format!("{step:?}"));
        self.trees.borrow_mut().push(tree.path().to_path_buf());
        if self.hostile && step == PackageStep::Test {
            plant_everything(self.scene, tree.path());
        }
        Ok(StepOutcome::finished(step, 0, "", ""))
    }

    fn with_push_access<T>(
        &self,
        _update: &PushUpdate,
        use_access: impl FnOnce() -> T,
    ) -> Result<T, BoundaryError> {
        self.log.borrow_mut().push("push access".to_owned());
        Ok(use_access())
    }

    fn current_main(&self) -> Option<Sha> {
        Some(self.scene.repos.main_head())
    }

    fn record_merge(&self, _landed: &LandedMerge) -> EvidenceResult {
        EvidenceResult::NotWritten {
            reason: "no record repo in this test".to_owned(),
        }
    }
}

impl TrialPorts for Ports<'_> {
    fn revoke_read_access(&self) -> bool {
        true
    }

    fn run_package_step(
        &self,
        step: PackageStep,
        tree: &TestTree,
    ) -> Result<StepOutcome, BoundaryError> {
        self.trees.borrow_mut().push(tree.path().to_path_buf());
        Ok(StepOutcome::finished(step, 0, "", ""))
    }
}

fn merge(scene: &Scene, runner: &dyn GitRunner, ports: &Ports<'_>) -> MergeOutcome {
    let sources = scene.sources();
    let ctx = scene.context(runner, &sources);
    let scopes = whole_repo();
    let request = MergeRequest {
        commit: &scene.commit,
        scopes: &scopes,
        policy: MergePolicy::Submission,
    };
    run_merge(&ctx, ports, &request).expect("the merge finishes")
}

/// Prepares the merge, lets `plant` act on the workspace, then lands: what a package step run in
/// the workspace could do before `land`.
fn land_after_planting(scene: &Scene, plant: fn(&Scene, &Path)) -> MergeOutcome {
    let sources = scene.sources();
    let ctx = scene.context(&SystemRunner, &sources);
    let ports = Ports::new(scene, false);
    let scopes = whole_repo();
    let request = MergeRequest {
        commit: &scene.commit,
        scopes: &scopes,
        policy: MergePolicy::Submission,
    };
    let Prepared::Ready(Ready {
        workspace,
        base,
        head,
        ..
    }) = prepare(&ctx, &ports, &request).expect("prepares")
    else {
        unreachable!("the fork's commit is ready to merge");
    };
    plant(scene, workspace.path());
    land(&ctx, &ports, &workspace, &PushUpdate { base, head }).expect("lands")
}

fn merged_head(outcome: &MergeOutcome) -> Sha {
    let MergeOutcome::Merged { head, .. } = outcome else {
        unreachable!("expected a merge: {outcome:?}");
    };
    head.clone()
}

// ---- repo code in the test tree cannot change what land does ----

#[test]
fn a_package_step_that_plants_config_refs_and_attributes_in_its_tree_changes_nothing_land_does() {
    let scene = Scene::new();
    let ports = Ports::new(&scene, true);

    let outcome = merge(&scene, &SystemRunner, &ports);

    let head = merged_head(&outcome);
    assert_eq!(scene.repos.main_head(), head);
    assert_eq!(scene.evil_main(), scene.base.as_str());
    assert!(!scene.receive_pack_ran());
    let tree_config = fs::read_to_string(scene.test_dir.join(".git/config")).expect("tree config");
    assert!(tree_config.contains("pushInsteadOf"), "{tree_config}");
    let config = fs::read_to_string(scene.clone_dir.join(".git/config")).expect("config");
    for planted in ["pushInsteadOf", "pushurl", "receivepack", "include"] {
        assert!(!config.contains(planted), "{planted} reached the workspace");
    }
    let attributes = scene.clone_dir.join(".git/info/attributes");
    assert_eq!(
        fs::read_to_string(attributes).expect("attributes"),
        "* merge=text\n"
    );
    // `land` names the sha and the lease itself and never reads this ref. Whether a push to a URL
    // moves it to the head (git 2.56) or leaves it at main (2.50, 2.55) is git's business; what
    // matters is that it is not the value the step planted in its own tree.
    let tracking = fixture_git(&scene.clone_dir, &["rev-parse", "refs/remotes/origin/main"]);
    let planted_file = scene.test_dir.join(".git/refs/remotes/origin/main");
    let planted = fs::read_to_string(planted_file).expect("planted ref");
    assert_ne!(tracking, planted.trim());
    assert!(
        [scene.base.as_str(), head.as_str()].contains(&tracking.as_str()),
        "{tracking}"
    );
}

#[test]
fn control_a_push_redirect_planted_in_the_workspace_sends_the_push_elsewhere() {
    let scene = Scene::new();

    let outcome = land_after_planting(&scene, redirect_pushes);

    assert!(
        matches!(outcome, MergeOutcome::PushFailed { .. }),
        "{outcome:?}"
    );
    assert_ne!(scene.evil_main(), scene.base.as_str());
    assert_eq!(scene.repos.main_head(), scene.base);
}

#[test]
fn control_an_included_redirect_planted_in_the_workspace_sends_the_push_elsewhere() {
    let scene = Scene::new();

    let outcome = land_after_planting(&scene, include_a_redirect);

    assert!(
        matches!(outcome, MergeOutcome::PushFailed { .. }),
        "{outcome:?}"
    );
    assert_ne!(scene.evil_main(), scene.base.as_str());
}

#[test]
fn control_a_push_url_planted_in_the_workspace_sends_the_push_elsewhere() {
    let scene = Scene::new();

    let outcome = land_after_planting(&scene, set_a_push_url);

    assert!(
        matches!(outcome, MergeOutcome::PushFailed { .. }),
        "{outcome:?}"
    );
    assert_ne!(scene.evil_main(), scene.base.as_str());
    assert_eq!(scene.repos.main_head(), scene.base);
}

#[test]
fn control_a_receive_pack_planted_in_the_workspace_runs_when_land_pushes() {
    let scene = Scene::new();

    land_after_planting(&scene, name_a_receive_pack);

    assert!(scene.receive_pack_ran());
}

#[test]
fn control_objects_wiped_from_the_workspace_fail_the_push() {
    let scene = Scene::new();

    let outcome = land_after_planting(&scene, wipe_objects);

    assert!(
        matches!(outcome, MergeOutcome::PushFailed { .. }),
        "{outcome:?}"
    );
    assert_eq!(scene.repos.main_head(), scene.base);
}

// ---- the test tree ----

#[test]
fn both_package_steps_run_in_one_clone_of_the_head_apart_from_the_workspace() {
    let scene = Scene::new();
    let ports = Ports::new(&scene, false);

    let outcome = merge(&scene, &SystemRunner, &ports);

    let head = merged_head(&outcome);
    assert_eq!(
        *ports.trees.borrow(),
        vec![scene.test_dir.clone(), scene.test_dir.clone()]
    );
    assert!(!scene.test_dir.starts_with(&scene.clone_dir));
    assert!(!scene.clone_dir.starts_with(&scene.test_dir));
    assert_eq!(
        fixture_git(&scene.test_dir, &["rev-parse", "HEAD"]),
        head.as_str()
    );
    assert!(scene.test_dir.join("b.txt").is_file());
}

fn files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            files_under(&path, found);
        } else {
            found.push(path);
        }
    }
}

#[test]
fn the_test_tree_copies_objects_instead_of_linking_them_to_the_workspace() {
    let scene = Scene::new();
    let ports = Ports::new(&scene, false);
    merge(&scene, &SystemRunner, &ports);

    let mut objects = Vec::new();
    files_under(&scene.test_dir.join(".git/objects"), &mut objects);

    assert_ne!(objects.len(), 0);
    for object in objects {
        let links = fs::metadata(&object).expect("metadata").nlink();
        assert_eq!(links, 1, "{} is linked to another file", object.display());
    }
}

#[test]
fn a_test_tree_that_cannot_be_cloned_ends_the_merge_before_any_repo_code_or_write_token() {
    struct FailsTestTreeClone<'a>(&'a Path);

    impl GitRunner for FailsTestTreeClone<'_> {
        fn run(&self, command: &GitCommand) -> Result<GitOutput, RunError> {
            let is_clone = command.argv.get(1).is_some_and(|arg| arg == "clone");
            if is_clone && command.argv.last().is_some_and(|last| last == self.0) {
                return Ok(GitOutput {
                    exit_code: Some(128),
                    stdout: String::new(),
                    stderr: "fatal: no room".to_owned(),
                });
            }
            SystemRunner.run(command)
        }
    }
    let scene = Scene::new();
    let ports = Ports::new(&scene, false);

    let outcome = merge(&scene, &FailsTestTreeClone(&scene.test_dir), &ports);

    let MergeOutcome::GitFailed { result } = &outcome else {
        unreachable!("expected GitFailed: {outcome:?}");
    };
    assert_eq!(result.exit_code, Some(128));
    assert_eq!(*ports.log.borrow(), Vec::<String>::new());
    assert_eq!(scene.repos.main_head(), scene.base);
}

#[test]
fn a_trial_runs_its_package_steps_in_its_own_clone_which_nothing_lands_from() {
    let scene = Scene::new();
    let ports = Ports::new(&scene, false);
    let sources = scene.sources();
    let ctx = TrialContext {
        git: &scene.git,
        runner: &SystemRunner,
        sources: &sources,
        clone_dir: &scene.clone_dir,
    };

    let outcome = run_trial(&ctx, &ports, &scene.base, &scene.commit).expect("trial finishes");

    assert!(matches!(outcome, TrialOutcome::Clean { .. }), "{outcome:?}");
    assert_eq!(
        *ports.trees.borrow(),
        vec![scene.clone_dir.clone(), scene.clone_dir.clone()]
    );
    assert_eq!(scene.repos.main_head(), scene.base);
}

// ---- clone_test_tree on its own ----

struct Scripted {
    clone_exit: i32,
    head_exit: i32,
    head_stdout: String,
}

impl GitRunner for Scripted {
    fn run(&self, command: &GitCommand) -> Result<GitOutput, RunError> {
        let is_clone = command.argv.get(1).is_some_and(|arg| arg == "clone");
        let (exit, stdout) = if is_clone {
            (self.clone_exit, String::new())
        } else {
            (self.head_exit, self.head_stdout.clone())
        };
        Ok(GitOutput {
            exit_code: Some(exit),
            stdout,
            stderr: "fatal: scripted".to_owned(),
        })
    }
}

fn sha(character: char) -> Sha {
    Sha::parse(&character.to_string().repeat(40)).expect("test sha")
}

fn lander_git() -> Git {
    Git::new("/lander/home".into(), "/lander/hooks".into())
}

#[test]
fn a_test_tree_inside_the_workspace_is_refused_before_git_runs() {
    let workspace = workspace_at("/ws");

    let error = clone_test_tree(
        &lander_git(),
        &support::AlwaysOk,
        &workspace,
        &sha('1'),
        Path::new("/ws/tree"),
    )
    .expect_err("refused");

    assert!(
        matches!(error, CloneError::TestTreeInWorkspace { .. }),
        "{error}"
    );
}

#[test]
fn a_test_tree_destination_that_is_in_use_is_refused() {
    let workspace = workspace_at("/ws");
    let scratch = TempDir::new().expect("tempdir");
    fs::write(scratch.path().join("file"), "x").expect("write");

    let error = clone_test_tree(
        &lander_git(),
        &support::AlwaysOk,
        &workspace,
        &sha('1'),
        scratch.path(),
    )
    .expect_err("refused");

    assert!(
        matches!(error, CloneError::DestinationInUse { .. }),
        "{error}"
    );
}

fn scripted_tree(runner: &Scripted) -> Result<TestTreeOutcome, CloneError> {
    let scratch = TempDir::new().expect("tempdir");
    clone_test_tree(
        &lander_git(),
        runner,
        &workspace_at("/ws"),
        &sha('1'),
        &scratch.path().join("tree"),
    )
}

#[test]
fn a_tree_at_another_commit_than_the_head_is_an_error() {
    let runner = Scripted {
        clone_exit: 0,
        head_exit: 0,
        head_stdout: format!("{}\n", sha('9')),
    };

    let error = scripted_tree(&runner).expect_err("not at the head");

    assert!(
        matches!(error, CloneError::TestTreeNotAtHead { .. }),
        "{error}"
    );
}

#[test]
fn a_failed_clone_or_head_read_is_a_failed_outcome_with_the_git_output() {
    let failing_clone = Scripted {
        clone_exit: 128,
        head_exit: 0,
        head_stdout: String::new(),
    };
    let failing_head = Scripted {
        clone_exit: 0,
        head_exit: 128,
        head_stdout: String::new(),
    };
    for runner in [failing_clone, failing_head] {
        match scripted_tree(&runner).expect("runs") {
            TestTreeOutcome::Failed(output) => assert_eq!(output.exit_code, Some(128)),
            TestTreeOutcome::Cloned(tree) => unreachable!("cloned {}", tree.path().display()),
        }
    }
}

#[test]
fn a_test_tree_below_a_symlink_to_the_workspace_is_refused() {
    let scratch = TempDir::new().expect("tempdir");
    let workspace_dir = scratch.path().join("ws");
    fs::create_dir(&workspace_dir).expect("workspace dir");
    let workspace = workspace_at(&workspace_dir.display().to_string());
    let link = scratch.path().join("link");
    std::os::unix::fs::symlink(&workspace_dir, &link).expect("symlink");

    let error = clone_test_tree(
        &lander_git(),
        &support::AlwaysOk,
        &workspace,
        &sha('1'),
        &link.join("tree"),
    )
    .expect_err("refused");

    assert!(
        matches!(error, CloneError::TestTreeInWorkspace { .. }),
        "{error}"
    );
}

#[test]
fn a_head_read_that_failed_is_a_failed_outcome_even_when_it_printed_the_head() {
    let runner = Scripted {
        clone_exit: 0,
        head_exit: 128,
        head_stdout: format!("{}\n", sha('1')),
    };

    match scripted_tree(&runner).expect("runs") {
        TestTreeOutcome::Failed(output) => assert_eq!(output.exit_code, Some(128)),
        TestTreeOutcome::Cloned(tree) => unreachable!("cloned {}", tree.path().display()),
    }
}

//! Requirement S3 (`docs/proposals/real-use.md`): a fork that carries hooks, `.gitattributes`
//! filters and drivers, a malformed pack or a file name with control characters is refused or
//! inert. Every test runs the lander's own `prepare` with real git against bare fixture
//! repositories.
//!
//! Each test that depends on a part of the fixed git environment has a control: the same
//! scenario with that one setting stripped from every command, where the planted thing runs or
//! the bad fork gets through. A control that stops showing that would mean the test beside it no
//! longer proves anything.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

mod support;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tessel_lander::coverage::{parse_name_status, NameStatusError};
use tessel_lander::git::{Git, GitCommand};
use tessel_lander::merge::{
    prepare, MergeContext, MergeError, MergePolicy, MergeRequest, Prepared,
};
use tessel_lander::outcome::MergeOutcome;
use tessel_lander::run::{run, GitOutput, GitRunner, RunError};
use tessel_lander::types::Sha;
use tessel_lander::workspace::CloneError;

use support::repos::{file_scope, whole_repo, LocalPorts, Repos};
use support::{file_names, fixture_git, write_script};

/// Strips one setting from every command it runs: an environment variable, or an entry of the
/// `GIT_CONFIG_COUNT` list by its key. After a successful clone it plants `hooks` into the
/// clone's own `.git/hooks`, which is where a hook from git's template directory would sit, and
/// runs `after_clone` on the clone (before the lander has written to it) and `after_fetch` on
/// the workspace once the fork is fetched (after).
#[derive(Default)]
struct Tamper<'a> {
    without: Option<&'a str>,
    hooks: Option<&'a Path>,
    after_clone: Option<fn(&Path)>,
    after_fetch: Option<fn(&Path)>,
}

fn without_setting(command: &GitCommand, setting: &str) -> GitCommand {
    let mut stripped = command.clone();
    let mut removed = stripped.env.remove(setting).is_some();
    let count: usize = stripped.env["GIT_CONFIG_COUNT"]
        .to_str()
        .and_then(|count| count.parse().ok())
        .expect("a count");
    let mut kept = Vec::new();
    for index in 0..count {
        let key = stripped.env.remove(&format!("GIT_CONFIG_KEY_{index}"));
        let value = stripped.env.remove(&format!("GIT_CONFIG_VALUE_{index}"));
        let (key, value) = (key.expect("key"), value.expect("value"));
        if key == setting {
            removed = true;
        } else {
            kept.push((key, value));
        }
    }
    assert!(removed, "{setting} is not in the fixed environment");
    stripped
        .env
        .insert("GIT_CONFIG_COUNT".to_owned(), kept.len().to_string().into());
    for (index, (key, value)) in kept.into_iter().enumerate() {
        stripped.env.insert(format!("GIT_CONFIG_KEY_{index}"), key);
        stripped
            .env
            .insert(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    stripped
}

fn delete_attributes(clone: &Path) {
    fs::remove_file(clone.join(".git/info/attributes")).expect("the lander wrote it");
}

fn block_attributes(clone: &Path) {
    let info = clone.join(".git/info");
    fs::remove_dir_all(&info).expect("git made info");
    fs::write(&info, "a file where a directory goes").expect("block info");
}

const HOOKS: [&str; 4] = [
    "pre-rebase",
    "post-checkout",
    "post-rewrite",
    "reference-transaction",
];

fn plant_hooks(clone: &Path, markers: &Path) {
    let dir = clone.join(".git").join("hooks");
    fs::create_dir_all(&dir).expect("hooks dir");
    for name in HOOKS {
        let marker = markers.join(format!("hook-{name}"));
        write_script(&dir.join(name), &format!("touch '{}'", marker.display()));
    }
}

impl GitRunner for Tamper<'_> {
    fn run(&self, command: &GitCommand) -> Result<GitOutput, RunError> {
        let output = match self.without {
            Some(setting) => run(&without_setting(command, setting))?,
            None => run(command)?,
        };
        let is_clone = command.argv.get(1).is_some_and(|arg| arg == "clone");
        if is_clone && output.success() {
            let clone = Path::new(command.argv.last().expect("destination"));
            if let Some(markers) = self.hooks {
                plant_hooks(clone, markers);
            }
            if let Some(after_clone) = self.after_clone {
                after_clone(clone);
            }
        }
        let is_fetch = command.argv.get(3).is_some_and(|arg| arg == "fetch");
        if let (Some(after_fetch), true, true) = (self.after_fetch, is_fetch, output.success()) {
            after_fetch(Path::new(&command.argv[2]));
        }
        Ok(output)
    }
}

/// Bare main and fork, a lander environment of its own whose home the test can write to, and a
/// directory for the markers that planted code leaves when it runs.
struct Scene {
    repos: Repos,
    git: Git,
    markers: PathBuf,
}

impl Scene {
    fn new() -> Scene {
        let repos = Repos::new();
        let git = Git::create_in(&repos.scratch.path().join("isolated")).expect("environment");
        let markers = repos.scratch.path().join("markers");
        fs::create_dir(&markers).expect("markers dir");
        Scene {
            repos,
            git,
            markers,
        }
    }

    /// The config a host's own account might carry, in the `HOME` the lander runs git with: git
    /// reads it unless `GIT_CONFIG_GLOBAL` points elsewhere.
    fn plant_home_config(&self) {
        let file = self.repos.scratch.path().join("isolated/home/.gitconfig");
        let file = file.display().to_string();
        let touch = |name: &str| format!("touch '{}'", self.markers.join(name).display());
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
        ] {
            fixture_git(
                self.repos.scratch.path(),
                &["config", "--file", &file, key, &value],
            );
        }
    }

    fn ran(&self) -> Vec<String> {
        file_names(&self.markers)
    }

    fn prepare_with(&self, runner: &dyn GitRunner, commit: &Sha, name: &str) -> Prepared {
        self.try_prepare_with(runner, commit, name)
            .expect("prepare finishes")
    }

    fn try_prepare_with(
        &self,
        runner: &dyn GitRunner,
        commit: &Sha,
        name: &str,
    ) -> Result<Prepared, MergeError> {
        let sources = self.repos.sources();
        let pinned = self.repos.main_head();
        let clone_dir = self.repos.scratch.path().join(name);
        let ctx = MergeContext {
            git: &self.git,
            runner,
            sources: &sources,
            clone_dir: &clone_dir,
            pinned_main: &pinned,
        };
        let scopes = whole_repo();
        let request = MergeRequest {
            commit,
            scopes: &scopes,
            policy: MergePolicy::Submission,
        };
        prepare(&ctx, &LocalPorts::new(&self.repos), &request)
    }

    fn prepare(&self, commit: &Sha) -> Prepared {
        self.prepare_with(&Tamper::default(), commit, "workspace")
    }
}

fn commit_executable(repo: &Path, name: &str, body: &str) {
    write_script(&repo.join(name), body);
    fixture_git(repo, &["add", "--", name]);
    fixture_git(repo, &["commit", "--quiet", "-m", &format!("add {name}")]);
}

/// The fork's `commit` and main each moved on from the shared base, so the rebase has work to do.
fn fork_and_main_diverge(repos: &Repos, fork: impl FnOnce(&Repos) -> Sha) -> Sha {
    let base = repos.head();
    let forked = fork(repos);
    repos.push(&repos.fork);
    repos.reset_to(base.as_str());
    repos.commit("main-only.txt", "main moved on\n");
    repos.push(&repos.main);
    forked
}

fn ready(prepared: Prepared) -> tessel_lander::merge::Ready {
    match prepared {
        Prepared::Ready(ready) => ready,
        Prepared::Done(outcome) => unreachable!("expected a ready merge: {outcome:?}"),
    }
}

fn done(prepared: Prepared) -> MergeOutcome {
    match prepared {
        Prepared::Done(outcome) => outcome,
        Prepared::Ready(ready) => unreachable!("expected an outcome: {ready:?}"),
    }
}

// ---- hooks ----

/// The fork commits files named and shaped like hooks of every common kind.
fn fork_with_committed_hooks(scene: &Scene) -> Sha {
    let marker = |name: &str| scene.markers.join(format!("committed-{name}"));
    fork_and_main_diverge(&scene.repos, |repos| {
        for (path, name) in [
            (".githooks/pre-rebase", "githooks"),
            ("hooks/post-checkout", "hooks"),
            (".husky/pre-commit", "husky"),
        ] {
            let body = format!("touch '{}'", marker(name).display());
            commit_executable(&repos.work, path, &body);
        }
        repos.head()
    })
}

#[test]
fn hooks_in_the_clones_own_hooks_directory_and_committed_in_the_fork_do_not_run() {
    let scene = Scene::new();
    let forked = fork_with_committed_hooks(&scene);
    let runner = Tamper {
        without: None,
        hooks: Some(&scene.markers),
        ..Tamper::default()
    };

    let prepared = scene.prepare_with(&runner, &forked, "workspace");

    // Inert: nothing runs and the merge goes on. `core.hooksPath` names an empty directory the
    // lander owns, so the hooks planted in the clone's `.git/hooks` are never looked up, and a
    // committed `.githooks/` or `hooks/` is only a file: no config the fork can set names it.
    ready(prepared);
    assert_eq!(scene.ran(), Vec::<String>::new());
}

#[test]
fn control_without_core_hooks_path_the_hooks_in_the_clone_run() {
    let scene = Scene::new();
    let forked = fork_with_committed_hooks(&scene);
    let runner = Tamper {
        without: Some("core.hooksPath"),
        hooks: Some(&scene.markers),
        ..Tamper::default()
    };

    ready(scene.prepare_with(&runner, &forked, "workspace"));

    let ran = scene.ran();
    for hook in [
        "hook-pre-rebase",
        "hook-post-checkout",
        "hook-reference-transaction",
    ] {
        assert!(
            ran.contains(&hook.to_owned()),
            "{hook} did not run: {ran:?}"
        );
    }
    assert!(
        !ran.iter().any(|name| name.starts_with("committed-")),
        "a committed file ran: {ran:?}"
    );
}

// ---- .gitattributes filters and drivers ----

/// The fork commits `.gitattributes` that names the filter, merge driver and diff driver `evil`
/// for every file, a new file to check out through them, and an edit of `a.txt`; main edits
/// `a.txt` as well when `conflict` is set.
fn fork_with_attributes(scene: &Scene, conflict: bool) -> Sha {
    let repos = &scene.repos;
    let base = repos.head();
    repos.commit(".gitattributes", "* filter=evil merge=evil diff=evil\n");
    repos.commit("new.txt", "fork file\n");
    let forked = repos.commit("a.txt", "one\nFORK\nthree\n");
    repos.push(&repos.fork);
    repos.reset_to(base.as_str());
    if conflict {
        repos.commit("a.txt", "one\nMAIN\nthree\n");
    } else {
        repos.commit("main-only.txt", "main moved on\n");
    }
    repos.push(&repos.main);
    forked
}

#[test]
fn filters_and_drivers_a_forks_gitattributes_names_are_not_defined_so_nothing_runs_in_a_conflict() {
    let scene = Scene::new();
    scene.plant_home_config();
    let forked = fork_with_attributes(&scene, true);

    let outcome = done(scene.prepare(&forked));

    // Inert: the attributes name `evil`, and only a config file can define a driver of that
    // name. The fork's config is never fetched, the clone's was written by git, and the host's
    // home config is off (`GIT_CONFIG_GLOBAL=/dev/null`), so `evil` has no command to run. The
    // built-in drivers need no config; the tests on `info/attributes` cover those.
    assert!(matches!(outcome, MergeOutcome::Conflict { ref files, .. } if *files == ["a.txt"]));
    assert_eq!(scene.ran(), Vec::<String>::new());
}

#[test]
fn control_with_the_home_config_readable_the_filter_and_the_merge_driver_run() {
    let scene = Scene::new();
    scene.plant_home_config();
    let forked = fork_with_attributes(&scene, true);
    let runner = Tamper {
        without: Some("GIT_CONFIG_GLOBAL"),
        hooks: None,
        after_fetch: Some(delete_attributes),
        ..Tamper::default()
    };

    let outcome = done(scene.prepare_with(&runner, &forked, "workspace"));

    assert!(
        matches!(outcome, MergeOutcome::Conflict { .. }),
        "{outcome:?}"
    );
    let ran = scene.ran();
    assert!(ran.contains(&"merge-driver".to_owned()), "{ran:?}");
    assert!(
        ran.iter().any(|name| name.starts_with("filter-")),
        "{ran:?}"
    );
}

#[test]
fn filters_and_diff_programs_a_forks_gitattributes_names_do_not_run_when_the_change_is_clean() {
    let scene = Scene::new();
    scene.plant_home_config();
    let forked = fork_with_attributes(&scene, false);

    let ready = ready(scene.prepare(&forked));
    let diff = run(&scene
        .git
        .diff_command(&ready.workspace, &ready.base, &ready.head))
    .expect("diff runs");

    assert!(diff.success(), "{}", diff.stderr);
    assert!(diff.stdout.contains("+FORK"), "{}", diff.stdout);
    assert_eq!(scene.ran(), Vec::<String>::new());
}

#[test]
fn a_host_wide_textconv_and_external_diff_do_not_run_in_the_diff_a_person_reads() {
    let scene = Scene::new();
    scene.plant_home_config();
    let forked = fork_with_attributes(&scene, false);
    let ready = ready(scene.prepare(&forked));
    let diff = scene
        .git
        .diff_command(&ready.workspace, &ready.base, &ready.head);

    // Here the home config is readable, so \`--no-ext-diff\` and \`--no-textconv\` are the only
    // defence: the fork's \`diff=evil\` attribute reaches a textconv, and \`diff.external\` is
    // set for every file.
    let output = run(&without_setting(&diff, "GIT_CONFIG_GLOBAL")).expect("diff runs");

    assert!(output.success(), "{}", output.stderr);
    assert!(output.stdout.contains("+FORK"), "{}", output.stdout);
    assert_eq!(scene.ran(), Vec::<String>::new());
}

#[test]
fn control_with_the_home_config_readable_the_filter_runs_on_a_clean_change() {
    let scene = Scene::new();
    scene.plant_home_config();
    let forked = fork_with_attributes(&scene, false);
    let runner = Tamper {
        without: Some("GIT_CONFIG_GLOBAL"),
        hooks: None,
        ..Tamper::default()
    };

    ready(scene.prepare_with(&runner, &forked, "workspace"));

    let ran = scene.ran();
    assert!(
        ran.iter().any(|name| name.starts_with("filter-")),
        "{ran:?}"
    );
}

/// The fork's `.gitattributes` names git's built-in `driver` for every file, and a commit after it
/// edits `a.txt` where main does too.
fn fork_with_builtin_driver(scene: &Scene, driver: &str) -> Sha {
    let repos = &scene.repos;
    let base = repos.head();
    repos.commit(".gitattributes", &format!("* merge={driver}\n"));
    let forked = repos.commit("a.txt", "one\nFORK\nthree\n");
    repos.push(&repos.fork);
    repos.reset_to(base.as_str());
    repos.commit("a.txt", "one\nMAIN\nthree\n");
    repos.push(&repos.main);
    forked
}

#[test]
fn a_fork_naming_a_builtin_merge_driver_cannot_turn_a_conflict_into_a_merge() {
    for driver in ["union", "binary"] {
        let scene = Scene::new();
        let forked = fork_with_builtin_driver(&scene, driver);
        let main_before = scene.repos.main_head();

        let outcome = done(scene.prepare(&forked));

        // Inert: `info/attributes` in the clone sets `merge=text` for every path and outranks
        // the tree's `.gitattributes`, so the conflict stops the rebase.
        assert!(
            matches!(outcome, MergeOutcome::Conflict { ref files, .. } if *files == ["a.txt"]),
            "{driver}: {outcome:?}"
        );
        assert_eq!(scene.repos.main_head(), main_before);
    }
}

#[test]
fn control_without_info_attributes_the_union_driver_splices_both_sides() {
    let scene = Scene::new();
    let forked = fork_with_builtin_driver(&scene, "union");
    let runner = Tamper {
        after_fetch: Some(delete_attributes),
        ..Tamper::default()
    };

    let ready = ready(scene.prepare_with(&runner, &forked, "workspace"));

    let spliced = fs::read_to_string(ready.workspace.path().join("a.txt")).expect("a.txt");
    assert!(
        spliced.contains("MAIN") && spliced.contains("FORK"),
        "{spliced}"
    );
}

#[test]
fn a_clone_whose_info_attributes_cannot_be_written_is_an_error_not_a_workspace() {
    let scene = Scene::new();
    let forked = fork_with_builtin_driver(&scene, "union");
    let runner = Tamper {
        after_clone: Some(block_attributes),
        ..Tamper::default()
    };

    let error = scene
        .try_prepare_with(&runner, &forked, "workspace")
        .expect_err("no workspace without the attributes");

    assert!(
        matches!(error, MergeError::Clone(CloneError::WriteAttributes { .. })),
        "{error}"
    );
}

// ---- malformed packs ----

fn git_with_stdin(repo: &Path, args: &[&str], stdin: &str) -> String {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("git runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("git ends");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_owned()
}

/// A commit on `HEAD` whose tree also holds `.git/hooks/post-checkout`. Git's own commands refuse
/// to make that tree, so it is built object by object.
fn commit_with_a_dot_git_directory(repo: &Path) -> Sha {
    let hook = git_with_stdin(
        repo,
        &["hash-object", "-w", "--stdin"],
        "#!/bin/sh\ntouch hooked\n",
    );
    let hooks = git_with_stdin(
        repo,
        &["mktree"],
        &format!("100755 blob {hook}\tpost-checkout\n"),
    );
    let dot_git = git_with_stdin(repo, &["mktree"], &format!("040000 tree {hooks}\thooks\n"));
    let listing = fixture_git(repo, &["ls-tree", "HEAD"]);
    let tree = git_with_stdin(
        repo,
        &["mktree"],
        &format!("{listing}\n040000 tree {dot_git}\t.git\n"),
    );
    let commit = fixture_git(
        repo,
        &["commit-tree", &tree, "-p", "HEAD", "-m", "plant a hook"],
    );
    Sha::parse(&commit).expect("sha")
}

fn fork_with_a_dot_git_directory(scene: &Scene) -> Sha {
    let repos = &scene.repos;
    let planted = commit_with_a_dot_git_directory(&repos.work);
    let fork = repos.fork.display().to_string();
    let refspec = format!("{planted}:refs/heads/main");
    repos.work_git(&["push", "--quiet", "--force", &fork, &refspec]);
    planted
}

#[test]
fn a_fork_whose_pack_holds_a_dot_git_tree_entry_is_refused_at_the_fetch() {
    let scene = Scene::new();
    let planted = fork_with_a_dot_git_directory(&scene);
    let main_before = scene.repos.main_head();

    let outcome = done(scene.prepare(&planted));

    // Refused: `transfer.fsckObjects` makes the fetch run `index-pack --fsck-objects`, which
    // rejects the tree entry, so the outcome is `Clone` with git's quoted output and the fork's
    // objects never reach the clone.
    let MergeOutcome::Clone { result } = outcome else {
        unreachable!("{outcome:?}")
    };
    assert!(!result.success());
    assert!(result.stderr.contains("hasDotgit"), "{}", result.stderr);
    assert_eq!(scene.repos.main_head(), main_before);
}

#[test]
fn control_without_transfer_fsck_objects_the_dot_git_tree_entry_is_fetched() {
    let scene = Scene::new();
    let planted = fork_with_a_dot_git_directory(&scene);
    let runner = Tamper {
        without: Some("transfer.fsckObjects"),
        hooks: None,
        ..Tamper::default()
    };

    let outcome = scene.prepare_with(&runner, &planted, "workspace");

    let reached_the_clone = match &outcome {
        Prepared::Ready(_) => true,
        Prepared::Done(outcome) => !matches!(outcome, MergeOutcome::Clone { .. }),
    };
    assert!(reached_the_clone, "{outcome:?}");
}

/// A `.gitmodules` whose submodule url starts with `-`, which a later `git submodule` would read
/// as an option. It is an ordinary file to every check but fsck's `gitmodulesUrl`.
fn fork_with_an_option_as_a_submodule_url(scene: &Scene) -> Sha {
    let repos = &scene.repos;
    let forked = repos.commit(
        ".gitmodules",
        "[submodule \"x\"]\n\tpath = x\n\turl = --upload-pack=touch-a-file\n",
    );
    repos.push(&repos.fork);
    repos.reset_to("HEAD~1");
    forked
}

#[test]
fn a_fork_whose_gitmodules_url_is_an_option_is_refused_by_fsck_and_only_by_fsck() {
    let scene = Scene::new();
    let forked = fork_with_an_option_as_a_submodule_url(&scene);

    let outcome = done(scene.prepare(&forked));

    let MergeOutcome::Clone { result } = outcome else {
        unreachable!("{outcome:?}")
    };
    assert!(result.stderr.contains("gitmodulesUrl"), "{}", result.stderr);
}

#[test]
fn control_without_transfer_fsck_objects_the_option_url_gitmodules_is_fetched() {
    let scene = Scene::new();
    let forked = fork_with_an_option_as_a_submodule_url(&scene);
    let runner = Tamper {
        without: Some("transfer.fsckObjects"),
        ..Tamper::default()
    };

    let prepared = scene.prepare_with(&runner, &forked, "workspace");

    ready(prepared);
}

#[test]
fn a_fork_with_a_corrupt_object_is_refused_by_git_whatever_the_settings() {
    let scene = Scene::new();
    let forked = scene.repos.commit("b.txt", "fork work\n");
    scene.repos.push(&scene.repos.fork);
    let blob = fixture_git(
        &scene.repos.fork,
        &["rev-parse", &format!("{forked}:b.txt")],
    );
    let object = scene
        .repos
        .fork
        .join("objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    fs::remove_file(&object).expect("loose object");
    fs::write(&object, b"not a zlib stream").expect("corrupt it");
    scene.repos.reset_to("HEAD~1");

    let outcome = done(scene.prepare(&forked));

    // Refused, but not by a setting: git cannot read the object, so the fetch fails whether or
    // not fsck is on. This pins that a corrupt fork is `Clone`, not a panic or a merge.
    assert!(matches!(outcome, MergeOutcome::Clone { .. }), "{outcome:?}");
}

// ---- file names with control characters ----

const CONTROL_NAME: &str = "ctl\u{1b}[31m\r\u{7}\tname\n.txt";

fn control_name_fork(repos: &Repos) -> Sha {
    let forked = repos.commit(CONTROL_NAME, "fork\n");
    repos.push(&repos.fork);
    repos.reset_to("HEAD~1");
    forked
}

#[test]
fn a_file_name_with_control_characters_is_reported_whole_as_data() {
    let scene = Scene::new();
    let forked = control_name_fork(&scene.repos);
    let ports = LocalPorts::new(&scene.repos);

    let refused = scene
        .repos
        .merge(&ports, &forked, &[], MergePolicy::Submission)
        .expect("the merge finishes");
    let covered = scene
        .repos
        .merge(
            &ports,
            &forked,
            &[file_scope(
                CONTROL_NAME,
                tessel_coordinator::protocol::Mode::Create,
            )],
            MergePolicy::Submission,
        )
        .expect("the merge finishes");

    // Inert: `-z` keeps git from quoting the name, so the lander compares and returns exactly
    // the bytes of the name. It never puts the name in a command, and its errors print it with
    // `{:?}` (next test), so nothing reaches a terminal raw from here.
    assert!(
        matches!(
            refused,
            MergeOutcome::Uncovered { ref files, total: 1, .. } if *files == [CONTROL_NAME]
        ),
        "{refused:?}"
    );
    assert!(
        matches!(covered, MergeOutcome::Merged { .. }),
        "{covered:?}"
    );
}

#[test]
fn a_conflict_on_a_file_with_control_characters_in_its_name_names_it_whole() {
    let scene = Scene::new();
    let repos = &scene.repos;
    let base = repos.head();
    let forked = repos.commit(CONTROL_NAME, "fork\n");
    repos.push(&repos.fork);
    repos.reset_to(base.as_str());
    repos.commit(CONTROL_NAME, "main\n");
    repos.push(&repos.main);

    let outcome = done(scene.prepare(&forked));

    assert!(
        matches!(outcome, MergeOutcome::Conflict { ref files, .. } if *files == [CONTROL_NAME]),
        "{outcome:?}"
    );
}

#[test]
fn an_error_that_quotes_a_name_status_record_shows_control_characters_escaped() {
    let record = format!("X\0{CONTROL_NAME}\0");

    let error = parse_name_status(&record).expect_err("X is not a status");

    let NameStatusError::UnknownStatus { .. } = error else {
        unreachable!("{error:?}")
    };
    let shown = error.to_string();
    assert!(!shown.chars().any(char::is_control), "{shown:?}");
    assert!(shown.contains("\\u{1b}"), "{shown:?}");
}

//! Port of `tessel-steward/src/merge-trial.test.ts` (`runTrial`): 16 `it`, no `it.each`. A fake git
//! runner and fake ports stand in for the sandbox and the platform; the real git is in
//! `trial_git.rs`. The title of each test is the TS title in snake case, so its original is
//! recognisable.
//!
//! The TS test "has no way to push or to mint a write token in its dependencies" asserted the
//! keys of an object. The Rust equivalent is a compile-time fact, so it is the two `compile_fail`
//! doctests on `TrialPorts` (with `merge-trial-types.test.ts`, whose `@ts-expect-error` lines
//! prove the same). That makes 15 tests here and 2 doctests.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use tessel_lander::git::{Git, GitCommand, MergeSources};
use tessel_lander::merge::{BoundaryError, MergeError};
use tessel_lander::outcome::{PackageStep, StepOutcome, TrialOutcome};
use tessel_lander::run::{GitOutput, GitRunner, RunError};
use tessel_lander::trial::{run_trial, TrialContext, TrialPorts};
use tessel_lander::types::Sha;
use tessel_lander::workspace::TestTree;

fn sha(character: char) -> Sha {
    Sha::parse(&character.to_string().repeat(40)).expect("test sha")
}

fn main_sha() -> Sha {
    sha('6')
}
fn merge_base() -> Sha {
    sha('2')
}
fn commit() -> Sha {
    sha('3')
}
fn head() -> Sha {
    sha('4')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GitStep {
    Clone,
    Fetch,
    OnMain,
    Exists,
    Reachable,
    MergeBase,
    Rebase,
    Conflicts,
    Head,
}

impl GitStep {
    fn label(self) -> &'static str {
        match self {
            GitStep::Clone => "clone",
            GitStep::Fetch => "fetch",
            GitStep::OnMain => "onmain",
            GitStep::Exists => "exists",
            GitStep::Reachable => "reachable",
            GitStep::MergeBase => "mergebase",
            GitStep::Rebase => "rebase",
            GitStep::Conflicts => "conflicts",
            GitStep::Head => "head",
        }
    }
}

/// Which step a command is, by its argv text, in the order the steward's test matches them.
fn git_step(command: &GitCommand) -> GitStep {
    let text = command
        .argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let matches = [
        (" clone ", GitStep::Clone),
        (" fetch ", GitStep::Fetch),
        ("refs/remotes/origin/main", GitStep::OnMain),
        ("--quiet", GitStep::Exists),
        ("--is-ancestor", GitStep::Reachable),
        (" merge-base ", GitStep::MergeBase),
        (" rebase ", GitStep::Rebase),
        ("--diff-filter=U", GitStep::Conflicts),
        ("HEAD^{commit}", GitStep::Head),
    ];
    matches
        .into_iter()
        .find(|(needle, _)| text.contains(needle))
        .map_or_else(
            || unreachable!("unexpected command {text}"),
            |(_, step)| step,
        )
}

#[derive(Clone)]
struct Reply {
    exit: i32,
    stdout: String,
}

fn exit(exit: i32) -> Reply {
    out(exit, "")
}

fn out(exit: i32, stdout: &str) -> Reply {
    Reply {
        exit,
        stdout: stdout.to_owned(),
    }
}

fn default_reply(step: GitStep) -> Reply {
    match step {
        GitStep::Exists => out(0, &format!("{}\n", commit())),
        GitStep::MergeBase => out(0, &format!("{}\n", merge_base())),
        GitStep::Head => out(0, &format!("{}\n", head())),
        GitStep::Clone
        | GitStep::Fetch
        | GitStep::OnMain
        | GitStep::Reachable
        | GitStep::Rebase
        | GitStep::Conflicts => exit(0),
    }
}

/// The fake runner and the fake ports in one value, so they share one event log.
struct Harness {
    git: HashMap<GitStep, Reply>,
    install: i32,
    test: i32,
    revoked_reads: bool,
    events: RefCell<Vec<String>>,
    commands: RefCell<Vec<GitCommand>>,
}

impl Harness {
    fn new() -> Harness {
        Harness {
            git: HashMap::new(),
            install: 0,
            test: 0,
            revoked_reads: true,
            events: RefCell::new(Vec::new()),
            commands: RefCell::new(Vec::new()),
        }
    }

    fn git(mut self, step: GitStep, reply: Reply) -> Harness {
        self.git.insert(step, reply);
        self
    }

    fn trial(&self) -> Result<TrialOutcome, MergeError> {
        let (git, sources) = (
            Git::new("/lander/home".into(), "/lander/hooks".into()),
            MergeSources {
                main_remote: "https://git.example/demo.git".to_owned(),
                fork_remote: "https://git.example/demo--a1.git".to_owned(),
                fork_branch: "main".to_owned(),
            },
        );
        let ctx = TrialContext {
            git: &git,
            runner: self,
            sources: &sources,
            clone_dir: Path::new("/workspace"),
        };
        run_trial(&ctx, self, &main_sha(), &commit())
    }

    fn outcome(&self) -> TrialOutcome {
        self.trial().expect("the trial finishes")
    }

    fn events(&self) -> Vec<String> {
        self.events.borrow().clone()
    }

    fn has_event(&self, event: &str) -> bool {
        self.events.borrow().iter().any(|e| e == event)
    }

    fn ran_package_step(&self) -> bool {
        self.events
            .borrow()
            .iter()
            .any(|e| e.starts_with("package:"))
    }

    fn argv(&self, step: GitStep) -> Vec<String> {
        self.commands
            .borrow()
            .iter()
            .find(|c| git_step(c) == step)
            .unwrap_or_else(|| unreachable!("no {} command ran", step.label()))
            .argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }
}

impl GitRunner for Harness {
    fn run(&self, command: &GitCommand) -> Result<GitOutput, RunError> {
        let step = git_step(command);
        self.events
            .borrow_mut()
            .push(format!("git:{}", step.label()));
        self.commands.borrow_mut().push(command.clone());
        let reply = self
            .git
            .get(&step)
            .cloned()
            .unwrap_or_else(|| default_reply(step));
        Ok(GitOutput {
            exit_code: Some(reply.exit),
            stdout: reply.stdout,
            stderr: format!("{} err", step.label()),
        })
    }
}

impl TrialPorts for Harness {
    fn revoke_read_access(&self) -> bool {
        self.events.borrow_mut().push("revoke-reads".to_owned());
        self.revoked_reads
    }

    fn run_package_step(
        &self,
        step: PackageStep,
        _tree: &TestTree,
    ) -> Result<StepOutcome, BoundaryError> {
        let (label, exit_code) = match step {
            PackageStep::Install => ("install", self.install),
            PackageStep::Test => ("test", self.test),
        };
        self.events.borrow_mut().push(format!("package:{label}"));
        Ok(StepOutcome::finished(
            step,
            exit_code,
            &format!("{label} out"),
            "",
        ))
    }
}

fn clean() -> TrialOutcome {
    TrialOutcome::Clean {
        base: main_sha(),
        head: head(),
        commit: commit(),
    }
}

#[test]
fn fetches_revokes_the_reads_pins_main_verifies_rebases_onto_the_given_main_then_tests() {
    let harness = Harness::new();

    assert_eq!(harness.outcome(), clean());

    assert_eq!(
        harness.events(),
        [
            "git:clone",
            "git:fetch",
            "revoke-reads",
            "git:onmain",
            "git:exists",
            "git:reachable",
            "git:mergebase",
            "git:rebase",
            "git:head",
            "package:install",
            "package:test",
        ]
    );
}

#[test]
fn rebases_the_commit_onto_the_main_it_was_given_not_onto_the_main_it_cloned() {
    let harness = Harness::new();
    harness.outcome();

    let rebase = harness.argv(GitStep::Rebase);
    assert_eq!(
        rebase[rebase.len() - 4..],
        [
            "--onto",
            main_sha().as_str(),
            merge_base().as_str(),
            commit().as_str()
        ]
    );
    assert!(harness
        .argv(GitStep::OnMain)
        .contains(&main_sha().to_string()));
}

#[test]
fn never_runs_a_push_whatever_the_tests_say() {
    for test in [0, 1, 124] {
        let mut harness = Harness::new();
        harness.test = test;
        harness.outcome();

        let words: Vec<String> = harness
            .commands
            .borrow()
            .iter()
            .flat_map(|c| c.argv.iter().map(|a| a.to_string_lossy().into_owned()))
            .collect();
        assert!(!words.iter().any(|w| w == "push"), "{words:?}");
        assert!(!words.iter().any(|w| w.starts_with("--force-with-lease")));
    }
}

#[test]
fn is_clean_when_the_rebased_commit_passes_the_tests() {
    assert!(matches!(
        Harness::new().outcome(),
        TrialOutcome::Clean { .. }
    ));
}

#[test]
fn reports_failing_tests_with_the_capped_output() {
    let mut harness = Harness::new();
    harness.test = 1;

    let outcome = harness.outcome();

    let TrialOutcome::TestsFailed {
        base,
        head: tested,
        commit: tried,
        result,
    } = outcome
    else {
        unreachable!("{outcome:?}")
    };
    assert_eq!((base, tested, tried), (main_sha(), head(), commit()));
    assert_eq!(result.step, PackageStep::Test);
    assert_eq!(result.exit_code, 1);
    assert_eq!(result.stdout, "test out");
    assert!(!result.passed);
}

#[test]
fn reports_a_test_step_that_timed_out_as_a_timeout_that_is_still_the_test_step() {
    for test in [124, 137] {
        let mut harness = Harness::new();
        harness.test = test;

        let outcome = harness.outcome();

        let TrialOutcome::Timeout {
            base,
            head: tested,
            commit: tried,
            result,
        } = outcome
        else {
            unreachable!("{outcome:?}")
        };
        assert_eq!((base, tested, tried), (main_sha(), head(), commit()));
        assert_eq!(result.step, PackageStep::Test);
        assert_eq!(result.exit_code, test);
        assert_eq!(result.stdout, "test out");
        assert!(result.timed_out);
    }
}

#[test]
fn reports_an_install_step_that_timed_out_as_a_timeout_running_no_tests() {
    let mut harness = Harness::new();
    harness.install = 124;

    let outcome = harness.outcome();

    assert!(
        matches!(
            outcome,
            TrialOutcome::Timeout { ref result, .. }
                if result.step == PackageStep::Install && result.timed_out
        ),
        "{outcome:?}"
    );
    assert!(!harness.has_event("package:test"));
}

#[test]
fn reports_a_stopped_rebase_as_a_conflict_with_the_unmerged_paths_and_runs_no_tests() {
    let harness = Harness::new()
        .git(GitStep::Rebase, exit(1))
        .git(GitStep::Conflicts, out(0, "src/a.ts\0b.md\0"));

    assert_eq!(
        harness.outcome(),
        TrialOutcome::Conflict {
            base: main_sha(),
            commit: commit(),
            files: vec!["src/a.ts".to_owned(), "b.md".to_owned()],
        }
    );
    assert!(!harness.ran_package_step());
}

#[test]
fn reports_a_commit_that_is_not_on_the_fork_as_commit_not_in_fork_and_runs_nothing_after() {
    for step in [GitStep::Exists, GitStep::Reachable] {
        let harness = Harness::new().git(step, exit(1));

        assert_eq!(harness.outcome(), TrialOutcome::CommitNotInFork);

        assert!(!harness.has_event("git:rebase"));
        assert!(!harness.ran_package_step());
    }
}

#[test]
fn reports_nothing_to_test_when_the_replay_leaves_main_unchanged_instead_of_testing_main() {
    let harness = Harness::new().git(GitStep::Head, out(0, &format!("{}\n", main_sha())));

    assert_eq!(
        harness.outcome(),
        TrialOutcome::NothingToTest {
            base: main_sha(),
            commit: commit(),
        }
    );
    assert!(!harness.ran_package_step());
}

#[test]
fn reports_a_dependency_refusal_as_install_not_as_a_test_failure() {
    let mut harness = Harness::new();
    harness.install = 10;

    let outcome = harness.outcome();

    assert!(
        matches!(
            outcome,
            TrialOutcome::Install { ref base, head: ref tested, .. }
                if *base == main_sha() && *tested == head()
        ),
        "{outcome:?}"
    );
}

#[test]
fn reports_a_failed_clone_or_fetch_as_clone() {
    for step in [GitStep::Clone, GitStep::Fetch] {
        let harness = Harness::new().git(step, exit(128));

        let outcome = harness.outcome();

        assert!(
            matches!(outcome, TrialOutcome::Clone { ref result } if result.exit_code == Some(128)),
            "{outcome:?}"
        );
        assert!(!harness.has_event("git:onmain"));
    }
}

#[test]
fn reports_a_main_that_is_not_on_mains_history_as_main_unreachable_and_tries_nothing() {
    for code in [1, 128] {
        let harness = Harness::new().git(GitStep::OnMain, exit(code));

        assert_eq!(
            harness.outcome(),
            TrialOutcome::MainUnreachable { main: main_sha() }
        );

        assert!(!harness.has_event("git:rebase"));
    }
}

#[test]
fn reports_a_git_failure_that_is_not_a_conflict_as_git_failed() {
    let harness = Harness::new()
        .git(GitStep::Rebase, exit(1))
        .git(GitStep::Conflicts, exit(0));

    assert!(matches!(harness.outcome(), TrialOutcome::GitFailed { .. }));
}

#[test]
fn runs_nothing_from_the_repo_when_a_read_token_cannot_be_revoked() {
    let mut harness = Harness::new();
    harness.revoked_reads = false;

    let error = harness.trial().expect_err("the trial is an error");

    assert!(matches!(error, MergeError::ReadAccessNotRevoked), "{error}");
    assert!(!harness.has_event("git:onmain"));
    assert!(!harness.ran_package_step());
}

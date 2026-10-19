//! Port of the 6 `runTrial` and 3 `reportTrial` tests of
//! `tessel-steward/src/merge-git.integration.test.ts`, against real git: bare repositories on
//! disk, the real runner, a real clone and rebase. The tests of the repo ("test.sh") really run,
//! in the trial's own clone. The title of each test is the TS title in snake case.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

mod support;

use tessel_lander::outcome::{PackageStep, TrialOutcome, TrialReport};
use tessel_lander::trial::{report_trial, TrialPlan};
use tessel_lander::types::Sha;

use support::fixture_git;
use support::repos::Repos;

/// main: base -> lib.txt "ok". The fork forks there and adds its own test, which needs lib.txt to
/// say "ok". Returns the fork's commit and main as the fork saw it; the work tree is left on main.
fn assuming_fork(repos: &Repos) -> (Sha, Sha) {
    repos.commit("lib.txt", "ok\n");
    repos.push(&repos.main);
    let old_main = repos.main_head();
    let assuming = repos.commit("test.sh", "grep -qx ok lib.txt\n");
    repos.push(&repos.fork);
    repos.reset_to(old_main.as_str());
    (assuming, old_main)
}

fn trial(repos: &Repos, main: &Sha, commit: &Sha) -> TrialOutcome {
    repos.trial(main, commit).expect("the trial finishes")
}

fn report(repos: &Repos, before: &Sha, main: &Sha, commit: &Sha) -> TrialReport {
    let plan = TrialPlan {
        before: before.clone(),
        main: main.clone(),
        commit: commit.clone(),
    };
    report_trial(&plan, &|tried, commit| repos.trial(tried, commit)).expect("the report")
}

// ---- runTrial against real git ----

#[test]
fn fails_the_assuming_forks_test_on_a_main_whose_lib_changed_and_leaves_main_and_the_fork_alone() {
    let repos = Repos::new();
    let (assuming, old_main) = assuming_fork(&repos);
    repos.commit("lib.txt", "broken\n");
    repos.push(&repos.main);
    let new_main = repos.main_head();
    let fork_before = fixture_git(&repos.fork, &["rev-parse", "main"]);

    let outcome = trial(&repos, &new_main, &assuming);

    assert!(
        matches!(
            outcome,
            TrialOutcome::TestsFailed { ref base, ref commit, ref result, .. }
                if *base == new_main && *commit == assuming
                    && result.step == PackageStep::Test && !result.passed
        ),
        "{outcome:?}"
    );
    assert_eq!(repos.main_head(), new_main);
    assert_eq!(
        fixture_git(&repos.fork, &["rev-parse", "main"]),
        fork_before
    );

    let before = trial(&repos, &old_main, &assuming);
    assert!(
        matches!(before, TrialOutcome::Clean { ref base, .. } if *base == old_main),
        "{before:?}"
    );
}

#[test]
fn is_clean_when_main_moved_on_without_touching_what_the_test_needs() {
    let repos = Repos::new();
    let (assuming, _) = assuming_fork(&repos);
    repos.commit("other.txt", "unrelated\n");
    repos.push(&repos.main);
    let new_main = repos.main_head();

    let outcome = trial(&repos, &new_main, &assuming);

    assert!(
        matches!(
            outcome,
            TrialOutcome::Clean { ref base, ref commit, .. }
                if *base == new_main && *commit == assuming
        ),
        "{outcome:?}"
    );
    assert_eq!(repos.main_head(), new_main);
}

#[test]
fn tries_main_at_the_given_sha_not_main_as_it_is_now() {
    let repos = Repos::new();
    let (assuming, _) = assuming_fork(&repos);
    let given = repos.main_head();
    repos.commit("lib.txt", "broken\n");
    repos.push(&repos.main);

    let outcome = trial(&repos, &given, &assuming);

    assert!(
        matches!(outcome, TrialOutcome::Clean { ref base, .. } if *base == given),
        "{outcome:?}"
    );
}

#[test]
fn reports_a_conflict_with_the_unmerged_files_and_leaves_main_alone() {
    let repos = Repos::new();
    let base = repos.head();
    let forked = repos.commit("a.txt", "one\nFORK\nthree\n");
    repos.push(&repos.fork);
    repos.reset_to(base.as_str());
    repos.commit("a.txt", "one\nMAIN\nthree\n");
    repos.push(&repos.main);
    let main_now = repos.main_head();

    let outcome = trial(&repos, &main_now, &forked);

    assert_eq!(
        outcome,
        TrialOutcome::Conflict {
            base: main_now.clone(),
            commit: forked,
            files: vec!["a.txt".to_owned()],
        }
    );
    assert_eq!(repos.main_head(), main_now);
}

#[test]
fn refuses_a_commit_that_is_not_on_the_fork_and_a_main_that_is_not_on_main() {
    let repos = Repos::new();
    let (assuming, _) = assuming_fork(&repos);
    let main_now = repos.main_head();
    let unknown = Sha::parse(&"1".repeat(40)).expect("sha");

    assert_eq!(
        trial(&repos, &main_now, &unknown),
        TrialOutcome::CommitNotInFork
    );
    assert_eq!(
        trial(&repos, &assuming, &assuming),
        TrialOutcome::MainUnreachable {
            main: assuming.clone()
        }
    );
}

#[test]
fn has_nothing_to_test_when_the_forks_work_is_already_in_main() {
    let repos = Repos::new();
    let forked = repos.commit("b.txt", "fork work\n");
    repos.push(&repos.fork);
    repos.push(&repos.main);

    let outcome = trial(&repos, &repos.main_head(), &forked);

    assert!(
        matches!(outcome, TrialOutcome::NothingToTest { .. }),
        "{outcome:?}"
    );
}

// ---- reportTrial against real git ----

#[test]
fn is_clean_on_the_baseline_and_fails_on_the_new_main_the_merge_broke_the_work() {
    let repos = Repos::new();
    let (assuming, old_main) = assuming_fork(&repos);
    repos.commit("junk.txt", "unrelated\n");
    repos.commit("lib.txt", "broken\n");
    repos.push(&repos.main);
    let new_main = repos.main_head();

    let result = report(&repos, &old_main, &new_main, &assuming);

    assert!(
        matches!(result.before, TrialOutcome::Clean { ref base, .. } if *base == old_main),
        "{result:?}"
    );
    assert!(
        matches!(
            result.after,
            Some(TrialOutcome::TestsFailed { ref base, .. }) if *base == new_main
        ),
        "{result:?}"
    );
    assert_eq!(repos.main_head(), new_main);
}

#[test]
fn does_not_try_the_new_main_when_the_work_already_fails_on_the_baseline() {
    let repos = Repos::new();
    repos.commit("lib.txt", "broken\n");
    repos.push(&repos.main);
    let old_main = repos.main_head();
    let wip = repos.commit("test.sh", "grep -qx ok lib.txt\n");
    repos.push(&repos.fork);
    repos.reset_to(old_main.as_str());
    repos.commit("other.txt", "unrelated\n");
    repos.push(&repos.main);

    let result = report(&repos, &old_main, &repos.main_head(), &wip);

    assert!(
        matches!(
            result.before,
            TrialOutcome::TestsFailed { ref base, .. } if *base == old_main
        ),
        "{result:?}"
    );
    assert_eq!(result.after, None);
}

#[test]
fn reports_a_conflict_that_is_new_on_the_new_main_next_to_a_clean_baseline() {
    let repos = Repos::new();
    let (assuming, old_main) = assuming_fork(&repos);
    repos.commit("test.sh", "echo main wrote this first\n");
    repos.push(&repos.main);

    let result = report(&repos, &old_main, &repos.main_head(), &assuming);

    assert!(
        matches!(result.before, TrialOutcome::Clean { .. }),
        "{result:?}"
    );
    assert!(
        matches!(
            result.after,
            Some(TrialOutcome::Conflict { ref files, .. }) if *files == ["test.sh"]
        ),
        "{result:?}"
    );
}

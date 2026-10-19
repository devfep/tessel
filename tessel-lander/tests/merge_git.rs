//! Port of the 11 `runMerge` tests of `tessel-steward/src/merge-git.integration.test.ts` (6
//! merge, 5 coverage), against real git: bare repositories on disk, the real runner, a real
//! clone and rebase. The platform ports are `LocalPorts`.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

mod support;

use std::cell::RefCell;
use std::time::Duration;

use tessel_coordinator::protocol::Mode;
use tessel_lander::merge::MergePolicy;
use tessel_lander::outcome::MergeOutcome;
use tessel_lander::types::Sha;

use support::repos::{file_scope, whole_repo, LocalPorts, Repos};

/// main: base -> m2 (c.txt). fork: base -> f1 (b.txt). Returns f1.
fn diverged_without_conflict(repos: &Repos) -> Sha {
    let base = repos.head();
    let forked = repos.commit("b.txt", "fork work\n");
    repos.push(&repos.fork);
    repos.reset_to(base.as_str());
    repos.commit("c.txt", "main moved on\n");
    repos.push(&repos.main);
    forked
}

fn merge(
    repos: &Repos,
    commit: &Sha,
    scopes: &[tessel_coordinator::protocol::ScopeClaim],
) -> MergeOutcome {
    let ports = LocalPorts::new(repos);
    repos
        .merge(&ports, commit, scopes, MergePolicy::Submission)
        .expect("the merge finishes")
}

// ---- runMerge against real git ----

#[test]
fn rebases_the_fork_commit_onto_main_and_pushes_it_to_main() {
    let repos = Repos::new();
    let forked = diverged_without_conflict(&repos);
    let main_before = repos.main_head();

    let outcome = merge(&repos, &forked, &whole_repo());

    let MergeOutcome::Merged { base, head, .. } = outcome else {
        unreachable!("{outcome:?}")
    };
    assert_eq!(base, main_before);
    assert_eq!(repos.main_head(), head);
    assert_eq!(
        support::fixture_git(&repos.main, &["rev-parse", "main~1"]),
        main_before.as_str()
    );
    assert_eq!(repos.show_on_main("b.txt"), "fork work");
    assert_eq!(repos.show_on_main("c.txt"), "main moved on");
}

#[test]
fn gives_the_same_head_when_the_same_merge_is_retried_a_second_later() {
    let repos = Repos::new();
    let forked = diverged_without_conflict(&repos);
    let main_before = repos.main_head();
    assert!(matches!(
        merge(&repos, &forked, &whole_repo()),
        MergeOutcome::Merged { .. }
    ));
    let first = repos.main_head();

    support::fixture_git(
        &repos.main,
        &["update-ref", "refs/heads/main", main_before.as_str()],
    );
    std::thread::sleep(Duration::from_millis(1100));
    assert!(matches!(
        merge(&repos, &forked, &whole_repo()),
        MergeOutcome::Merged { .. }
    ));

    assert_eq!(repos.main_head(), first);
}

#[test]
fn reports_the_unmerged_files_when_the_rebase_conflicts_and_leaves_main_alone() {
    let repos = Repos::new();
    let base = repos.head();
    let forked = repos.commit("a.txt", "one\nFORK\nthree\n");
    repos.push(&repos.fork);
    repos.reset_to(base.as_str());
    repos.commit("a.txt", "one\nMAIN\nthree\n");
    repos.push(&repos.main);
    let main_before = repos.main_head();

    let outcome = merge(&repos, &forked, &whole_repo());

    assert_eq!(
        outcome,
        MergeOutcome::Conflict {
            base: main_before.clone(),
            files: vec!["a.txt".to_owned()]
        }
    );
    assert_eq!(repos.main_head(), main_before);
}

#[test]
fn refuses_a_commit_that_is_not_on_the_forks_default_branch() {
    let repos = Repos::new();
    diverged_without_conflict(&repos);
    let only_on_main = repos.main_head();
    let unknown = Sha::parse(&"1".repeat(40)).expect("sha");

    assert_eq!(
        merge(&repos, &only_on_main, &whole_repo()),
        MergeOutcome::CommitNotInFork
    );
    assert_eq!(
        merge(&repos, &unknown, &whole_repo()),
        MergeOutcome::CommitNotInFork
    );
}

#[test]
fn does_not_overwrite_main_when_it_moved_after_the_clone() {
    let repos = Repos::new();
    let forked = diverged_without_conflict(&repos);
    let base = repos.main_head();
    let racer = RefCell::new(None);
    let mut ports = LocalPorts::new(&repos);
    ports.during_push_access = Box::new(|| {
        repos.reset_to(base.as_str());
        *racer.borrow_mut() = Some(repos.commit("d.txt", "someone else\n"));
        repos.push(&repos.main);
    });

    let outcome = repos
        .merge(&ports, &forked, &whole_repo(), MergePolicy::Submission)
        .expect("the merge finishes");

    let racer = racer.borrow().clone().expect("the racer committed");
    assert_eq!(
        outcome,
        MergeOutcome::MainMoved {
            expected: base.clone(),
            actual: racer.clone()
        }
    );
    assert_eq!(repos.main_head(), racer);
}

#[test]
fn reports_a_fork_commit_that_main_already_contains_as_already_merged() {
    let repos = Repos::new();
    let forked = repos.commit("b.txt", "fork work\n");
    repos.push(&repos.fork);
    repos.push(&repos.main);
    let main_before = repos.main_head();

    let outcome = merge(&repos, &forked, &whole_repo());

    assert_eq!(outcome, MergeOutcome::AlreadyMerged { base: main_before });
}

// ---- runMerge coverage against real git ----

#[test]
fn coverage_rejects_a_fork_commit_that_touches_an_unclaimed_file_as_uncovered_running_nothing() {
    let repos = Repos::new();
    let forked = diverged_without_conflict(&repos);
    let main_before = repos.main_head();
    let mut ports = LocalPorts::new(&repos);
    ports.refuse_push_access = true;

    let outcome = repos
        .merge(
            &ports,
            &forked,
            &[file_scope("a.txt", Mode::EditBody)],
            MergePolicy::Submission,
        )
        .expect("the merge finishes");

    assert!(
        matches!(
            outcome,
            MergeOutcome::Uncovered { ref base, ref files, total: 1, .. }
                if *base == main_before && *files == ["b.txt"]
        ),
        "{outcome:?}"
    );
    assert!(ports.log.borrow().is_empty());
    assert_eq!(repos.main_head(), main_before);
}

#[test]
fn coverage_judges_the_rebased_range_not_the_forks_diff_against_its_own_base() {
    let repos = Repos::new();
    let forked = diverged_without_conflict(&repos);

    let outcome = merge(&repos, &forked, &[file_scope("b.txt", Mode::Create)]);

    assert!(
        matches!(outcome, MergeOutcome::Merged { .. }),
        "{outcome:?}"
    );
}

#[test]
fn coverage_needs_edit_signature_on_the_old_path_and_create_on_the_new_path_for_a_real_rename() {
    let repos = Repos::new();
    repos.work_git(&["mv", "a.txt", "renamed.txt"]);
    repos.work_git(&["commit", "--quiet", "-m", "rename"]);
    let forked = repos.head();
    repos.push(&repos.fork);
    repos.work_git(&["reset", "--quiet", "--hard", "HEAD~1"]);

    let only_old = merge(&repos, &forked, &[file_scope("a.txt", Mode::EditSignature)]);
    assert!(
        matches!(only_old, MergeOutcome::Uncovered { ref files, .. } if *files == ["renamed.txt"]),
        "{only_old:?}"
    );

    let both = merge(
        &repos,
        &forked,
        &[
            file_scope("a.txt", Mode::EditSignature),
            file_scope("renamed.txt", Mode::Create),
        ],
    );
    assert!(matches!(both, MergeOutcome::Merged { .. }), "{both:?}");
}

#[test]
fn coverage_maps_a_real_delete_to_edit_signature_so_edit_body_does_not_cover_it() {
    let repos = Repos::new();
    repos.work_git(&["rm", "--quiet", "a.txt"]);
    repos.work_git(&["commit", "--quiet", "-m", "delete"]);
    let forked = repos.head();
    repos.push(&repos.fork);
    repos.work_git(&["reset", "--quiet", "--hard", "HEAD~1"]);

    let weak = merge(&repos, &forked, &[file_scope("a.txt", Mode::EditBody)]);
    assert!(
        matches!(weak, MergeOutcome::Uncovered { ref files, .. } if *files == ["a.txt"]),
        "{weak:?}"
    );
    let strong = merge(&repos, &forked, &[file_scope("a.txt", Mode::EditSignature)]);
    assert!(matches!(strong, MergeOutcome::Merged { .. }), "{strong:?}");
}

#[test]
fn coverage_carries_a_file_name_with_a_space_a_newline_and_non_ascii_characters_as_data() {
    let repos = Repos::new();
    let odd = "we ird\nnaïve $(x).txt";
    let forked = repos.commit(odd, "x\n");
    repos.push(&repos.fork);
    repos.reset_to("HEAD~1");

    let outcome = merge(&repos, &forked, &[]);

    assert!(
        matches!(
            outcome,
            MergeOutcome::Uncovered { ref files, total: 1, .. } if *files == [odd]
        ),
        "{outcome:?}"
    );
}

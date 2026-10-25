//! The candidate ref against real git: pushed, read, swept, fetched into a test tree and landed.

mod support;

use std::fs;

use support::fixture_git;
use support::repos::{LocalPorts, Repos};
use tessel_lander::candidate::{
    candidate_ref, push_candidate, read_candidate, sweep_candidates, CANDIDATE_NAMESPACE, MAX_SWEPT,
};
use tessel_lander::outcome::MergeOutcome;
use tessel_lander::run::SystemRunner;
use tessel_lander::types::Sha;
use tessel_lander::workspace::{fetch_candidate_tree, CandidateTreeOutcome, CloneError};

#[expect(clippy::expect_used, reason = "a failed expect is a failed test")]
fn remote(repos: &Repos) -> String {
    repos.main.to_str().expect("utf-8 path").to_owned()
}

/// A head one commit past main, in the work tree only.
fn new_head(repos: &Repos) -> Sha {
    repos.commit("b.txt", "feature\n")
}

#[test]
fn a_pushed_candidate_is_read_back_at_its_tip() {
    let repos = Repos::new();
    let head = new_head(&repos);
    let ports = LocalPorts::new(&repos);
    let workspace = repos.clone_of_work();
    push_candidate(
        repos.git(),
        &SystemRunner,
        &ports,
        &workspace,
        &remote(&repos),
        &head,
    )
    .expect("push");
    assert_eq!(
        repos.main_ref(&candidate_ref(&head)),
        Some(head.to_string())
    );
    let read = read_candidate(repos.git(), &SystemRunner, &remote(&repos), &head).expect("read");
    assert_eq!(read, Some(head));
    assert_eq!(*ports.log.borrow(), ["candidate push"]);
}

#[test]
fn a_missing_candidate_reads_as_none() {
    let repos = Repos::new();
    let head = new_head(&repos);
    let read = read_candidate(repos.git(), &SystemRunner, &remote(&repos), &head).expect("read");
    assert_eq!(read, None);
}

#[test]
fn a_refused_candidate_push_writes_nothing() {
    let repos = Repos::new();
    let head = new_head(&repos);
    let mut ports = LocalPorts::new(&repos);
    ports.refuse_candidate_access = true;
    let workspace = repos.clone_of_work();
    let result = push_candidate(
        repos.git(),
        &SystemRunner,
        &ports,
        &workspace,
        &remote(&repos),
        &head,
    );
    assert!(result.is_err());
    assert_eq!(repos.main_ref(&candidate_ref(&head)), None);
}

#[test]
fn the_sweep_removes_at_most_fifty_other_candidates_and_keeps_the_one_asked_for() {
    let repos = Repos::new();
    let head = new_head(&repos);
    let keep = candidate_ref(&head);
    repos.set_candidate(head.as_str(), &head);
    let base = repos.main_head();
    let names: Vec<String> = (0..60).map(|n| format!("{n:040x}")).collect();
    for name in &names {
        repos.set_candidate(name, &base);
    }
    let ports = LocalPorts::new(&repos);
    let workspace = repos.clone_of_work();
    let removed = sweep_candidates(
        repos.git(),
        &SystemRunner,
        &ports,
        &workspace,
        &remote(&repos),
        &head,
    )
    .expect("sweep");
    assert_eq!(removed, MAX_SWEPT);
    let left = names
        .iter()
        .filter(|name| {
            repos
                .main_ref(&format!("{CANDIDATE_NAMESPACE}{name}"))
                .is_some()
        })
        .count();
    assert_eq!(left, 60 - MAX_SWEPT);
    assert!(repos.main_ref(&keep).is_some(), "the asked-for ref stays");
}

#[test]
fn the_sweep_leaves_refs_that_are_not_of_the_exact_shape() {
    let repos = Repos::new();
    let head = new_head(&repos);
    let base = repos.main_head();
    for name in ["not-a-sha", &"A".repeat(40)] {
        repos.set_candidate(name, &base);
    }
    let ports = LocalPorts::new(&repos);
    let workspace = repos.clone_of_work();
    let removed = sweep_candidates(
        repos.git(),
        &SystemRunner,
        &ports,
        &workspace,
        &remote(&repos),
        &head,
    )
    .expect("sweep");
    assert_eq!(removed, 0);
    assert!(repos
        .main_ref(&format!("{CANDIDATE_NAMESPACE}not-a-sha"))
        .is_some());
}

#[test]
fn the_sweep_deletes_nothing_when_candidate_access_is_refused() {
    let repos = Repos::new();
    let head = new_head(&repos);
    let other = format!("{:040x}", 7);
    repos.set_candidate(&other, &repos.main_head());
    let mut ports = LocalPorts::new(&repos);
    ports.refuse_candidate_access = true;
    let workspace = repos.clone_of_work();
    let removed = sweep_candidates(
        repos.git(),
        &SystemRunner,
        &ports,
        &workspace,
        &remote(&repos),
        &head,
    )
    .expect("sweep");
    assert_eq!(removed, 0);
    assert!(repos
        .main_ref(&format!("{CANDIDATE_NAMESPACE}{other}"))
        .is_some());
}

#[test]
fn a_test_tree_is_the_candidate_at_its_head_with_no_remote_or_credential() {
    let repos = Repos::new();
    let head = new_head(&repos);
    repos.set_candidate(head.as_str(), &head);
    let destination = repos.scratch.path().join("tree");
    let outcome = fetch_candidate_tree(
        repos.git(),
        &SystemRunner,
        &remote(&repos),
        &head,
        &destination,
    )
    .expect("fetch");
    let CandidateTreeOutcome::Fetched(tree) = outcome else {
        unreachable!("the fetch failed");
    };
    assert_eq!(
        fixture_git(tree.path(), &["rev-parse", "HEAD"]),
        head.as_str()
    );
    assert!(destination.join("b.txt").is_file());
    let config = fs::read_to_string(destination.join(".git/config")).expect("config");
    assert!(!config.contains("[remote"), "{config}");
    assert!(!config.contains("url"), "{config}");
}

#[test]
fn a_candidate_whose_tip_is_not_the_head_makes_no_test_tree() {
    let repos = Repos::new();
    let head = new_head(&repos);
    repos.set_candidate(head.as_str(), &repos.main_head());
    let destination = repos.scratch.path().join("tree");
    let result = fetch_candidate_tree(
        repos.git(),
        &SystemRunner,
        &remote(&repos),
        &head,
        &destination,
    );
    assert!(matches!(result, Err(CloneError::TestTreeNotAtHead { .. })));
}

#[test]
fn a_missing_candidate_makes_no_test_tree() {
    let repos = Repos::new();
    let head = new_head(&repos);
    let destination = repos.scratch.path().join("tree");
    let outcome = fetch_candidate_tree(
        repos.git(),
        &SystemRunner,
        &remote(&repos),
        &head,
        &destination,
    )
    .expect("fetch runs");
    assert!(matches!(outcome, CandidateTreeOutcome::Failed(_)));
}

fn merged_head(outcome: &MergeOutcome) -> Option<&Sha> {
    if let MergeOutcome::Merged { head, .. } = outcome {
        return Some(head);
    }
    None
}

#[test]
fn land_pushes_the_head_to_main_and_deletes_the_ref() {
    let repos = Repos::new();
    let head = new_head(&repos);
    repos.set_candidate(head.as_str(), &head);
    let ports = LocalPorts::new(&repos);
    let outcome = repos.land_candidate(&ports, &head).expect("land");
    assert_eq!(merged_head(&outcome), Some(&head), "{outcome:?}");
    assert_eq!(repos.main_head(), head);
    assert_eq!(repos.main_ref(&candidate_ref(&head)), None);
    assert_eq!(*ports.log.borrow(), ["push access", "candidate delete"]);
}

#[test]
fn land_does_not_push_a_head_main_already_holds() {
    let repos = Repos::new();
    let head = new_head(&repos);
    repos.push(&repos.main);
    repos.set_candidate(head.as_str(), &head);
    let ports = LocalPorts::new(&repos);
    let outcome = repos.land_candidate(&ports, &head).expect("land");
    assert_eq!(merged_head(&outcome), Some(&head));
    assert_eq!(*ports.log.borrow(), ["candidate delete"]);
}

#[test]
fn land_with_the_ref_gone_and_the_head_on_main_is_landed() {
    let repos = Repos::new();
    let head = new_head(&repos);
    repos.push(&repos.main);
    let ports = LocalPorts::new(&repos);
    let outcome = repos.land_candidate(&ports, &head).expect("land");
    assert_eq!(merged_head(&outcome), Some(&head));
    assert!(ports.log.borrow().is_empty(), "nothing is written");
}

#[test]
fn land_with_the_ref_gone_and_the_head_not_on_main_is_candidate_missing() {
    let repos = Repos::new();
    let head = new_head(&repos);
    let ports = LocalPorts::new(&repos);
    let outcome = repos.land_candidate(&ports, &head).expect("land");
    assert!(
        matches!(outcome, MergeOutcome::CandidateMissing),
        "{outcome:?}"
    );
    assert!(ports.log.borrow().is_empty());
}

#[test]
fn land_with_a_tampered_tip_is_candidate_missing_and_pushes_nothing() {
    let repos = Repos::new();
    let head = new_head(&repos);
    let base = repos.main_head();
    repos.set_candidate(head.as_str(), &base);
    let ports = LocalPorts::new(&repos);
    let outcome = repos.land_candidate(&ports, &head).expect("land");
    assert!(
        matches!(outcome, MergeOutcome::CandidateMissing),
        "{outcome:?}"
    );
    assert_eq!(repos.main_head(), base);
    assert!(ports.log.borrow().is_empty());
}

#[test]
fn land_is_merged_even_when_the_ref_cannot_be_deleted() {
    let repos = Repos::new();
    let head = new_head(&repos);
    repos.set_candidate(head.as_str(), &head);
    let mut ports = LocalPorts::new(&repos);
    ports.refuse_candidate_access = true;
    let outcome = repos.land_candidate(&ports, &head).expect("land");
    assert_eq!(merged_head(&outcome), Some(&head), "{outcome:?}");
    assert!(
        repos.main_ref(&candidate_ref(&head)).is_some(),
        "swept later"
    );
}

#[test]
fn land_is_merged_when_main_moved_past_the_head_before_the_read_back() {
    let repos = Repos::new();
    let head = new_head(&repos);
    repos.set_candidate(head.as_str(), &head);
    let main = repos.main.display().to_string();
    let first = std::cell::Cell::new(true);
    let mut ports = LocalPorts::new(&repos);
    ports.before_current_main = Box::new(|| {
        if first.replace(false) {
            // A later landing builds on the head between this push and its read-back.
            let later = repos.commit("later.txt", "later\n");
            repos.work_git(&["push", "--quiet", &main, "HEAD:refs/heads/main"]);
            assert_ne!(later, head);
        }
    });
    let outcome = repos.land_candidate(&ports, &head).expect("land");
    assert_eq!(merged_head(&outcome), Some(&head), "{outcome:?}");
    assert_ne!(repos.main_head(), head, "main is past the head");
}

//! Port of the 3 tests of `tessel-steward/src/gate-trunk.integration.test.ts`: a fork that weakens
//! `tessel.toml` is judged by the trunk's gate, never its own. Against real git.
//!
//! Not ported: the steward's first test also ran `runTrial` on the fork's commit and checked the
//! tree the tests would see. `run_trial` is P1-LANDER-TRIAL's; its test should add that half.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

mod support;

use std::path::PathBuf;
use std::process::Command;

use tessel_lander::gate::{
    read_gate_plan, GateInstance, GatePlan, TestCommand, TrunkFile, TrunkFiles,
};
use tessel_lander::merge::{BoundaryError, MergePolicy};
use tessel_lander::outcome::MergeOutcome;
use tessel_lander::types::Sha;

use support::repos::{whole_repo, LocalPorts, Repos};

const STRONG: &str = "instance = \"standard-4\"
[[test]]
argv = [\"cargo\", \"test\", \"--workspace\", \"--locked\", \"--offline\"]
";
const WEAK: &str = "instance = \"standard-1\"
[[test]]
argv = [\"npm\", \"test\"]
";

/// Answers `read_file` the way a git host does: the file at a commit of one bare repository.
struct BareRepo(PathBuf);

impl TrunkFiles for BareRepo {
    fn read_file(&self, commit: &Sha, path: &str) -> Result<TrunkFile, BoundaryError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .arg("show")
            .arg(format!("{commit}:{path}"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").expect("PATH"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .map_err(|source| BoundaryError {
                operation: "read_file",
                message: source.to_string(),
            })?;
        Ok(if output.status.success() {
            TrunkFile::Found(output.stdout)
        } else {
            TrunkFile::Missing
        })
    }
}

/// Main holds the strong gate; the fork's only new commit weakens it and adds a file.
struct World {
    repos: Repos,
    base: Sha,
    commit: Sha,
}

fn world() -> World {
    let repos = Repos::new();
    repos.commit("tessel.toml", STRONG);
    repos.push(&repos.main);
    repos.push(&repos.fork);
    let base = repos.main_head();
    std::fs::write(repos.work.join("tessel.toml"), WEAK).expect("weaken the gate");
    repos.commit("feature.txt", "feature\n");
    repos.work_git(&[
        "commit",
        "--quiet",
        "--all",
        "--amend",
        "-m",
        "fork weakens the gate",
    ]);
    repos.push(&repos.fork);
    let commit = repos.head();
    World {
        repos,
        base,
        commit,
    }
}

fn strong_gate_commands() -> Vec<TestCommand> {
    vec![TestCommand {
        dir: ".".to_owned(),
        argv: ["cargo", "test", "--workspace", "--locked", "--offline"]
            .map(str::to_owned)
            .to_vec(),
    }]
}

#[test]
fn a_fork_that_weakens_tessel_toml_is_tried_by_the_trunks_gate_and_is_never_merged_the_change_to_the_gate_is_rejected(
) {
    let w = world();
    let plan = read_gate_plan(&BareRepo(w.repos.main.clone()), &w.base).expect("reads");
    let GatePlan::Configured(config) = plan else {
        unreachable!("{plan:?}")
    };
    assert_eq!(config.instance, Some(GateInstance::Standard4));
    assert_eq!(config.tests, strong_gate_commands());

    let ports = LocalPorts::new(&w.repos);
    let merged = w
        .repos
        .merge(&ports, &w.commit, &whole_repo(), MergePolicy::Submission)
        .expect("the merge finishes");

    assert!(
        matches!(
            merged,
            MergeOutcome::GateChanged { ref base, .. } if *base == w.base
        ),
        "{merged:?}"
    );
    assert!(
        ports.log.borrow().is_empty(),
        "nothing ran and no push access was asked for"
    );
    assert_eq!(w.repos.main_head(), w.base);
    assert_eq!(w.repos.show_on_main("tessel.toml"), STRONG.trim());
}

#[test]
fn a_fork_that_weakens_tessel_toml_would_get_the_weak_gate_if_it_were_read_from_the_fork_the_control_for_the_test_above(
) {
    let w = world();
    let from_fork = read_gate_plan(&BareRepo(w.repos.fork.clone()), &w.commit).expect("reads");
    let GatePlan::Configured(config) = &from_fork else {
        unreachable!("{from_fork:?}")
    };
    assert_eq!(config.instance, Some(GateInstance::Standard1));
    let from_trunk = read_gate_plan(&BareRepo(w.repos.main.clone()), &w.base).expect("reads");
    assert_ne!(from_trunk, from_fork);
}

#[test]
fn a_fork_that_weakens_tessel_toml_is_judged_by_main_as_it_was_at_the_pinned_commit_not_by_a_later_main(
) {
    let w = world();
    w.repos.reset_to(w.base.as_str());
    std::fs::write(
        w.repos.work.join("tessel.toml"),
        "instance = \"standard-2\"\n[[test]]\nargv = [\"npm\", \"test\"]\n",
    )
    .expect("later gate");
    w.repos
        .work_git(&["commit", "--quiet", "--all", "-m", "main changes its gate"]);
    w.repos.push(&w.repos.main);
    assert_ne!(w.repos.main_head(), w.base);

    let plan = read_gate_plan(&BareRepo(w.repos.main.clone()), &w.base).expect("reads");

    let GatePlan::Configured(config) = plan else {
        unreachable!("{plan:?}")
    };
    assert_eq!(config.instance, Some(GateInstance::Standard4));
}

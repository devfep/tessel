//! Runs one side of an A/B comparison in a scratch directory that is removed when the run ends.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::demo;
use crate::guard::ScratchRepo;
use crate::live::{self, LiveSetup, Steward};
use crate::local::{LocalServer, LocalSetup};
use crate::off::{self, OffConfig, OffResult};
use crate::on::{self, OnConfig, OnResult, Policy, REVIEWER};
use crate::tasks::Task;

/// Everything both sides of a comparison share.
#[derive(Debug, Clone)]
pub struct Spec {
    pub seed: u64,
    pub tasks: usize,
    pub overlap: f64,
    pub agents: usize,
    pub policy: Policy,
    pub work_ms: u64,
    pub task_timeout: Duration,
    pub max_denials: u32,
}

impl Spec {
    pub fn on_config(&self) -> OnConfig {
        OnConfig {
            agents: self.agents,
            policy: self.policy,
            work_ms: self.work_ms,
            task_timeout: self.task_timeout,
            max_denials: self.max_denials,
        }
    }
}

/// The scratch repository to target: `name`, or `swarm-s<seed>-<time>`. Only `swarm-*` names pass.
pub fn resolve_repo(name: Option<&str>, seed: u64, now_secs: u64) -> Result<ScratchRepo> {
    match name {
        Some(name) => ScratchRepo::parse(name),
        None => ScratchRepo::parse(&format!("swarm-s{seed}-{}", base36(now_secs))),
    }
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[usize::try_from(n % 36).unwrap_or(0)]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

pub async fn run_off(spec: &Spec, tasks: &[Task]) -> Result<OffResult> {
    let scratch = tempfile::tempdir().context("cannot create a scratch directory")?;
    let config = OffConfig {
        agents: spec.agents,
        work_ms: spec.work_ms,
    };
    off::run_off(tasks, scratch.path(), config).await
}

pub async fn run_on_local(spec: &Spec, tasks: &[Task], repo: &ScratchRepo) -> Result<OnResult> {
    let scratch = tempfile::tempdir().context("cannot create a scratch directory")?;
    let names = on::principals(spec.agents);
    let base = demo::base_tree();
    let server = LocalServer::start(LocalSetup {
        repo,
        scratch: &scratch.path().join("server"),
        base: &base,
        names: &names,
        reviewers: &[REVIEWER.to_string()],
    })
    .await?;
    let result = on::run_on(
        &server.endpoint,
        tasks,
        &scratch.path().join("agents"),
        &spec.on_config(),
    )
    .await;
    server.shutdown().await;
    result
}

pub async fn run_on_live(
    spec: &Spec,
    tasks: &[Task],
    repo: &ScratchRepo,
    steward: &Steward,
    coordinator: &str,
) -> Result<OnResult> {
    let scratch = tempfile::tempdir().context("cannot create a scratch directory")?;
    let names = on::principals(spec.agents);
    let agents = on::agent_names(spec.agents);
    let base = demo::base_tree();
    let seed_dir = scratch.path().join("setup");
    let endpoint = tokio::task::block_in_place(|| {
        live::provision(&LiveSetup {
            steward,
            coordinator,
            repo,
            scratch: &seed_dir,
            base: &base,
            agents: &agents,
            names: &names,
        })
    })?;
    on::run_on(
        &endpoint,
        tasks,
        &scratch.path().join("agents"),
        &spec.on_config(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_repo_is_a_scratch_name_and_a_given_one_is_checked() {
        let default = resolve_repo(None, 12, 1_791_000_000).unwrap();
        assert!(default.as_str().starts_with("swarm-s12-"), "{default}");
        assert_eq!(
            resolve_repo(Some("swarm-mine"), 1, 0).unwrap().as_str(),
            "swarm-mine"
        );
        assert!(resolve_repo(Some("tessel-dogfood"), 1, 0).is_err());
        assert!(resolve_repo(Some("demo"), 1, 0).is_err());
    }

    #[test]
    fn base36_is_lowercase_and_stable() {
        assert_eq!(base36(0), "");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }
}

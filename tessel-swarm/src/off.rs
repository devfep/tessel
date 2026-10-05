//! Mode `off`, the uncoordinated baseline. Every agent works from the same starting commit with no
//! claims; their work is then merged onto a trunk with plain git, in task order, and the tests
//! run after each merge. Everything here is local: the protocol has no client message for
//! `ReplayMerged`, so these numbers are never sent to a coordinator (a client that wrote evidence
//! events would break invariant 10).
//!
//! A merge that breaks the build or the tests is rolled back, so every merge is judged on a green
//! trunk and one break is not counted again in the merges after it.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;
use tessel_coordinator::protocol::{AgentId, CommitId, Event, EventKind, Outcome, RunId, Summary};
use tokio::sync::Semaphore;

use crate::demo::{self, Tree};
use crate::git::{self, Checks, Git};
use crate::tasks::{self, Task};

#[derive(Debug, Clone, Copy)]
pub struct OffConfig {
    pub agents: usize,
    /// Simulated time one agent spends on one task.
    pub work_ms: u64,
}

struct Branch {
    name: String,
    sha: String,
    measured_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeRecord {
    pub task: usize,
    pub agent: String,
    pub outcome: Outcome,
    /// Files left unmerged, for a textual conflict.
    pub conflicted_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Counts {
    pub clean: u64,
    pub textual_conflicts: u64,
    pub build_failed: u64,
    pub tests_failed: u64,
}

pub struct OffResult {
    pub wall_ms: u64,
    pub merges: Vec<MergeRecord>,
    pub counts: Counts,
    pub events: Vec<Event>,
    /// Computed by `Summary::from_events` over `events`, which exist only in this process.
    pub summary: Summary,
    /// Per task: the configured work time plus the measured time to edit, test and commit.
    pub work_ms: u64,
    pub wasted_ms: u64,
}

pub fn agent_name(index: usize) -> String {
    format!("a{index:02}")
}

pub async fn run_off(tasks: &[Task], scratch: &Path, config: OffConfig) -> Result<OffResult> {
    let started = Instant::now();
    let trunk = Git::new(&scratch.join("trunk"));
    std::fs::create_dir_all(&trunk.dir)?;
    let base_tree = demo::base_tree();
    let (repo, tree) = (trunk.clone(), base_tree.clone());
    let base = tokio::task::spawn_blocking(move || git::init_repo(&repo, &tree)).await??;

    work_in_parallel(tasks.len(), config).await?;

    let (repo, list) = (trunk.clone(), tasks.to_vec());
    let branches =
        tokio::task::spawn_blocking(move || commit_branches(&repo, &base, &base_tree, &list))
            .await??;
    let work_ms: Vec<u64> = branches
        .iter()
        .map(|b| config.work_ms + b.measured_ms)
        .collect();
    let (repo, list) = (trunk.clone(), tasks.to_vec());
    let agents = config.agents.max(1);
    let merges =
        tokio::task::spawn_blocking(move || merge_in_order(&repo, &list, &branches, agents))
            .await??;

    let wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(finish(merges, &work_ms, wall_ms))
}

/// The agents' simulated work: `agents` at a time, `work_ms` each.
async fn work_in_parallel(count: usize, config: OffConfig) -> Result<()> {
    let slots = Arc::new(Semaphore::new(config.agents.max(1)));
    let mut handles = Vec::new();
    for _ in 0..count {
        let slots = Arc::clone(&slots);
        handles.push(tokio::spawn(async move {
            let _slot = slots.acquire().await;
            tokio::time::sleep(std::time::Duration::from_millis(config.work_ms)).await;
        }));
    }
    for handle in handles {
        handle.await.context("a simulated agent panicked")?;
    }
    Ok(())
}

/// One commit per task, each on its own branch cut from the starting commit.
fn commit_branches(
    repo: &Git,
    base: &str,
    base_tree: &Tree,
    tasks: &[Task],
) -> Result<Vec<Branch>> {
    let mut out = Vec::new();
    for task in tasks {
        let started = Instant::now();
        let branch = format!("agent-{}", task.label());
        repo.run(&["checkout", "-q", "-B", &branch, base])?;
        git::write_tree(&repo.dir, &tasks::apply(task, base_tree)?)?;
        // A careful agent runs the tests before committing; `on` agents do the same.
        git::run_checks(&repo.dir)?;
        let sha = repo.commit_all(&task.intent())?;
        let measured_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        out.push(Branch {
            name: branch,
            sha,
            measured_ms,
        });
    }
    repo.run(&["checkout", "-q", "main"])?;
    Ok(out)
}

fn merge_in_order(
    repo: &Git,
    tasks: &[Task],
    branches: &[Branch],
    agents: usize,
) -> Result<Vec<(MergeRecord, String)>> {
    let mut out = Vec::new();
    for (task, branch) in tasks.iter().zip(branches) {
        let (merged, _) = repo.attempt(&["merge", "--no-ff", "--no-edit", "-q", &branch.name])?;
        let mut conflicted_files = Vec::new();
        let outcome = if merged {
            let outcome = match git::run_checks(&repo.dir)? {
                Checks::Pass { .. } => Outcome::Clean,
                Checks::BuildFailed => Outcome::BuildFailed,
                Checks::TestsFailed => Outcome::TestsFailed,
            };
            if outcome != Outcome::Clean {
                repo.run(&["reset", "-q", "--hard", "HEAD~1"])?;
            }
            outcome
        } else {
            let listing = repo.run(&["diff", "--name-only", "--diff-filter=U"])?;
            conflicted_files = listing.lines().map(str::to_string).collect();
            repo.run(&["merge", "--abort"])?;
            Outcome::TextualConflict
        };
        let record = MergeRecord {
            task: task.id,
            agent: agent_name(task.id % agents),
            outcome,
            conflicted_files,
        };
        out.push((record, branch.sha.clone()));
    }
    Ok(out)
}

fn finish(merged: Vec<(MergeRecord, String)>, work_ms: &[u64], wall_ms: u64) -> OffResult {
    let run = RunId("swarm-off-replay".into());
    let mut counts = Counts {
        clean: 0,
        textual_conflicts: 0,
        build_failed: 0,
        tests_failed: 0,
    };
    let mut events = Vec::new();
    let mut merges = Vec::new();
    let mut wasted_ms = 0;
    for (seq, (record, sha)) in merged.into_iter().enumerate() {
        if record.outcome != Outcome::Clean {
            wasted_ms += work_ms.get(seq).copied().unwrap_or(0);
        }
        match record.outcome {
            Outcome::Clean => counts.clean += 1,
            Outcome::TextualConflict => counts.textual_conflicts += 1,
            Outcome::BuildFailed => counts.build_failed += 1,
            Outcome::TestsFailed => counts.tests_failed += 1,
            Outcome::Inconclusive => {}
        }
        events.push(Event {
            seq: seq as u64,
            at_ms: wall_ms,
            run: run.clone(),
            kind: EventKind::ReplayMerged {
                agent: AgentId(record.agent.clone()),
                fork_commit: CommitId(sha),
                outcome: record.outcome,
            },
        });
        merges.push(record);
    }
    let summary = Summary::from_events(&events);
    OffResult {
        wall_ms,
        merges,
        counts,
        events,
        summary,
        work_ms: work_ms.iter().sum(),
        wasted_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::Kind;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn run(tasks: &[Task]) -> OffResult {
        let dir = scratch();
        let config = OffConfig {
            agents: 2,
            work_ms: 0,
        };
        tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(run_off(tasks, dir.path(), config))
            .unwrap()
    }

    fn body(id: usize, func: &str) -> Task {
        Task {
            id,
            func: func.into(),
            kind: Kind::Body,
        }
    }

    #[test]
    fn two_edits_to_one_return_line_conflict_textually() {
        let result = run(&[body(1, "unitPrice"), body(2, "unitPrice")]);
        let outcomes: Vec<Outcome> = result.merges.iter().map(|m| m.outcome).collect();
        assert_eq!(outcomes, [Outcome::Clean, Outcome::TextualConflict]);
        assert_eq!(result.merges[1].conflicted_files, ["src/pricing.ts"]);
        assert_eq!(
            (
                result.summary.replay_merges,
                result.summary.replay_conflicts
            ),
            (2, 1)
        );
    }

    #[test]
    fn a_signature_change_and_an_added_caller_break_the_tests_together() {
        let signature = Task {
            id: 1,
            func: "unitPrice".into(),
            kind: Kind::Signature,
        };
        let caller = Task {
            id: 2,
            func: "unitPrice".into(),
            kind: Kind::Add {
                host: "src/pricing.ts".into(),
                constant: 3,
            },
        };
        let result = run(&[signature, caller]);
        let outcomes: Vec<Outcome> = result.merges.iter().map(|m| m.outcome).collect();
        assert_eq!(outcomes, [Outcome::Clean, Outcome::TestsFailed]);
        assert_eq!(result.counts.tests_failed, 1);
        assert_eq!(result.summary.replay_conflicts, 1);
    }

    #[test]
    fn a_rename_and_an_added_caller_break_the_build() {
        let rename = Task {
            id: 1,
            func: "taxFor".into(),
            kind: Kind::Rename,
        };
        let caller = Task {
            id: 2,
            func: "taxFor".into(),
            kind: Kind::Add {
                host: "src/invoice.ts".into(),
                constant: 2,
            },
        };
        let result = run(&[rename, caller]);
        let outcomes: Vec<Outcome> = result.merges.iter().map(|m| m.outcome).collect();
        assert_eq!(outcomes, [Outcome::Clean, Outcome::BuildFailed]);
    }

    #[test]
    fn independent_edits_all_land_and_a_broken_merge_is_rolled_back() {
        let tasks = [body(1, "unitPrice"), body(2, "restock")];
        let result = run(&tasks);
        assert_eq!(result.counts.clean, 2);
        assert_eq!(result.wasted_ms, 0);
        let signature = Task {
            id: 1,
            func: "unitPrice".into(),
            kind: Kind::Signature,
        };
        let caller = Task {
            id: 2,
            func: "unitPrice".into(),
            kind: Kind::Add {
                host: "src/pricing.ts".into(),
                constant: 3,
            },
        };
        let after_break = run(&[signature, caller, body(3, "restock")]);
        assert_eq!(after_break.merges[2].outcome, Outcome::Clean);
    }
}

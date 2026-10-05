//! Mode `off`, the uncoordinated baseline: no claims. Everything here is local: the protocol has
//! no client message for `ReplayMerged`, so these numbers are never sent to a coordinator (a
//! client that wrote evidence events would break invariant 10).
//!
//! The model of the agents: `agents` of them work at the same time. Task `i` is started by the
//! agent that finished task `i - agents`, and that agent pulls the trunk first, so task `i`
//! branches from the trunk as it stood after tasks `1..=i - agents` were merged (tasks 1 to
//! `agents` branch from the starting commit). Agents do not see work still in flight, which is
//! what makes the merges conflict. Branches are built in parallel, one directory per agent slot.
//! Merging then runs in task order with plain git, and the tests run after each merge.
//!
//! A merge that breaks the build or the tests is rolled back, so every merge is judged on a green
//! trunk and one break is not counted again in the merges after it. For that reason this harness
//! does not measure how long main stayed green.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use tessel_coordinator::protocol::{AgentId, CommitId, Event, EventKind, Outcome, RunId, Summary};
use tokio::sync::{oneshot, watch};

use crate::demo;
use crate::git::{self, Checks, Git};
use crate::tasks::{self, Task};

#[derive(Debug, Clone, Copy)]
pub struct OffConfig {
    pub agents: usize,
    /// Time one agent spends on one task, before the measured edit, test and commit.
    pub work_ms: u64,
}

struct Branch {
    name: String,
    sha: String,
    slot: PathBuf,
    /// Merges done before this branch was cut.
    branched_after: usize,
    measured_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeRecord {
    pub task: usize,
    pub agent: String,
    pub outcome: Outcome,
    /// Tasks merged into the trunk before this task's branch was cut.
    pub branched_after_merge: usize,
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

/// Heads of the trunk: entry `k` is its head after `k` tasks were merged (or rolled back).
type Heads = watch::Receiver<Vec<String>>;

struct Job {
    task: Task,
    index: usize,
    agents: usize,
    work_ms: u64,
    trunk: PathBuf,
    slot: PathBuf,
    heads: Heads,
}

pub async fn run_off(tasks: &[Task], scratch: &Path, config: OffConfig) -> Result<OffResult> {
    let started = Instant::now();
    let agents = config.agents.max(1);
    let trunk = Git::new(&scratch.join("trunk"));
    std::fs::create_dir_all(&trunk.dir)?;
    let repo = trunk.clone();
    let base =
        tokio::task::spawn_blocking(move || git::init_repo(&repo, &demo::base_tree())).await??;
    let (heads_tx, heads) = watch::channel(vec![base]);
    let mut branches = Vec::new();
    let mut builders = Vec::new();
    for (index, task) in tasks.iter().enumerate() {
        let (tx, rx) = oneshot::channel();
        branches.push(rx);
        let job = Job {
            task: task.clone(),
            index,
            agents,
            work_ms: config.work_ms,
            trunk: trunk.dir.clone(),
            slot: scratch.join(format!("slot{}", index % agents)),
            heads: heads.clone(),
        };
        builders.push(tokio::spawn(build_one(job, tx)));
    }
    let merged = merge_all(&trunk, tasks, branches, &heads_tx, agents).await;
    drop(heads_tx);
    for builder in builders {
        builder.await.context("a simulated agent panicked")??;
    }
    let (merges, work_ms) = merged?;
    let wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(finish(merges, &work_ms, wall_ms))
}

/// One agent's task: wait until the trunk has the merges it would have pulled, work, then build
/// the branch.
async fn build_one(job: Job, done: oneshot::Sender<Branch>) -> Result<()> {
    let branched_after = (job.index + 1).saturating_sub(job.agents);
    let started = Instant::now();
    let sha = {
        let mut heads = job.heads.clone();
        let guard = heads
            .wait_for(|h| h.len() > branched_after)
            .await
            .map_err(|_| anyhow!("the trunk stopped before {branched_after} was merged"))?;
        guard[branched_after].clone()
    };
    tokio::time::sleep(Duration::from_millis(job.work_ms)).await;
    let mut branch = tokio::task::spawn_blocking(move || build_branch(&job, &sha)).await??;
    branch.branched_after = branched_after;
    branch.measured_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let _ = done.send(branch);
    Ok(())
}

/// Cuts a branch from `sha` in the agent's own directory, applies the task, runs the tests the
/// way a careful agent would (`on` agents do the same) and commits.
fn build_branch(job: &Job, sha: &str) -> Result<Branch> {
    std::fs::create_dir_all(&job.slot)?;
    let work = Git::new(&job.slot);
    if !job.slot.join(".git").exists() {
        work.run(&["init", "-q", "-b", "main"])?;
    }
    work.run(&["fetch", "-q", &job.trunk.to_string_lossy(), "main"])?;
    let name = format!("agent-{}", job.task.label());
    work.run(&["checkout", "-q", "-f", "-B", &name, sha])?;
    let tree = git::read_tree(&job.slot)?;
    git::write_tree(&job.slot, &tasks::apply(&job.task, &tree)?)?;
    git::run_checks(&job.slot)?;
    let sha = work.commit_all(&job.task.intent())?;
    Ok(Branch {
        name,
        sha,
        slot: job.slot.clone(),
        branched_after: 0,
        measured_ms: 0,
    })
}

/// Merges the branches in task order, publishing the trunk's head after each one.
async fn merge_all(
    trunk: &Git,
    tasks: &[Task],
    branches: Vec<oneshot::Receiver<Branch>>,
    heads: &watch::Sender<Vec<String>>,
    agents: usize,
) -> Result<(Vec<(MergeRecord, String)>, Vec<u64>)> {
    let mut merged = Vec::new();
    let mut work_ms = Vec::new();
    for (task, ready) in tasks.iter().zip(branches) {
        let branch = ready
            .await
            .map_err(|_| anyhow!("{} produced no branch", task.label()))?;
        work_ms.push(branch.measured_ms);
        let (repo, id) = (trunk.clone(), task.id);
        let (record, head) =
            tokio::task::spawn_blocking(move || merge_one(&repo, id, &branch, agents)).await??;
        heads.send_modify(|h| h.push(head));
        let sha = record.1;
        merged.push((record.0, sha));
    }
    Ok((merged, work_ms))
}

fn merge_one(
    repo: &Git,
    task: usize,
    branch: &Branch,
    agents: usize,
) -> Result<((MergeRecord, String), String)> {
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", branch.name);
    repo.run(&["fetch", "-q", &branch.slot.to_string_lossy(), &refspec])?;
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
        task,
        agent: agent_name(task % agents),
        outcome,
        branched_after_merge: branch.branched_after,
        conflicted_files,
    };
    let head = repo.run(&["rev-parse", "HEAD"])?;
    Ok(((record, branch.sha.clone()), head))
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
        run_with(tasks, 2, 0)
    }

    fn run_with(tasks: &[Task], agents: usize, work_ms: u64) -> OffResult {
        let dir = scratch();
        let config = OffConfig { agents, work_ms };
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

    #[test]
    fn each_task_branches_from_the_trunk_as_it_stood_agents_tasks_earlier() {
        let tasks: Vec<Task> = (1..=5).map(|id| body(id, "restock")).collect();
        let two = run_with(&tasks, 2, 0);
        let cut: Vec<usize> = two.merges.iter().map(|m| m.branched_after_merge).collect();
        assert_eq!(cut, [0, 0, 1, 2, 3]);
        let one = run_with(&tasks[..3], 1, 0);
        let cut: Vec<usize> = one.merges.iter().map(|m| m.branched_after_merge).collect();
        assert_eq!(cut, [0, 1, 2]);
        assert_eq!(
            one.counts.clean, 3,
            "an agent that pulls before each task never conflicts with itself"
        );
    }

    #[test]
    fn agents_work_at_the_same_time() {
        let tasks = [
            body(1, "restock"),
            body(2, "taxFor"),
            body(3, "available"),
            body(4, "loyaltyPoints"),
        ];
        let result = run_with(&tasks, 4, 1000);
        assert_eq!(result.counts.clean, 4);
        assert!(
            result.wall_ms < result.work_ms,
            "wall {} ms against {} ms of work in total: the branches were built one after another",
            result.wall_ms,
            result.work_ms
        );
    }
}

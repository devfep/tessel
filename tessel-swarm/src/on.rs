//! Mode `on`, the coordinated run. Scripted agents speak the real protocol over WebSocket: claim
//! before editing, respect a denial, commit, push to their fork, `Submit`, then wait for the
//! outcome. Every number about the coordinator comes from its event log through
//! `Summary::from_events`; the harness adds only what the log cannot hold (wall time and the
//! time agents spent on work that was later rejected).

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use tessel_coordinator::protocol::{
    uncovered, Assumption, ClaimId, ClientMsg, CommitId, DecisionRecord, Event, EventKind, Fence,
    Intent, OnConflict, ScopeClaim, ServerMsg, Summary,
};

use crate::conn::{read_log, Conn};
use crate::endpoint::Endpoint;
use crate::git::{self, Checks, Git};
use crate::tasks::{self, Kind, Task};

pub const REVIEWER: &str = "swarm-reviewer";
pub const OBSERVER: &str = "swarm-observer";

/// What an agent does when a claim is denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Policy {
    /// Queue behind the holder and take the claim when it is released.
    Wait,
    /// Put the task back and pick other work; try it again later.
    Skip,
}

#[derive(Debug, Clone)]
pub struct OnConfig {
    pub agents: usize,
    pub policy: Policy,
    /// Time an agent spends on a task between its claim and its commit.
    pub work_ms: u64,
    /// Longest an agent waits for a grant, and again for a merge.
    pub task_timeout: Duration,
    /// How often a skipped task may be denied before its agent gives up on it.
    pub max_denials: u32,
}

pub fn agent_names(count: usize) -> Vec<String> {
    (1..=count).map(|i| format!("a{i:02}")).collect()
}

/// Every name that needs an identity token: the agents, the reviewer and the observer.
pub fn principals(count: usize) -> Vec<String> {
    let mut names = agent_names(count);
    names.push(REVIEWER.to_string());
    names.push(OBSERVER.to_string());
    names
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Merged,
    /// The steward or the coordinator refused the submission.
    Rejected,
    /// Denied more often than `max_denials` allows.
    Starved,
    TimedOut,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskResult {
    pub task: usize,
    pub agent: String,
    pub result: Resolution,
    pub denials: u32,
    /// From the claim to the commit being pushed: the time that is lost if the work is rejected.
    pub work_ms: u64,
    /// Time spent queued or retrying before the claim was granted.
    pub waited_ms: u64,
    pub note: Option<String>,
}

pub struct OnResult {
    pub wall_ms: u64,
    pub events: Vec<Event>,
    /// Computed by `Summary::from_events` over the coordinator's own event log.
    pub summary: Summary,
    /// `SubmitRejected` events in the log. `Summary` has no field for them.
    pub rejected_in_log: u64,
    /// `WaitQueued` events: claims that queued behind a holder. `Summary` has no field for them.
    pub waits_in_log: u64,
    pub results: Vec<TaskResult>,
    pub work_ms_total: u64,
    pub wasted_ms: u64,
    pub waited_ms: u64,
}

struct Item {
    task: Task,
    denials: u32,
}

struct Ctx {
    endpoint: Endpoint,
    queue: Mutex<VecDeque<Item>>,
    results: Mutex<Vec<TaskResult>>,
    scratch: PathBuf,
    config: OnConfig,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub async fn run_on(
    endpoint: &Endpoint,
    tasks: &[Task],
    scratch: &Path,
    config: &OnConfig,
) -> Result<OnResult> {
    let queue = tasks
        .iter()
        .cloned()
        .map(|task| Item { task, denials: 0 })
        .collect();
    let ctx = Arc::new(Ctx {
        endpoint: endpoint.clone(),
        queue: Mutex::new(queue),
        results: Mutex::new(Vec::new()),
        scratch: scratch.to_path_buf(),
        config: config.clone(),
    });
    let reviewer = tokio::spawn(review_loop(Arc::clone(&ctx)));
    let started = Instant::now();
    let mut agents = Vec::new();
    for name in agent_names(config.agents) {
        agents.push(tokio::spawn(agent_main(Arc::clone(&ctx), name)));
    }
    let mut failure = None;
    for agent in agents {
        if let Err(error) = agent.await.context("an agent task panicked")? {
            failure.get_or_insert(error);
        }
    }
    let wall_ms = millis(started);
    reviewer.abort();
    if let Some(error) = failure {
        return Err(error);
    }
    let observer = endpoint.token_of(OBSERVER)?;
    let events = read_log(
        &endpoint.ws_url,
        observer,
        OBSERVER,
        Duration::from_secs(60),
    )
    .await?;
    Ok(summarize(events, take_results(&ctx), wall_ms))
}

fn take_results(ctx: &Ctx) -> Vec<TaskResult> {
    let mut results = std::mem::take(&mut *lock(&ctx.results));
    results.sort_by_key(|r| r.task);
    results
}

fn summarize(events: Vec<Event>, results: Vec<TaskResult>, wall_ms: u64) -> OnResult {
    let summary = Summary::from_events(&events);
    let rejected_in_log = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::SubmitRejected { .. }))
        .count() as u64;
    let waits_in_log = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::WaitQueued { .. }))
        .count() as u64;
    let rejected = |r: &&TaskResult| r.result == Resolution::Rejected;
    OnResult {
        wall_ms,
        summary,
        rejected_in_log,
        waits_in_log,
        work_ms_total: results.iter().map(|r| r.work_ms).sum(),
        wasted_ms: results.iter().filter(rejected).map(|r| r.work_ms).sum(),
        waited_ms: results.iter().map(|r| r.waited_ms).sum(),
        results,
        events,
    }
}

/// Approves every submission held for review, as a configured reviewer would. A review gate is
/// part of the system under test, so the harness answers it instead of skipping it.
async fn review_loop(ctx: Arc<Ctx>) -> Result<()> {
    let token = ctx.endpoint.token_of(REVIEWER)?;
    let mut conn = Conn::open(&ctx.endpoint.ws_url, token).await?;
    conn.hello(REVIEWER, "reviewer").await?;
    conn.send(&ClientMsg::Watch { from_seq: 0 }).await?;
    let mut decided: HashSet<ClaimId> = HashSet::new();
    loop {
        let Some(ServerMsg::Event { event }) = conn.recv(Duration::from_secs(3600)).await? else {
            continue;
        };
        let EventKind::ReviewRequested { claim, .. } = event.kind else {
            continue;
        };
        if decided.insert(claim) {
            let req = conn.next_req();
            let note = Some("scripted reviewer: approves every held submission".to_string());
            conn.send(&ClientMsg::Review {
                req,
                claim,
                approve: true,
                note,
            })
            .await?;
        }
    }
}

async fn agent_main(ctx: Arc<Ctx>, name: String) -> Result<()> {
    let dir = ctx.scratch.join(&name);
    std::fs::create_dir_all(&dir)?;
    let remote = ctx.endpoint.remote.clone();
    let (work, head) = tokio::task::spawn_blocking(move || remote.checkout(&dir)).await??;
    let token = ctx.endpoint.token_of(&name)?;
    let mut conn = Conn::open(&ctx.endpoint.ws_url, token).await?;
    conn.hello(&name, &head).await?;
    loop {
        let Some(mut item) = lock(&ctx.queue).pop_front() else {
            return Ok(());
        };
        let started = Instant::now();
        let step = run_task(&ctx, &work, &mut conn, &name, &item.task).await?;
        let waited_ms = millis(started).saturating_sub(step.work_ms);
        let work_ms = step.work_ms;
        match step.end {
            End::Denied => {
                item.denials += 1;
                if item.denials > ctx.config.max_denials {
                    record(
                        &ctx,
                        &name,
                        &item,
                        Resolution::Starved,
                        work_ms,
                        waited_ms,
                        None,
                    );
                } else {
                    lock(&ctx.queue).push_back(item);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            }
            End::Done(resolution, note) => {
                record(&ctx, &name, &item, resolution, work_ms, waited_ms, note);
                if resolution == Resolution::TimedOut {
                    return Ok(());
                }
            }
        }
    }
}

fn record(
    ctx: &Ctx,
    agent: &str,
    item: &Item,
    result: Resolution,
    work_ms: u64,
    waited_ms: u64,
    note: Option<String>,
) {
    lock(&ctx.results).push(TaskResult {
        task: item.task.id,
        agent: agent.to_string(),
        result,
        denials: item.denials,
        work_ms,
        waited_ms,
        note,
    });
}

enum End {
    /// The claim was refused and the agent should pick other work.
    Denied,
    Done(Resolution, Option<String>),
}

struct Step {
    end: End,
    work_ms: u64,
}

impl Step {
    fn done(resolution: Resolution, note: impl Into<String>, work_ms: u64) -> Self {
        Self {
            end: End::Done(resolution, Some(note.into())),
            work_ms,
        }
    }
}

/// A granted claim: what to present to amend, release or submit it.
struct Held {
    claim: ClaimId,
    fence: Fence,
    scopes: Vec<ScopeClaim>,
}

enum Grant {
    Granted(Held),
    Denied,
    TimedOut,
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(job).await?
}

async fn run_task(
    ctx: &Ctx,
    work: &Git,
    conn: &mut Conn,
    agent: &str,
    task: &Task,
) -> Result<Step> {
    let policy = ctx.config.policy;
    let timeout = ctx.config.task_timeout;
    let (tree, after) = checkout_and_edit(ctx, work, task).await?;
    let mut scopes = tasks::plan_claims(&tasks::touched(&tree, &after));
    scopes.extend(tasks::dependencies(task, &tree));
    let req = conn.next_req();
    let on_conflict = if policy == Policy::Wait {
        OnConflict::Wait
    } else {
        OnConflict::Fail
    };
    let intent = Intent {
        summary: task.intent(),
        task_ref: Some(task.label()),
        assumptions: assumptions_of(task, &tree),
    };
    conn.send(&ClientMsg::Claim {
        req,
        intent,
        scopes: scopes.clone(),
        on_conflict,
    })
    .await?;
    let mut held = match await_grant(conn, req, scopes, timeout).await? {
        Grant::Granted(held) => held,
        Grant::Denied => {
            return Ok(Step {
                end: End::Denied,
                work_ms: 0,
            })
        }
        Grant::TimedOut => return Ok(Step::done(Resolution::TimedOut, "no grant in time", 0)),
    };
    let granted = Instant::now();
    // Main may have moved while this agent waited: edit what is there now, not what was.
    let (tree, after) = checkout_and_edit(ctx, work, task).await?;
    let touched = tasks::touched(&tree, &after);
    if let Some(note) = ensure_covered(conn, &mut held, &touched).await? {
        release(conn, &held).await?;
        return Ok(Step::done(Resolution::Failed, note, millis(granted)));
    }
    tokio::time::sleep(Duration::from_millis(ctx.config.work_ms)).await;
    let sha = match commit_and_push(ctx, work, agent, &after, task).await {
        Ok(sha) => sha,
        Err(error) => {
            release(conn, &held).await?;
            return Ok(Step::done(
                Resolution::Failed,
                format!("{error:#}"),
                millis(granted),
            ));
        }
    };
    let work_ms = millis(granted);
    let end = submit_and_wait(conn, &held, &sha, touched, timeout).await?;
    Ok(Step {
        end: End::Done(end.0, end.1),
        work_ms,
    })
}

/// Brings the working directory to the trunk's head and applies the task to it.
async fn checkout_and_edit(
    ctx: &Ctx,
    work: &Git,
    task: &Task,
) -> Result<(crate::demo::Tree, crate::demo::Tree)> {
    let (remote, repo, task) = (ctx.endpoint.remote.clone(), work.clone(), task.clone());
    blocking(move || {
        remote.sync(&repo)?;
        let tree = git::read_tree(&repo.dir)?;
        let after = tasks::apply(&task, &tree)?;
        Ok((tree, after))
    })
    .await
}

fn assumptions_of(task: &Task, tree: &crate::demo::Tree) -> Vec<Assumption> {
    let Kind::Add { .. } = task.kind else {
        return Vec::new();
    };
    tasks::dependencies(task, tree)
        .into_iter()
        .map(|claim| Assumption {
            scope: claim.scope,
            statement: format!("{} keeps the signature it has now", task.func),
        })
        .collect()
}

async fn await_grant(
    conn: &mut Conn,
    req: tessel_coordinator::protocol::RequestId,
    scopes: Vec<ScopeClaim>,
    limit: Duration,
) -> Result<Grant> {
    let deadline = Instant::now() + limit;
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(Grant::TimedOut);
        };
        match conn.recv(left).await? {
            None => return Ok(Grant::TimedOut),
            Some(ServerMsg::Granted {
                req: r,
                claim,
                fence,
                ..
            }) if r == req => {
                return Ok(Grant::Granted(Held {
                    claim,
                    fence,
                    scopes,
                }));
            }
            Some(ServerMsg::Denied { req: r, .. }) if r == req => return Ok(Grant::Denied),
            Some(ServerMsg::Error { code, message, .. }) => {
                anyhow::bail!("claim refused ({code:?}): {message}");
            }
            Some(_) => {}
        }
    }
}

/// Amends the claim when the edit touches more than was claimed. `Some(reason)` when it could not.
async fn ensure_covered(
    conn: &mut Conn,
    held: &mut Held,
    touched: &[ScopeClaim],
) -> Result<Option<String>> {
    let missing = uncovered(&held.scopes, touched);
    if missing.is_empty() {
        return Ok(None);
    }
    let add = tasks::plan_claims(&missing);
    let req = conn.next_req();
    conn.send(&ClientMsg::Amend {
        req,
        claim: held.claim,
        fence: held.fence,
        add: add.clone(),
    })
    .await?;
    loop {
        match conn.recv(Duration::from_secs(30)).await? {
            Some(ServerMsg::Granted { req: r, fence, .. }) if r == req => {
                held.fence = fence;
                held.scopes.extend(add);
                return Ok(None);
            }
            Some(ServerMsg::Denied { req: r, .. }) if r == req => {
                return Ok(Some("the amended scopes are held by another agent".into()));
            }
            Some(ServerMsg::Error { code, message, .. }) => {
                return Ok(Some(format!("amend refused ({code:?}): {message}")));
            }
            Some(_) => {}
            None => return Ok(Some("no answer to the amend".into())),
        }
    }
}

async fn release(conn: &mut Conn, held: &Held) -> Result<()> {
    let req = Some(conn.next_req());
    conn.send(&ClientMsg::Release {
        claim: held.claim,
        fence: held.fence,
        req,
    })
    .await
}

/// Writes the edit, checks it the way a careful agent would, commits and pushes it to the fork.
async fn commit_and_push(
    ctx: &Ctx,
    work: &Git,
    agent: &str,
    after: &crate::demo::Tree,
    task: &Task,
) -> Result<String> {
    let (remote, repo, tree, name, intent) = (
        ctx.endpoint.remote.clone(),
        work.clone(),
        after.clone(),
        agent.to_string(),
        task.intent(),
    );
    blocking(move || {
        git::write_tree(&repo.dir, &tree)?;
        match git::run_checks(&repo.dir)? {
            Checks::Pass { .. } => {}
            Checks::BuildFailed | Checks::TestsFailed => {
                anyhow::bail!("the agent's own checks failed on the checkout it edited");
            }
        }
        let sha = repo.commit_all(&intent)?;
        remote.push(&repo, &name)?;
        Ok(sha)
    })
    .await
}

async fn submit_and_wait(
    conn: &mut Conn,
    held: &Held,
    sha: &str,
    touched: Vec<ScopeClaim>,
    limit: Duration,
) -> Result<(Resolution, Option<String>)> {
    let req = conn.next_req();
    let decisions = DecisionRecord {
        evidence: vec!["node --test passed on the checkout this change was made on".to_string()],
        ..DecisionRecord::default()
    };
    conn.send(&ClientMsg::Submit {
        req,
        claim: held.claim,
        fence: held.fence,
        fork_commit: CommitId(sha.to_string()),
        touched,
        decisions,
    })
    .await?;
    let deadline = Instant::now() + limit;
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Ok((
                Resolution::TimedOut,
                Some("no merge outcome in time".into()),
            ));
        };
        match conn.recv(left).await? {
            None => {
                return Ok((
                    Resolution::TimedOut,
                    Some("no merge outcome in time".into()),
                ))
            }
            Some(ServerMsg::Merged { claim, .. }) if claim == held.claim => {
                return Ok((Resolution::Merged, None));
            }
            Some(ServerMsg::SubmitRejected { claim, reason }) if claim == held.claim => {
                // A rejected claim stays open with its fence; free its scopes for the others.
                release(conn, held).await?;
                return Ok((Resolution::Rejected, Some(reason)));
            }
            Some(ServerMsg::Uncovered { claim, .. }) if claim == held.claim => {
                release(conn, held).await?;
                return Ok((
                    Resolution::Rejected,
                    Some("submission touched unclaimed scopes".into()),
                ));
            }
            Some(ServerMsg::Error { code, message, .. }) => {
                return Ok((
                    Resolution::Failed,
                    Some(format!("submit refused ({code:?}): {message}")),
                ));
            }
            Some(_) => {}
        }
    }
}

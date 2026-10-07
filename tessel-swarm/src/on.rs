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
use crate::events;
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
    /// Claim with `OnConflict::Shadow`: when denied, do the work in the agent's own fork, push
    /// and submit it for verification only, and never expect a merge. The run then waits for the
    /// coordinator to try that work against what blocked it. Experiment runs only.
    Shadow,
}

#[derive(Debug, Clone)]
pub struct OnConfig {
    pub agents: usize,
    pub policy: Policy,
    /// Time an agent spends on a task between its claim and its commit.
    pub work_ms: u64,
    /// Longest an agent waits for a grant, and again for a merge. Under the shadow policy it is
    /// also the longest the run waits, once the agents are done, for shadow trials to be logged.
    pub task_timeout: Duration,
    /// Under the shadow policy: longest a shadow agent waits for the trial of its own claim before
    /// it takes another task. The run still waits up to `task_timeout` at the end.
    pub trial_wait: Duration,
    /// How often a skipped task may be denied before its agent gives up on it.
    pub max_denials: u32,
    /// Answer every submission held for review with an approval. On by default, for the local
    /// target and the swarm coordinator, where it is the only reviewer.
    pub scripted_reviewer: bool,
}

pub fn agent_names(count: usize) -> Vec<String> {
    (1..=count).map(|i| format!("a{i:02}")).collect()
}

/// Every name that needs an identity token: the agents, the observer and, when one is scripted,
/// the reviewer.
pub fn principals(count: usize, scripted_reviewer: bool) -> Vec<String> {
    let mut names = agent_names(count);
    if scripted_reviewer {
        names.push(REVIEWER.to_string());
    }
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
    /// No agent was left to take it: every agent had stopped.
    NotRun,
    /// Denied under the shadow policy: worked and submitted for verification, never to merge.
    Shadowed,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskResult {
    pub task: usize,
    pub agent: String,
    pub result: Resolution,
    pub denials: u32,
    /// From the claim to the commit being pushed: the time that is lost if the work is rejected.
    pub work_ms: u64,
    /// Time from sending a claim to its answer (a grant, a denial or a timeout), summed over the
    /// attempts. Time spent preparing the claim and backing off between attempts is not in it.
    pub waited_ms: u64,
    pub note: Option<String>,
}

/// What the log says about shadow claims. Zero everywhere unless the shadow policy ran.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ShadowTrials {
    /// `ClaimShadowed` events.
    pub claims: u64,
    /// `DenialVerified` events the trial could not judge. They count nothing.
    pub inconclusive: u64,
    /// Shadow claims with no `DenialVerified` event: no trial ran for them.
    pub never_verified: u64,
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
    /// Approvals in the log. The only reviewer is the script.
    pub reviews_approved: u64,
    /// Rejections in the log.
    pub reviews_rejected: u64,
    /// Claims flagged for review that were never decided.
    pub reviews_held: u64,
    pub scripted_reviewer: bool,
    pub shadow_trials: ShadowTrials,
    pub results: Vec<TaskResult>,
    pub work_ms_total: u64,
    pub wasted_ms: u64,
    pub waited_ms: u64,
}

struct Item {
    task: Task,
    denials: u32,
    waited_ms: u64,
}

struct Ctx {
    endpoint: Endpoint,
    queue: Mutex<VecDeque<Item>>,
    results: Mutex<Vec<TaskResult>>,
    scratch: PathBuf,
    config: OnConfig,
    /// The log as the shared observer has read it so far: shadow policy only.
    log: Option<tokio::sync::watch::Receiver<Vec<Event>>>,
    /// Shadow claims the coordinator answered with `Accepted`: the log must show each one's
    /// `Submitted` before the final wait may conclude that nothing is owed.
    accepted_shadows: Mutex<Vec<ClaimId>>,
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
        .map(|task| Item {
            task,
            denials: 0,
            waited_ms: 0,
        })
        .collect();
    let (log, watcher) = if config.policy == Policy::Shadow {
        let (tx, rx) = tokio::sync::watch::channel(Vec::new());
        (
            Some(rx),
            Some(tokio::spawn(watch_log(endpoint.clone(), tx))),
        )
    } else {
        (None, None)
    };
    let ctx = Arc::new(Ctx {
        endpoint: endpoint.clone(),
        queue: Mutex::new(queue),
        results: Mutex::new(Vec::new()),
        scratch: scratch.to_path_buf(),
        config: config.clone(),
        log,
        accepted_shadows: Mutex::new(Vec::new()),
    });
    let (stop_reviewer, stopped) = tokio::sync::watch::channel(false);
    let reviewer = config
        .scripted_reviewer
        .then(|| tokio::spawn(review_loop(Arc::clone(&ctx), stopped)));
    let started = Instant::now();
    let mut agents = Vec::new();
    for name in agent_names(config.agents) {
        agents.push(tokio::spawn(agent_main(Arc::clone(&ctx), name)));
    }
    let mut failure = None;
    for agent in agents {
        let ended = agent
            .await
            .context("an agent task panicked")
            .and_then(|r| r);
        if let Err(error) = ended {
            failure.get_or_insert(error);
        }
    }
    let wall_ms = millis(started);
    record_unrun(&ctx);
    let verified = if config.policy == Policy::Shadow && failure.is_none() {
        await_verification(&ctx).await
    } else {
        Ok(())
    };
    let watched = settle_watcher(watcher).await;
    let _ = stop_reviewer.send(true);
    let reviewed = settle_reviewer(reviewer).await;
    let outcome = failure.map_or(verified, Err);
    let outcome = with_cause(outcome, reviewed);
    with_cause(outcome, watched)?;
    let observer = endpoint.token_of(OBSERVER)?;
    let events = read_log(
        &endpoint.ws_url,
        observer,
        OBSERVER,
        Duration::from_secs(60),
    )
    .await?;
    Ok(summarize(
        events,
        take_results(&ctx),
        wall_ms,
        config.scripted_reviewer,
    ))
}

/// Waits, on the shared log watch, until every shadow trial the log owes has been recorded, or
/// `task_timeout` has passed. Whatever is still owed then stays out of the counts: the report says
/// it never ran. An empty log is a watch that has not caught up, not a log that owes nothing.
async fn await_verification(ctx: &Ctx) -> Result<()> {
    let Some(mut log) = ctx.log.clone() else {
        return Ok(());
    };
    let accepted = lock(&ctx.accepted_shadows).clone();
    let owed_nothing = |events: &Vec<Event>| verification_settled(events, &accepted);
    let waited = tokio::time::timeout(ctx.config.task_timeout, log.wait_for(owed_nothing)).await;
    match waited {
        Ok(Ok(_)) | Err(_) => Ok(()),
        Ok(Err(_)) => anyhow::bail!("the log watch stopped before the shadow trials were logged"),
    }
}

/// True when the log holds the `Submitted` of every accepted shadow claim and owes no trial. An
/// accepted claim missing from the log is a watch that has not caught up, not a log that owes
/// nothing.
fn verification_settled(events: &[Event], accepted: &[ClaimId]) -> bool {
    let submitted = |claim: &ClaimId| {
        events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::Submitted { claim: c, .. } if c == claim))
    };
    accepted.iter().all(submitted) && events::awaiting_verification(events) == 0
}

/// Ends the shared watcher. One that already stopped (a seq gap, or no way to reconnect) has an
/// error that is returned, because counts read from the log it left behind cannot be trusted.
/// Awaiting after `abort` still yields the real result of a task that had already finished.
async fn settle_watcher(watcher: Option<tokio::task::JoinHandle<Result<()>>>) -> Result<()> {
    let Some(watcher) = watcher else {
        return Ok(());
    };
    watcher.abort();
    match watcher.await {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(anyhow::Error::new(error).context("the log watcher panicked")),
    }
}

/// Waits for the scripted reviewer, which ends when told to stop. Its error is returned rather
/// than propagated, so the caller can still report the watcher's.
async fn settle_reviewer(reviewer: Option<tokio::task::JoinHandle<Result<()>>>) -> Result<()> {
    let Some(reviewer) = reviewer else {
        return Ok(());
    };
    reviewer.await.context("the reviewer task panicked")?
}

/// The run's outcome with a helper's error as its root cause. An agent or the final wait often
/// fails only because the log watcher or the reviewer died, so that error leads and the other one
/// is kept as context.
fn with_cause(outcome: Result<()>, cause: Result<()>) -> Result<()> {
    match (outcome, cause) {
        (outcome, Ok(())) => outcome,
        (Ok(()), Err(cause)) => Err(cause),
        (Err(error), Err(cause)) => Err(cause.context(format!("{error:#}"))),
    }
}

/// One connection that only follows the log: the run's single observer. It replays from seq 0
/// once, appends every event to a log it keeps gap-free, and publishes the log on `tx`; waiters
/// read that instead of connecting themselves, so waiting adds no load or log entries to the
/// coordinator being measured. A dropped connection is reopened from the last seq it has; a gap
/// ends the watch with an error, which wakes every waiter.
async fn watch_log(endpoint: Endpoint, tx: tokio::sync::watch::Sender<Vec<Event>>) -> Result<()> {
    let token = endpoint.token_of(OBSERVER)?;
    let mut failures = 0_u32;
    loop {
        let before = tx.borrow().len();
        match follow_log(&endpoint.ws_url, token, &tx).await {
            Followed::Gap(error) => return Err(error),
            Followed::Closed(error) => {
                failures = if tx.borrow().len() > before {
                    0
                } else {
                    failures + 1
                };
                if failures > 5 {
                    return Err(error.context("the log watch could not reconnect"));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

enum Followed {
    /// The connection ended; the watch may reopen it.
    Closed(anyhow::Error),
    /// The coordinator's log skipped a seq. Fatal.
    Gap(anyhow::Error),
}

async fn follow_log(
    url: &str,
    token: &crate::endpoint::Token,
    tx: &tokio::sync::watch::Sender<Vec<Event>>,
) -> Followed {
    let mut conn = match Conn::open(url, token).await {
        Ok(conn) => conn,
        Err(error) => return Followed::Closed(error),
    };
    let from_seq = tx.borrow().len() as u64;
    let started = async {
        conn.hello(OBSERVER, "observer").await?;
        conn.send(&ClientMsg::Watch { from_seq }).await
    };
    if let Err(error) = started.await {
        return Followed::Closed(error);
    }
    loop {
        let event = match conn.recv(Duration::from_secs(3600)).await {
            Ok(Some(ServerMsg::Event { event })) => event,
            Ok(Some(_) | None) => continue,
            Err(error) => return Followed::Closed(error),
        };
        let mut appended = Ok(false);
        tx.send_modify(|log| appended = events::append(log, event));
        if let Err(error) = appended {
            return Followed::Gap(error);
        }
    }
}

/// Tasks still queued when the last agent stopped (a timeout ends an agent) are not finished;
/// they are recorded so that every task is accounted for.
fn record_unrun(ctx: &Ctx) {
    let left: Vec<Item> = lock(&ctx.queue).drain(..).collect();
    for item in left {
        let note = Some("no agent was left to take it".to_string());
        record(ctx, "none", &item, Resolution::NotRun, 0, note);
    }
}

fn take_results(ctx: &Ctx) -> Vec<TaskResult> {
    let mut results = std::mem::take(&mut *lock(&ctx.results));
    results.sort_by_key(|r| r.task);
    results
}

fn summarize(
    events: Vec<Event>,
    results: Vec<TaskResult>,
    wall_ms: u64,
    scripted_reviewer: bool,
) -> OnResult {
    let summary = Summary::from_events(&events);
    let counts = events::count(&events);
    let results = settled(results, &counts);
    let rejected = |r: &&TaskResult| r.result == Resolution::Rejected;
    OnResult {
        wall_ms,
        summary,
        rejected_in_log: counts.rejected,
        waits_in_log: counts.waits,
        reviews_approved: counts.approvals,
        reviews_rejected: counts.review_rejections,
        reviews_held: counts.held_for_review,
        scripted_reviewer,
        shadow_trials: ShadowTrials {
            claims: counts.shadow_claims,
            inconclusive: counts.shadow_inconclusive,
            never_verified: counts.shadow_unverified,
        },
        work_ms_total: results.iter().map(|r| r.work_ms).sum(),
        wasted_ms: results.iter().filter(rejected).map(|r| r.work_ms).sum(),
        waited_ms: results.iter().map(|r| r.waited_ms).sum(),
        results,
        events,
    }
}

/// An agent that stops waiting leaves its submission with the coordinator, which may still decide
/// it before the log is read. The log is the record, so a timed-out task whose claim the log
/// shows as merged or rejected is counted as that, and the table cannot count it twice.
fn settled(results: Vec<TaskResult>, counts: &events::LogCounts) -> Vec<TaskResult> {
    let mut results = results;
    for r in results
        .iter_mut()
        .filter(|r| r.result == Resolution::TimedOut)
    {
        let label = format!("t{:02}", r.task);
        let (to, note) = if counts.merged_task_refs.contains(&label) {
            (Resolution::Merged, "merged after the agent stopped waiting")
        } else if counts.rejected_task_refs.contains(&label) {
            (
                Resolution::Rejected,
                "rejected after the agent stopped waiting",
            )
        } else {
            continue;
        };
        r.result = to;
        r.note = Some(note.to_string());
    }
    results
}

/// Approves every submission held for review. It runs on the local target and on the swarm
/// coordinator (live included), where it is the only reviewer. It ends when told to stop, so its
/// errors surface.
async fn review_loop(ctx: Arc<Ctx>, mut stop: tokio::sync::watch::Receiver<bool>) -> Result<()> {
    let token = ctx.endpoint.token_of(REVIEWER)?;
    let mut conn = Conn::open(&ctx.endpoint.ws_url, token).await?;
    conn.hello(REVIEWER, "reviewer").await?;
    conn.send(&ClientMsg::Watch { from_seq: 0 }).await?;
    let mut decided: HashSet<ClaimId> = HashSet::new();
    loop {
        let message = tokio::select! {
            _ = stop.changed() => return Ok(()),
            message = conn.recv(Duration::from_secs(3600)) => message?,
        };
        let Some(ServerMsg::Event { event }) = message else {
            continue;
        };
        let Some(claim) = events::review_requested(&event.kind) else {
            continue;
        };
        if decided.insert(claim) {
            let req = conn.next_req();
            let note = Some("scripted reviewer".to_string());
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
        let step = run_task(&ctx, &work, &mut conn, &name, &item.task).await?;
        item.waited_ms += step.waited_ms;
        let work_ms = step.work_ms;
        match step.end {
            End::Denied => {
                item.denials += 1;
                if item.denials > ctx.config.max_denials {
                    record(&ctx, &name, &item, Resolution::Starved, work_ms, None);
                } else {
                    lock(&ctx.queue).push_back(item);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            }
            End::Done(resolution, note) => {
                record(&ctx, &name, &item, resolution, work_ms, note);
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
    note: Option<String>,
) {
    lock(&ctx.results).push(TaskResult {
        task: item.task.id,
        agent: agent.to_string(),
        result,
        denials: item.denials,
        work_ms,
        waited_ms: item.waited_ms,
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
    waited_ms: u64,
}

impl Step {
    fn done(resolution: Resolution, note: impl Into<String>, work_ms: u64) -> Self {
        Self {
            end: End::Done(resolution, Some(note.into())),
            work_ms,
            waited_ms: 0,
        }
    }

    fn waited(mut self, waited_ms: u64) -> Self {
        self.waited_ms += waited_ms;
        self
    }
}

/// A granted claim: what to present to amend, release or submit it.
struct Held {
    claim: ClaimId,
    fence: Fence,
    scopes: Vec<ScopeClaim>,
    /// A shadow claim: it places no lock, and its submission is never merged.
    shadow: bool,
}

enum Grant {
    /// Granted, or shadowed (`Held::shadow`): the agent may work either way.
    Granted(Held),
    Denied,
    TimedOut,
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(job).await?
}

/// The part of an agent's task that is the same for every claim it holds: the checkout, the
/// agent that works on it and the task.
struct Job<'a> {
    work: &'a Git,
    agent: &'a str,
    task: &'a Task,
}

async fn run_task(
    ctx: &Ctx,
    work: &Git,
    conn: &mut Conn,
    agent: &str,
    task: &Task,
) -> Result<Step> {
    let (req, scopes) = send_claim(ctx, work, conn, task).await?;
    let claimed = Instant::now();
    let answer = await_grant(conn, req, scopes, ctx.config.task_timeout).await?;
    let waited_ms = millis(claimed);
    let held = match answer {
        Grant::Granted(held) => held,
        Grant::Denied => {
            return Ok(Step {
                end: End::Denied,
                work_ms: 0,
                waited_ms,
            })
        }
        Grant::TimedOut => {
            return Ok(Step::done(Resolution::TimedOut, "no grant in time", 0).waited(waited_ms))
        }
    };
    let job = Job { work, agent, task };
    let step = work_and_submit(ctx, &job, conn, held).await?;
    Ok(step.waited(waited_ms))
}

/// Plans the claim for the edit as the trunk stands and sends it.
async fn send_claim(
    ctx: &Ctx,
    work: &Git,
    conn: &mut Conn,
    task: &Task,
) -> Result<(tessel_coordinator::protocol::RequestId, Vec<ScopeClaim>)> {
    let (tree, after) = checkout_and_edit(ctx, work, task).await?;
    let mut scopes = tasks::plan_claims(&tasks::touched(&tree, &after));
    scopes.extend(tasks::dependencies(task, &tree));
    let req = conn.next_req();
    let on_conflict = match ctx.config.policy {
        Policy::Wait => OnConflict::Wait,
        Policy::Skip => OnConflict::Fail,
        Policy::Shadow => OnConflict::Shadow,
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
    Ok((req, scopes))
}

/// With a grant in hand: edits the trunk as it stands now, commits, pushes and submits. A shadow
/// claim skips the simulated work time, so that its submission is on record before the work that
/// blocked it can merge, and is then left open: releasing it would drop its verification.
async fn work_and_submit(ctx: &Ctx, job: &Job<'_>, conn: &mut Conn, held: Held) -> Result<Step> {
    let mut held = held;
    let timeout = ctx.config.task_timeout;
    let granted = Instant::now();
    // Main may have moved while this agent waited: edit what is there now, not what was.
    let (tree, after) = checkout_and_edit(ctx, job.work, job.task).await?;
    let touched = tasks::touched(&tree, &after);
    if let Some(note) = ensure_covered(conn, &mut held, &touched).await? {
        release(conn, &held).await?;
        return Ok(Step::done(Resolution::Failed, note, millis(granted)));
    }
    if !held.shadow {
        tokio::time::sleep(Duration::from_millis(ctx.config.work_ms)).await;
    }
    // A shadow agent submits first and spends its work time afterwards (`finish_shadow`).
    let pushed = commit_and_push(ctx, job.work, job.agent, &after, job.task).await;
    let sha = match pushed {
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
    if held.shadow {
        let pushed = Pushed {
            sha,
            touched,
            granted,
        };
        return finish_shadow(ctx, conn, &held, pushed).await;
    }
    let work_ms = millis(granted);
    let end = submit_and_wait(conn, &held, &sha, touched, timeout).await?;
    Ok(Step {
        end: End::Done(end.0, end.1),
        work_ms,
        waited_ms: 0,
    })
}

/// A shadow agent's commit, pushed to its fork, and when its claim was answered.
struct Pushed {
    sha: String,
    touched: Vec<ScopeClaim>,
    granted: Instant,
}

/// Submits the shadow work at once, so that it is on record before the work that blocked it can
/// merge, then spends the task's work time like any agent. It then waits for the trial of its own
/// claim before it returns: the next task force-pushes this agent's fork, and the trial needs the
/// commit that was submitted to still be there. The wait is counted as waiting, not as work.
async fn finish_shadow(ctx: &Ctx, conn: &mut Conn, held: &Held, pushed: Pushed) -> Result<Step> {
    let timeout = ctx.config.task_timeout;
    let Pushed {
        sha,
        touched,
        granted,
    } = pushed;
    let (resolution, note) = submit_shadow(conn, held, &sha, touched, timeout).await?;
    if resolution != Resolution::Shadowed {
        let note = note.unwrap_or_default();
        return Ok(Step::done(resolution, note, millis(granted)));
    }
    lock(&ctx.accepted_shadows).push(held.claim);
    tokio::time::sleep(Duration::from_millis(ctx.config.work_ms)).await;
    let work_ms = millis(granted);
    let waiting = Instant::now();
    await_own_trial(ctx, held.claim).await?;
    Ok(Step {
        end: End::Done(resolution, note),
        work_ms,
        waited_ms: millis(waiting),
    })
}

/// Waits, on the shared log watch, until nothing is owed to `claim` any more: its trial is logged,
/// or none can run (its blocker ended unmerged, or merged before it submitted). It gives up after
/// `trial_wait`; whatever is still owed then stays owed and the report counts it as never
/// verified. The log must hold the claim's own submission first: until the watch has caught up
/// with it, an empty log would owe nothing.
async fn await_own_trial(ctx: &Ctx, claim: ClaimId) -> Result<()> {
    let Some(mut log) = ctx.log.clone() else {
        return Ok(());
    };
    let settled = |events: &Vec<Event>| trial_settled(events, claim);
    let waited = tokio::time::timeout(ctx.config.trial_wait, log.wait_for(settled)).await;
    match waited {
        Ok(Ok(_)) | Err(_) => Ok(()),
        Ok(Err(_)) => anyhow::bail!("the log watch stopped before the trial was logged"),
    }
}

fn trial_settled(log: &[Event], claim: ClaimId) -> bool {
    let submitted = log
        .iter()
        .any(|e| matches!(&e.kind, EventKind::Submitted { claim: c, .. } if *c == claim));
    submitted && events::awaiting_verification_of(log, claim) == 0
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
                    shadow: false,
                }));
            }
            Some(ServerMsg::Shadowed {
                req: r,
                claim,
                fence,
                ..
            }) if r == req => {
                return Ok(Grant::Granted(Held {
                    claim,
                    fence,
                    scopes,
                    shadow: true,
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

async fn send_submit(
    conn: &mut Conn,
    held: &Held,
    sha: &str,
    touched: Vec<ScopeClaim>,
) -> Result<()> {
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
    .await
}

/// A shadow submission is recorded and never queued, so the only answer is `Accepted`.
async fn submit_shadow(
    conn: &mut Conn,
    held: &Held,
    sha: &str,
    touched: Vec<ScopeClaim>,
    limit: Duration,
) -> Result<(Resolution, Option<String>)> {
    send_submit(conn, held, sha, touched).await?;
    let deadline = Instant::now() + limit;
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Ok((Resolution::TimedOut, Some("no answer to the submit".into())));
        };
        match conn.recv(left).await? {
            None => return Ok((Resolution::TimedOut, Some("no answer to the submit".into()))),
            Some(ServerMsg::Accepted { claim, .. }) if claim == held.claim => {
                let note = "shadow: submitted for verification, never queued to merge";
                return Ok((Resolution::Shadowed, Some(note.into())));
            }
            Some(ServerMsg::Uncovered { claim, .. }) if claim == held.claim => {
                release(conn, held).await?;
                return Ok((
                    Resolution::Failed,
                    Some("shadow submission touched unclaimed scopes".into()),
                ));
            }
            Some(ServerMsg::Error { code, message, .. }) => {
                return Ok((
                    Resolution::Failed,
                    Some(format!("shadow submit refused ({code:?}): {message}")),
                ));
            }
            Some(_) => {}
        }
    }
}

async fn submit_and_wait(
    conn: &mut Conn,
    held: &Held,
    sha: &str,
    touched: Vec<ScopeClaim>,
    limit: Duration,
) -> Result<(Resolution, Option<String>)> {
    send_submit(conn, held, sha, touched).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_coordinator::protocol::RunId;

    fn timed_out(task: usize) -> TaskResult {
        TaskResult {
            task,
            agent: "a01".into(),
            result: Resolution::TimedOut,
            denials: 0,
            work_ms: 0,
            waited_ms: 0,
            note: None,
        }
    }

    #[test]
    fn a_log_the_watch_has_not_caught_up_with_settles_nothing() {
        assert!(
            !trial_settled(&[], ClaimId(2)),
            "an empty log owes nothing but proves nothing"
        );
    }

    fn submitted(seq: u64, claim: u64) -> Event {
        Event {
            seq,
            at_ms: 0,
            run: RunId("t".into()),
            kind: EventKind::Submitted {
                claim: ClaimId(claim),
                fork_commit: CommitId("c".repeat(40)),
                touched: Vec::new(),
                decisions: DecisionRecord::default(),
            },
        }
    }

    fn shadowed_by_holder() -> Vec<Event> {
        use tessel_coordinator::protocol::{AgentId, Conflict, Mode, Scope};
        let intent = || Intent {
            summary: "t01: x".into(),
            task_ref: Some("t01".into()),
            assumptions: Vec::new(),
        };
        let scope = ScopeClaim {
            scope: Scope::File {
                path: "src/a.ts".into(),
            },
            mode: Mode::EditBody,
        };
        let at = |seq, kind| Event {
            seq,
            at_ms: 0,
            run: RunId("t".into()),
            kind,
        };
        vec![
            at(
                0,
                EventKind::ClaimGranted {
                    agent: AgentId("holder".into()),
                    claim: ClaimId(1),
                    fence: Fence(1),
                    scopes: vec![scope.clone()],
                    intent: intent(),
                    race: None,
                    at_risk: Vec::new(),
                },
            ),
            at(
                1,
                EventKind::ClaimShadowed {
                    agent: AgentId("shadow".into()),
                    claim: ClaimId(2),
                    scopes: vec![scope.clone()],
                    conflicts: vec![Conflict {
                        requested: scope.clone(),
                        held: scope,
                        held_by: AgentId("holder".into()),
                        their_intent: intent(),
                        race: None,
                    }],
                },
            ),
        ]
    }

    #[test]
    fn a_trial_still_owed_keeps_the_log_unsettled() {
        let mut log = shadowed_by_holder();
        log.push(submitted(2, 2));
        assert!(!verification_settled(&log, &[ClaimId(2)]));
    }

    #[test]
    fn an_accepted_shadow_the_log_does_not_show_yet_is_not_settled() {
        let log = vec![submitted(0, 1)];
        assert!(!verification_settled(&log, &[ClaimId(1), ClaimId(2)]));
    }

    #[test]
    fn a_log_with_every_accepted_shadow_submitted_and_nothing_owed_is_settled() {
        let log = vec![submitted(0, 1), submitted(1, 2)];
        assert!(verification_settled(&log, &[ClaimId(1), ClaimId(2)]));
    }

    #[tokio::test]
    async fn a_watcher_that_stopped_with_an_error_fails_the_run() {
        let watcher = tokio::spawn(async { anyhow::bail!("the event log has a gap") });
        while !watcher.is_finished() {
            tokio::task::yield_now().await;
        }
        let error = settle_watcher(Some(watcher)).await.unwrap_err();
        assert!(format!("{error:#}").contains("gap"), "{error:#}");
    }

    #[tokio::test]
    async fn a_watcher_finished_with_an_error_reports_it() {
        let watcher = tokio::spawn(async { anyhow::bail!("the event log has a gap") });
        tokio::task::yield_now().await;
        let error = settle_watcher(Some(watcher)).await.unwrap_err();
        assert!(format!("{error:#}").contains("gap"), "{error:#}");
    }

    #[test]
    fn the_watchers_error_leads_and_the_failure_it_caused_is_kept() {
        let both = with_cause(
            Err(anyhow::anyhow!("the log watch stopped")),
            Err(anyhow::anyhow!("the event log has a gap")),
        )
        .unwrap_err();
        let message = format!("{both:#}");
        assert!(message.contains("the log watch stopped"), "{message}");
        assert!(message.ends_with("the event log has a gap"), "{message}");
        let alone = with_cause(Ok(()), Err(anyhow::anyhow!("gap"))).unwrap_err();
        assert_eq!(format!("{alone:#}"), "gap");
        let other = with_cause(Err(anyhow::anyhow!("agent")), Ok(())).unwrap_err();
        assert_eq!(format!("{other:#}"), "agent");
        with_cause(Ok(()), Ok(())).unwrap();
    }

    fn ctx_watching(
        accepted: Vec<ClaimId>,
        timeout: Duration,
    ) -> (Ctx, tokio::sync::watch::Sender<Vec<Event>>) {
        let (tx, rx) = tokio::sync::watch::channel(Vec::new());
        let ctx = Ctx {
            endpoint: Endpoint {
                ws_url: String::new(),
                tokens: std::collections::HashMap::new(),
                remote: crate::endpoint::Remote::Local {
                    trunk: PathBuf::new(),
                    forks: PathBuf::new(),
                },
            },
            queue: Mutex::new(VecDeque::new()),
            results: Mutex::new(Vec::new()),
            scratch: PathBuf::new(),
            config: OnConfig {
                agents: 1,
                policy: Policy::Shadow,
                work_ms: 0,
                task_timeout: timeout,
                trial_wait: timeout,
                max_denials: 1,
                scripted_reviewer: false,
            },
            log: Some(rx),
            accepted_shadows: Mutex::new(accepted),
        };
        (ctx, tx)
    }

    #[tokio::test]
    async fn an_accepted_claim_whose_submit_never_reaches_the_log_is_waited_for_until_the_timeout()
    {
        let timeout = Duration::from_millis(300);
        let (ctx, _tx) = ctx_watching(vec![ClaimId(2)], timeout);
        let started = Instant::now();
        await_verification(&ctx).await.unwrap();
        assert!(
            started.elapsed() >= timeout,
            "settled on a log without the submit"
        );
    }

    #[tokio::test]
    async fn a_watch_that_ended_before_an_accepted_submit_arrived_is_an_error() {
        let (ctx, tx) = ctx_watching(vec![ClaimId(2)], Duration::from_secs(30));
        drop(tx);
        let error = await_verification(&ctx).await.unwrap_err();
        assert!(error.to_string().contains("log watch stopped"), "{error}");
    }

    #[tokio::test]
    async fn a_log_holding_every_accepted_submit_settles_at_once() {
        let (ctx, tx) = ctx_watching(vec![ClaimId(2)], Duration::from_secs(30));
        tx.send_modify(|log| log.push(submitted(0, 2)));
        let started = Instant::now();
        await_verification(&ctx).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// A coordinator that answers the first submit it reads with `Accepted`.
    async fn accepting_coordinator() -> String {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let Ok(ClientMsg::Submit { req, claim, .. }) = serde_json::from_str(&text) else {
                    continue;
                };
                let accepted = ServerMsg::Accepted {
                    req,
                    claim,
                    queue_position: 0,
                };
                let reply = serde_json::to_string(&accepted).unwrap();
                socket.send(Message::text(reply)).await.unwrap();
            }
        });
        url
    }

    #[tokio::test]
    async fn a_shadow_submit_the_coordinator_accepted_is_owed_to_the_final_wait() {
        let url = accepting_coordinator().await;
        let token = crate::endpoint::Token::new("t".into());
        let mut conn = Conn::open(&url, &token).await.unwrap();
        let (ctx, _tx) = ctx_watching(Vec::new(), Duration::from_millis(100));
        let held = Held {
            claim: ClaimId(7),
            fence: Fence(1),
            scopes: Vec::new(),
            shadow: true,
        };
        let pushed = Pushed {
            sha: "c".repeat(40),
            touched: Vec::new(),
            granted: Instant::now(),
        };
        finish_shadow(&ctx, &mut conn, &held, pushed).await.unwrap();
        assert_eq!(*lock(&ctx.accepted_shadows), [ClaimId(7)]);
    }

    #[tokio::test]
    async fn a_watcher_still_following_is_stopped_without_error() {
        let watcher = tokio::spawn(std::future::pending::<Result<()>>());
        settle_watcher(Some(watcher)).await.unwrap();
        settle_watcher(None).await.unwrap();
    }

    #[test]
    fn a_timed_out_task_the_log_shows_decided_is_counted_as_decided() {
        let mut counts = events::LogCounts::default();
        counts.merged_task_refs.insert("t02".into());
        counts.rejected_task_refs.insert("t03".into());
        let settled = settled(vec![timed_out(2), timed_out(3), timed_out(4)], &counts);
        let got: Vec<Resolution> = settled.iter().map(|r| r.result).collect();
        assert_eq!(
            got,
            [
                Resolution::Merged,
                Resolution::Rejected,
                Resolution::TimedOut
            ]
        );
        assert!(settled[0]
            .note
            .as_deref()
            .is_some_and(|n| n.contains("stopped waiting")));
    }

    #[test]
    fn the_summary_counts_a_timed_out_task_once_when_the_log_shows_it_merged() {
        use tessel_coordinator::protocol::{AgentId, Fence, Intent};
        let at = |seq, kind| Event {
            seq,
            at_ms: 0,
            run: RunId("t".into()),
            kind,
        };
        let log = vec![
            at(
                0,
                EventKind::ClaimGranted {
                    agent: AgentId("a01".into()),
                    claim: ClaimId(1),
                    fence: Fence(1),
                    scopes: Vec::new(),
                    intent: Intent {
                        summary: "t02: x".into(),
                        task_ref: Some("t02".into()),
                        assumptions: Vec::new(),
                    },
                    race: None,
                    at_risk: Vec::new(),
                },
            ),
            at(
                1,
                EventKind::Merged {
                    claim: ClaimId(1),
                    head: CommitId("c".repeat(40)),
                },
            ),
        ];
        let result = summarize(log, vec![timed_out(2)], 10, false);
        assert_eq!(result.results[0].result, Resolution::Merged);
        assert_eq!(result.summary.merges, 1);
    }
}

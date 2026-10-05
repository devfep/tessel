//! The CLI commands. Each talks to the daemon over the Unix socket, except `start` (which
//! spawns it), `daemon` (which is it) and `inbox` (which reads files).

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command as Process, ExitCode, Stdio};
use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::json;
use tessel_coordinator::protocol::{ClaimId, Mode, ScopeClaim};

use crate::config::Config;
use crate::daemon;
use crate::hook::{self, Installed};
use crate::render::{needs_attention, notice_text, outcome_text, status_text};
use crate::rpc::{self, ClaimOutcome, ClientError, Reply, Request};
use crate::scope;
use crate::state::{self, Connection, State};
use crate::worktree::Worktree;
use crate::{Command, HookAction};

/// Exit code of `claim` when the coordinator denied it.
const EXIT_DENIED: u8 = 3;
/// Exit code of `claim --wait` when the request is queued.
const EXIT_QUEUED: u8 = 4;
/// Exit code of the hook when it blocks an edit.
const EXIT_BLOCK: u8 = 2;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const START_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run(command: Command) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    match command {
        Command::Start { summary, task } => start(&cwd, summary, task).await,
        Command::Claim {
            scopes,
            mode,
            wait,
            assume,
        } => claim(&cwd, &scopes, mode.into(), wait, assume).await,
        Command::Status { json } => status(&cwd, json).await,
        Command::Inbox { all } => inbox(&cwd, all),
        Command::Release { claim } => release(&cwd, claim).await,
        Command::Stop => stop(&cwd).await,
        Command::Hook {
            action: HookAction::PreEdit,
        } => pre_edit(&cwd).await,
        Command::Hook {
            action: HookAction::Install,
        } => install(&cwd),
        Command::Daemon { summary, task } => {
            let worktree = Worktree::discover(&cwd)?;
            let config = load_config(&worktree)?;
            daemon::run(
                worktree,
                config,
                daemon::Args {
                    summary,
                    task_ref: task,
                },
            )
            .await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn say(text: &str) {
    // A closed stdout (for example `| head`) is not an error worth reporting.
    let _ = std::io::stdout().write_all(text.as_bytes());
}

fn complain(text: &str) {
    let _ = std::io::stderr().write_all(text.as_bytes());
}

fn load_config(worktree: &Worktree) -> anyhow::Result<Config> {
    Ok(Config::load(&worktree.root, |name| {
        std::env::var(name).ok()
    })?)
}

/// Calls the daemon, turning "not running" into advice.
async fn call_daemon(worktree: &Worktree, request: &Request) -> anyhow::Result<Reply> {
    match rpc::call(&worktree.sock(), request, CALL_TIMEOUT).await {
        Ok(reply) => Ok(reply),
        Err(ClientError::NotRunning) => {
            bail!("no daemon is running for this worktree; run `tessel start \"<intent>\"` first")
        }
        Err(e) => Err(e.into()),
    }
}

// ---------- start ----------

async fn start(cwd: &Path, summary: String, task: Option<String>) -> anyhow::Result<ExitCode> {
    let worktree = Worktree::discover(cwd)?;
    load_config(&worktree)?;
    worktree.prepare_dir()?;
    if let Ok(Reply::Status { state }) =
        rpc::call(&worktree.sock(), &Request::Status, Duration::from_secs(2)).await
    {
        say(&format!(
            "daemon already running for this worktree (pid {}, agent {}, {})\n",
            state.pid,
            state.agent,
            connection_word(state.connection)
        ));
        return Ok(ExitCode::SUCCESS);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(worktree.log_path())
        .context("cannot open daemon.log")?;
    let exe = std::env::current_exe().context("cannot locate the tessel binary")?;
    let mut daemon = Process::new(exe);
    daemon.arg("daemon").arg("--summary").arg(&summary);
    if let Some(task) = &task {
        daemon.arg("--task").arg(task);
    }
    let mut child = daemon
        .current_dir(&worktree.root)
        .stdin(Stdio::null())
        .stdout(log.try_clone().context("cannot share daemon.log")?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .context("cannot start the daemon")?;
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            bail!(
                "the daemon exited ({status}) before connecting; last log lines:\n{}",
                log_tail(&worktree)
            );
        }
        if let Ok(Reply::Status { state }) =
            rpc::call(&worktree.sock(), &Request::Status, Duration::from_secs(1)).await
        {
            if state.connection == Connection::Online {
                say(&format!(
                    "started: agent {} is online on repo {} (pid {})\n",
                    state.agent, state.repo, state.pid
                ));
                return Ok(ExitCode::SUCCESS);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let reason = match State::read(&worktree) {
                Ok(Some(state)) => state.last_error.unwrap_or_else(|| "no answer yet".into()),
                Ok(None) | Err(_) => "no state written".to_string(),
            };
            bail!(
                "the daemon is running (pid {}) but not connected: {reason}",
                child.id()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn connection_word(connection: Connection) -> &'static str {
    match connection {
        Connection::Connecting => "connecting",
        Connection::Online => "online",
        Connection::Reconnecting => "reconnecting",
        Connection::Stopped => "stopped",
    }
}

fn log_tail(worktree: &Worktree) -> String {
    let mut text = String::new();
    if let Ok(mut file) = std::fs::File::open(worktree.log_path()) {
        let _ = file.read_to_string(&mut text);
    }
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(5)..].join("\n")
}

// ---------- claim / release / stop ----------

async fn claim(
    cwd: &Path,
    args: &[String],
    mode: Mode,
    wait: bool,
    assume: Vec<String>,
) -> anyhow::Result<ExitCode> {
    let worktree = Worktree::discover(cwd)?;
    let mut scopes = Vec::new();
    for arg in args {
        scopes.push(ScopeClaim {
            scope: scope::parse(arg)?,
            mode,
        });
    }
    let request = Request::Claim {
        scopes,
        wait,
        assumptions: assume,
    };
    let Reply::Claim { outcome } = call_daemon(&worktree, &request).await? else {
        bail!("the daemon answered with something unexpected");
    };
    let hint = format!("tessel claim {} --wait", args.join(" "));
    say(&outcome_text(&outcome, &hint));
    Ok(ExitCode::from(match outcome {
        ClaimOutcome::Granted { .. } | ClaimOutcome::Covered => 0,
        ClaimOutcome::Denied { .. } => EXIT_DENIED,
        ClaimOutcome::Queued { .. } => EXIT_QUEUED,
        ClaimOutcome::Refused { .. } => 1,
    }))
}

async fn release(cwd: &Path, claim: Option<u64>) -> anyhow::Result<ExitCode> {
    let worktree = Worktree::discover(cwd)?;
    let request = Request::Release {
        claim: claim.map(ClaimId),
    };
    match call_daemon(&worktree, &request).await? {
        Reply::Released { claims } if claims.is_empty() => {
            say("nothing to release\n");
            Ok(ExitCode::SUCCESS)
        }
        Reply::Released { claims } => {
            let ids: Vec<String> = claims.iter().map(|c| c.0.to_string()).collect();
            say(&format!("released claim(s) {}\n", ids.join(", ")));
            Ok(ExitCode::SUCCESS)
        }
        Reply::Failed { message } => bail!("{message}"),
        Reply::Status { .. } | Reply::Claim { .. } | Reply::Stopping => {
            bail!("the daemon answered with something unexpected")
        }
    }
}

async fn stop(cwd: &Path) -> anyhow::Result<ExitCode> {
    let worktree = Worktree::discover(cwd)?;
    match rpc::call(&worktree.sock(), &Request::Stop, CALL_TIMEOUT).await {
        Ok(_) => {}
        Err(ClientError::NotRunning) => {
            say("no daemon was running\n");
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => return Err(e.into()),
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while worktree.sock().exists() {
        if tokio::time::Instant::now() >= deadline {
            bail!("the daemon acknowledged the stop but is still shutting down");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    say("stopped: claims released, socket closed\n");
    Ok(ExitCode::SUCCESS)
}

// ---------- status / inbox ----------

async fn status(cwd: &Path, json: bool) -> anyhow::Result<ExitCode> {
    let worktree = Worktree::discover(cwd)?;
    let live = rpc::call(&worktree.sock(), &Request::Status, Duration::from_secs(5)).await;
    let (running, state) = match live {
        Ok(Reply::Status { state }) => (true, Some(*state)),
        Ok(_) | Err(_) => (false, State::read(&worktree)?),
    };
    let unread = state::unread_count(&worktree)?;
    if json {
        let doc = json!({ "daemon_running": running, "state": state, "unread_inbox": unread });
        say(&format!("{}\n", serde_json::to_string_pretty(&doc)?));
        return Ok(ExitCode::SUCCESS);
    }
    match state {
        Some(state) => say(&status_text(&state, running, unread)),
        None => say("daemon: not running; run `tessel start \"<intent>\"`\n"),
    }
    Ok(ExitCode::SUCCESS)
}

fn inbox(cwd: &Path, all: bool) -> anyhow::Result<ExitCode> {
    let worktree = Worktree::discover(cwd)?;
    let notices = state::take_inbox(&worktree, all)?;
    if notices.is_empty() {
        say("inbox empty\n");
    }
    for notice in &notices {
        let marker = if needs_attention(notice.kind) {
            "!"
        } else {
            " "
        };
        say(&format!("{marker} {}", notice_text(notice)));
    }
    Ok(ExitCode::SUCCESS)
}

// ---------- hook ----------

async fn pre_edit(cwd: &Path) -> anyhow::Result<ExitCode> {
    let mut stdin = String::new();
    std::io::stdin()
        .read_to_string(&mut stdin)
        .context("cannot read the hook input from stdin")?;
    let verdict = hook::pre_edit(&stdin, cwd).await;
    if verdict.allow {
        return Ok(ExitCode::SUCCESS);
    }
    complain(&verdict.message);
    Ok(ExitCode::from(EXIT_BLOCK))
}

fn install(cwd: &Path) -> anyhow::Result<ExitCode> {
    let worktree = Worktree::discover(cwd)?;
    let exe = std::env::current_exe().context("cannot locate the tessel binary")?;
    let word = match hook::install(&worktree, &exe)? {
        Installed::Added => "installed the PreToolUse hook",
        Installed::Updated => "updated the PreToolUse hook",
        Installed::AlreadyPresent => "the PreToolUse hook was already installed",
    };
    say(&format!("{word} in .claude/settings.local.json\n"));
    Ok(ExitCode::SUCCESS)
}

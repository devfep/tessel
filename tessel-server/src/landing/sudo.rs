//! The runner started through `sudo -u <account>` (docs/proposals/own-users.md, sections 2 and 3).
//!
//! The server never runs repo code itself. It starts the `tessel-runner` binary as the runner's own
//! account through a sudoers rule that allows exactly two commands: that binary with one fixed
//! configuration path, and `kill -KILL -1`. The request is JSON on the runner's standard input and
//! the result one JSON line on its standard output, as for the in-process runner.
//!
//! `kill -1` takes every process of the account, so one run may be live at a time: a gate covers
//! the run, the kill and the poll, across every repository of the server, which share one
//! `SudoRunner`. After each run, and at start, the account's processes are killed and `ps` is
//! polled until none is left. If one survives, the gate stays closed: a later run would share the
//! account with it, so no later run starts.
//!
//! The holder of the gate is this runner itself. It polls whether its call was given up, and on a
//! deadline it kills the account before it waits for `sudo`: `sudo` exits only after the kill, and
//! the thread that called it is not cancelled by the call's timeout.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use tessel_runner::{RunRequest, RunResult, RunnerError};

use super::ports::TestRunner;

/// The most bytes of the runner's reply the server reads. The runner bounds its own output; a
/// reply past this is not the runner's (rule 3).
const MAX_REPLY_BYTES: usize = 8 * 1024 * 1024;
/// How long to wait for the reply's stream to close once the runner has exited.
const REPLY_GRACE: Duration = Duration::from_secs(5);
const RUN_POLL: Duration = Duration::from_millis(20);
const PS_POLL: Duration = Duration::from_millis(50);
/// How long between two kills while processes are left.
const KILL_ROUND: Duration = Duration::from_secs(1);

/// Where the programs are, and who the runner is. Every path is absolute in production, because
/// the sudoers rule pins them.
#[derive(Debug, Clone)]
pub struct SudoRunnerSettings {
    /// The runner's account, `_tessel-runner`.
    pub user: String,
    pub sudo: PathBuf,
    /// The `tessel-runner` binary, root-owned.
    pub runner: PathBuf,
    /// The configuration path the sudoers rule allows as the runner's only argument.
    pub runner_config: PathBuf,
    /// `/bin/kill`, run as the account with `-KILL -1`.
    pub kill: PathBuf,
    /// `ps`, run as the server with `-U <account> -o pid=`.
    pub ps: PathBuf,
    /// How long processes of the account may take to disappear after a kill.
    pub settle: Duration,
}

/// Why the runner could not be started.
#[derive(Debug, thiserror::Error)]
pub enum SudoError {
    #[error("cannot run {what}: {source}")]
    Run {
        what: &'static str,
        source: io::Error,
    },
    #[error(
        "sudo refused the kill ({0}); /etc/sudoers.d/tessel must allow it, without a password"
    )]
    SudoRefused(String),
    #[error("processes of {user} are left {secs} s after kill -KILL -1: {pids}")]
    Survivors {
        user: String,
        secs: u64,
        pids: String,
    },
}

/// The sudo-started runner, shared by every repository of the server.
pub struct SudoRunner {
    settings: SudoRunnerSettings,
    gate: Mutex<()>,
    closed: AtomicBool,
}

impl SudoRunner {
    /// Kills what an earlier server left running as the account, and checks that none is left. A
    /// sudo that refuses, or a survivor, is an error: the server must not start.
    pub fn new(settings: SudoRunnerSettings) -> Result<SudoRunner, SudoError> {
        let runner = SudoRunner {
            settings,
            gate: Mutex::new(()),
            closed: AtomicBool::new(false),
        };
        runner.kill_all()?;
        Ok(runner)
    }

    fn sudo(&self) -> Command {
        let mut command = Command::new(&self.settings.sudo);
        command
            .env_clear()
            .args(["-n", "-u", &self.settings.user, "--"]);
        command
    }

    /// Kills every process of the account and polls until none is left or `settle` has passed.
    fn kill_all(&self) -> Result<(), SudoError> {
        let deadline = Instant::now() + self.settings.settle;
        loop {
            self.kill_once()?;
            let round_end = (Instant::now() + KILL_ROUND).min(deadline);
            loop {
                let left = self.survivors()?;
                if left.is_empty() {
                    return Ok(());
                }
                if Instant::now() >= round_end {
                    if Instant::now() >= deadline {
                        return Err(SudoError::Survivors {
                            user: self.settings.user.clone(),
                            secs: self.settings.settle.as_secs(),
                            pids: left.join(" "),
                        });
                    }
                    break;
                }
                thread::sleep(PS_POLL);
            }
        }
    }

    fn kill_once(&self) -> Result<(), SudoError> {
        let output = self
            .sudo()
            .arg(&self.settings.kill)
            .args(["-KILL", "-1"])
            .stdin(Stdio::null())
            .output()
            .map_err(|source| SudoError::Run {
                what: "sudo",
                source,
            })?;
        // `kill -1` with nothing to kill exits non-zero, which is fine; a `sudo:` line is not.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(line) = stderr.lines().find(|line| line.starts_with("sudo:")) {
            return Err(SudoError::SudoRefused(line.chars().take(200).collect()));
        }
        Ok(())
    }

    /// The pids of the account's processes, from `ps`, which needs no privilege.
    fn survivors(&self) -> Result<Vec<String>, SudoError> {
        let output = Command::new(&self.settings.ps)
            .env_clear()
            .args(["-U", &self.settings.user, "-o", "pid="])
            .stdin(Stdio::null())
            .output()
            .map_err(|source| SudoError::Run { what: "ps", source })?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect())
    }

    fn run_locked(
        &self,
        request: &RunRequest,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<RunResult, RunnerError> {
        let text = serde_json::to_string(request).map_err(|_| unsupervised("request"))?;
        let mut child = self
            .sudo()
            .arg(&self.settings.runner)
            .arg("--config")
            .arg(&self.settings.runner_config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| RunnerError::Unsupervised {
                what: "sudo",
                kind: error.kind(),
            })?;
        let mut stdin = child.stdin.take();
        let writer = thread::spawn(move || {
            if let Some(stdin) = stdin.as_mut() {
                // A runner that refuses before it reads closes the pipe; that is not an error here.
                stdin.write_all(text.as_bytes()).ok();
            }
        });
        let stdout = child.stdout.take();
        let (sender, reply) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stdout) = stdout {
                stdout
                    .take(u64::try_from(MAX_REPLY_BYTES + 1).unwrap_or(u64::MAX))
                    .read_to_end(&mut bytes)
                    .ok();
            }
            sender.send(bytes).ok();
        });
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    self.kill_all().ok();
                    child.wait().ok();
                    return Err(RunnerError::Unsupervised {
                        what: "wait",
                        kind: error.kind(),
                    });
                }
            }
            if cancelled() {
                // The account first, then the wait: sudo exits only once its command is dead.
                self.kill_all().ok();
                child.wait().ok();
                return Err(unsupervised("cancelled"));
            }
            thread::sleep(RUN_POLL);
        };
        writer.join().ok();
        let bytes = reply.recv_timeout(REPLY_GRACE).unwrap_or_default();
        match status.code() {
            Some(0) if bytes.len() <= MAX_REPLY_BYTES => {
                serde_json::from_slice(&bytes).map_err(|_| unsupervised("reply"))
            }
            Some(0) => Err(unsupervised("reply too large")),
            Some(2) => Err(unsupervised("refused")),
            Some(3) => Err(unsupervised("unsupervised")),
            Some(_) | None => Err(unsupervised("sudo")),
        }
    }
}

fn unsupervised(what: &'static str) -> RunnerError {
    RunnerError::Unsupervised {
        what,
        kind: io::ErrorKind::Other,
    }
}

impl TestRunner for SudoRunner {
    fn run(&self, request: &RunRequest) -> Result<RunResult, RunnerError> {
        self.run_cancellable(request, &|| false)
    }

    fn run_cancellable(
        &self,
        request: &RunRequest,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<RunResult, RunnerError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(unsupervised("gate closed"));
        }
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if self.closed.load(Ordering::SeqCst) {
            return Err(unsupervised("gate closed"));
        }
        let outcome = self.run_locked(request, cancelled);
        if let Err(error) = self.kill_all() {
            // The run's own result stands; the next run would share the account with a survivor.
            tracing::error!(error = %error, "processes of the runner account survive; no more runs");
            self.closed.store(true, Ordering::SeqCst);
        }
        outcome
    }
}

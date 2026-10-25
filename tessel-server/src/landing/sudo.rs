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
//! polled until none is left. If one survives, or `ps` or the kill fails, the gate stays closed:
//! a later run would share the account with a survivor, so no later run starts.
//!
//! The holder of the gate is this runner itself. It polls whether its call was given up, and on a
//! deadline it kills the account before it waits for `sudo`: `sudo` exits only after the kill, and
//! the thread that called it is not cancelled by the call's timeout. Every wait is bounded.
//!
//! A call's directory in the handoff directory is private (0700) while the call is made. Only
//! while its own run holds the gate is it opened to the runner's group (0750), and it is closed
//! again before the gate is let go, so a run's code cannot read another call's tree.
//!
//! What this does not cover is launchd, cron and at: `ps` sees a job only while it runs.
//! `check-own-users.sh` denies cron and at to the account and probes `launchctl`; the design
//! records that the server does not (docs/proposals/own-users.md, section 3).

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use tessel_runner::{RunRequest, RunResult, RunnerError, MAX_FIRST_FAILURE_BYTES, MAX_TAIL_BYTES};

use super::names::{quote, MAX_LOGGED_CHARS};
use super::ports::TestRunner;

/// The most bytes of the runner's reply line the server reads. The runner bounds its own output;
/// a reply past this is not the runner's (rule 3).
const MAX_REPLY_BYTES: usize = 8 * 1024 * 1024;
/// The most bytes of the runner's standard error kept for the log.
const MAX_STDERR_BYTES: u64 = 2048;
/// How long to wait for the reply once the runner has exited.
const REPLY_GRACE: Duration = Duration::from_secs(5);
/// How long to wait for `sudo` to exit after the account was killed.
const EXIT_GRACE: Duration = Duration::from_secs(15);
const RUN_POLL: Duration = Duration::from_millis(20);
const PS_POLL: Duration = Duration::from_millis(50);
/// How long between two kills while processes are left.
const KILL_ROUND: Duration = Duration::from_secs(1);
const CLOSED: u32 = 0o700;
const OPEN_TO_GROUP: u32 = 0o750;

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
    /// Where each call's directory is; the one a request's tree lies in is opened for its run.
    pub handoff_dir: PathBuf,
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
    /// sudo that refuses, a `ps` that fails or a survivor is an error: the server must not start.
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

    /// The pids of the account's processes, from `ps`, which needs no privilege. `ps` exits 1 when
    /// nothing matches, which is the answer wanted; any message on its standard error, or an exit
    /// other than 0 and 1, is a failure and not an empty list.
    fn survivors(&self) -> Result<Vec<String>, SudoError> {
        let failed = |why: String| SudoError::Run {
            what: "ps",
            source: io::Error::other(why),
        };
        let output = Command::new(&self.settings.ps)
            .env_clear()
            .args(["-U", &self.settings.user, "-o", "pid="])
            .stdin(Stdio::null())
            .output()
            .map_err(|source| SudoError::Run { what: "ps", source })?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            return Err(failed(stderr.trim().chars().take(200).collect()));
        }
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(failed(format!("exit {:?}", output.status.code())));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect())
    }

    /// The call's own directory in the handoff directory, if the request's tree lies in one.
    /// `Ok(None)` when the tree is not in the handoff directory. A path below it that climbs out
    /// (a `..`, a root, a prefix) is refused: opening what it climbed to would open a directory
    /// that is not a call's.
    fn call_dir_of(&self, tree: &str) -> Result<Option<PathBuf>, RunnerError> {
        let Ok(inside) = Path::new(tree).strip_prefix(&self.settings.handoff_dir) else {
            return Ok(None);
        };
        let mut components = inside.components();
        match components.next() {
            None => Ok(None),
            Some(Component::Normal(first))
                if components.all(|c| matches!(c, Component::Normal(_))) =>
            {
                Ok(Some(self.settings.handoff_dir.join(first)))
            }
            Some(_) => Err(unsupervised("handoff")),
        }
    }

    fn set_mode(dir: &Path, mode: u32) -> io::Result<()> {
        fs::set_permissions(dir, fs::Permissions::from_mode(mode))
    }

    /// Waits for `child` to exit for at most `grace`.
    fn wait_bounded(child: &mut Child, grace: Duration) -> Option<ExitStatus> {
        let end = Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < end => thread::sleep(RUN_POLL),
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// Kills the account and waits for `sudo` to follow. A kill that fails, or a `sudo` that does
    /// not exit, closes the gate.
    fn stop_run(&self, child: &mut Child) {
        let killed = self.kill_all();
        let exited = Self::wait_bounded(child, EXIT_GRACE).is_some();
        if killed.is_err() || !exited {
            if let Err(error) = &killed {
                tracing::error!(error = %error, "cannot stop the runner's account; no more runs");
            }
            self.closed.store(true, Ordering::SeqCst);
        }
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
            .stderr(Stdio::piped())
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
        let reply = whole_reply(child.stdout.take());
        let said = bounded_stderr(child.stderr.take());
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    self.stop_run(&mut child);
                    return Err(RunnerError::Unsupervised {
                        what: "wait",
                        kind: error.kind(),
                    });
                }
            }
            if cancelled() {
                // The account first, then the wait: sudo exits only once its command is dead.
                self.stop_run(&mut child);
                return Err(unsupervised("cancelled"));
            }
            thread::sleep(RUN_POLL);
        };
        writer.join().ok();
        let bytes = reply.recv_timeout(REPLY_GRACE).unwrap_or_default();
        let stderr = said.recv_timeout(REPLY_GRACE).unwrap_or_default();
        match status.code() {
            Some(0) if bytes.len() <= MAX_REPLY_BYTES => serde_json::from_slice(&bytes)
                .map(bound)
                .map_err(|_| unsupervised("reply")),
            Some(0) => Err(unsupervised("reply too large")),
            Some(code @ (2 | 3)) => {
                tracing::warn!(
                    code,
                    said = %quote(&String::from_utf8_lossy(&stderr), MAX_LOGGED_CHARS),
                    "the runner did not run the tests"
                );
                Err(unsupervised(if code == 2 {
                    "refused"
                } else {
                    "unsupervised"
                }))
            }
            Some(_) | None => {
                tracing::warn!(
                    said = %quote(&String::from_utf8_lossy(&stderr), MAX_LOGGED_CHARS),
                    "sudo did not start the runner"
                );
                Err(unsupervised("sudo"))
            }
        }
    }
}

/// The runner's standard output to its end, at most [`MAX_REPLY_BYTES`] and a byte over, on a
/// thread. The reply is exactly one line followed by the end of the stream: a process of the same
/// account can write into the runner's output (on Linux, through `/proc/<pid>/fd/1`), and a line
/// it adds before or after the runner's own must fail the reply and not be taken for it. An output
/// of more than one line is sent as empty, which does not parse.
fn whole_reply(stdout: Option<std::process::ChildStdout>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(stdout) = stdout {
            let limit = u64::try_from(MAX_REPLY_BYTES + 1).unwrap_or(u64::MAX);
            stdout.take(limit).read_to_end(&mut bytes).ok();
        }
        let line = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        if line.contains(&b'\n') {
            bytes.clear();
        } else if bytes.len() > line.len() {
            bytes.pop();
        }
        sender.send(bytes).ok();
    });
    receiver
}

fn bounded_stderr(stderr: Option<std::process::ChildStderr>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(stderr) = stderr {
            stderr.take(MAX_STDERR_BYTES).read_to_end(&mut bytes).ok();
        }
        sender.send(bytes).ok();
    });
    receiver
}

/// The result with its streams and its failure excerpt cut to the bounds the runner promises, so a
/// reply that is not the runner's cannot carry more.
fn bound(mut result: RunResult) -> RunResult {
    if cut_tail(&mut result.stdout, MAX_TAIL_BYTES) {
        result.stdout_truncated = true;
    }
    if cut_tail(&mut result.stderr, MAX_TAIL_BYTES) {
        result.stderr_truncated = true;
    }
    if let Some(excerpt) = result.first_failure.as_mut() {
        let mut end = MAX_FIRST_FAILURE_BYTES.min(excerpt.len());
        while !excerpt.is_char_boundary(end) {
            end -= 1;
        }
        excerpt.truncate(end);
    }
    result
}

/// Keeps the last `max` bytes of `text`, cut on a character boundary. True when it cut.
fn cut_tail(text: &mut String, max: usize) -> bool {
    if text.len() <= max {
        return false;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.drain(..start);
    true
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
        // A call given up while it waited for the gate must not start a run.
        if cancelled() {
            return Err(unsupervised("cancelled"));
        }
        let call_dir = self.call_dir_of(&request.tree)?;
        if let Some(dir) = &call_dir {
            Self::set_mode(dir, OPEN_TO_GROUP).map_err(|error| RunnerError::Unsupervised {
                what: "handoff",
                kind: error.kind(),
            })?;
        }
        let outcome = self.run_locked(request, cancelled);
        if let Some(dir) = &call_dir {
            if let Err(error) = Self::set_mode(dir, CLOSED) {
                tracing::error!(error = %error, "cannot close a call's directory; no more runs");
                self.closed.store(true, Ordering::SeqCst);
            }
        }
        if let Err(error) = self.kill_all() {
            // The run's own result stands; the next run would share the account with a survivor.
            tracing::error!(error = %error, "the runner account cannot be cleared; no more runs");
            self.closed.store(true, Ordering::SeqCst);
        }
        outcome
    }
}

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
//! deadline it kills the account until `sudo` has exited: `sudo` exits only after the kill, and
//! the thread that called it is not cancelled by the call's timeout. A cancel that comes before
//! `sudo` has switched to the account finds nothing to kill, and `sudo` then starts the runner, so
//! the kill is repeated each round until `sudo` has exited. A `sudo` still there once
//! `EXIT_GRACE` has passed closes the gate. Every wait is bounded: a `kill` or a `ps` that has not
//! exited within `settle` is cut off and counts as a failure of that command.
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
use std::process::{Child, Command, ExitStatus, Output, Stdio};
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
/// The most bytes of a `kill` or `ps` output the server reads: a pid list, or a message.
const MAX_COMMAND_BYTES: u64 = 1024 * 1024;
/// How long to wait for the reply once the runner has exited.
const REPLY_GRACE: Duration = Duration::from_secs(5);
/// How long the kills are repeated while `sudo` has not exited; a `sudo` left then closes the gate.
const EXIT_GRACE: Duration = Duration::from_secs(15);
const RUN_POLL: Duration = Duration::from_millis(20);
const PS_POLL: Duration = Duration::from_millis(50);
/// How long between two kills while processes are left, and how long a round waits for `sudo`.
const KILL_ROUND: Duration = Duration::from_secs(1);
const CLOSED: u32 = 0o700;
const OPEN_TO_GROUP: u32 = 0o750;

/// A child's `try_wait`, as the waits poll it.
type Poll<'a> = &'a mut dyn FnMut() -> io::Result<Option<ExitStatus>>;

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
    /// How long processes of the account may take to disappear after a kill, and how long one
    /// `kill` or `ps` may take before it is cut off.
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
        let mut command = self.sudo();
        command.arg(&self.settings.kill).args(["-KILL", "-1"]);
        let output = self.output_bounded(command, "sudo")?;
        // `kill -1` with nothing to kill exits non-zero, which is fine; a `sudo:` line is not.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(line) = stderr.lines().find(|line| line.starts_with("sudo:")) {
            return Err(SudoError::SudoRefused(line.chars().take(200).collect()));
        }
        Ok(())
    }

    /// Runs `command` to its end and collects its output, but waits at most `settle` for it. A
    /// command cut off at that time is an error, never an empty answer. `sudo` is root's, so the
    /// kill that cuts it off may be refused; its process is then left to end on its own and is
    /// reaped by a thread.
    fn output_bounded(
        &self,
        mut command: Command,
        what: &'static str,
    ) -> Result<Output, SudoError> {
        let failed = |source: io::Error| SudoError::Run { what, source };
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(failed)?;
        let stdout = read_capped(child.stdout.take(), MAX_COMMAND_BYTES);
        let stderr = read_capped(child.stderr.take(), MAX_COMMAND_BYTES);
        let waited = Self::wait_bounded(&mut || child.try_wait(), self.settings.settle);
        let status = match waited {
            Ok(Some(status)) => status,
            Ok(None) => {
                reap(child);
                let secs = self.settings.settle.as_secs();
                let why = format!("no exit within {secs} s");
                return Err(failed(io::Error::new(io::ErrorKind::TimedOut, why)));
            }
            Err(error) => {
                reap(child);
                return Err(failed(error));
            }
        };
        let collect =
            |stream: mpsc::Receiver<io::Result<Vec<u8>>>| match stream.recv_timeout(REPLY_GRACE) {
                Ok(read) => read.map_err(failed),
                Err(_) => {
                    let secs = REPLY_GRACE.as_secs();
                    let why = format!("exited, but its output did not end within {secs} s");
                    Err(failed(io::Error::new(io::ErrorKind::TimedOut, why)))
                }
            };
        Ok(Output {
            status,
            stdout: collect(stdout)?,
            stderr: collect(stderr)?,
        })
    }

    /// The pids of the account's processes, from `ps`, which needs no privilege. `ps` exits 1 when
    /// nothing matches, which is the answer wanted; any message on its standard error, or an exit
    /// other than 0 and 1, is a failure and not an empty list.
    fn survivors(&self) -> Result<Vec<String>, SudoError> {
        let failed = |why: String| SudoError::Run {
            what: "ps",
            source: io::Error::other(why),
        };
        let mut command = Command::new(&self.settings.ps);
        command
            .env_clear()
            .args(["-U", &self.settings.user, "-o", "pid="]);
        let output = self.output_bounded(command, "ps")?;
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

    /// Polls for an exit, `poll` being the child's `try_wait`, for at most `grace`. An error from
    /// the poll is the caller's to act on: the exit can no longer be seen.
    fn wait_bounded(poll: Poll<'_>, grace: Duration) -> io::Result<Option<ExitStatus>> {
        let end = Instant::now() + grace;
        loop {
            match poll()? {
                Some(status) => return Ok(Some(status)),
                None if Instant::now() < end => thread::sleep(RUN_POLL),
                None => return Ok(None),
            }
        }
    }

    /// Kills the account each round until `sudo` has exited or `EXIT_GRACE` has passed. A cancel
    /// can come before `sudo` has switched to the account: the first kill then finds nothing, and
    /// `sudo` goes on to start the runner, so the kill is repeated until `sudo` is gone. A kill
    /// that fails, a wait that cannot see `sudo` exit, or a `sudo` still there after the grace,
    /// closes the gate.
    fn stop_run(&self, mut child: Child) {
        if !self.stop_polling(&mut || child.try_wait()) {
            reap(child);
        }
    }

    /// Closes the gate for good: every line that does says what failed, whose account it is and
    /// that only a restart reopens it.
    fn close_gate(&self, what: &str) {
        tracing::error!(
            "{what}; no more runs as {} until a restart",
            self.settings.user
        );
        self.closed.store(true, Ordering::SeqCst);
    }

    /// True when `sudo` was seen to exit; false when the gate was closed instead.
    fn stop_polling(&self, poll: Poll<'_>) -> bool {
        let end = Instant::now() + EXIT_GRACE;
        loop {
            if let Err(error) = self.kill_all() {
                self.close_gate(&format!("cannot stop the account's processes: {error}"));
                return false;
            }
            let left = end
                .saturating_duration_since(Instant::now())
                .min(KILL_ROUND);
            match Self::wait_bounded(poll, left) {
                Ok(Some(_)) => return true,
                Ok(None) => {}
                Err(error) => {
                    self.close_gate(&format!("cannot tell whether sudo has exited: {error}"));
                    return false;
                }
            }
            if Instant::now() >= end {
                let secs = EXIT_GRACE.as_secs();
                self.close_gate(&format!("sudo did not exit within {secs} s of the kills"));
                return false;
            }
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
        let said = read_capped(child.stderr.take(), MAX_STDERR_BYTES);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    self.stop_run(child);
                    return Err(RunnerError::Unsupervised {
                        what: "wait",
                        kind: error.kind(),
                    });
                }
            }
            if cancelled() {
                // The account first, then the wait: sudo exits only once its command is dead.
                self.stop_run(child);
                return Err(unsupervised("cancelled"));
            }
            thread::sleep(RUN_POLL);
        };
        writer.join().ok();
        let bytes = reply.recv_timeout(REPLY_GRACE).unwrap_or_default();
        // The stderr is only logged, so a read that failed is logged as empty.
        let stderr = said.recv_timeout(REPLY_GRACE).ok().and_then(Result::ok);
        let stderr = stderr.unwrap_or_default();
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

/// Ends a child the server will not wait for: the kill may be refused (`sudo` is root's), and a
/// thread reaps it whenever it does exit, so that it never stays a zombie.
fn reap(mut child: Child) {
    child.kill().ok();
    thread::spawn(move || child.wait().ok());
}

/// A stream to its end or to `limit` bytes, whichever comes first, on a thread. A read that
/// fails is sent as the error, never as the bytes before it.
fn read_capped<R: Read + Send + 'static>(
    stream: Option<R>,
    limit: u64,
) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = match stream {
            Some(stream) => stream.take(limit).read_to_end(&mut bytes).map(|_| bytes),
            None => Ok(bytes),
        };
        sender.send(read).ok();
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
                self.close_gate(&format!("cannot close a call's directory: {error}"));
            }
        }
        if let Err(error) = self.kill_all() {
            // The run's own result stands; the next run would share the account with a survivor.
            self.close_gate(&format!("the account cannot be cleared: {error}"));
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::sync::{Arc, OnceLock};

    use super::*;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The one subscriber of this test binary; global, because a callsite that first ran with none
    /// would stay silent for a scoped one.
    fn captured() -> &'static Captured {
        static CAPTURED: OnceLock<Captured> = OnceLock::new();
        CAPTURED.get_or_init(|| {
            let sink = Captured::default();
            let writer = sink.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_ansi(false)
                .finish();
            tracing::subscriber::set_global_default(subscriber).unwrap();
            sink
        })
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn runner_with_quiet_account(dir: &Path) -> SudoRunner {
        let quiet = script(dir, "quiet", "exit 1");
        SudoRunner {
            settings: SudoRunnerSettings {
                user: "_tessel-runner".to_owned(),
                sudo: quiet.clone(),
                runner: quiet.clone(),
                runner_config: quiet.clone(),
                kill: quiet.clone(),
                ps: quiet,
                handoff_dir: dir.join("handoff"),
                settle: Duration::from_secs(3),
            },
            gate: Mutex::new(()),
            closed: AtomicBool::new(false),
        }
    }

    #[test]
    fn a_wait_that_cannot_be_polled_closes_the_gate_at_once_and_says_why() {
        let log = captured();
        let dir = tempfile::tempdir().unwrap();
        let runner = runner_with_quiet_account(dir.path());
        let started = Instant::now();
        runner.stop_polling(&mut || Err(io::Error::other("waitpid lost the child")));
        assert!(
            started.elapsed() < EXIT_GRACE / 3,
            "kills were repeated for {:?}",
            started.elapsed()
        );
        assert!(runner.closed.load(Ordering::SeqCst));
        let text = String::from_utf8_lossy(&log.0.lock().unwrap()).into_owned();
        assert!(text.contains("waitpid lost the child"), "{text}");
        assert!(!text.contains("did not exit within"), "{text}");
        assert!(
            text.contains("no more runs as _tessel-runner until a restart"),
            "{text}"
        );
    }

    struct FailingRead;

    impl Read for FailingRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("the pipe broke"))
        }
    }

    #[test]
    fn a_read_that_fails_is_an_error_and_not_the_bytes_before_it() {
        let failed = read_capped(Some(FailingRead), 10).recv().unwrap();
        assert!(failed.is_err());
        let read = read_capped(Some(&b"123 456"[..]), 3).recv().unwrap();
        assert_eq!(read.unwrap(), b"123");
        assert!(read_capped(None::<&[u8]>, 3)
            .recv()
            .unwrap()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_wait_that_sees_the_exit_leaves_the_gate_open() {
        let dir = tempfile::tempdir().unwrap();
        let runner = runner_with_quiet_account(dir.path());
        runner.stop_polling(&mut || Ok(Some(ExitStatus::from_raw(0))));
        assert!(!runner.closed.load(Ordering::SeqCst));
    }
}

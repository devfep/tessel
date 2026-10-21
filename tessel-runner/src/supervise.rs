//! Runs the planned commands one at a time under one deadline, with a fixed environment, a closed
//! stdin, bounded output and a process group that is killed when the run ends.
//!
//! The lander's git supervisor was not reused: it errors on a timeout and on output past a limit,
//! where the runner must return the tail of what the command wrote, set a flag when it cut, and
//! keep one deadline across several commands.
//!
//! There is no sandbox (M5): a command that leaves its process group (`setsid`) outlives the
//! kill, and the output drain gives up on it after [`DRAIN_GRACE`].

use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::RunnerError;
use crate::failure::first_failure;
use crate::request::{Plan, PlannedCommand};
use crate::result::{ResultStep, RunResult};

/// The most bytes of each stream kept, from the end.
pub const MAX_TAIL_BYTES: usize = 256 * 1024;
/// The exit code reported for a run killed at its timeout, as `timeout(1)` reports a kill.
const KILLED_EXIT_CODE: i32 = 137;
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// How long to wait for the output pipes to close after the group is killed.
const DRAIN_GRACE: Duration = Duration::from_millis(500);
const READ_CHUNK: usize = 64 * 1024;

/// The last [`MAX_TAIL_BYTES`] of a stream.
#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Tail {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > 2 * MAX_TAIL_BYTES {
            self.cut();
        }
    }

    fn cut(&mut self) {
        let surplus = self.bytes.len().saturating_sub(MAX_TAIL_BYTES);
        if surplus > 0 {
            self.bytes.drain(..surplus);
            self.truncated = true;
        }
    }

    fn text(&mut self) -> (String, bool) {
        self.cut();
        (
            String::from_utf8_lossy(&self.bytes).into_owned(),
            self.truncated,
        )
    }
}

type SharedTail = Arc<Mutex<Tail>>;

fn lock(tail: &SharedTail) -> std::sync::MutexGuard<'_, Tail> {
    tail.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Ended {
    Exited(i32),
    TimedOut,
}

/// Runs every command of `plan` in order and stops at the first that does not exit 0.
pub fn run_plan(env: &[(String, String)], plan: &Plan) -> Result<RunResult, RunnerError> {
    let started = Instant::now();
    let deadline = started + plan.timeout;
    let stdout: SharedTail = Arc::default();
    let stderr: SharedTail = Arc::default();
    let mut exit_code = 0;
    let mut timed_out = false;
    for command in &plan.commands {
        match run_command(command, env, deadline, &stdout, &stderr)? {
            Ended::Exited(code) => exit_code = code,
            Ended::TimedOut => {
                exit_code = KILLED_EXIT_CODE;
                timed_out = true;
            }
        }
        if exit_code != 0 {
            break;
        }
    }
    let (stdout, stdout_truncated) = lock(&stdout).text();
    let (stderr, stderr_truncated) = lock(&stderr).text();
    let passed = exit_code == 0 && !timed_out;
    let first_failure = if passed {
        None
    } else {
        first_failure(&stdout, &stderr)
    };
    Ok(RunResult {
        step: ResultStep::Test,
        exit_code,
        passed,
        timed_out,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        first_failure,
        wall_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

fn run_command(
    command: &PlannedCommand,
    env: &[(String, String)],
    deadline: Instant,
    stdout: &SharedTail,
    stderr: &SharedTail,
) -> Result<Ended, RunnerError> {
    let Some((program, args)) = command.argv.split_first() else {
        return Err(RunnerError::unsupervised(
            "start",
            &io::Error::from(io::ErrorKind::InvalidInput),
        ));
    };
    let mut child = Command::new(program)
        .args(args)
        .current_dir(&command.dir)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| RunnerError::unsupervised("start", &error))?;
    let group = child.id();
    let (done, finished) = mpsc::channel();
    let readers = start_reader(child.stdout.take(), stdout, &done)
        .and_then(|()| start_reader(child.stderr.take(), stderr, &done));
    drop(done);
    let ended = match readers {
        Ok(()) => wait_for(&mut child, deadline),
        Err(error) => Err(error),
    };
    kill_group(group);
    let reaped = reap(&mut child);
    let drained = drain(&finished);
    let ended = ended?;
    reaped?;
    drained?;
    Ok(ended)
}

/// Polls until the child exits or the deadline passes.
fn wait_for(child: &mut Child, deadline: Instant) -> Result<Ended, RunnerError> {
    loop {
        let waited = child
            .try_wait()
            .map_err(|error| RunnerError::unsupervised("wait", &error))?;
        if let Some(status) = waited {
            return Ok(Ended::Exited(exit_code_of(status)));
        }
        if Instant::now() >= deadline {
            return Ok(Ended::TimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn exit_code_of(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(KILLED_EXIT_CODE)
}

/// Waits for the child after its group was killed, so it is never left a zombie.
fn reap(child: &mut Child) -> Result<(), RunnerError> {
    child
        .wait()
        .map(|_| ())
        .map_err(|error| RunnerError::unsupervised("wait", &error))
}

/// Reads a pipe on its own thread, so a full pipe never stalls the command.
fn start_reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    tail: &SharedTail,
    done: &Sender<io::Result<()>>,
) -> Result<(), RunnerError> {
    let Some(mut pipe) = pipe else {
        return Err(RunnerError::unsupervised(
            "read output",
            &io::Error::from(io::ErrorKind::NotFound),
        ));
    };
    let tail = Arc::clone(tail);
    let done = done.clone();
    thread::Builder::new()
        .name("tessel-runner-output".to_owned())
        .spawn(move || {
            let mut chunk = vec![0_u8; READ_CHUNK];
            let result = loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break Ok(()),
                    Ok(count) => lock(&tail).push(&chunk[..count]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => break Err(error),
                }
            };
            // The receiver is gone only after the run stopped waiting; nobody is left to tell.
            done.send(result).ok();
        })
        .map(|_| ())
        .map_err(|error| RunnerError::unsupervised("read output", &error))
}

/// Waits for both readers to see their pipe close. A process that left the group and kept a pipe
/// open ends the wait after [`DRAIN_GRACE`], with what was read so far.
fn drain(finished: &Receiver<io::Result<()>>) -> Result<(), RunnerError> {
    let give_up = Instant::now() + DRAIN_GRACE;
    while let Ok(result) = finished.recv_timeout(give_up.saturating_duration_since(Instant::now()))
    {
        result.map_err(|error| RunnerError::unsupervised("read output", &error))?;
    }
    Ok(())
}

/// Kills every process in the group `group`, the pid of the command that leads it.
fn kill_group(group: u32) {
    let Ok(group) = i32::try_from(group) else {
        return;
    };
    // SAFETY: `killpg` takes two integers and touches no memory. The group was created for this
    // command and nothing else is signalled; ESRCH (everything already gone) is the normal case.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
}

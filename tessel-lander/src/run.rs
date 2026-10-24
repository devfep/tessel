//! Runs a [`GitCommand`] with exactly its environment, its timeout and a bound on its output.
//!
//! The timeout covers the whole run, reading the output included. The command runs in its own
//! process group, and the group is killed when the run ends, so a helper git started (ssh, a
//! remote helper) neither outlives the run nor keeps a pipe open past the deadline.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::git::GitCommand;

/// Most bytes of stdout or stderr kept. A longer stream is an error, never a truncated result:
/// a cut-off file list would let a change look covered.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// What a finished command wrote. Non-UTF-8 bytes (file names are untrusted) become U+FFFD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// `None` when a signal ended the process.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("cannot start `{command}`: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("`{command}` ran longer than {seconds} s and was killed")]
    TimedOut { command: String, seconds: u64 },
    #[error("`{command}` wrote more than {limit} bytes to {stream}")]
    OutputTooLarge {
        command: String,
        stream: &'static str,
        limit: usize,
    },
    #[error("cannot wait for `{command}`: {source}")]
    Wait {
        command: String,
        source: std::io::Error,
    },
    #[error("cannot read {stream} of `{command}`: {source}")]
    Read {
        command: String,
        stream: &'static str,
        source: std::io::Error,
    },
    #[error("the thread reading the output of `{command}` stopped without a result")]
    ReaderLost { command: String },
}

/// Runs git commands for the merge. The lander uses [`SystemRunner`]; a test substitutes a fake.
pub trait GitRunner {
    fn run(&self, command: &GitCommand) -> Result<GitOutput, RunError>;
}

/// Runs each command as a child process with [`run`].
#[derive(Debug, Clone, Copy)]
pub struct SystemRunner;

impl GitRunner for SystemRunner {
    fn run(&self, command: &GitCommand) -> Result<GitOutput, RunError> {
        run(command)
    }
}

/// One reader's result: which stream, and its bytes or the read error.
type StreamResult = (&'static str, io::Result<Vec<u8>>);

/// Runs `command` to completion. The process sees `command.env` and nothing else.
pub fn run(command: &GitCommand) -> Result<GitOutput, RunError> {
    let description = describe(command);
    let Some((program, args)) = command.argv.split_first() else {
        return Err(RunError::Spawn {
            command: description,
            source: io::Error::new(io::ErrorKind::InvalidInput, "empty argv"),
        });
    };
    let deadline = Instant::now() + command.timeout;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(&command.dir)
        .env_clear()
        .envs(&command.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|source| RunError::Spawn {
            command: description.clone(),
            source,
        })?;
    let group = child.id();
    let (sender, receiver) = mpsc::channel();
    let started = spawn_reader("stdout", child.stdout.take(), &sender)
        .and_then(|()| spawn_reader("stderr", child.stderr.take(), &sender));
    drop(sender);
    let outcome = started
        .map_err(|()| RunError::ReaderLost {
            command: description.clone(),
        })
        .and_then(|()| supervise(child, &receiver, deadline, command.timeout, &description));
    kill_group(group);
    outcome
}

fn supervise(
    child: Child,
    receiver: &Receiver<StreamResult>,
    deadline: Instant,
    timeout: Duration,
    description: &str,
) -> Result<GitOutput, RunError> {
    let exit_code = wait_until(child, deadline, timeout, description)?;
    let (stdout, stderr) = gather(receiver, deadline, timeout, description)?;
    Ok(GitOutput {
        exit_code,
        stdout: finish(&stdout, "stdout", description)?,
        stderr: finish(&stderr, "stderr", description)?,
    })
}

fn describe(command: &GitCommand) -> String {
    let words: Vec<String> = command
        .argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    words.join(" ")
}

fn timed_out(description: &str, timeout: Duration) -> RunError {
    RunError::TimedOut {
        command: description.to_owned(),
        seconds: timeout.as_secs(),
    }
}

/// Polls until the child exits or `deadline` passes. On the deadline the child is handed to a
/// thread that reaps it once the caller has killed its group, so it is never left a zombie and
/// `run` does not wait.
fn wait_until(
    mut child: Child,
    deadline: Instant,
    timeout: Duration,
    description: &str,
) -> Result<Option<i32>, RunError> {
    loop {
        let waited = child.try_wait().map_err(|source| RunError::Wait {
            command: description.to_owned(),
            source,
        })?;
        if let Some(status) = waited {
            return Ok(status.code());
        }
        if Instant::now() >= deadline {
            thread::spawn(move || child.wait());
            return Err(timed_out(description, timeout));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Collects both streams until `deadline`. A grandchild that keeps a pipe open after git exits
/// ends the run with a timeout, not a hang.
fn gather(
    receiver: &Receiver<StreamResult>,
    deadline: Instant,
    timeout: Duration,
    description: &str,
) -> Result<(Vec<u8>, Vec<u8>), RunError> {
    let (mut stdout, mut stderr) = (None, None);
    while stdout.is_none() || stderr.is_none() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (stream, result) = receiver
            .recv_timeout(remaining)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => timed_out(description, timeout),
                RecvTimeoutError::Disconnected => RunError::ReaderLost {
                    command: description.to_owned(),
                },
            })?;
        let bytes = result.map_err(|source| RunError::Read {
            command: description.to_owned(),
            stream,
            source,
        })?;
        if stream == "stdout" {
            stdout = Some(bytes);
        } else {
            stderr = Some(bytes);
        }
    }
    Ok((stdout.unwrap_or_default(), stderr.unwrap_or_default()))
}

/// Reads a pipe on its own thread so a full pipe never stalls the child. The `Err` is a pipe that
/// was not there to take.
fn spawn_reader<R: Read + Send + 'static>(
    stream: &'static str,
    pipe: Option<R>,
    sender: &Sender<StreamResult>,
) -> Result<(), ()> {
    let pipe = pipe.ok_or(())?;
    let sender = sender.clone();
    thread::spawn(move || {
        // The receiver is gone only after the run ended; nobody is left to tell.
        sender.send((stream, read_bounded(pipe))).ok();
    });
    Ok(())
}

/// Reads up to one byte past the limit, then stops; the child gets a broken pipe on its next
/// write.
fn read_bounded<R: Read>(pipe: R) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let limit = u64::try_from(MAX_OUTPUT_BYTES + 1).unwrap_or(u64::MAX);
    pipe.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn finish(bytes: &[u8], stream: &'static str, description: &str) -> Result<String, RunError> {
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(RunError::OutputTooLarge {
            command: description.to_owned(),
            stream,
            limit: MAX_OUTPUT_BYTES,
        });
    }
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// Kills every process in the group `group`, the pid of the child that leads it. The child leads
/// its own group (`process_group(0)`).
fn kill_group(group: u32) {
    let Ok(group) = i32::try_from(group) else {
        return;
    };
    // SAFETY: `killpg` takes two integers and touches no memory. The group was created for this
    // child and nothing else is signalled; ESRCH (everything already gone) is the normal case.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("disk on fire"))
        }
    }

    #[test]
    fn a_read_error_is_an_error_not_short_output() {
        let error = read_bounded(FailingReader).expect_err("read fails");
        assert_eq!(error.to_string(), "disk on fire");
        let (sender, receiver) = mpsc::channel();
        sender
            .send(("stdout", Err(io::Error::other("disk on fire"))))
            .unwrap();
        let error = gather(
            &receiver,
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(5),
            "git x",
        )
        .expect_err("read error surfaces");
        assert!(
            matches!(
                error,
                RunError::Read {
                    stream: "stdout",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_reader_that_stopped_without_a_result_is_an_error_not_empty_output() {
        let (sender, receiver) = mpsc::channel::<StreamResult>();
        sender.send(("stdout", Ok(b"x".to_vec()))).unwrap();
        drop(sender);
        let error = gather(
            &receiver,
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(5),
            "git x",
        )
        .expect_err("stderr never arrived");
        assert!(matches!(error, RunError::ReaderLost { .. }), "{error}");
    }

    #[test]
    fn both_streams_are_returned_in_whichever_order_they_arrive() {
        let (sender, receiver) = mpsc::channel::<StreamResult>();
        sender.send(("stderr", Ok(b"e".to_vec()))).unwrap();
        sender.send(("stdout", Ok(b"o".to_vec()))).unwrap();
        let (stdout, stderr) = gather(
            &receiver,
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(5),
            "git x",
        )
        .expect("both arrived");
        assert_eq!((stdout, stderr), (b"o".to_vec(), b"e".to_vec()));
    }
}

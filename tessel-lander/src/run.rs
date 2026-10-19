//! Runs a [`GitCommand`] with exactly its environment, its timeout and a bound on its output.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};
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
}

/// Runs `command` to completion. The process sees `command.env` and nothing else.
pub fn run(command: &GitCommand) -> Result<GitOutput, RunError> {
    let description = describe(command);
    let Some((program, args)) = command.argv.split_first() else {
        return Err(RunError::Spawn {
            command: description,
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv"),
        });
    };
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .envs(&command.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| RunError::Spawn {
            command: description.clone(),
            source,
        })?;
    let stdout = capture(child.stdout.take());
    let stderr = capture(child.stderr.take());
    let status = wait_within(&mut child, command.timeout, &description)?;
    Ok(GitOutput {
        exit_code: status,
        stdout: collect(stdout, "stdout", &description)?,
        stderr: collect(stderr, "stderr", &description)?,
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

fn wait_within(
    child: &mut Child,
    timeout: Duration,
    description: &str,
) -> Result<Option<i32>, RunError> {
    let deadline = Instant::now() + timeout;
    loop {
        let waited = child.try_wait().map_err(|source| RunError::Wait {
            command: description.to_owned(),
            source,
        })?;
        if let Some(status) = waited {
            return Ok(status.code());
        }
        if Instant::now() >= deadline {
            // Already exited or not, the kill's own error carries nothing to act on.
            child.kill().ok();
            child.wait().ok();
            return Err(RunError::TimedOut {
                command: description.to_owned(),
                seconds: timeout.as_secs(),
            });
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Reads a pipe on its own thread so a full pipe never stalls the child. Reading stops one byte
/// past the limit; the child then gets a broken pipe on its next write.
fn capture<R: Read + Send + 'static>(pipe: Option<R>) -> Option<JoinHandle<Vec<u8>>> {
    let pipe = pipe?;
    Some(thread::spawn(move || {
        let mut bytes = Vec::new();
        let limit = u64::try_from(MAX_OUTPUT_BYTES + 1).unwrap_or(u64::MAX);
        // A read error ends the stream early; the exit code already reports the failure.
        pipe.take(limit).read_to_end(&mut bytes).ok();
        bytes
    }))
}

fn collect(
    handle: Option<JoinHandle<Vec<u8>>>,
    stream: &'static str,
    description: &str,
) -> Result<String, RunError> {
    let bytes = handle.and_then(|h| h.join().ok()).unwrap_or_default();
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(RunError::OutputTooLarge {
            command: description.to_owned(),
            stream,
            limit: MAX_OUTPUT_BYTES,
        });
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

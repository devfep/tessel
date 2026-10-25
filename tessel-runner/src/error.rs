//! Why the runner did not run, in two kinds that the binary maps to two exit codes.
//!
//! Every message is fixed text. None quotes the request's tree, a command's output or anything
//! else a repository wrote (CLAUDE.md rule 3).

use std::io;

/// Why a configuration file is not acceptable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigReason {
    #[error("the file does not exist or cannot be opened")]
    Missing,
    #[error("it must be a regular file, not a link or a directory")]
    NotRegular,
    #[error("it must be owned by root or by the user who runs the runner")]
    NotOwned,
    #[error(
        "it must not be writable by group or other, and must have no setuid, setgid or sticky bit"
    )]
    WrongMode,
    #[error(
        "it must be TOML of at most 4096 bytes with only the table [runner] and its known keys; \
         work_dir and handoff_dir go together with a path, and every directory is absolute"
    )]
    Invalid,
    #[error("[runner] timeout_secs must be at least 1")]
    ZeroTimeout,
    #[error("[runner] timeout_secs must be at most 86400")]
    TimeoutTooLong,
    #[error("the login of the user who runs the runner cannot be determined")]
    LoginUnknown,
    #[error("[runner] user must be the login of the user who runs the runner")]
    WrongUser,
}

/// Why a request is refused before anything runs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestRefusal {
    #[error(
        "the request must be one JSON object of at most 65536 bytes with the keys tree, tests \
         and timeout_secs"
    )]
    Malformed,
    #[error("timeout_secs must be at least 1")]
    ZeroTimeout,
    /// The lander's own reason for rejecting the commands, fixed text of the lander.
    #[error("the test commands are not allowed: {0}")]
    NotAllowed(String),
    #[error("tree must be an absolute path")]
    TreeNotAbsolute,
    #[error("tree must be a directory, not a link")]
    TreeNotDirectory,
    #[error("tree must be owned by the user who runs the runner")]
    TreeNotOwned,
    #[error("a test dir must be a directory inside the tree")]
    DirNotDirectory,
    #[error("a test dir leaves the tree")]
    DirEscapes,
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// Exit 2: the configuration says this runner may not start.
    #[error("{file}: {reason}")]
    Config { file: String, reason: ConfigReason },
    /// Exit 2: nothing was run.
    #[error("request refused: {0}")]
    Request(#[from] RequestRefusal),
    /// Exit 3: the run could not be supervised. `what` is a fixed word, `kind` is the operating
    /// system's error class.
    #[error("cannot supervise the test command ({what}): {kind}")]
    Unsupervised {
        what: &'static str,
        kind: io::ErrorKind,
    },
}

impl RunnerError {
    /// The binary's exit code: 2 when refused before anything ran, 3 when the run could not be
    /// supervised.
    pub fn exit_code(&self) -> u8 {
        match self {
            RunnerError::Config { .. } | RunnerError::Request(_) => 2,
            RunnerError::Unsupervised { .. } => 3,
        }
    }

    pub(crate) fn unsupervised(what: &'static str, error: &io::Error) -> RunnerError {
        RunnerError::Unsupervised {
            what,
            kind: error.kind(),
        }
    }
}

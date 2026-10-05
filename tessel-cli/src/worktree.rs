//! The git worktree the CLI works in, and the `.tessel/` directory beside its files.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

/// The longest Unix socket path macOS accepts is 103 bytes plus the terminator.
const MAX_SOCKET_PATH_BYTES: usize = 100;

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("not inside a git worktree ({0}); run tessel from the agent's worktree")]
    NotARepo(String),
    #[error("git {args} failed: {message}")]
    Git { args: String, message: String },
    #[error("cannot run git: {0}; is git installed?")]
    NoGit(std::io::Error),
    #[error(
        "the socket path {0} is too long for a Unix socket (limit {MAX_SOCKET_PATH_BYTES} \
         bytes); use a worktree with a shorter path"
    )]
    SocketPathTooLong(String),
    #[error("cannot update {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct Worktree {
    /// Canonical top level of the worktree.
    pub root: PathBuf,
}

impl Worktree {
    /// The worktree containing `cwd`.
    pub fn discover(cwd: &Path) -> Result<Self, WorktreeError> {
        let top = git(cwd, &["rev-parse", "--show-toplevel"]).map_err(|e| match e {
            WorktreeError::Git { message, .. } => WorktreeError::NotARepo(message),
            other => other,
        })?;
        let root = Path::new(&top)
            .canonicalize()
            .map_err(|source| WorktreeError::Io { path: top, source })?;
        let worktree = Self { root };
        let sock = worktree.sock();
        if sock.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
            return Err(WorktreeError::SocketPathTooLong(sock.display().to_string()));
        }
        Ok(worktree)
    }

    /// `git rev-parse HEAD` in the worktree.
    pub fn head(&self) -> Result<String, WorktreeError> {
        git(&self.root, &["rev-parse", "HEAD"])
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join(".tessel")
    }

    pub fn sock(&self) -> PathBuf {
        self.dir().join("sock")
    }

    pub fn state_path(&self) -> PathBuf {
        self.dir().join("state.json")
    }

    pub fn inbox_path(&self) -> PathBuf {
        self.dir().join("inbox.jsonl")
    }

    pub fn inbox_cursor_path(&self) -> PathBuf {
        self.dir().join("inbox.cursor")
    }

    pub fn pid_path(&self) -> PathBuf {
        self.dir().join("daemon.pid")
    }

    pub fn log_path(&self) -> PathBuf {
        self.dir().join("daemon.log")
    }

    /// Creates `.tessel/` and makes sure git ignores it: through the repo's own ignore rules if
    /// they already cover it, else through `.git/info/exclude`, which is never committed.
    pub fn prepare_dir(&self) -> Result<(), WorktreeError> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|source| WorktreeError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        if self.is_ignored()? {
            return Ok(());
        }
        let exclude = self.exclude_path()?;
        if let Some(parent) = exclude.parent() {
            std::fs::create_dir_all(parent).map_err(|source| WorktreeError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let io_err = |source| WorktreeError::Io {
            path: exclude.display().to_string(),
            source,
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude)
            .map_err(io_err)?;
        file.write_all(b"/.tessel/\n").map_err(io_err)
    }

    fn is_ignored(&self) -> Result<bool, WorktreeError> {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["check-ignore", "-q", ".tessel/state.json"])
            .status()
            .map_err(WorktreeError::NoGit)?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            Some(_) | None => Err(WorktreeError::Git {
                args: "check-ignore".into(),
                message: format!("exit status {status}"),
            }),
        }
    }

    fn exclude_path(&self) -> Result<PathBuf, WorktreeError> {
        let path = git(&self.root, &["rev-parse", "--git-path", "info/exclude"])?;
        Ok(self.root.join(path))
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(WorktreeError::NoGit)?;
    if !output.status.success() {
        return Err(WorktreeError::Git {
            args: args.join(" "),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

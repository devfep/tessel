//! The git worktree the CLI works in, and the `.tessel/` directory beside its files.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
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
    #[error("cannot use {path} for the daemon socket: {reason}")]
    SocketDir { path: String, reason: String },
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
    sock: PathBuf,
}

impl Worktree {
    /// The worktree containing `cwd`.
    pub fn discover(cwd: &Path) -> Result<Self, WorktreeError> {
        let top = git(cwd, &["rev-parse", "--show-toplevel"]).map_err(|e| match e {
            WorktreeError::Git { message, .. } => WorktreeError::NotARepo(message),
            other @ (WorktreeError::NotARepo(_)
            | WorktreeError::NoGit(_)
            | WorktreeError::SocketPathTooLong(_)
            | WorktreeError::SocketDir { .. }
            | WorktreeError::Io { .. }) => other,
        })?;
        let root = Path::new(&top)
            .canonicalize()
            .map_err(|source| WorktreeError::Io { path: top, source })?;
        let sock = socket_path(&root, &socket_base())?;
        Ok(Self { root, sock })
    }

    /// `git rev-parse HEAD` in the worktree.
    pub fn head(&self) -> Result<String, WorktreeError> {
        git(&self.root, &["rev-parse", "HEAD"])
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join(".tessel")
    }

    /// The daemon socket. It lives in a short per-user directory, not in `.tessel/`, because a
    /// worktree path can be deep enough to exceed the Unix socket path limit.
    pub fn sock(&self) -> PathBuf {
        self.sock.clone()
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

    pub fn lock_path(&self) -> PathBuf {
        self.dir().join("daemon.lock")
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

/// `$XDG_RUNTIME_DIR` if set, else the system temp directory.
fn socket_base() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(std::env::temp_dir, PathBuf::from)
}

/// `<base>/tessel-<uid>/<hash of root>.sock`, with the directory created private (0700) or, if it
/// exists, verified to be a real directory owned by this user with mode 0700.
fn socket_path(root: &Path, base: &Path) -> Result<PathBuf, WorktreeError> {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    socket_path_for(root, base, unsafe { libc::geteuid() })
}

fn socket_path_for(root: &Path, base: &Path, uid: u32) -> Result<PathBuf, WorktreeError> {
    let dir = base.join(format!("tessel-{uid}"));
    let dir_error = |reason: String| WorktreeError::SocketDir {
        path: dir.display().to_string(),
        reason,
    };
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(dir_error(e.to_string())),
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| dir_error(e.to_string()))?;
    if !meta.is_dir() {
        return Err(dir_error("it is not a directory".into()));
    }
    if meta.uid() != uid {
        return Err(dir_error(format!(
            "it is owned by uid {}, not by you (uid {uid})",
            meta.uid()
        )));
    }
    let mode = meta.mode() & 0o777;
    if mode != 0o700 {
        return Err(dir_error(format!(
            "its mode is {mode:o}, not 700; fix it with `chmod 700` or remove it"
        )));
    }
    // Two worktrees whose paths collide under this 64-bit hash would share one socket, and the
    // later daemon would replace the earlier one's. With n worktrees on a machine the odds are
    // about n^2 / 2^65, which is negligible.
    let name = fnv1a_64(root.as_os_str().as_encoded_bytes());
    let sock = dir.join(format!("{name:016x}.sock"));
    if sock.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
        return Err(WorktreeError::SocketPathTooLong(sock.display().to_string()));
    }
    Ok(sock)
}

/// FNV-1a, 64 bit. Stable across Rust versions, unlike `DefaultHasher`.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_matches_the_published_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn the_socket_name_depends_on_the_worktree_path() {
        let base = tempfile::tempdir().unwrap();
        let one = socket_path(Path::new("/work/one"), base.path()).unwrap();
        let two = socket_path(Path::new("/work/two"), base.path()).unwrap();
        assert_ne!(one, two);
        assert_eq!(
            one,
            socket_path(Path::new("/work/one"), base.path()).unwrap()
        );
        assert_eq!(one.parent(), two.parent());
    }

    #[test]
    fn a_deep_worktree_gets_a_short_socket() {
        let base = tempfile::tempdir().unwrap();
        let deep = format!("/{}", "deeply/nested/".repeat(30));
        let sock = socket_path(Path::new(&deep), base.path()).unwrap();
        assert!(sock.as_os_str().len() < deep.len());
    }

    #[test]
    fn a_directory_with_the_wrong_mode_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        socket_path(Path::new("/work/one"), base.path()).unwrap();
        let dir = std::fs::read_dir(base.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap();
        assert_eq!(dir.metadata().unwrap().mode() & 0o777, 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = socket_path(Path::new("/work/one"), base.path()).unwrap_err();
        assert!(matches!(err, WorktreeError::SocketDir { .. }), "{err}");
        assert!(err.to_string().contains("755"), "{err}");
    }

    #[test]
    fn a_symlink_in_place_of_the_directory_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        socket_path(Path::new("/work/one"), base.path()).unwrap();
        let dir = std::fs::read_dir(base.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap();
        std::fs::remove_dir(&dir).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), &dir).unwrap();
        let err = socket_path(Path::new("/work/one"), base.path()).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[test]
    fn a_base_too_long_for_a_socket_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let long = base.path().join("x".repeat(90));
        std::fs::create_dir(&long).unwrap();
        let err = socket_path(Path::new("/work/one"), &long).unwrap_err();
        assert!(matches!(err, WorktreeError::SocketPathTooLong(_)), "{err}");
    }

    #[test]
    fn a_directory_owned_by_someone_else_is_refused() {
        let base = tempfile::tempdir().unwrap();
        // The directory is made by this user, so it cannot belong to the uid in its name.
        let err = socket_path_for(Path::new("/work/one"), base.path(), 4_000_001).unwrap_err();
        assert!(matches!(err, WorktreeError::SocketDir { .. }), "{err}");
        assert!(err.to_string().contains("owned by uid"), "{err}");
    }
}

//! The directory a merge runs git in, and the only step that makes one.
//!
//! Git reads a repository's own config, and that config can name a filter, a merge driver or a
//! hook that runs repo code (`tests/repo_code.rs` shows an in-place rebase running them). A
//! [`Workspace`] is therefore never a path the caller names: [`clone_main`] makes one only when
//! git itself created the directory by cloning, so its config is the one git wrote. `git clone`
//! refuses a destination that already holds anything, and [`clone_main`] checks the same before
//! it calls the runner, so a runner that skips git cannot pass an existing repository off as a
//! workspace.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::git::{Git, MergeSources};
use crate::run::{GitOutput, GitRunner, RunError};

/// A clone of main made by [`clone_main`]. Every git command that reads a repository's config
/// takes one of these, never a path.
///
/// The `runner` given to [`clone_main`] is trusted to run the command it is handed; the one the
/// lander uses is [`crate::run::SystemRunner`].
///
/// Neither a struct literal nor a constructor is available outside this module:
///
/// ```compile_fail,E0451
/// use std::path::PathBuf;
/// use tessel_lander::workspace::Workspace;
///
/// let _ = Workspace { path: PathBuf::from("/a/repository/with/its/own/config") };
/// ```
///
/// ```compile_fail,E0624
/// use std::path::PathBuf;
/// use tessel_lander::workspace::Workspace;
///
/// let _ = Workspace::new(PathBuf::from("/a/repository/with/its/own/config"));
/// ```
#[derive(Debug)]
pub struct Workspace {
    path: PathBuf,
}

impl Workspace {
    fn new(path: PathBuf) -> Workspace {
        Workspace { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// How the clone step ended.
#[derive(Debug)]
pub enum CloneOutcome {
    Cloned(Workspace),
    /// Git exited non-zero (or was killed). There is no workspace; the output is untrusted data.
    Failed(GitOutput),
}

#[derive(Debug, thiserror::Error)]
pub enum CloneError {
    #[error("cannot clone main into {path}: it exists and is not an empty directory")]
    DestinationInUse { path: PathBuf },
    #[error("cannot check that {path} is a clone destination: {source}")]
    DestinationUnreadable { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Run(#[from] RunError),
}

/// Whether `destination` is missing or an empty directory. A symlink is never one: it may lead
/// to a repository.
fn ensure_clonable(destination: &Path) -> Result<(), CloneError> {
    let unreadable = |source| CloneError::DestinationUnreadable {
        path: destination.to_path_buf(),
        source,
    };
    let in_use = || CloneError::DestinationInUse {
        path: destination.to_path_buf(),
    };
    let metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(unreadable(error)),
    };
    if !metadata.is_dir() {
        return Err(in_use());
    }
    let mut entries = fs::read_dir(destination).map_err(unreadable)?;
    match entries.next() {
        None => Ok(()),
        Some(_) => Err(in_use()),
    }
}

/// Clones main into `destination`, which must be missing or an empty directory (checked here,
/// not left to git), and returns the workspace only if git exited 0.
pub fn clone_main(
    git: &Git,
    runner: &dyn GitRunner,
    sources: &MergeSources,
    destination: &Path,
) -> Result<CloneOutcome, CloneError> {
    ensure_clonable(destination)?;
    let output = runner.run(&git.clone_command(sources, destination))?;
    if output.success() {
        Ok(CloneOutcome::Cloned(Workspace::new(
            destination.to_path_buf(),
        )))
    } else {
        Ok(CloneOutcome::Failed(output))
    }
}

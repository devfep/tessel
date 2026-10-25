//! The directory one call works in, and its removal.

use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

/// A call's directory under `<work_dir>/<tenant>/<repo>/`. The clones of a call are made below
/// it, and it is removed when the call ends, whether the call finished, was given up or panicked.
#[derive(Debug)]
pub struct CallDir {
    path: PathBuf,
}

impl CallDir {
    pub fn create(path: PathBuf) -> io::Result<Self> {
        std::fs::create_dir(&path)?;
        Ok(Self { path })
    }

    /// Like [`CallDir::create`], for a directory that is not the server's alone: mode 0700, so
    /// nobody else can enter it until it is opened on purpose.
    pub fn create_private(path: PathBuf) -> io::Result<Self> {
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self { path })
    }

    /// A path for a clone below the directory. Nothing exists there yet.
    pub fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Removes the directory and everything below it. Nothing to remove is not a failure.
    pub fn remove(&self) {
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                path = %self.path.display(),
                error = %error,
                "cannot remove a call's clones; remove the directory by hand"
            ),
        }
    }
}

impl Drop for CallDir {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_directory_is_removed_with_everything_in_it_when_it_drops() {
        let root = tempfile::tempdir().unwrap();
        let dir = CallDir::create(root.path().join("call-1")).unwrap();
        std::fs::create_dir_all(dir.child("main/.git")).unwrap();
        std::fs::write(dir.child("main/.git/HEAD"), "x").unwrap();
        let path = root.path().join("call-1");
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn removing_twice_or_after_the_directory_is_gone_is_quiet() {
        let root = tempfile::tempdir().unwrap();
        let dir = CallDir::create(root.path().join("call-2")).unwrap();
        dir.remove();
        dir.remove();
        assert!(CallDir::create(root.path().join("call-2")).is_ok());
    }

    #[test]
    fn a_call_directory_that_exists_is_not_taken_over() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("call-3")).unwrap();
        assert!(CallDir::create(root.path().join("call-3")).is_err());
    }
}

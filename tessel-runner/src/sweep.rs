//! Emptying the runner's work area and clearing its account's files out of shared directories.
//!
//! Everything the account leaves behind must not reach the next run (docs/proposals/own-users.md,
//! section 3): an immutable flag (`uchg`), an ACL that denies deleting, a read-only directory, a
//! chain of directories longer than a path may be, and a file in `/tmp` are all removed. Nothing is
//! followed through a link: a link is removed as a link.
//!
//! The shared directories are swept at their top level only: an entry of the account inside a
//! directory that belongs to another user survives (and so does a file the account made in a
//! world-writable directory below one). Only the directories the account owns are cleared whole.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Removes everything inside `dir`, and leaves `dir`.
pub(crate) fn empty_dir(dir: &Path) -> io::Result<()> {
    let entries: Vec<PathBuf> = fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<_>>()?;
    for entry in entries {
        remove_all(&entry)?;
    }
    Ok(())
}

/// Removes `path` and what is below it, without following a link. `remove_dir_all` works from
/// directory descriptors, so a chain of directories longer than a path may be is removed like any
/// other. If the removal fails (an immutable flag, an ACL that denies deletion, a directory with no
/// permissions), `make_removable` clears what blocks it with the system's own tree walkers, which
/// also work from descriptors, and the removal is tried again.
fn remove_all(path: &Path) -> io::Result<()> {
    let is_dir = fs::symlink_metadata(path)?.is_dir();
    let attempt = || {
        if is_dir {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    };
    if attempt().is_ok() {
        return Ok(());
    }
    make_removable(path);
    attempt()
}

/// Clears, below `path`, the permissions, the user flags and the ACLs that stop a removal. Each
/// tool is best effort: what it could not change shows when the removal is tried again.
fn make_removable(path: &Path) {
    // Permissions first, so that a directory with none can be walked; then the flags and the ACLs,
    // which an immutable file would have refused; then the permissions again for what they refused.
    run_tool("/bin/chmod", &["-R", "u+rwx"], path);
    #[cfg(target_os = "macos")]
    {
        run_tool("/usr/bin/chflags", &["-R", "-P", "0"], path);
        run_tool("/bin/chmod", &["-R", "-N"], path);
        run_tool("/bin/chmod", &["-R", "u+rwx"], path);
    }
}

/// `path` is always absolute here, so it cannot be read as a flag (the BSD tools take no `--`).
fn run_tool(program: &str, flags: &[&str], path: &Path) {
    let _ = std::process::Command::new(program)
        .args(flags)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Removes, from each of `dirs`, the entries that belong to `uid`, except those that hold one of
/// `keep` (the directories the runner runs in). A directory that is missing is skipped, an entry of
/// another user is left alone, and a failure on one entry does not stop the others.
pub(crate) fn sweep_owned(dirs: &[PathBuf], keep: &[&Path], uid: u32) -> io::Result<()> {
    let mut first_error = None;
    for dir in dirs {
        // Resolved first, so that `/var/folders/..` and `/private/var/folders/..` are one path and
        // the `keep` check compares like with like.
        let Ok(dir) = fs::canonicalize(dir) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.uid() != uid || keep.iter().any(|kept| kept.starts_with(&path)) {
                continue;
            }
            if let Err(error) = remove_all(&path) {
                first_error.get_or_insert(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// The shared directories of this system, where a process of the account can leave files.
pub(crate) fn default_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut dirs = vec![
            PathBuf::from("/private/tmp"),
            PathBuf::from("/private/var/tmp"),
            PathBuf::from("/Users/Shared"),
        ];
        dirs.extend(darwin_user_dirs());
        dirs
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![PathBuf::from("/tmp"), PathBuf::from("/var/tmp")]
    }
}

/// `DARWIN_USER_TEMP_DIR` (`T`) and its siblings `C` and `0` under `/var/folders/<xx>/<hash>/`.
#[cfg(target_os = "macos")]
fn darwin_user_dirs() -> Vec<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let mut buffer = [0_u8; 1024];
    // SAFETY: `buffer` is live for the call and `confstr` writes at most `buffer.len()` bytes.
    let length = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if length == 0 || length > buffer.len() {
        return Vec::new();
    }
    let raw = &buffer[..length - 1];
    let temp = PathBuf::from(OsStr::from_bytes(raw));
    let Some(hash_dir) = temp.parent().map(Path::to_path_buf) else {
        return Vec::new();
    };
    vec![temp.clone(), hash_dir.join("C"), hash_dir.join("0")]
}

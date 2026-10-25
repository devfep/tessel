//! Emptying the runner's work area and clearing its account's files out of shared directories.
//!
//! Everything the account leaves behind must not reach the next run (docs/proposals/own-users.md,
//! section 3): an immutable flag (`uchg`), an ACL that denies deleting, a read-only directory and a
//! file in `/tmp` are all removed. Nothing is followed through a link: a link is removed as a link.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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

/// Removes `path` and what is below it, without following a link. The order matters: a flag can
/// stop an ACL from being cleared, and an ACL can stop a removal, so every flag goes first, then
/// every ACL, then the entries.
fn remove_all(path: &Path) -> io::Result<()> {
    unlock(path)?;
    clear_acls(path);
    remove_entry(path)
}

/// Clears the flags below `path` and makes every directory there readable and writable.
fn unlock(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    clear_flags(path);
    if meta.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(path)? {
            unlock(&entry?.path())?;
        }
    }
    Ok(())
}

fn remove_entry(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        for entry in fs::read_dir(path)? {
            remove_entry(&entry?.path())?;
        }
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
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

/// Clears the user flags (`uchg`) of `path`; nothing else can be cleared without privilege.
fn clear_flags(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        if let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) {
            // SAFETY: `c_path` is a NUL-terminated path that lives for the call.
            unsafe {
                libc::lchflags(c_path.as_ptr(), 0);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
    }
}

/// Removes every ACL below `path`, so that one denying deletion cannot keep an entry.
fn clear_acls(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("/bin/chmod")
            .arg("-RN")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
    }
}

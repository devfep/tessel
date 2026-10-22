//! The git host's credential: a token in a file the operator names with `[lander] token_file`.
//!
//! Git reads the token through its askpass hook, which is this server's own executable run as
//! `tessel-server askpass <prompt>`. The token is never in an argument, a URL, the value of an
//! environment variable, a log line or an error: the environment carries only the file's path,
//! and errors name the path and the key, never the content.

use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use tessel_lander::git::Git;

/// The configuration key that names the file.
pub const TOKEN_FILE_KEY: &str = "token_file";

/// The variable that carries the file's path to the askpass hook.
pub const ASKPASS_TOKEN_FILE_VAR: &str = "TESSEL_ASKPASS_TOKEN_FILE";

/// The user name the git host expects with an installation or access token.
pub const TOKEN_USERNAME: &str = "x-access-token";

/// A token is a few hundred bytes; a file much larger than this is not one.
const MAX_TOKEN_BYTES: u64 = 16 * 1024;

/// Why the token file cannot be used. Names the file and the key, never any content.
#[derive(Debug, thiserror::Error)]
#[error("[lander] {TOKEN_FILE_KEY} = {path:?} {reason}")]
pub struct TokenFileError {
    path: String,
    reason: &'static str,
}

/// The token in `path`, without one trailing newline. The file must be a regular file with no
/// group or other permission bits (so mode 0600) and must not be empty.
pub fn read_token(path: &Path) -> Result<String, TokenFileError> {
    let refuse = |reason| TokenFileError {
        path: path.display().to_string(),
        reason,
    };
    // Non-blocking, so a named pipe is refused below instead of waiting for a writer.
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                refuse("does not exist")
            } else {
                refuse("cannot be opened by the server")
            }
        })?;
    let meta = file
        .metadata()
        .map_err(|_| refuse("cannot be inspected by the server"))?;
    if !meta.is_file() {
        return Err(refuse("is not a regular file"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(refuse(
            "has group or other permissions; the token file must be mode 0600 (chmod 600)",
        ));
    }
    let mut text = String::new();
    let read = file
        .by_ref()
        .take(MAX_TOKEN_BYTES + 1)
        .read_to_string(&mut text);
    if read.is_err() {
        return Err(refuse("cannot be read as text"));
    }
    if text.len() as u64 > MAX_TOKEN_BYTES {
        return Err(refuse("is too large to be a token"));
    }
    let token = text.strip_suffix('\n').map_or(text.as_str(), |rest| {
        rest.strip_suffix('\r').unwrap_or(rest)
    });
    if token.is_empty() {
        return Err(refuse("is empty"));
    }
    if token.contains(['\n', '\r']) {
        return Err(refuse("contains a line break; a token is one line"));
    }
    Ok(token.to_owned())
}

/// Checks the file as [`read_token`] does and drops the content.
pub fn check_token_file(path: &Path) -> Result<(), TokenFileError> {
    read_token(path).map(drop)
}

/// The prompt, when this process was started as git's askpass hook. Git runs the hook as
/// `<program> <prompt>`: one argument and no subcommand, so the hook is told apart by the
/// variable the lander sets in the environment of its git commands. An ordinary command line
/// (`serve --config ...`, `--version`) is never taken for a prompt.
pub fn askpass_prompt(args: &[OsString], token_file: Option<&OsStr>) -> Option<String> {
    token_file?;
    let [_program, prompt] = args else {
        return None;
    };
    let prompt = prompt.to_string_lossy();
    (!prompt.starts_with('-')).then(|| prompt.into_owned())
}

/// What the askpass hook prints for `prompt`, or `None` when it prints nothing and exits 1:
/// no token file named, an unreadable one, or a prompt that is neither for a user name nor for a
/// password.
pub fn askpass_answer(prompt: &str, token_file: Option<&OsStr>) -> Option<String> {
    let path = Path::new(token_file?);
    if prompt.starts_with("Username") {
        return Some(TOKEN_USERNAME.to_owned());
    }
    if prompt.starts_with("Password") {
        return read_token(path).ok();
    }
    None
}

/// `git` with the askpass hook: `program` run as the hook, told to read `token_file`.
#[must_use]
pub fn with_askpass(git: Git, program: &Path, token_file: &Path) -> Git {
    git.with_env("GIT_ASKPASS", program.as_os_str().to_owned())
        .with_env(
            ASKPASS_TOKEN_FILE_VAR,
            OsString::from(token_file.as_os_str()),
        )
}

/// Whether `location` is a URL on a git host rather than a path.
pub fn is_http_url(location: &str) -> bool {
    location.starts_with("https://") || location.starts_with("http://")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;

    const SECRET: &str = "ghs_not-a-real-token-0123456789";

    fn file_with(dir: &Path, name: &str, text: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn the_token_is_the_file_content_without_one_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        for (text, token) in [
            (SECRET.to_owned(), SECRET.to_owned()),
            (format!("{SECRET}\n"), SECRET.to_owned()),
            (format!("{SECRET}\r\n"), SECRET.to_owned()),
        ] {
            let path = file_with(dir.path(), "token", &text, 0o600);
            assert_eq!(read_token(&path).unwrap(), token, "{text:?}");
        }
        let readable_by_owner_only = file_with(dir.path(), "ro", SECRET, 0o400);
        assert_eq!(read_token(&readable_by_owner_only).unwrap(), SECRET);
    }

    #[test]
    fn a_token_with_a_line_break_inside_is_refused_without_its_content() {
        let dir = tempfile::tempdir().unwrap();
        for text in [
            format!("{SECRET}\nsecond"),
            format!("{SECRET}\n\n"),
            format!("{SECRET}\r\n\r\n"),
            format!("{SECRET}\rmore"),
            format!("first\n{SECRET}\n"),
        ] {
            let path = file_with(dir.path(), "token", &text, 0o600);
            let error = read_token(&path).unwrap_err().to_string();
            assert!(
                error.contains("line break") && error.contains(TOKEN_FILE_KEY),
                "{error}"
            );
            assert!(!error.contains(SECRET), "{error}");
        }
    }

    #[test]
    fn a_token_file_that_cannot_be_used_is_refused_naming_the_file_and_the_key_and_no_content() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let group_readable = file_with(dir.path(), "group", SECRET, 0o640);
        let world_readable = file_with(dir.path(), "world", SECRET, 0o644);
        let world_writable = file_with(dir.path(), "write", SECRET, 0o602);
        let empty = file_with(dir.path(), "empty", "", 0o600);
        let only_newline = file_with(dir.path(), "newline", "\n", 0o600);
        let large = file_with(dir.path(), "large", &SECRET.repeat(1000), 0o600);
        let binary = {
            let path = dir.path().join("binary");
            fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            path
        };
        let directory = dir.path().to_path_buf();
        for path in [
            missing,
            group_readable,
            world_readable,
            world_writable,
            empty,
            only_newline,
            large,
            binary,
            directory,
        ] {
            let error = read_token(&path).unwrap_err().to_string();
            assert!(error.contains(&path.display().to_string()), "{error}");
            assert!(error.contains(TOKEN_FILE_KEY), "{error}");
            assert!(!error.contains(SECRET), "{error}");
        }
    }

    #[test]
    fn a_named_pipe_is_refused_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let error = read_token(&path).unwrap_err().to_string();
        assert!(error.contains("not a regular file"), "{error}");
    }

    #[test]
    fn a_symlink_to_a_good_file_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = file_with(dir.path(), "target", SECRET, 0o600);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(read_token(&link).unwrap(), SECRET);
    }

    #[test]
    fn the_hook_answers_a_user_name_prompt_and_a_password_prompt_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(dir.path(), "token", &format!("{SECRET}\n"), 0o600);
        let file = Some(path.as_os_str());
        assert_eq!(
            askpass_answer("Username for 'https://github.com': ", file).as_deref(),
            Some(TOKEN_USERNAME)
        );
        assert_eq!(
            askpass_answer("Password for 'https://x-access-token@github.com': ", file).as_deref(),
            Some(SECRET)
        );
        for other in [
            "Enter passphrase for key: ",
            "",
            "password for x",
            "Verification code",
        ] {
            assert_eq!(askpass_answer(other, file), None, "{other:?}");
        }
        for prompt in ["Username for 'https://github.com': ", "Password for 'x': "] {
            assert_eq!(askpass_answer(prompt, None), None, "no variable: {prompt}");
        }
    }

    #[test]
    fn the_hook_gives_no_password_from_a_file_that_is_no_longer_usable() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(dir.path(), "token", SECRET, 0o644);
        assert_eq!(
            askpass_answer("Password for 'x': ", Some(path.as_os_str())),
            None
        );
        let missing = dir.path().join("missing");
        assert_eq!(
            askpass_answer("Password for 'x': ", Some(missing.as_os_str())),
            None
        );
    }

    #[test]
    fn the_hook_is_told_apart_by_the_variable_and_one_argument_that_is_no_option() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        let file = Some(OsStr::new("/t"));
        let prompt = args(&["tessel-server", "Username for 'https://h': "]);
        assert_eq!(
            askpass_prompt(&prompt, file).as_deref(),
            Some("Username for 'https://h': ")
        );
        assert_eq!(askpass_prompt(&prompt, None), None, "no variable, no hook");
        for ordinary in [
            args(&["tessel-server"]),
            args(&["tessel-server", "--version"]),
            args(&["tessel-server", "serve", "--config", "c.toml"]),
        ] {
            assert_eq!(askpass_prompt(&ordinary, file), None, "{ordinary:?}");
        }
    }

    #[test]
    fn only_http_and_https_locations_are_urls() {
        for url in ["https://github.com/o/r.git", "http://localhost/r.git"] {
            assert!(is_http_url(url), "{url}");
        }
        for path in [
            "/srv/git/r.git",
            "file:///srv/r.git",
            "git@github.com:o/r.git",
            "./https://x",
        ] {
            assert!(!is_http_url(path), "{path}");
        }
    }
}

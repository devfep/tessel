//! The runner's configuration file, `tessel-runner.toml`, and the check that this process is the
//! one user it names.
//!
//! The runner executes repository code with the privileges of whoever starts it, so it starts only
//! for the one user the file names. The file must exist, be a regular file owned by the effective
//! user with mode 0600, and name that user's login. The login is resolved from the password
//! database for the effective uid with `getpwuid_r`, never from `$USER` or any other variable the
//! caller can set.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde::Deserialize;

use crate::error::{ConfigReason, RunnerError};

/// The file's name when no path is given.
pub const CONFIG_FILE_NAME: &str = "tessel-runner.toml";
/// The longest a run may take when the file does not say.
pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;
/// The largest `timeout_secs` a configuration may set: one day.
pub const MAX_TIMEOUT_SECS: u64 = 86_400;
const MAX_CONFIG_BYTES: u64 = 4096;
/// The only variables a test command inherits from the runner, by name.
const INHERITED_ENV: [&str; 5] = ["PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR"];
const PASSWD_BUFFER_START: usize = 1024;
const PASSWD_BUFFER_MAX: usize = 1 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    runner: RawRunner,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRunner {
    user: String,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

/// What the runner may do: for whom, for how long, and with which environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerConfig {
    user: String,
    timeout_secs: u64,
    env: Vec<(String, String)>,
}

impl RunnerConfig {
    /// The login the runner starts for.
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The longest any run may take; a request may ask for less.
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    /// Replaces the whole environment of a test command. [`RunnerConfig::load`] fills it with the
    /// runner's own `PATH`, `HOME`, `CARGO_HOME`, `RUSTUP_HOME` and `TMPDIR` that are set, and
    /// nothing else. The user check cannot be reached from here: only `load` makes a config.
    #[must_use]
    pub fn with_env(mut self, env: Vec<(String, String)>) -> RunnerConfig {
        self.env = env;
        self
    }

    pub(crate) fn env(&self) -> &[(String, String)] {
        &self.env
    }

    /// Reads and checks the configuration file at `path`.
    pub fn load(path: &Path) -> Result<RunnerConfig, RunnerError> {
        let file = path.display().to_string();
        let refuse = |reason| RunnerError::Config {
            file: file.clone(),
            reason,
        };
        let text = read_private_file(path).map_err(refuse)?;
        let raw: RawFile = toml::from_str(&text).map_err(|_| refuse(ConfigReason::Invalid))?;
        if raw.runner.timeout_secs == 0 {
            return Err(refuse(ConfigReason::ZeroTimeout));
        }
        if raw.runner.timeout_secs > MAX_TIMEOUT_SECS {
            return Err(refuse(ConfigReason::TimeoutTooLong));
        }
        let login = effective_login().ok_or_else(|| refuse(ConfigReason::LoginUnknown))?;
        if raw.runner.user != login {
            return Err(refuse(ConfigReason::WrongUser));
        }
        Ok(RunnerConfig {
            user: raw.runner.user,
            timeout_secs: raw.runner.timeout_secs,
            env: inherited_env(),
        })
    }
}

fn inherited_env() -> Vec<(String, String)> {
    let mut env = Vec::new();
    for name in INHERITED_ENV {
        if let Ok(value) = std::env::var(name) {
            env.push((name.to_owned(), value));
        }
    }
    env
}

/// Opens without following a link, then checks the open file, so what is checked is what is read.
fn read_private_file(path: &Path) -> Result<String, ConfigReason> {
    let file = open_without_following(path)?;
    let meta = file.metadata().map_err(|_| ConfigReason::Missing)?;
    if !meta.is_file() {
        return Err(ConfigReason::NotRegular);
    }
    if meta.uid() != effective_uid() {
        return Err(ConfigReason::NotOwned);
    }
    if meta.mode() & 0o777 != 0o600 {
        return Err(ConfigReason::WrongMode);
    }
    let mut text = String::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| ConfigReason::Invalid)?;
    if u64::try_from(text.len()).map_or(true, |len| len > MAX_CONFIG_BYTES) {
        return Err(ConfigReason::Invalid);
    }
    Ok(text)
}

fn open_without_following(path: &Path) -> Result<File, ConfigReason> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ELOOP) => ConfigReason::NotRegular,
            Some(_) | None => ConfigReason::Missing,
        })
}

/// The uid this process runs as.
pub fn effective_uid() -> u32 {
    // SAFETY: `geteuid` takes no argument, touches no memory and cannot fail.
    unsafe { libc::geteuid() }
}

/// The login name of the effective uid, from the password database. `None` when the uid has no
/// entry or its name is not UTF-8.
pub fn effective_login() -> Option<String> {
    let uid = effective_uid();
    let mut buffer = vec![0_u8; PASSWD_BUFFER_START];
    loop {
        // SAFETY: an all-zero `passwd` is a valid value (null pointers and zero integers); it is
        // only read after `getpwuid_r` fills it.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `entry`, `buffer` and `found` are live for the call, `buffer.len()` is the
        // buffer's real size, and `getpwuid_r` writes only inside them.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                &raw mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &raw mut found,
            )
        };
        if status == libc::ERANGE && buffer.len() < PASSWD_BUFFER_MAX {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || found.is_null() {
            return None;
        }
        // SAFETY: on success `pw_name` points at a NUL-terminated string inside `buffer`, which
        // is still alive and unchanged.
        let name = unsafe { CStr::from_ptr(entry.pw_name) };
        return name.to_str().ok().map(str::to_owned);
    }
}

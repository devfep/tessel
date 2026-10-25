//! The runner's configuration file, `tessel-runner.toml`, and the check that this process is the
//! one user it names.
//!
//! The runner executes repository code with the privileges of whoever starts it, so it starts only
//! for the one user the file names. The file must exist, be a regular file owned by root or by the
//! effective user that nobody else can write (no group or other write bit, no setuid, setgid or
//! sticky bit), and name that user's login. A root-owned file is the one a service account cannot
//! rewrite (docs/proposals/own-users.md). The login is resolved from the password database for the
//! effective uid with `getpwuid_r`, never from `$USER` or any other variable the caller can set.
//!
//! With `work_dir` and `handoff_dir` the configuration describes a sandbox (the runner started
//! through the sudo rule as its own account): the environment of a test command then comes from the
//! file alone (`path`, `cargo_home`, `tmpdir`, and `HOME=/var/empty`), and the runner copies the
//! tree from the handoff directory into its own work directory. Without them the runner is the
//! in-process runner of one trusted user, and it passes on the few variables listed in
//! `INHERITED_ENV`.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ConfigReason, RunnerError};

/// The file's name when no path is given.
pub const CONFIG_FILE_NAME: &str = "tessel-runner.toml";
/// The longest a run may take when the file does not say.
pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;
/// The largest `timeout_secs` a configuration may set: one day.
pub const MAX_TIMEOUT_SECS: u64 = 86_400;
const MAX_CONFIG_BYTES: u64 = 4096;
/// The default ceiling on processes of the runner's account, in a sandbox. Sized above the peak of
/// `cargo test --workspace` on the dogfood Mac (docs/proposals/own-users.md, section 2).
pub const DEFAULT_MAX_PROCESSES: u64 = 4096;
/// The home of a sandboxed test command: a directory nothing can write.
const SANDBOX_HOME: &str = "/var/empty";
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
    path: Option<String>,
    cargo_home: Option<String>,
    tmpdir: Option<String>,
    work_dir: Option<String>,
    handoff_dir: Option<String>,
    max_processes: Option<u64>,
    sweep_dirs: Option<Vec<String>>,
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
    sandbox: Option<Sandbox>,
}

/// What a sandboxed runner may touch: where trees arrive, where it works, how many processes its
/// account may have, and which shared directories it clears of its own files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    work_dir: PathBuf,
    handoff_dir: PathBuf,
    max_processes: u64,
    sweep_dirs: Option<Vec<PathBuf>>,
}

impl Sandbox {
    /// The runner's own directory: each run empties it first and copies the tree into it.
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// Where the lander leaves the trees to test. The runner reads there and never writes.
    pub fn handoff_dir(&self) -> &Path {
        &self.handoff_dir
    }

    /// The ceiling on the account's processes, soft and hard.
    pub fn max_processes(&self) -> u64 {
        self.max_processes
    }

    /// The shared directories to clear of this account's files, when the file names them; `None`
    /// means the system's own (`/tmp`, `/var/tmp`, `/Users/Shared`, the per-user temporary
    /// directories).
    pub fn sweep_dirs(&self) -> Option<&[PathBuf]> {
        self.sweep_dirs.as_deref()
    }
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

    /// The environment of a test command.
    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }

    /// The sandbox this configuration describes, if it describes one.
    pub fn sandbox(&self) -> Option<&Sandbox> {
        self.sandbox.as_ref()
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
        let (env, sandbox) = match sandbox_of(&raw.runner) {
            Ok(Some((env, sandbox))) => (env, Some(sandbox)),
            Ok(None) => (inherited_env(), None),
            Err(reason) => return Err(refuse(reason)),
        };
        if sandbox.is_some() && effective_uid() == 0 {
            return Err(refuse(ConfigReason::RootSandbox));
        }
        Ok(RunnerConfig {
            user: raw.runner.user,
            timeout_secs: raw.runner.timeout_secs,
            env,
            sandbox,
        })
    }
}

fn absolute(value: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    (path.is_absolute() && !value.contains('\0')).then_some(path)
}

/// The sandbox and its environment, when the file asks for one: `work_dir` and `handoff_dir`
/// together, then a `path`, and every directory absolute. `Ok(None)` when it asks for neither.
type SandboxEnv = (Vec<(String, String)>, Sandbox);

fn sandbox_of(raw: &RawRunner) -> Result<Option<SandboxEnv>, ConfigReason> {
    let (Some(work), Some(handoff)) = (&raw.work_dir, &raw.handoff_dir) else {
        let any = raw.work_dir.is_some()
            || raw.handoff_dir.is_some()
            || raw.path.is_some()
            || raw.cargo_home.is_some()
            || raw.tmpdir.is_some()
            || raw.max_processes.is_some()
            || raw.sweep_dirs.is_some();
        return if any {
            Err(ConfigReason::Invalid)
        } else {
            Ok(None)
        };
    };
    let (work_dir, handoff_dir) = (
        absolute(work).ok_or(ConfigReason::Invalid)?,
        absolute(handoff).ok_or(ConfigReason::Invalid)?,
    );
    let path = raw.path.as_ref().ok_or(ConfigReason::Invalid)?;
    let mut env = vec![("PATH".to_owned(), path.clone())];
    for (name, value) in [("CARGO_HOME", &raw.cargo_home), ("TMPDIR", &raw.tmpdir)] {
        if let Some(value) = value {
            absolute(value).ok_or(ConfigReason::Invalid)?;
            env.push((name.to_owned(), value.clone()));
        }
    }
    env.push(("HOME".to_owned(), SANDBOX_HOME.to_owned()));
    let max_processes = raw.max_processes.unwrap_or(DEFAULT_MAX_PROCESSES);
    if max_processes == 0 {
        return Err(ConfigReason::Invalid);
    }
    let sweep_dirs = match &raw.sweep_dirs {
        None => None,
        Some(dirs) => {
            let mut parsed = Vec::with_capacity(dirs.len());
            for dir in dirs {
                parsed.push(absolute(dir).ok_or(ConfigReason::Invalid)?);
            }
            Some(parsed)
        }
    };
    Ok(Some((
        env,
        Sandbox {
            work_dir,
            handoff_dir,
            max_processes,
            sweep_dirs,
        },
    )))
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
    if !owner_acceptable(meta.uid(), effective_uid()) {
        return Err(ConfigReason::NotOwned);
    }
    if !mode_acceptable(meta.mode()) {
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

/// Root owns the file a service account must not rewrite; the effective user owns one it made.
fn owner_acceptable(file_uid: u32, effective: u32) -> bool {
    file_uid == 0 || file_uid == effective
}

/// No group or other write bit, and none of the setuid, setgid and sticky bits.
fn mode_acceptable(mode: u32) -> bool {
    mode & 0o7022 == 0
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

#[cfg(test)]
mod tests {
    use super::{mode_acceptable, owner_acceptable};

    #[test]
    fn a_file_owned_by_root_or_by_the_effective_user_has_an_acceptable_owner() {
        assert!(owner_acceptable(0, 501));
        assert!(owner_acceptable(501, 501));
        assert!(!owner_acceptable(502, 501));
        assert!(!owner_acceptable(0o777, 0));
    }

    #[test]
    fn only_a_mode_without_group_other_write_or_special_bits_is_acceptable() {
        assert!(mode_acceptable(0o100_644));
        assert!(mode_acceptable(0o100_600));
        assert!(!mode_acceptable(0o100_664));
        assert!(!mode_acceptable(0o100_646));
        assert!(!mode_acceptable(0o104_644));
        assert!(!mode_acceptable(0o102_644));
        assert!(!mode_acceptable(0o101_644));
    }
}

//! The limits of the runner's account, set before anything runs.

use std::io;

use crate::config::RunnerConfig;
use crate::error::RunnerError;

/// Lowers the process limit of this process and its children to the sandbox's `max_processes`,
/// soft and hard, and turns core files off. A configuration without a sandbox sets nothing. The
/// caller is the one-shot runner process: the limits stay with it.
pub fn apply_limits(config: &RunnerConfig) -> Result<(), RunnerError> {
    let Some(sandbox) = config.sandbox() else {
        return Ok(());
    };
    set(libc::RLIMIT_NPROC, sandbox.max_processes())
        .map_err(|error| RunnerError::unsupervised("limit", &error))?;
    set(libc::RLIMIT_CORE, 0).map_err(|error| RunnerError::unsupervised("limit", &error))
}

fn set(resource: libc::c_int, value: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    // SAFETY: `limit` is a valid `rlimit` that lives for the call.
    let status = unsafe { libc::setrlimit(resource as _, &raw const limit) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

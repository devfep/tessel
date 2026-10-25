//! The limits of the runner's account, set before anything runs.

use std::io;

use crate::config::RunnerConfig;
use crate::error::RunnerError;

/// The type of a resource argument to `getrlimit` and `setrlimit`: glibc declares an unsigned
/// enum type for it, every other libc (macOS, musl) a plain `c_int`.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
type Resource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
type Resource = libc::c_int;

/// Makes this process refuse to be inspected or written into by other processes of the same
/// account: on Linux, `/proc/<pid>/fd/1` of a dumpable process can be opened by its owner's other
/// processes, and a line written there would pass for the runner's reply. On other systems this
/// does nothing; the Mac has no `/proc`, and its `task_for_pid` needs privilege.
pub fn make_undumpable() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `prctl` with `PR_SET_DUMPABLE` takes two integers and touches no memory.
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        }
    }
}

/// Lowers the process limit of this process and its children to the sandbox's `max_processes`,
/// soft and hard, and turns core files off. A limit above the hard limit this process inherited
/// cannot be set without privilege, so the lower of the two is used. (On Linux the limit counts
/// threads too, and the default was measured as processes on a Mac.) A configuration without a
/// sandbox sets nothing. The caller is the one-shot runner process: the limits stay with it.
pub fn apply_limits(config: &RunnerConfig) -> Result<(), RunnerError> {
    let Some(sandbox) = config.sandbox() else {
        return Ok(());
    };
    let inherited = hard_limit(libc::RLIMIT_NPROC)
        .map_err(|error| RunnerError::unsupervised("limit", &error))?;
    set(libc::RLIMIT_NPROC, sandbox.max_processes().min(inherited))
        .map_err(|error| RunnerError::unsupervised("limit", &error))?;
    set(libc::RLIMIT_CORE, 0).map_err(|error| RunnerError::unsupervised("limit", &error))
}

fn hard_limit(resource: Resource) -> io::Result<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid `rlimit` that lives for the call, and `getrlimit` writes only it.
    let status = unsafe { libc::getrlimit(resource, &raw mut limit) };
    if status == 0 {
        Ok(limit.rlim_max)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn set(resource: Resource, value: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    // SAFETY: `limit` is a valid `rlimit` that lives for the call.
    let status = unsafe { libc::setrlimit(resource, &raw const limit) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn a_process_that_made_itself_undumpable_reports_it() {
        super::make_undumpable();
        // SAFETY: `prctl` with `PR_GET_DUMPABLE` takes integers and touches no memory.
        let dumpable = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
        assert_eq!(dumpable, 0);
    }
}

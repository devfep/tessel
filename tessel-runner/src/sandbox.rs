//! A run in the sandbox: the tree is copied from the handoff directory into the runner's own work
//! directory, and the tests run on the copy.
//!
//! The copy uses `std::fs` and no git (CLAUDE.md rule 4). The lander made the tree and owns it;
//! the runner owns the copy and everything the tests write in it, so the lander never touches
//! files it cannot remove, and the tests cannot reach the lander's clones.

use std::fs;
use std::io;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::config::{effective_uid, RunnerConfig, Sandbox};
use crate::error::RunnerError;
use crate::request::{Plan, PlannedCommand, RunRequest};
use crate::result::RunResult;
use crate::{supervise, sweep, toolchain};

/// The name of the copy inside the work directory.
const COPY_NAME: &str = "tree";

pub(crate) fn run(
    config: &RunnerConfig,
    sandbox: &Sandbox,
    request: &RunRequest,
) -> Result<RunResult, RunnerError> {
    let plan = request.plan(config)?;
    prepare_directories(config, sandbox)
        .map_err(|error| RunnerError::unsupervised("sweep", &error))?;
    let copy = sandbox.work_dir().join(COPY_NAME);
    copy_tree(&plan.root, &copy).map_err(|error| RunnerError::unsupervised("copy", &error))?;
    let plan = rebase(plan, &copy);
    toolchain::check(config, sandbox, &plan.root)?;
    supervise::run_plan(config.env(), &plan)
}

/// Empties the work directory and the temporary directory, and clears the account's files out of
/// the shared directories.
fn prepare_directories(config: &RunnerConfig, sandbox: &Sandbox) -> io::Result<()> {
    fs::create_dir_all(sandbox.work_dir())?;
    sweep::empty_dir(sandbox.work_dir())?;
    if let Some((_, tmpdir)) = config.env().iter().find(|(name, _)| name == "TMPDIR") {
        let tmpdir = PathBuf::from(tmpdir);
        if tmpdir.is_dir() {
            sweep::empty_dir(&tmpdir)?;
        }
    }
    let dirs = sandbox
        .sweep_dirs()
        .map_or_else(sweep::default_dirs, <[PathBuf]>::to_vec);
    let keep = [
        fs::canonicalize(sandbox.work_dir())?,
        fs::canonicalize(sandbox.handoff_dir())?,
    ];
    let keep_refs: Vec<&Path> = keep.iter().map(PathBuf::as_path).collect();
    // Entries that stay are another user's to remove, or sit where the account cannot delete.
    let _stayed = sweep::sweep_owned(&dirs, &keep_refs, effective_uid());
    Ok(())
}

/// The plan with its tree and its commands' directories moved to the copy.
fn rebase(plan: Plan, copy: &Path) -> Plan {
    let old_root = plan.root;
    let commands = plan
        .commands
        .into_iter()
        .map(|command| PlannedCommand {
            dir: match command.dir.strip_prefix(&old_root) {
                Ok(inside) => copy.join(inside),
                Err(_) => copy.to_path_buf(),
            },
            argv: command.argv,
        })
        .collect();
    Plan {
        root: copy.to_path_buf(),
        commands,
        timeout: plan.timeout,
    }
}

/// Copies `from` to `to`, which must not exist. Links are copied as links and never followed;
/// anything that is not a file, a directory or a link (a socket, a device) is left out. The copy is
/// writable by its owner whatever the original's permissions were.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    fs::create_dir(to)?;
    fs::set_permissions(
        to,
        fs::Permissions::from_mode((meta.mode() & 0o777) | 0o700),
    )?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = fs::symlink_metadata(&source)?.file_type();
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_symlink() {
            symlink(fs::read_link(&source)?, &target)?;
        } else if kind.is_file() {
            fs::copy(&source, &target)?;
            let mode = fs::metadata(&target)?.mode() & 0o777;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode | 0o600))?;
        }
    }
    Ok(())
}

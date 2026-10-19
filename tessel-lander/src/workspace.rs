//! The directory a merge runs git in, and the only step that makes one.
//!
//! Git reads a repository's own config, and that config can name a filter, a merge driver or a
//! hook that runs repo code (`tests/repo_code.rs` shows an in-place rebase running them). A
//! [`Workspace`] is therefore never a path the caller names: [`clone_main`] makes one only when
//! git itself created the directory by cloning, so its config is the one git wrote. `git clone`
//! refuses a destination that already holds anything, and [`clone_main`] checks the same before
//! it calls the runner, so a runner that skips git cannot pass an existing repository off as a
//! workspace. An empty directory inside another repository passes that check; git is then kept
//! from finding the parent by `GIT_DIR` and `GIT_WORK_TREE` on every command that takes a
//! workspace (see [`crate::git`]).

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::git::{Git, MergeSources};
use crate::run::{GitOutput, GitRunner, RunError};
use crate::types::Sha;

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

/// The directory the repo's install and test steps run in. It is never a [`Workspace`]: `land`
/// pushes from the workspace with the write credential live, and repo code run there could
/// rewrite that clone's `.git/config` (`url.*.pushInsteadOf`, `remote.<url>.pushurl`,
/// `include.path`), its refs, its objects or its `info/attributes`. The lander makes a tree with
/// [`clone_test_tree`], a second clone of the rebased head, and gives a port nothing else to run
/// repo code in.
///
/// What holds is the type: the port API cannot be handed the workspace, and nothing written inside
/// the tree changes what `land` does. Code running as the same OS user can still write the
/// workspace's files by path (the tree's `origin` and reflog name it); keeping it from them is the
/// sandbox's job (M4).
///
/// Neither a struct literal nor a constructor is available outside this crate, a tree cannot be
/// copied, and a [`Workspace`] is not a tree. Each snippet below fails to compile for the reason
/// it shows:
///
/// ```compile_fail,E0451
/// use std::path::PathBuf;
/// use tessel_lander::workspace::TestTree;
///
/// let _ = TestTree { path: PathBuf::from("/a/repository/with/its/own/config") };
/// ```
///
/// ```compile_fail,E0624
/// use tessel_lander::workspace::{TestTree, Workspace};
///
/// fn smuggle(workspace: Workspace) -> TestTree {
///     TestTree::from_workspace(workspace)
/// }
/// ```
///
/// ```compile_fail,E0599
/// use tessel_lander::workspace::TestTree;
///
/// fn copy(tree: TestTree) -> TestTree {
///     tree.clone()
/// }
/// ```
///
/// ```compile_fail,E0308
/// use tessel_lander::outcome::{PackageStep, StepOutcome};
/// use tessel_lander::merge::{BoundaryError, MergePorts};
/// use tessel_lander::workspace::Workspace;
///
/// fn run_in_the_workspace(
///     ports: &impl MergePorts,
///     workspace: &Workspace,
/// ) -> Result<StepOutcome, BoundaryError> {
///     ports.run_package_step(PackageStep::Test, workspace)
/// }
/// ```
#[derive(Debug)]
pub struct TestTree {
    path: PathBuf,
}

impl TestTree {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A trial's own clone is its test tree: nothing runs git in it afterwards and nothing pushes
    /// from it, and taking the workspace by value leaves no handle that could.
    pub(crate) fn from_workspace(workspace: Workspace) -> TestTree {
        TestTree {
            path: workspace.path,
        }
    }
}

/// How [`clone_test_tree`] ended.
#[derive(Debug)]
pub enum TestTreeOutcome {
    Cloned(TestTree),
    /// Git exited non-zero, or did not name a commit at `HEAD`. There is no tree; the output is
    /// untrusted data.
    Failed(GitOutput),
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
    #[error("cannot clone main into {path}: it contains `..`; name the directory directly")]
    DestinationHasParentDir { path: PathBuf },
    #[error("cannot check that {path} is a clone destination: {source}")]
    DestinationUnreadable { path: PathBuf, source: io::Error },
    #[error("cannot write {path} in the clone of main, so it is not used: {source}")]
    WriteAttributes { path: PathBuf, source: io::Error },
    #[error("cannot make a test tree at {path}: it lies inside the workspace {workspace}")]
    TestTreeInWorkspace { path: PathBuf, workspace: PathBuf },
    #[error("the test tree at {path} is at {actual}, not at the head {expected}")]
    TestTreeNotAtHead {
        path: PathBuf,
        expected: Sha,
        actual: Sha,
    },
    #[error(transparent)]
    Run(#[from] RunError),
}

/// Attributes the clone's own `info/attributes` sets for every path. It outranks every
/// `.gitattributes` in the tree, so a fork cannot pick a built-in merge driver: `merge=union`
/// would splice both sides of a conflict into a clean merge. With `text` a conflict always stops
/// the rebase. A repo's own built-in driver on main is overridden too, which can only add
/// conflicts.
const MERGE_ATTRIBUTES: &str = "* merge=text\n";

/// Writes [`MERGE_ATTRIBUTES`] into the `.git` directory git made. A real `git clone` into an
/// empty directory always makes one: the argv has no `--separate-git-dir` and the clone's
/// environment has no `GIT_DIR`. The skip exists for test runners that run no git, which leave
/// no `.git`; the runner is trusted to run the command it is handed (see [`Workspace`]).
fn write_merge_attributes(destination: &Path) -> Result<(), CloneError> {
    let git_dir = destination.join(".git");
    if !git_dir.is_dir() {
        return Ok(());
    }
    let info = git_dir.join("info");
    let path = info.join("attributes");
    let write = |source| CloneError::WriteAttributes {
        path: path.clone(),
        source,
    };
    fs::create_dir_all(&info).map_err(write)?;
    fs::write(&path, MERGE_ATTRIBUTES).map_err(write)
}

/// Whether `destination` is missing or an empty directory. A path with `..` is refused: when its
/// earlier part is missing it reads as missing yet resolves to an existing directory. A symlink
/// is never one: it may lead to a repository.
fn ensure_clonable(destination: &Path) -> Result<(), CloneError> {
    if destination.components().any(|c| c == Component::ParentDir) {
        return Err(CloneError::DestinationHasParentDir {
            path: destination.to_path_buf(),
        });
    }
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
/// not left to git), and returns the workspace only if git exited 0. The workspace holds the
/// absolute path: git resolves a relative `GIT_DIR` after `-C`.
pub fn clone_main(
    git: &Git,
    runner: &dyn GitRunner,
    sources: &MergeSources,
    destination: &Path,
) -> Result<CloneOutcome, CloneError> {
    let destination =
        &std::path::absolute(destination).map_err(|source| CloneError::DestinationUnreadable {
            path: destination.to_path_buf(),
            source,
        })?;
    ensure_clonable(destination)?;
    let output = runner.run(&git.clone_command(sources, destination))?;
    if output.success() {
        write_merge_attributes(destination)?;
        Ok(CloneOutcome::Cloned(Workspace::new(destination.clone())))
    } else {
        Ok(CloneOutcome::Failed(output))
    }
}

/// `path` with symlinks resolved as far as it exists, and the part that does not exist as written.
/// A missing destination may sit below a symlink to the workspace, or be spelled `/tmp/x` for
/// `/private/tmp/x`; comparing the text of two paths would miss both.
fn resolve_existing(path: &Path) -> io::Result<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
                    return Err(error);
                };
                missing.push(name);
                existing = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Makes the tree the repo's tests run in: a second clone of `workspace`, at `head`, into
/// `destination`. The same checks as [`clone_main`] apply to the destination (missing or an empty
/// directory, absolute, no `..`, not a symlink), and it may not lie inside `workspace` (compared after
/// resolving symlinks). The clone
/// copies objects instead of linking them, so nothing written there reaches the workspace's
/// object store. It runs no repo code and needs no credential. The caller owns `destination` and
/// removes it; the lander does not.
///
/// Git writes this clone's config, so the tree starts with none of the workspace's settings,
/// and nothing written in the tree changes the workspace, which `land` pushes from. The clone
/// still names the workspace (its `origin`, its reflog), so code that runs as the same OS user
/// and writes there by path is the sandbox's to stop, not this function's.
pub fn clone_test_tree(
    git: &Git,
    runner: &dyn GitRunner,
    workspace: &Workspace,
    head: &Sha,
    destination: &Path,
) -> Result<TestTreeOutcome, CloneError> {
    let destination =
        &std::path::absolute(destination).map_err(|source| CloneError::DestinationUnreadable {
            path: destination.to_path_buf(),
            source,
        })?;
    ensure_clonable(destination)?;
    let resolve = |path: &Path| {
        resolve_existing(path).map_err(|source| CloneError::DestinationUnreadable {
            path: path.to_path_buf(),
            source,
        })
    };
    if resolve(destination)?.starts_with(resolve(workspace.path())?) {
        return Err(CloneError::TestTreeInWorkspace {
            path: destination.clone(),
            workspace: workspace.path().to_path_buf(),
        });
    }
    let output = runner.run(&git.clone_test_tree_command(workspace, destination))?;
    if !output.success() {
        return Ok(TestTreeOutcome::Failed(output));
    }
    let cloned = Workspace::new(destination.clone());
    let at_head = runner.run(&git.head_command(&cloned))?;
    if !at_head.success() {
        return Ok(TestTreeOutcome::Failed(at_head));
    }
    let Ok(actual) = Sha::parse(at_head.stdout.trim()) else {
        return Ok(TestTreeOutcome::Failed(at_head));
    };
    if actual != *head {
        return Err(CloneError::TestTreeNotAtHead {
            path: destination.clone(),
            expected: head.clone(),
            actual,
        });
    }
    Ok(TestTreeOutcome::Cloned(TestTree { path: cloned.path }))
}

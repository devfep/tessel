//! The git invocations of a merge, as data: argv plus a fixed environment. Nothing here runs a
//! command; [`crate::run::run`] does. Every value is an argv element or an environment value,
//! never shell text.
//!
//! # Why repo code cannot run
//!
//! - Hooks: `core.hooksPath` names an empty directory the lander owns.
//! - Filters, merge drivers, textconv, external diff: each needs a setting in a
//!   config file. System and global config are off and `HOME` is an empty directory. **Repo-local
//!   config is not neutralised**: git runs a `filter.*` or `merge.*.driver` it finds in
//!   `.git/config`. So every command except `clone` takes a [`Workspace`], a type only the clone
//!   step ([`crate::workspace::clone_main`]) can make: a directory git itself created, whose
//!   local config git wrote and which holds no such setting (`git clone` copies no config from
//!   the source; a repo's `.gitattributes` can name a driver but cannot define one). No builder
//!   here takes a bare path where it needs one.
//!   The diff commands also pass `--no-ext-diff` and `--no-textconv`.
//! - Repository discovery: every command that takes a [`Workspace`] sets `GIT_DIR` and
//!   `GIT_WORK_TREE` to it, so git never searches the parent directories and a workspace with no
//!   `.git` of its own fails instead of finding another repository's config. (Env, not
//!   `--git-dir`, to keep the argv the steward's tests pin; not `GIT_CEILING_DIRECTORIES`, which
//!   only bounds the search and leaves discovery on.)
//! - Aliases: only builtin subcommands are invoked, and git never lets an alias shadow a builtin.
//! - `core.fsmonitor` is off, `core.hooksPath` is fixed, `core.sshCommand` is plain `ssh` and the
//!   credential helper list is reset, all by env config that outranks repo config. This is
//!   hardening, not a fix for repo code that runs in a workspace (see
//!   [`crate::merge::MergePorts::run_package_step`]).
//! - Transports: `protocol.allow=never` with an allowlist (https, ssh, file), so `ext::` and
//!   plain `http`/`git` remotes are refused.
//! - Ambient environment: the command's environment is exactly `GitCommand::env`, so inherited
//!   `GIT_*` variables, `GIT_EXTERNAL_DIFF` and the like are never seen.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::run::{run, RunError};
use crate::types::{is_safe_branch_name, Sha};
use crate::workspace::Workspace;

pub const MAIN_BRANCH: &str = "main";
pub const COMMITTER_NAME: &str = "Tessel Lander";
pub const COMMITTER_EMAIL: &str = "lander@tessel.invalid";
/// Every replayed commit gets this committer date, so a retried rebase yields the same ids.
pub const COMMITTER_DATE: &str = "2026-01-01T00:00:00Z";
/// Git's POST buffer for a push: the gateway's body limit, so git sends no probe request.
pub const MAX_PUSH_BODY_BYTES: u64 = 16 * 1024 * 1024;

const CLONE_TIMEOUT: Duration = Duration::from_secs(60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// The timeout of the quick local commands (base, verify, merge-base, diff, head, show).
pub const LOCAL_TIMEOUT: Duration = Duration::from_secs(6);
/// Upper bound on the quick local git commands of one merge; the time budget is sized from it.
pub const LOCAL_GIT_COMMANDS_MAX: usize = 8;
const REBASE_TIMEOUT: Duration = Duration::from_secs(30);
const PUSH_TIMEOUT: Duration = Duration::from_secs(30);

/// One git invocation. `argv[0]` is `git`; the environment is the whole environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommand {
    pub argv: Vec<OsString>,
    pub env: BTreeMap<String, OsString>,
    pub timeout: Duration,
}

/// What a merge reads. `fork_branch` is the fork's default branch. Where it runs is a
/// [`Workspace`], which only the clone step makes.
#[derive(Debug, Clone)]
pub struct MergeSources {
    pub main_remote: String,
    pub fork_remote: String,
    pub fork_branch: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("refusing to use the unsafe fork branch name {name:?}")]
    UnsafeForkBranch { name: String },
    #[error("cannot create {path} for the git environment: {source}")]
    CreateDir { path: PathBuf, source: io::Error },
    #[error("{path} must be an empty directory: it holds {entry:?}")]
    NotEmpty { path: PathBuf, entry: OsString },
    #[error("cannot read {path}: {source}")]
    ReadDir { path: PathBuf, source: io::Error },
    #[error("cannot check the git version: {source}")]
    VersionCheck { source: RunError },
    #[error(
        "git reported {found:?}; need git {MIN_GIT_MAJOR}.{MIN_GIT_MINOR} or newer \
         (older git ignores GIT_CONFIG_COUNT, which drops the fixed settings)"
    )]
    UnsupportedGit { found: String },
}

const MIN_GIT_MAJOR: u32 = 2;
const MIN_GIT_MINOR: u32 = 31;

/// Accepts the output of `git --version` when it names git 2.31 or newer.
pub fn ensure_supported_git(version_output: &str) -> Result<(), CommandError> {
    let unsupported = || CommandError::UnsupportedGit {
        found: version_output.trim().to_owned(),
    };
    let numbers = version_output
        .trim()
        .strip_prefix("git version ")
        .ok_or_else(unsupported)?;
    let mut parts = numbers
        .split(|c: char| !c.is_ascii_digit())
        .map(str::parse::<u32>);
    let (Some(Ok(major)), Some(Ok(minor))) = (parts.next(), parts.next()) else {
        return Err(unsupported());
    };
    if (major, minor) >= (MIN_GIT_MAJOR, MIN_GIT_MINOR) {
        Ok(())
    } else {
        Err(unsupported())
    }
}

/// The fixed environment of every git command and the builders for the merge's commands.
///
/// A builder that runs in a repository takes a [`Workspace`], never a path:
///
/// ```compile_fail,E0308
/// use std::path::Path;
/// use tessel_lander::git::Git;
///
/// let git = Git::new("/lander/home".into(), "/lander/hooks".into());
/// let _ = git.base_command(Path::new("/a/repository/with/its/own/config"));
/// ```
#[derive(Debug, Clone)]
pub struct Git {
    home: PathBuf,
    hooks_dir: PathBuf,
    path: OsString,
}

impl Git {
    /// Uses `home` as `HOME` and `hooks_dir` as `core.hooksPath`; both must be empty directories
    /// that exist when a command runs. `PATH` is the only variable taken from the process, so
    /// git finds its own helpers.
    pub fn new(home: PathBuf, hooks_dir: PathBuf) -> Git {
        let path = std::env::var_os("PATH").unwrap_or_default();
        Git {
            home,
            hooks_dir,
            path,
        }
    }

    /// Creates empty `home` and `hooks` directories under `root` and uses them. Fails if either
    /// holds anything (a hook there would run) or if `git` is older than 2.31.
    pub fn create_in(root: &Path) -> Result<Git, CommandError> {
        let home = root.join("home");
        let hooks_dir = root.join("hooks");
        for dir in [&home, &hooks_dir] {
            fs::create_dir_all(dir).map_err(|source| CommandError::CreateDir {
                path: dir.clone(),
                source,
            })?;
            let mut entries = fs::read_dir(dir).map_err(|source| CommandError::ReadDir {
                path: dir.clone(),
                source,
            })?;
            if let Some(entry) = entries.next() {
                let entry = entry.map_err(|source| CommandError::ReadDir {
                    path: dir.clone(),
                    source,
                })?;
                return Err(CommandError::NotEmpty {
                    path: dir.clone(),
                    entry: entry.file_name(),
                });
            }
        }
        let git = Git::new(home, hooks_dir);
        git.check_version()?;
        Ok(git)
    }

    /// Runs `git --version` and requires 2.31 or newer.
    pub fn check_version(&self) -> Result<(), CommandError> {
        let output = run(&self.command(&["--version"], LOCAL_TIMEOUT))
            .map_err(|source| CommandError::VersionCheck { source })?;
        ensure_supported_git(&output.stdout)
    }

    /// Settings applied to every command, as `GIT_CONFIG_COUNT` entries. They outrank repo config.
    fn config(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("core.hooksPath", self.hooks_dir.clone().into_os_string()),
            ("transfer.fsckObjects", "true".into()),
            ("core.fsmonitor", "false".into()),
            ("core.attributesFile", "/dev/null".into()),
            ("core.sshCommand", "ssh".into()),
            ("credential.helper", "".into()),
            ("protocol.allow", "never".into()),
            ("protocol.https.allow", "always".into()),
            ("protocol.ssh.allow", "always".into()),
            ("protocol.file.allow", "user".into()),
        ]
    }

    /// The environment of every git command: no user config, no prompt, no editor.
    pub fn env(&self) -> BTreeMap<String, OsString> {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_owned(), self.path.clone());
        env.insert("HOME".to_owned(), self.home.clone().into_os_string());
        env.insert("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".into());
        env.insert("GIT_CONFIG_NOSYSTEM".to_owned(), "1".into());
        env.insert("GIT_ATTR_NOSYSTEM".to_owned(), "1".into());
        env.insert("GIT_TERMINAL_PROMPT".to_owned(), "0".into());
        env.insert("GIT_EDITOR".to_owned(), "true".into());
        env.insert("LC_ALL".to_owned(), "C".into());
        let settings = self.config();
        env.insert(
            "GIT_CONFIG_COUNT".to_owned(),
            settings.len().to_string().into(),
        );
        for (index, (key, value)) in settings.into_iter().enumerate() {
            env.insert(format!("GIT_CONFIG_KEY_{index}"), key.into());
            env.insert(format!("GIT_CONFIG_VALUE_{index}"), value);
        }
        env
    }

    fn command(&self, args: &[&str], timeout: Duration) -> GitCommand {
        let mut argv = vec![OsString::from("git")];
        argv.extend(args.iter().map(OsString::from));
        GitCommand {
            argv,
            env: self.env(),
            timeout,
        }
    }

    fn local(&self, workspace: &Workspace, args: &[&str], timeout: Duration) -> GitCommand {
        let mut command = self.command(&[], timeout);
        command.argv.push("-C".into());
        command.argv.push(workspace.path().into());
        // Without these git searches the parent directories for a repository, so a workspace
        // with no `.git` of its own (an empty directory inside another repository) would run
        // that repository's config.
        command
            .env
            .insert("GIT_DIR".to_owned(), workspace.path().join(".git").into());
        command
            .env
            .insert("GIT_WORK_TREE".to_owned(), workspace.path().into());
        command.argv.extend(args.iter().map(OsString::from));
        command
    }

    /// Clones main with its full history (the rebase needs the merge-base) into `destination`,
    /// which git refuses unless it is missing or empty. [`crate::workspace::clone_main`] runs it
    /// and makes the [`Workspace`].
    pub fn clone_command(&self, sources: &MergeSources, destination: &Path) -> GitCommand {
        let mut command = self.command(
            &[
                "clone",
                "--no-tags",
                &format!("--branch={MAIN_BRANCH}"),
                "--",
            ],
            CLONE_TIMEOUT,
        );
        command.argv.push((&sources.main_remote).into());
        command.argv.push(destination.into());
        command
    }

    /// Fetches only the fork's default branch, into `refs/remotes/fork/<branch>`.
    pub fn fetch_fork_command(
        &self,
        workspace: &Workspace,
        sources: &MergeSources,
    ) -> Result<GitCommand, CommandError> {
        let branch = &sources.fork_branch;
        let reference = fork_ref(branch)?;
        Ok(self.local(
            workspace,
            &[
                "fetch",
                "--no-tags",
                "--",
                &sources.fork_remote,
                &format!("+refs/heads/{branch}:{reference}"),
            ],
            FETCH_TIMEOUT,
        ))
    }

    /// Resolves main as cloned. Stdout is the sha.
    pub fn base_command(&self, workspace: &Workspace) -> GitCommand {
        let reference = format!("refs/remotes/origin/{MAIN_BRANCH}^{{commit}}");
        self.local(
            workspace,
            &["rev-parse", "--verify", &reference],
            LOCAL_TIMEOUT,
        )
    }

    /// Exit 0 when `main` is a commit on main's history as cloned.
    pub fn on_main_command(&self, workspace: &Workspace, main: &Sha) -> GitCommand {
        let reference = format!("refs/remotes/origin/{MAIN_BRANCH}");
        self.local(
            workspace,
            &["merge-base", "--is-ancestor", main.as_str(), &reference],
            LOCAL_TIMEOUT,
        )
    }

    /// Exit 0 when the commit object was fetched, 1 when it was not.
    pub fn commit_exists_command(&self, workspace: &Workspace, commit: &Sha) -> GitCommand {
        let object = format!("{commit}^{{commit}}");
        self.local(
            workspace,
            &["rev-parse", "--verify", "--quiet", &object],
            LOCAL_TIMEOUT,
        )
    }

    /// Exit 0 when the commit is reachable from the fork's default branch, 1 when it is not.
    pub fn reachable_command(
        &self,
        workspace: &Workspace,
        sources: &MergeSources,
        commit: &Sha,
    ) -> Result<GitCommand, CommandError> {
        let reference = fork_ref(&sources.fork_branch)?;
        Ok(self.local(
            workspace,
            &["merge-base", "--is-ancestor", commit.as_str(), &reference],
            LOCAL_TIMEOUT,
        ))
    }

    /// Stdout is the merge-base of main and the commit.
    pub fn merge_base_command(
        &self,
        workspace: &Workspace,
        base: &Sha,
        commit: &Sha,
    ) -> GitCommand {
        self.local(
            workspace,
            &["merge-base", base.as_str(), commit.as_str()],
            LOCAL_TIMEOUT,
        )
    }

    /// Replays `merge_base..commit` onto `base` with a fixed committer; leaves HEAD at the result.
    /// Author name, email and date stay those of the replayed commits.
    pub fn rebase_command(
        &self,
        workspace: &Workspace,
        base: &Sha,
        merge_base: &Sha,
        commit: &Sha,
    ) -> GitCommand {
        let mut command = self.local(
            workspace,
            &[
                "-c",
                "commit.gpgsign=false",
                "rebase",
                "--onto",
                base.as_str(),
                merge_base.as_str(),
                commit.as_str(),
            ],
            REBASE_TIMEOUT,
        );
        command
            .env
            .insert("GIT_COMMITTER_NAME".to_owned(), COMMITTER_NAME.into());
        command
            .env
            .insert("GIT_COMMITTER_EMAIL".to_owned(), COMMITTER_EMAIL.into());
        command
            .env
            .insert("GIT_COMMITTER_DATE".to_owned(), COMMITTER_DATE.into());
        command
    }

    /// Stdout is the NUL-separated repo paths that are unmerged after a stopped rebase.
    pub fn conflicts_command(&self, workspace: &Workspace) -> GitCommand {
        self.local(
            workspace,
            &["diff", "--name-only", "--diff-filter=U", "-z"],
            LOCAL_TIMEOUT,
        )
    }

    /// Stdout is the NUL-separated `git diff --name-status` records of `base..head`, with renames
    /// detected. `-z` keeps paths unquoted; they are untrusted data and are never put in a
    /// command.
    pub fn changed_files_command(
        &self,
        workspace: &Workspace,
        base: &Sha,
        head: &Sha,
    ) -> GitCommand {
        let range = format!("{base}..{head}");
        self.local(
            workspace,
            &[
                "diff",
                "--name-status",
                "-z",
                "-M",
                "--no-ext-diff",
                &range,
                "--",
            ],
            LOCAL_TIMEOUT,
        )
    }

    /// Stdout is the patch of `base..head` for a person to read: renames detected, no colour, and
    /// no external diff or text conversion program. Pass the merge-base as `base` to see only what
    /// the commit adds.
    pub fn diff_command(&self, workspace: &Workspace, base: &Sha, head: &Sha) -> GitCommand {
        let range = format!("{base}..{head}");
        self.local(
            workspace,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "-M",
                &range,
                "--",
            ],
            LOCAL_TIMEOUT,
        )
    }

    /// Stdout is the content of `path` in `commit` (exit non-zero when it is not there).
    pub fn show_file_command(&self, workspace: &Workspace, commit: &Sha, path: &str) -> GitCommand {
        let object = format!("{commit}:{path}");
        self.local(workspace, &["show", &object], LOCAL_TIMEOUT)
    }

    /// Stdout is the sha HEAD points at.
    pub fn head_command(&self, workspace: &Workspace) -> GitCommand {
        self.local(
            workspace,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            LOCAL_TIMEOUT,
        )
    }

    /// Pushes `head` to main only if main is still `base`. Never forces: `head` descends from
    /// `base`, and the lease names the sha that was read at the start.
    pub fn push_command(
        &self,
        workspace: &Workspace,
        sources: &MergeSources,
        base: &Sha,
        head: &Sha,
    ) -> GitCommand {
        self.local(
            workspace,
            &[
                "-c",
                &format!("http.postBuffer={MAX_PUSH_BODY_BYTES}"),
                "push",
                "--porcelain",
                &format!("--force-with-lease=refs/heads/{MAIN_BRANCH}:{base}"),
                "--",
                &sources.main_remote,
                &format!("{head}:refs/heads/{MAIN_BRANCH}"),
            ],
            PUSH_TIMEOUT,
        )
    }
}

fn fork_ref(branch: &str) -> Result<String, CommandError> {
    if !is_safe_branch_name(branch) {
        return Err(CommandError::UnsafeForkBranch {
            name: branch.to_owned(),
        });
    }
    Ok(format!("refs/remotes/fork/{branch}"))
}

/// Splits `git diff -z` output into paths. Paths are untrusted data.
pub fn parse_nul_separated(stdout: &str) -> Vec<String> {
    stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

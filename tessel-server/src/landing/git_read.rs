//! Reading refs and ancestry from the remote with the lander's own git environment.
//!
//! The lander's clone and merge steps live in `tessel-lander`. These are the reads around them:
//! where a branch of a remote points, and which commits are on main.

use std::ffi::OsString;
use std::time::Duration;

use tessel_lander::git::{Git, GitCommand, MergeSources, MAIN_BRANCH};
use tessel_lander::run::{run, GitRunner, SystemRunner};
use tessel_lander::types::Sha;
use tessel_lander::workspace::{clone_main, CloneOutcome};

use super::names::{quote, MAX_LOGGED_CHARS};

const LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a read of the remote did not give an answer. The text is safe to log: git's output in it is
/// bounded and quoted.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ReadError(pub String);

fn failed(what: &str, detail: impl std::fmt::Display) -> ReadError {
    ReadError(format!("{what}: {detail}"))
}

/// Where `branch` of `remote` points, or `None` when the remote has no such branch.
pub fn read_branch(git: &Git, remote: &str, branch: &str) -> Result<Option<Sha>, ReadError> {
    let reference = format!("refs/heads/{branch}");
    let command = GitCommand {
        argv: ["git", "ls-remote", "--"]
            .into_iter()
            .map(OsString::from)
            .chain([OsString::from(remote), OsString::from(&reference)])
            .collect(),
        env: git.env(),
        timeout: LS_REMOTE_TIMEOUT,
    };
    let output = run(&command).map_err(|error| failed("cannot run git ls-remote", error))?;
    if !output.success() {
        return Err(failed(
            "git ls-remote did not succeed",
            format!(
                "exit {:?}, stderr {}",
                output.exit_code,
                quote(&output.stderr, MAX_LOGGED_CHARS)
            ),
        ));
    }
    for line in output.stdout.lines() {
        if let Some((sha, name)) = line.split_once('\t') {
            if name == reference {
                return Sha::parse(sha)
                    .map(Some)
                    .map_err(|_| failed("git ls-remote named no commit", quote(line, 100)));
            }
        }
    }
    Ok(None)
}

/// Main's head and which of `commits` are ancestors of it, from a clone of main in `clone_dir`.
pub fn main_and_ancestors(
    git: &Git,
    remote: &str,
    clone_dir: &std::path::Path,
    commits: &[Sha],
) -> Result<(Sha, Vec<Sha>), ReadError> {
    let sources = MergeSources {
        main_remote: remote.to_owned(),
        fork_remote: String::new(),
        fork_branch: MAIN_BRANCH.to_owned(),
    };
    let workspace = match clone_main(git, &SystemRunner, &sources, clone_dir) {
        Ok(CloneOutcome::Cloned(workspace)) => workspace,
        Ok(CloneOutcome::Failed(output)) => {
            return Err(failed(
                "cannot clone main",
                format!(
                    "exit {:?}, stderr {}",
                    output.exit_code,
                    quote(&output.stderr, MAX_LOGGED_CHARS)
                ),
            ))
        }
        Err(error) => return Err(failed("cannot clone main", error)),
    };
    let head = SystemRunner
        .run(&git.base_command(&workspace))
        .map_err(|error| failed("cannot read main's head", error))?;
    let main = Sha::parse(head.stdout.trim())
        .map_err(|_| failed("main's head is not a commit", quote(&head.stdout, 100)))?;
    let mut landed = Vec::new();
    for commit in commits {
        let known = SystemRunner
            .run(&git.commit_exists_command(&workspace, commit))
            .map_err(|error| failed("cannot look for a commit", error))?;
        match known.exit_code {
            Some(0) => {}
            // A commit main's history does not hold cannot be an ancestor of main.
            Some(1) => continue,
            Some(_) | None => {
                return Err(failed(
                    "git rev-parse did not answer",
                    quote(&known.stderr, MAX_LOGGED_CHARS),
                ))
            }
        }
        let answer = SystemRunner
            .run(&git.on_main_command(&workspace, commit))
            .map_err(|error| failed("cannot check ancestry", error))?;
        match answer.exit_code {
            Some(0) => landed.push(commit.clone()),
            Some(1) => {}
            Some(_) | None => {
                return Err(failed(
                    "git merge-base --is-ancestor did not answer",
                    quote(&answer.stderr, MAX_LOGGED_CHARS),
                ))
            }
        }
    }
    Ok((main, landed))
}

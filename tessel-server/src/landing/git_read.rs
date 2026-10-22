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

/// `git ls-remote` for `branch` of `remote`, in the lander's fixed environment. It runs outside any
/// workspace, so `GIT_DIR` names the lander's empty `HOME`: git then finds no repository, and the
/// config of whatever repository the server's working directory sits in (a `url.*.insteadOf`, say)
/// cannot redirect the read.
pub(super) fn ls_remote_command(git: &Git, remote: &str, reference: &str) -> GitCommand {
    let mut env = git.env();
    if let Some(home) = env.get("HOME").cloned() {
        env.insert("GIT_DIR".to_owned(), home);
    }
    GitCommand {
        argv: ["git", "ls-remote", "--"]
            .into_iter()
            .map(OsString::from)
            .chain([OsString::from(remote), OsString::from(reference)])
            .collect(),
        env,
        timeout: LS_REMOTE_TIMEOUT,
    }
}

/// Where `branch` of `remote` points, or `None` when the remote has no such branch.
pub fn read_branch(git: &Git, remote: &str, branch: &str) -> Result<Option<Sha>, ReadError> {
    let reference = format!("refs/heads/{branch}");
    let command = ls_remote_command(git, remote, &reference);
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

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn run_git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// Runs `command` as the lander's runner would, but from `cwd`, which the real runner takes
    /// from the server process.
    fn run_in(cwd: &std::path::Path, command: &GitCommand) -> String {
        let (program, args) = command.argv.split_first().unwrap();
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(&command.env)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn the_read_ignores_the_config_of_the_repository_the_server_runs_in() {
        let root = tempfile::tempdir().unwrap();
        let (real, decoy, cwd) = (
            root.path().join("real.git"),
            root.path().join("decoy.git"),
            root.path().join("cwd"),
        );
        for bare in [&real, &decoy] {
            std::fs::create_dir(bare).unwrap();
            run_git(
                bare,
                &["init", "--quiet", "--bare", "--initial-branch=main"],
            );
        }
        std::fs::create_dir(&cwd).unwrap();
        run_git(&cwd, &["init", "--quiet", "--initial-branch=main"]);
        for (repo, text) in [(&real, "real"), (&decoy, "decoy")] {
            let tree = run_git(repo, &["hash-object", "-w", "-t", "tree", "/dev/null"]);
            let commit = run_git(repo, &["commit-tree", &tree, "-m", text]);
            run_git(repo, &["update-ref", "refs/heads/main", &commit]);
        }
        let real_head = run_git(&real, &["rev-parse", "main"]);
        let key = format!("url.{}.insteadOf", decoy.display());
        run_git(&cwd, &["config", &key, &real.display().to_string()]);
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let git = Git::new(home.clone(), root.path().join("hooks"));
        let command = ls_remote_command(&git, &real.display().to_string(), "refs/heads/main");
        assert_eq!(
            command.env.get("GIT_DIR"),
            Some(&home.clone().into_os_string())
        );
        let seen = run_in(&cwd, &command);
        assert!(seen.starts_with(&real_head), "{seen}");
    }
}

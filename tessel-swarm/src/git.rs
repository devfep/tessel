//! Plain git and node, run as child processes in scratch directories.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::demo::Tree;

/// Every commit the harness makes carries this date, so the same tree gives the same commit id
/// on any machine.
const COMMIT_DATE: &str = "2026-10-01T00:00:00Z";

/// A git working directory plus the environment its commands run with. Secrets are replaced with
/// `[redacted]` in any text this type returns.
#[derive(Debug, Clone)]
pub struct Git {
    pub dir: PathBuf,
    env: Vec<(String, String)>,
    secrets: Vec<String>,
}

impl Git {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            env: Vec::new(),
            secrets: Vec::new(),
        }
    }

    /// Sends `Authorization: Bearer <token>` with every request this git makes. The token travels
    /// in the environment, never in a URL or an argument.
    #[must_use]
    pub fn with_bearer(mut self, token: &str) -> Self {
        self.env = vec![
            ("GIT_CONFIG_COUNT".into(), "1".into()),
            ("GIT_CONFIG_KEY_0".into(), "http.extraHeader".into()),
            (
                "GIT_CONFIG_VALUE_0".into(),
                format!("Authorization: Bearer {token}"),
            ),
        ];
        self.secrets.push(token.to_string());
        self
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.dir)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args([
                "-c",
                "user.name=swarm",
                "-c",
                "user.email=swarm@example.invalid",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_DATE", COMMIT_DATE)
            .env("GIT_COMMITTER_DATE", COMMIT_DATE);
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd
    }

    fn scrub(&self, text: &str) -> String {
        self.secrets.iter().fold(text.to_string(), |acc, secret| {
            acc.replace(secret, "[redacted]")
        })
    }

    /// Runs git and returns trimmed stdout. Fails on a non-zero exit with git's own message.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let (ok, out) = self.attempt(args)?;
        if !ok {
            bail!("git {} failed: {}", args.join(" "), out.trim());
        }
        Ok(out.trim().to_string())
    }

    /// Runs git and returns whether it succeeded, with its combined output.
    pub fn attempt(&self, args: &[&str]) -> Result<(bool, String)> {
        let output = self
            .command(args)
            .output()
            .with_context(|| format!("cannot run git in {}", self.dir.display()))?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok((output.status.success(), self.scrub(&text)))
    }

    /// Stages everything, commits, and returns the commit id.
    pub fn commit_all(&self, message: &str) -> Result<String> {
        self.run(&["add", "-A"])?;
        self.run(&["commit", "-q", "--no-verify", "-m", message])?;
        self.run(&["rev-parse", "HEAD"])
    }
}

fn skipped(name: &str) -> bool {
    name == ".git" || name == "node_modules"
}

/// Makes `dir` hold exactly `tree` (apart from `.git`): writes every file and removes the others.
pub fn write_tree(dir: &Path, tree: &Tree) -> Result<()> {
    let mut existing = Vec::new();
    collect(dir, dir, &mut existing)?;
    for path in existing.iter().filter(|p| !tree.contains_key(*p)) {
        fs::remove_file(dir.join(path)).with_context(|| format!("cannot remove {path}"))?;
    }
    for (path, text) in tree {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::read_to_string(&target).ok().as_deref() != Some(text) {
            fs::write(&target, text).with_context(|| format!("cannot write {path}"))?;
        }
    }
    Ok(())
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let entry = entry?;
        if skipped(&entry.file_name().to_string_lossy()) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            collect(root, &entry.path(), out)?;
        } else {
            let relative = entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(relative);
        }
    }
    Ok(())
}

pub fn read_tree(dir: &Path) -> Result<Tree> {
    let mut paths = Vec::new();
    collect(dir, dir, &mut paths)?;
    let mut tree = Tree::new();
    for path in paths {
        let text =
            fs::read_to_string(dir.join(&path)).with_context(|| format!("cannot read {path}"))?;
        tree.insert(path, text);
    }
    Ok(tree)
}

/// A new repository on branch `main` holding `tree` in one commit; returns the commit id.
pub fn init_repo(git: &Git, tree: &Tree) -> Result<String> {
    git.run(&["init", "-q", "-b", "main"])?;
    write_tree(&git.dir, tree)?;
    git.commit_all("Starting repository")
}

/// What running the repository's checks on a checkout found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checks {
    Pass {
        tests: u32,
    },
    /// A module could not be linked: a test or module imports something that is gone.
    BuildFailed,
    TestsFailed,
}

/// Runs `node --test` in `dir`. A link failure (a missing export or module) is a failed build;
/// any other failure is a failed test.
pub fn run_checks(dir: &Path) -> Result<Checks> {
    let output = Command::new("node")
        .arg("--test")
        .current_dir(dir)
        .env_remove("NODE_OPTIONS")
        .output()
        .context("cannot run node; the demo repository's tests need Node 22.18 or newer")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        return Ok(Checks::Pass {
            tests: passed_count(&text),
        });
    }
    let link = [
        "does not provide an export named",
        "ERR_MODULE_NOT_FOUND",
        "Cannot find module",
    ];
    if link.iter().any(|needle| text.contains(needle)) {
        return Ok(Checks::BuildFailed);
    }
    Ok(Checks::TestsFailed)
}

fn passed_count(text: &str) -> u32 {
    text.lines()
        .find_map(|line| line.strip_prefix("# pass "))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::base_tree;

    #[test]
    fn the_starting_repository_passes_its_own_tests() {
        let dir = tempfile::tempdir().unwrap();
        let git = Git::new(dir.path());
        init_repo(&git, &base_tree()).unwrap();
        assert_eq!(run_checks(dir.path()).unwrap(), Checks::Pass { tests: 12 });
    }

    #[test]
    fn same_tree_gives_the_same_commit_id() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let first = init_repo(&Git::new(a.path()), &base_tree()).unwrap();
        let second = init_repo(&Git::new(b.path()), &base_tree()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn write_then_read_round_trips_and_removes_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut tree = base_tree();
        write_tree(dir.path(), &tree).unwrap();
        assert_eq!(read_tree(dir.path()).unwrap(), tree);
        tree.remove("test/taxFor.test.ts");
        write_tree(dir.path(), &tree).unwrap();
        assert_eq!(read_tree(dir.path()).unwrap(), tree);
    }

    #[test]
    fn output_never_contains_the_bearer_token() {
        let dir = tempfile::tempdir().unwrap();
        let git = Git::new(dir.path()).with_bearer("s3cret-token");
        git.run(&["init", "-q"]).unwrap();
        let (_, listing) = git.attempt(&["config", "--list"]).unwrap();
        assert!(listing.contains("extraheader"), "{listing}");
        assert!(!listing.contains("s3cret-token"), "{listing}");
        assert!(listing.contains("[redacted]"));
    }
}

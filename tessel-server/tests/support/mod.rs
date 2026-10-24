//! Fixtures for the landing tests: a bare main, a bare fork per agent and a work tree that feeds
//! both, built with plain git under a fixed author. Remotes are paths, which the lander's git
//! environment reads (`protocol.file.allow=user`).

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tessel_runner::{ResultStep, RunRequest, RunResult, RunnerError};
use tessel_server::config::{LanderConfig, RepositoryConfig};
use tessel_server::landing::TestRunner;

/// The gate every fixture repository declares.
pub const GATE: &str =
    "instance = \"standard-1\"\n\n[[test]]\nargv = [\"cargo\", \"test\", \"--offline\"]\n";

/// Runs git in `repo` with a cleared environment and a fixed author, and returns trimmed stdout.
pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.invalid")
        .env("GIT_AUTHOR_DATE", "2025-03-04T05:06:07+02:00")
        .env("GIT_COMMITTER_NAME", "Fixture Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.invalid")
        .env("GIT_COMMITTER_DATE", "2025-03-04T05:06:07+02:00")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_owned()
}

/// A main, the fork of agent `a1` and a work tree, all under one temporary directory.
pub struct Repos {
    pub root: tempfile::TempDir,
    pub main: PathBuf,
    pub forks: PathBuf,
    pub work: PathBuf,
}

impl Repos {
    /// Main and the fork of `a1` hold one commit: `tessel.toml`, `src/a.rs` and `README`.
    pub fn new() -> Repos {
        let root = tempfile::tempdir().expect("tempdir");
        let (main, forks, work) = (
            root.path().join("main.git"),
            root.path().join("forks"),
            root.path().join("work"),
        );
        let fork = forks.join("a1.git");
        for bare in [&main, &fork] {
            fs::create_dir_all(bare).expect("bare dir");
            git(
                bare,
                &["init", "--quiet", "--bare", "--initial-branch=main"],
            );
        }
        fs::create_dir(&work).expect("work dir");
        git(&work, &["init", "--quiet", "--initial-branch=main"]);
        let repos = Repos {
            root,
            main,
            forks,
            work,
        };
        repos.write(&[
            ("tessel.toml", GATE),
            ("src/a.rs", "one\n"),
            ("README", "r\n"),
        ]);
        git(&repos.work, &["add", "--all"]);
        git(&repos.work, &["commit", "--quiet", "-m", "base"]);
        repos.push(&repos.main);
        repos.push(&fork);
        repos
    }

    fn write(&self, files: &[(&str, &str)]) {
        for (name, contents) in files {
            let path = self.work.join(name);
            fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            fs::write(path, contents).expect("write");
        }
    }

    fn push(&self, remote: &Path) {
        let remote = remote.display().to_string();
        git(
            &self.work,
            &["push", "--quiet", "--force", &remote, "HEAD:main"],
        );
    }

    pub fn head(&self) -> String {
        git(&self.work, &["rev-parse", "HEAD"])
    }

    pub fn main_head(&self) -> String {
        git(&self.main, &["rev-parse", "main"])
    }

    pub fn fork_of(&self, agent: &str) -> PathBuf {
        self.forks.join(format!("{agent}.git"))
    }

    /// Commits `files` on the work tree's head and returns the commit.
    pub fn commit(&self, files: &[(&str, &str)]) -> String {
        self.write(files);
        git(&self.work, &["add", "--all"]);
        git(&self.work, &["commit", "--quiet", "-m", "change"]);
        self.head()
    }

    /// Commits `files` on the work tree's head, pushes the commit to a new branch of the fork of
    /// `a1` without force, and puts the work tree back where it was.
    pub fn fork_commit(&self, files: &[(&str, &str)]) -> String {
        let before = self.head();
        let commit = self.commit(files);
        let fork = self.fork_of("a1").display().to_string();
        let target = format!("HEAD:refs/heads/tessel/{commit}");
        git(&self.work, &["push", "--quiet", &fork, &target]);
        git(&self.work, &["reset", "--quiet", "--hard", &before]);
        commit
    }

    /// Commits `files` on the work tree's head, force-pushes the commit to the fork's `main` and
    /// puts the work tree back. A Trial without a commit reads that `main`.
    pub fn fork_main_commit(&self, files: &[(&str, &str)]) -> String {
        let before = self.head();
        let commit = self.commit(files);
        self.push(&self.fork_of("a1"));
        git(&self.work, &["reset", "--quiet", "--hard", &before]);
        commit
    }

    /// Commits `files` on the work tree's head and pushes it to main.
    pub fn main_commit(&self, files: &[(&str, &str)]) -> String {
        let commit = self.commit(files);
        self.push(&self.main);
        commit
    }

    pub fn show_on_main(&self, path: &str) -> String {
        git(&self.main, &["show", &format!("main:{path}")])
    }

    /// The `[[repository]]` keys that make the server land on this main.
    pub fn repository(&self, name: &str) -> RepositoryConfig {
        RepositoryConfig {
            tenant: "acme".to_string(),
            name: name.to_string(),
            run: "test".to_string(),
            reviewers: Vec::new(),
            sensitive: Vec::new(),
            remote: Some(self.main.display().to_string()),
            fork: Some(format!("{}/{{agent}}.git", self.forks.display())),
        }
    }

    /// The `[lander]` table, with the runner configuration at `runner.toml` under the root.
    pub fn lander(&self) -> LanderConfig {
        LanderConfig {
            work_dir: self.root.path().join("lander"),
            runner_config: self.root.path().join("runner.toml"),
            token_file: None,
        }
    }

    /// Where a landing's clones are made for `name`.
    pub fn clones(&self, name: &str) -> PathBuf {
        self.root.path().join("lander/acme").join(name)
    }

    /// Writes a runner configuration for the user running the tests, mode 0600.
    pub fn write_runner_config(&self, mode: u32) {
        let login = tessel_runner::effective_login().expect("a login");
        let path = self.lander().runner_config;
        fs::write(&path, format!("[runner]\nuser = \"{login}\"\n")).expect("runner config");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod");
    }
}

#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every line the process logged so far that mentions `needle`. One subscriber serves the whole
/// test binary; a test looks only for lines of its own repository name.
pub fn log_lines_with(needle: &str) -> Vec<String> {
    static LOG: OnceLock<Captured> = OnceLock::new();
    let log = LOG.get_or_init(|| {
        let log = Captured::default();
        let sink = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
        log
    });
    let text =
        String::from_utf8_lossy(&log.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned();
    text.lines()
        .filter(|line| line.contains(needle))
        .map(str::to_owned)
        .collect()
}

/// Installs the subscriber, so lines logged from now on are captured.
pub fn capture_logs() {
    log_lines_with("");
}

// ---- A runner that is not the runner ----

pub type Script = Box<dyn Fn(&RunRequest) -> Result<RunResult, RunnerError> + Send + Sync>;

/// A runner that records what it was asked and answers from a script.
pub struct Scripted {
    pub seen: Mutex<Vec<RunRequest>>,
    script: Script,
}

impl Scripted {
    pub fn new(
        script: impl Fn(&RunRequest) -> Result<RunResult, RunnerError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            script: Box::new(script),
        })
    }

    pub fn runs(&self) -> usize {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl TestRunner for Scripted {
    fn run(&self, request: &RunRequest) -> Result<RunResult, RunnerError> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        (self.script)(request)
    }
}

pub fn result(exit_code: i32, first_failure: Option<&str>) -> RunResult {
    RunResult {
        step: ResultStep::Test,
        exit_code,
        passed: exit_code == 0,
        timed_out: false,
        stdout: "out".to_string(),
        stderr: "err".to_string(),
        stdout_truncated: false,
        stderr_truncated: false,
        first_failure: first_failure.map(str::to_owned),
        wall_ms: 1,
    }
}

pub fn passing() -> Arc<Scripted> {
    Scripted::new(|_| Ok(result(0, None)))
}

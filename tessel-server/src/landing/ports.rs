//! What a landing needs from the machine it runs on: the runner that runs the repository's tests,
//! and the lander's ports over a git remote, reached by path or by URL.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use tessel_lander::gate::{parse_gate_config, GateConfig, MAX_CONFIG_BYTES, TESSEL_TOML_PATH};
use tessel_lander::git::{Git, MAIN_BRANCH};
use tessel_lander::merge::{BoundaryError, MergePorts, PushUpdate};
use tessel_lander::outcome::{EvidenceResult, LandedMerge, PackageStep, StepOutcome};
use tessel_lander::trial::TrialPorts;
use tessel_lander::types::Sha;
use tessel_lander::workspace::TestTree;
use tessel_runner::{RequestTest, RunRequest, RunResult, RunnerConfig, RunnerError};

use super::deadline::Stop;
use super::git_read::read_branch;
use super::names::{quote, MAX_LOGGED_CHARS};

/// Runs a repository's test commands. The one boundary where repository code runs (rule 4).
pub trait TestRunner: Send + Sync + 'static {
    fn run(&self, request: &RunRequest) -> Result<RunResult, RunnerError>;
}

/// The runner library, started for the user its configuration file names. The file is read and
/// checked on every run, so a file made unreadable, or a user who is no longer the one named, stops
/// the next run before it starts.
#[derive(Debug)]
pub struct LocalRunner {
    config_file: PathBuf,
}

impl LocalRunner {
    /// Checks the configuration once, so a server that could never run a test does not start.
    pub fn new(config_file: PathBuf) -> Result<Self, RunnerError> {
        RunnerConfig::load(&config_file)?;
        Ok(Self { config_file })
    }
}

impl TestRunner for LocalRunner {
    fn run(&self, request: &RunRequest) -> Result<RunResult, RunnerError> {
        let config = RunnerConfig::load(&self.config_file)?;
        tessel_runner::run(&config, request)
    }
}

fn boundary(operation: &'static str, message: impl Into<String>) -> BoundaryError {
    BoundaryError {
        operation,
        message: message.into(),
    }
}

/// Why the trunk's gate could not be read.
#[derive(Debug)]
enum GateFault {
    /// A fact about the trunk, not a failure of the machine: the file is missing or not a valid
    /// gate. Retrying cannot change it, so the landing is refused. Holds the log message.
    Unusable(String),
    /// The read itself failed, which a retry may get past.
    Unreadable(BoundaryError),
}

/// The trunk's gate, read from the test tree. A submission cannot change `tessel.toml` (the
/// lander refuses it before the tests), so the tree's file is main's.
fn read_gate(tree: &TestTree) -> Result<GateConfig, GateFault> {
    read_gate_in(tree.path())
}

fn read_gate_in(root: &Path) -> Result<GateConfig, GateFault> {
    let path = root.join(TESSEL_TOML_PATH);
    let missing = || {
        GateFault::Unusable(format!(
            "the trunk has no {TESSEL_TOML_PATH} to say how to test it"
        ))
    };
    let meta = fs::symlink_metadata(&path).map_err(|_| missing())?;
    if !meta.is_file() {
        return Err(missing());
    }
    let limit = u64::try_from(MAX_CONFIG_BYTES + 1).unwrap_or(u64::MAX);
    let mut text = String::new();
    File::open(&path)
        .and_then(|file| file.take(limit).read_to_string(&mut text))
        .map_err(|_| {
            GateFault::Unreadable(boundary(
                "read the gate",
                format!("cannot read {TESSEL_TOML_PATH}"),
            ))
        })?;
    parse_gate_config(&text).map_err(|invalid| {
        GateFault::Unusable(format!(
            "the trunk's {TESSEL_TOML_PATH} is not a valid gate: {}",
            quote(&invalid.reason, MAX_LOGGED_CHARS)
        ))
    })
}

/// The lander's ports for one call over a git remote. The credential, when the remote needs one, is
/// the token file git reads through its askpass hook; the ports mint and revoke nothing.
///
/// The tests run through [`TestRunner`] on the lander's `TestTree`. The install step is not run:
/// the machine's own toolchain and dependencies are what the tests use.
pub struct Ports<'a> {
    pub(super) git: &'a Git,
    pub(super) remote: &'a str,
    pub(super) runner: &'a dyn TestRunner,
    pub(super) stop: &'a Stop,
    first_failure: Mutex<Option<String>>,
    gate_refusal: Mutex<Option<String>>,
}

impl<'a> Ports<'a> {
    pub fn new(git: &'a Git, remote: &'a str, runner: &'a dyn TestRunner, stop: &'a Stop) -> Self {
        Self {
            git,
            remote,
            runner,
            stop,
            first_failure: Mutex::new(None),
            gate_refusal: Mutex::new(None),
        }
    }

    /// The first failing test of the last run, bounded and quoted, for the server log only.
    pub fn take_first_failure(&self) -> Option<String> {
        self.first_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Why the trunk's gate is missing or invalid, when the last run stopped for that: a refusal,
    /// not an infrastructure failure. The parser's error is quoted and bounded.
    pub fn take_gate_refusal(&self) -> Option<String> {
        self.gate_refusal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn step(&self, step: PackageStep, tree: &TestTree) -> Result<StepOutcome, BoundaryError> {
        match self.before_step(step)? {
            Some(done) => Ok(done),
            None => self.run_tests(tree),
        }
    }

    /// What needs no tree: refusing a step once the call is over, and the install step, which is
    /// not run here and passes. `None` for the test step, which runs.
    fn before_step(&self, step: PackageStep) -> Result<Option<StepOutcome>, BoundaryError> {
        if self.stop.requested() {
            return Err(boundary(
                "run_package_step",
                "the call's deadline passed or the call was given up before the tests ran",
            ));
        }
        Ok(match step {
            PackageStep::Install => Some(StepOutcome::finished(step, 0, "", "")),
            PackageStep::Test => None,
        })
    }

    fn run_tests(&self, tree: &TestTree) -> Result<StepOutcome, BoundaryError> {
        let gate = match read_gate(tree) {
            Ok(gate) => gate,
            Err(GateFault::Unreadable(error)) => return Err(error),
            Err(GateFault::Unusable(message)) => {
                *self
                    .gate_refusal
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(message.clone());
                return Err(boundary("read the gate", message));
            }
        };
        let Some(tree_path) = tree.path().to_str() else {
            return Err(boundary(
                "run_package_step",
                "the test tree's path is not UTF-8",
            ));
        };
        let request = RunRequest {
            tree: tree_path.to_owned(),
            tests: gate
                .tests
                .into_iter()
                .map(|test| RequestTest {
                    dir: test.dir,
                    argv: test.argv,
                })
                .collect(),
            timeout_secs: self.stop.run_seconds(),
        };
        let result = self
            .runner
            .run(&request)
            .map_err(|error| boundary("run_package_step", error.to_string()))?;
        if let Some(block) = &result.first_failure {
            *self
                .first_failure
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(quote(block, MAX_LOGGED_CHARS));
        }
        Ok(result.into_step_outcome())
    }
}

impl MergePorts for Ports<'_> {
    /// Nothing is minted for a read, so nothing is revoked. A token file is read only by git's
    /// askpass hook, while one of the lander's git commands runs; none is running when this
    /// returns. This is not the isolation the trait describes, though: the runner runs in this
    /// process as the server's own user, so the repository's test code can read a token file the
    /// server can. A trunk whose tests are not trusted needs its tests run in CI, or the token
    /// scoped to the repositories it lands on and nothing more.
    fn revoke_read_access(&self) -> bool {
        true
    }

    fn run_package_step(
        &self,
        step: PackageStep,
        tree: &TestTree,
    ) -> Result<StepOutcome, BoundaryError> {
        self.step(step, tree)
    }

    /// Nothing is minted for the push: it uses the token file through git's askpass hook, or no
    /// credential for a path or a public remote, and nothing is revoked afterwards. A call past
    /// its deadline, or given up, must not push.
    fn with_push_access<T>(
        &self,
        _update: &PushUpdate,
        use_access: impl FnOnce() -> T,
    ) -> Result<T, BoundaryError> {
        if self.stop.requested() {
            return Err(boundary(
                "with_push_access",
                "the call's deadline passed or the call was given up, so main was not pushed",
            ));
        }
        Ok(use_access())
    }

    fn current_main(&self) -> Option<Sha> {
        match read_branch(self.git, self.remote, MAIN_BRANCH) {
            Ok(main) => main,
            Err(error) => {
                tracing::warn!(error = %error, "cannot read main back after the push");
                None
            }
        }
    }

    /// No evidence record is written until P1-EVIDENCE.
    fn record_merge(&self, _landed: &LandedMerge) -> EvidenceResult {
        EvidenceResult::NotWritten {
            reason: "the local lander writes no evidence record yet".to_owned(),
        }
    }
}

impl TrialPorts for Ports<'_> {
    fn revoke_read_access(&self) -> bool {
        true
    }

    fn run_package_step(
        &self,
        step: PackageStep,
        tree: &TestTree,
    ) -> Result<StepOutcome, BoundaryError> {
        self.step(step, tree)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::repo::now_ms;

    struct NeverRuns;

    impl TestRunner for NeverRuns {
        fn run(&self, _request: &RunRequest) -> Result<RunResult, RunnerError> {
            unreachable!("no test run is started in these tests")
        }
    }

    fn update() -> PushUpdate {
        let sha = |digit: &str| Sha::parse(&digit.repeat(40)).unwrap();
        PushUpdate {
            base: sha("a"),
            head: sha("b"),
        }
    }

    fn trunk_with(bytes: Option<&[u8]>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        if let Some(bytes) = bytes {
            fs::write(dir.path().join(TESSEL_TOML_PATH), bytes).unwrap();
        }
        dir
    }

    fn assert_unusable(result: Result<GateConfig, GateFault>, expected: &str) {
        match result {
            Err(GateFault::Unusable(message)) => {
                assert!(message.contains(expected), "{message}");
            }
            other => panic!("expected an unusable gate, got {other:?}"),
        }
    }

    fn assert_unreadable(result: Result<GateConfig, GateFault>) {
        assert!(
            matches!(result, Err(GateFault::Unreadable(_))),
            "expected a failed read, got {result:?}"
        );
    }

    #[test]
    fn a_trunk_with_no_gate_file_is_unusable() {
        let dir = trunk_with(None);
        assert_unusable(read_gate_in(dir.path()), "has no tessel.toml");
    }

    #[test]
    fn a_gate_file_that_is_not_utf8_is_unusable_not_a_failed_read() {
        let dir = trunk_with(Some(b"instance = \"\xff\xfe\"\n"));
        assert_unusable(read_gate_in(dir.path()), "not a valid gate");
    }

    #[test]
    fn a_gate_file_the_server_cannot_open_is_a_failed_read() {
        let dir = trunk_with(Some(b"instance = \"standard-1\"\n"));
        let file = dir.path().join(TESSEL_TOML_PATH);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
        let result = read_gate_in(dir.path());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert_unreadable(result);
    }

    #[test]
    fn a_gate_file_in_a_directory_the_server_cannot_search_is_a_failed_read_not_a_missing_one() {
        let dir = trunk_with(Some(b"instance = \"standard-1\"\n"));
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o000)).unwrap();
        let result = read_gate_in(dir.path());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_unreadable(result);
    }

    #[test]
    fn a_push_is_allowed_before_the_deadline_and_never_after_it_or_after_the_call_was_given_up() {
        let git = Git::new("home".into(), "hooks".into());
        let live = Stop::new(now_ms() + 60_000);
        let ports = Ports::new(&git, "remote", &NeverRuns, &live);
        let pushed = Cell::new(false);
        let outcome = ports.with_push_access(&update(), || pushed.set(true));
        assert!(outcome.is_ok() && pushed.get(), "a live call pushes");

        for stop in [Stop::new(now_ms().saturating_sub(1)), live.clone()] {
            stop.abandon();
            let ports = Ports::new(&git, "remote", &NeverRuns, &stop);
            let pushed = Cell::new(false);
            let outcome = ports.with_push_access(&update(), || pushed.set(true));
            assert!(outcome.is_err(), "a call that is over is refused the push");
            assert!(!pushed.get(), "and the push is never attempted");
        }
    }

    #[test]
    fn no_test_starts_once_the_call_is_over() {
        let git = Git::new("home".into(), "hooks".into());
        let stop = Stop::new(0);
        let ports = Ports::new(&git, "remote", &NeverRuns, &stop);
        let error = ports.before_step(PackageStep::Test).unwrap_err();
        assert!(
            error.message.contains("deadline passed"),
            "{}",
            error.message
        );
    }

    #[test]
    fn the_install_step_passes_without_running_anything_and_is_not_test_evidence() {
        let git = Git::new("home".into(), "hooks".into());
        let stop = Stop::new(now_ms() + 60_000);
        let ports = Ports::new(&git, "remote", &NeverRuns, &stop);
        let install = ports.before_step(PackageStep::Install).unwrap().unwrap();
        assert_eq!((install.exit_code, install.passed), (0, false));
        assert!(ports.before_step(PackageStep::Test).unwrap().is_none());
    }
}

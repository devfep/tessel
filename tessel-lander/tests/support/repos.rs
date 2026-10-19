//! A local bare "main", a bare "fork" of it and a work tree that feeds both, plus the ports of a
//! merge that act on them. A merge's remotes are plain paths, which `protocol.file.allow=user`
//! lets git read.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use tempfile::TempDir;
use tessel_coordinator::protocol::{Mode, Scope, ScopeClaim};
use tessel_lander::git::{Git, MergeSources};
use tessel_lander::merge::{
    prepare, run_merge, BoundaryError, MergeContext, MergeError, MergePolicy, MergePorts,
    MergeRequest, Prepared, PushUpdate,
};
use tessel_lander::outcome::{
    EvidenceResult, LandedMerge, MergeOutcome, PackageStep, StepOutcome, TrialOutcome,
};
use tessel_lander::run::SystemRunner;
use tessel_lander::trial::{run_trial, TrialContext, TrialPorts};
use tessel_lander::types::Sha;
use tessel_lander::workspace::TestTree;

use super::{commit_file, fixture_git};

pub struct Repos {
    pub scratch: TempDir,
    pub main: PathBuf,
    pub fork: PathBuf,
    pub work: PathBuf,
    git: Git,
    runs: AtomicU32,
}

impl Repos {
    /// `main` and `fork` hold one commit, `a.txt`; the work tree is at it.
    pub fn new() -> Repos {
        let scratch = TempDir::new().expect("tempdir");
        let (main, fork, work) = (
            scratch.path().join("main.git"),
            scratch.path().join("fork.git"),
            scratch.path().join("work"),
        );
        for bare in [&main, &fork] {
            fs::create_dir(bare).expect("bare dir");
            fixture_git(
                bare,
                &["init", "--quiet", "--bare", "--initial-branch=main"],
            );
        }
        fs::create_dir(&work).expect("work dir");
        fixture_git(&work, &["init", "--quiet", "--initial-branch=main"]);
        let git = Git::create_in(&scratch.path().join("lander")).expect("environment");
        let repos = Repos {
            scratch,
            main,
            fork,
            work,
            git,
            runs: AtomicU32::new(0),
        };
        repos.commit("a.txt", "one\ntwo\nthree\n");
        repos.push(&repos.main);
        repos.push(&repos.fork);
        repos
    }

    pub fn commit(&self, name: &str, contents: &str) -> Sha {
        commit_file(&self.work, name, contents)
    }

    pub fn work_git(&self, args: &[&str]) -> String {
        fixture_git(&self.work, args)
    }

    pub fn head(&self) -> Sha {
        Sha::parse(&self.work_git(&["rev-parse", "HEAD"])).expect("sha")
    }

    pub fn reset_to(&self, commit: &str) {
        self.work_git(&["reset", "--quiet", "--hard", commit]);
    }

    pub fn push(&self, remote: &Path) {
        let remote = remote.display().to_string();
        self.work_git(&["push", "--quiet", "--force", &remote, "main"]);
    }

    pub fn main_head(&self) -> Sha {
        Sha::parse(&fixture_git(&self.main, &["rev-parse", "main"])).expect("sha")
    }

    pub fn show_on_main(&self, path: &str) -> String {
        fixture_git(&self.main, &["show", &format!("main:{path}")])
    }

    pub fn sources(&self) -> MergeSources {
        MergeSources {
            main_remote: self.main.display().to_string(),
            fork_remote: self.fork.display().to_string(),
            fork_branch: "main".to_owned(),
        }
    }

    fn next_clone_dir(&self) -> PathBuf {
        let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        self.scratch.path().join(format!("workspace-{run}"))
    }

    /// Runs the merge of `commit` with main pinned at where it is now.
    pub fn merge(
        &self,
        ports: &impl MergePorts,
        commit: &Sha,
        scopes: &[ScopeClaim],
        policy: MergePolicy,
    ) -> Result<MergeOutcome, MergeError> {
        let (sources, pinned, clone_dir, test_dir) = (
            self.sources(),
            self.main_head(),
            self.next_clone_dir(),
            self.next_clone_dir(),
        );
        let ctx = MergeContext {
            git: &self.git,
            runner: &SystemRunner,
            sources: &sources,
            clone_dir: &clone_dir,
            test_dir: &test_dir,
            pinned_main: &pinned,
        };
        let request = MergeRequest {
            commit,
            scopes,
            policy,
        };
        run_merge(&ctx, ports, &request)
    }

    /// One trial of `commit` on main at `main`, in a clone of its own. Its "tests" run the
    /// `test.sh` of the rebased work tree.
    pub fn trial(&self, main: &Sha, commit: &Sha) -> Result<TrialOutcome, MergeError> {
        let (sources, clone_dir) = (self.sources(), self.next_clone_dir());
        let ctx = TrialContext {
            git: &self.git,
            runner: &SystemRunner,
            sources: &sources,
            clone_dir: &clone_dir,
        };
        run_trial(&ctx, &ShellTests, main, commit)
    }

    /// Like [`Repos::merge`], but only `prepare`.
    pub fn prepare(
        &self,
        ports: &impl MergePorts,
        commit: &Sha,
        scopes: &[ScopeClaim],
        policy: MergePolicy,
    ) -> Result<Prepared, MergeError> {
        let (sources, pinned, clone_dir, test_dir) = (
            self.sources(),
            self.main_head(),
            self.next_clone_dir(),
            self.next_clone_dir(),
        );
        let ctx = MergeContext {
            git: &self.git,
            runner: &SystemRunner,
            sources: &sources,
            clone_dir: &clone_dir,
            test_dir: &test_dir,
            pinned_main: &pinned,
        };
        let request = MergeRequest {
            commit,
            scopes,
            policy,
        };
        prepare(&ctx, ports, &request)
    }
}

pub fn whole_repo() -> Vec<ScopeClaim> {
    vec![
        ScopeClaim {
            scope: Scope::Dir {
                path: String::new(),
            },
            mode: Mode::EditSignature,
        },
        ScopeClaim {
            scope: Scope::Dir {
                path: String::new(),
            },
            mode: Mode::Create,
        },
    ]
}

pub fn file_scope(path: &str, mode: Mode) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::File {
            path: path.to_owned(),
        },
        mode,
    }
}

/// Ports over the local repositories: every credential is granted, every package step passes,
/// and main is read from the bare repository.
pub struct LocalPorts<'a> {
    repos: &'a Repos,
    /// Package steps and push access, in the order they happened.
    pub log: RefCell<Vec<String>>,
    /// Runs inside the push access, before the push.
    pub during_push_access: Box<dyn Fn() + 'a>,
    pub refuse_push_access: bool,
}

impl<'a> LocalPorts<'a> {
    pub fn new(repos: &'a Repos) -> LocalPorts<'a> {
        LocalPorts {
            repos,
            log: RefCell::new(Vec::new()),
            during_push_access: Box::new(|| {}),
            refuse_push_access: false,
        }
    }
}

impl MergePorts for LocalPorts<'_> {
    fn revoke_read_access(&self) -> bool {
        true
    }

    fn run_package_step(
        &self,
        step: PackageStep,
        _tree: &TestTree,
    ) -> Result<StepOutcome, BoundaryError> {
        self.log.borrow_mut().push(format!("{step:?}"));
        Ok(StepOutcome::finished(step, 0, "", ""))
    }

    fn with_push_access<T>(
        &self,
        _update: &PushUpdate,
        use_access: impl FnOnce() -> T,
    ) -> Result<T, BoundaryError> {
        self.log.borrow_mut().push("push access".to_owned());
        if self.refuse_push_access {
            return Err(BoundaryError {
                operation: "with_push_access",
                message: "must not push".to_owned(),
            });
        }
        (self.during_push_access)();
        Ok(use_access())
    }

    fn current_main(&self) -> Option<Sha> {
        Some(self.repos.main_head())
    }

    fn record_merge(&self, _landed: &LandedMerge) -> EvidenceResult {
        EvidenceResult::NotWritten {
            reason: "no record repo in this test".to_owned(),
        }
    }
}

/// Trial ports whose install step passes and whose test step runs `sh test.sh` in the tree.
struct ShellTests;

impl TrialPorts for ShellTests {
    fn revoke_read_access(&self) -> bool {
        true
    }

    fn run_package_step(
        &self,
        step: PackageStep,
        tree: &TestTree,
    ) -> Result<StepOutcome, BoundaryError> {
        if step == PackageStep::Install {
            return Ok(StepOutcome::finished(step, 0, "", ""));
        }
        let status = Command::new("sh")
            .arg("test.sh")
            .current_dir(tree.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .status()
            .map_err(|error| BoundaryError {
                operation: "run_package_step",
                message: error.to_string(),
            })?;
        Ok(StepOutcome::finished(
            step,
            status.code().unwrap_or(1),
            "",
            "",
        ))
    }
}

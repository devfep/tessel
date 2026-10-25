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
    prepare, run_merge, BoundaryError, CandidateWrite, MergeContext, MergeError, MergePolicy,
    MergePorts, MergeRequest, Prepared, PushUpdate,
};
use tessel_lander::outcome::{
    EvidenceResult, LandedMerge, MergeOutcome, PackageStep, StepOutcome, TrialOutcome,
};
use tessel_lander::run::{GitRunner, SystemRunner};
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

    /// Pushes the work tree's HEAD to a new branch of `remote`, without force.
    pub fn push_branch(&self, remote: &Path, branch: &str) {
        let remote = remote.display().to_string();
        let target = format!("HEAD:refs/heads/{branch}");
        self.work_git(&["push", "--quiet", &remote, &target]);
    }

    /// Deletes a branch of the bare fork; the objects stay in its store.
    pub fn delete_fork_branch(&self, branch: &str) {
        let reference = format!("refs/heads/{branch}");
        fixture_git(&self.fork, &["update-ref", "-d", &reference]);
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
        }
    }

    pub fn git(&self) -> &Git {
        &self.git
    }

    /// Puts `sha` on main's candidate ref of `name`, as the lander's push would.
    pub fn set_candidate(&self, name: &str, sha: &Sha) {
        let target = format!("{sha}:refs/tessel/candidate/{name}");
        let main = self.main.display().to_string();
        self.work_git(&["push", "--quiet", "--force", &main, &target]);
    }

    /// The tip of `reference` in main's bare repository, or `None`.
    pub fn main_ref(&self, reference: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.main)
            .args(["rev-parse", "--verify", "--quiet", reference])
            .output()
            .expect("run git");
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// A clone of the work tree, which holds every commit a test made, in a directory of its own.
    pub fn clone_of_work(&self) -> tessel_lander::workspace::Workspace {
        let sources = MergeSources {
            main_remote: self.work.display().to_string(),
            fork_remote: String::new(),
        };
        let dir = self.next_clone_dir();
        match tessel_lander::workspace::clone_main(&self.git, &SystemRunner, &sources, &dir)
            .expect("clone step")
        {
            tessel_lander::workspace::CloneOutcome::Cloned(workspace) => workspace,
            tessel_lander::workspace::CloneOutcome::Failed(output) => {
                unreachable!("the clone failed: {}", output.stderr)
            }
        }
    }

    /// The `Land` of the staged landing for `head`, with main pinned at where it is now.
    pub fn land_candidate(
        &self,
        ports: &impl MergePorts,
        head: &Sha,
    ) -> Result<MergeOutcome, MergeError> {
        self.land_candidate_with(&SystemRunner, ports, head)
    }

    /// Like [`Repos::land_candidate`], with the git runner a test chooses.
    pub fn land_candidate_with(
        &self,
        runner: &dyn GitRunner,
        ports: &impl MergePorts,
        head: &Sha,
    ) -> Result<MergeOutcome, MergeError> {
        let (sources, pinned, clone_dir, test_dir) = (
            self.sources(),
            self.main_head(),
            self.next_clone_dir(),
            self.next_clone_dir(),
        );
        let ctx = MergeContext {
            git: &self.git,
            runner,
            sources: &sources,
            clone_dir: &clone_dir,
            test_dir: &test_dir,
            pinned_main: &pinned,
        };
        let update = PushUpdate {
            base: pinned.clone(),
            head: head.clone(),
        };
        tessel_lander::candidate::land_candidate(&ctx, ports, &update)
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
    /// Refuses every write to the candidate refs, as a call that was given up would.
    pub refuse_candidate_access: bool,
    /// Runs when main is read back, after the push.
    pub before_current_main: Box<dyn Fn() + 'a>,
}

impl<'a> LocalPorts<'a> {
    pub fn new(repos: &'a Repos) -> LocalPorts<'a> {
        LocalPorts {
            repos,
            log: RefCell::new(Vec::new()),
            during_push_access: Box::new(|| {}),
            refuse_push_access: false,
            refuse_candidate_access: false,
            before_current_main: Box::new(|| {}),
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

    fn with_candidate_access<T>(
        &self,
        write: &CandidateWrite<'_>,
        use_access: impl FnOnce() -> T,
    ) -> Result<T, BoundaryError> {
        let what = match write {
            CandidateWrite::Push { .. } => "candidate push",
            CandidateWrite::Delete { .. } => "candidate delete",
        };
        self.log.borrow_mut().push(what.to_owned());
        if self.refuse_candidate_access {
            return Err(BoundaryError {
                operation: "with_candidate_access",
                message: "must not write".to_owned(),
            });
        }
        Ok(use_access())
    }

    fn current_main(&self) -> Option<Sha> {
        (self.before_current_main)();
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

//! The local lander: answers the host's calls with the real lander and the real runner, in
//! process, against a git remote reached by path.
//!
//! Until the core stages a landing (HOST-MERGE-STANDIN), the host's one `Prepare` call stands for
//! the whole merge. Here it runs the lander's `run_merge`: clone main, fetch the agent's fork,
//! rebase, check coverage and the gate, run the trunk's test command on a clone of the rebased
//! head, push main with a compare-and-swap, and read main back. `Prepared` is the answer only when
//! that read found the head on main. The lander runs git and never repo code; the runner runs repo
//! code and never git (rule 4).
//!
//! Each call is bounded by its own `deadline_ms`. A call still running at its deadline is
//! answered `Unknown`, its clones are removed, and the work it left behind is stopped from
//! pushing, so a late landing cannot follow an answer that said the call did not finish.

mod answer;
mod deadline;
mod git_read;
mod head;
pub mod names;
mod ports;
mod trial;
mod workdir;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tessel_coordinator::host::call::{Call, CallAnswer, Refusal, Request};
use tessel_coordinator::merge::RuleKind;
use tessel_coordinator::protocol::{AgentId, CommitId, ScopeClaim};
use tessel_lander::git::{CommandError, Git, MergeSources, MAIN_BRANCH};
use tessel_lander::merge::{run_merge, MergeContext, MergePolicy, MergeRequest};
use tessel_lander::run::SystemRunner;
use tessel_lander::types::Sha;
use tessel_runner::RunnerError;

use crate::config::{LanderConfig, RepositoryConfig, FORK_AGENT_PLACEHOLDER};
use crate::id::RepoId;
use crate::repo::{BoxFuture, CallPort};

use answer::merge_answer;
use deadline::Stop;
use git_read::read_branch;
use names::{quote, MAX_LOGGED_CHARS};
use ports::Ports;
use workdir::CallDir;

pub use ports::{LocalRunner, TestRunner};

/// Why a lander could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum LandingError {
    #[error("repository {repo}: remote and fork are not both set")]
    Unconfigured { repo: String },
    #[error("cannot prepare the work directory {path}: {source}")]
    WorkDir {
        path: String,
        source: std::io::Error,
    },
    #[error("the lander's git environment: {0}")]
    Git(#[from] CommandError),
    #[error("the runner: {0}")]
    Runner(#[from] RunnerError),
}

/// What `Test` and `Land` are refused with: they are never asked while `Prepare` is the merge.
pub const STAND_IN_MESSAGE: &str = "this lander runs the whole merge in the Prepare call; \
                                    Test and Land are not issued until the staged landing";

/// Answers the calls of one repository against its git remote.
#[derive(Clone)]
pub struct Landing {
    inner: Arc<Inner>,
}

struct Inner {
    repo: RepoId,
    remote: String,
    fork: String,
    dir: PathBuf,
    git: Git,
    runner: Arc<dyn TestRunner>,
    sequence: AtomicU64,
}

/// What one call works with: its directory and the signal to stop.
struct Work {
    dir: Arc<CallDir>,
    stop: Stop,
}

impl Landing {
    /// Sets up the work directory of `repo` under `lander.work_dir` and the lander's git
    /// environment in it. Clones a killed server left behind are removed.
    pub fn new(
        repo: RepoId,
        repository: &RepositoryConfig,
        lander: &LanderConfig,
        runner: Arc<dyn TestRunner>,
    ) -> Result<Self, LandingError> {
        let (Some(remote), Some(fork)) = (&repository.remote, &repository.fork) else {
            return Err(LandingError::Unconfigured {
                repo: repo.to_string(),
            });
        };
        let dir = lander.work_dir.join(repo.tenant()).join(repo.repo());
        clear_leftovers(&dir)?;
        let git = Git::create_in(&dir.join("git"))?;
        Ok(Self {
            inner: Arc::new(Inner {
                repo,
                remote: remote.clone(),
                fork: fork.clone(),
                dir,
                git,
                runner,
                sequence: AtomicU64::new(1),
            }),
        })
    }
}

/// Removes what an earlier run of this server left in `dir` (its git environment and the clones
/// of calls it was killed in) and makes sure `dir` exists.
fn clear_leftovers(dir: &std::path::Path) -> Result<(), LandingError> {
    let fail = |source| LandingError::WorkDir {
        path: dir.display().to_string(),
        source,
    };
    std::fs::create_dir_all(dir).map_err(fail)?;
    for entry in std::fs::read_dir(dir).map_err(fail)? {
        let entry = entry.map_err(fail)?;
        let name = entry.file_name();
        let left_over = name == "git" || name.to_string_lossy().starts_with("call-");
        if left_over {
            std::fs::remove_dir_all(entry.path()).map_err(fail)?;
        }
    }
    Ok(())
}

impl CallPort for Landing {
    fn answer(&self, call: Call) -> BoxFuture<CallAnswer> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.answer_within_deadline(call).await })
    }
}

impl Inner {
    /// Runs the call on a blocking thread and gives up on it at the call's deadline.
    async fn answer_within_deadline(self: Arc<Self>, call: Call) -> CallAnswer {
        let number = self.sequence.fetch_add(1, Ordering::SeqCst);
        let dir = match CallDir::create(self.dir.join(format!("call-{number}"))) {
            Ok(dir) => Arc::new(dir),
            Err(error) => {
                tracing::error!(
                    repo = %self.repo,
                    error = %error,
                    "cannot make the call's directory"
                );
                return CallAnswer::Unknown;
            }
        };
        let stop = Stop::new(call.deadline_ms);
        let remaining = stop.remaining();
        let work = Work {
            dir: Arc::clone(&dir),
            stop: stop.clone(),
        };
        let inner = Arc::clone(&self);
        let id = call.id.0;
        let blocking = tokio::task::spawn_blocking(move || inner.run(&call, &work));
        match tokio::time::timeout(remaining, blocking).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(error)) => {
                tracing::error!(repo = %self.repo, call = id, error = %error, "the call panicked");
                CallAnswer::Unknown
            }
            Err(_elapsed) => {
                stop.abandon();
                dir.remove();
                tracing::warn!(
                    repo = %self.repo,
                    call = id,
                    "the call passed its deadline; given up"
                );
                CallAnswer::Unknown
            }
        }
    }

    fn run(&self, call: &Call, work: &Work) -> CallAnswer {
        if work.stop.requested() {
            return CallAnswer::Unknown;
        }
        match &call.request {
            Request::Prepare {
                agent,
                fork_commit,
                scopes,
                ..
            } => self.prepare(work, agent, fork_commit, scopes),
            Request::Test { .. } | Request::Land { .. } => CallAnswer::Refused {
                refusal: Refusal::Rule {
                    rule: RuleKind::Other,
                    message: STAND_IN_MESSAGE.to_string(),
                },
            },
            Request::Trial {
                agent,
                commit,
                before,
                main,
            } => self.trial(work, agent, commit.as_ref(), (before, main)),
            Request::ReadHead { submitted } => self.read_head(work, submitted),
            Request::Reconcile { head } => self.reconcile(work, head),
        }
    }

    /// The fork of `agent`, or `None` when the name could not be a path component.
    fn fork_of(&self, agent: &AgentId) -> Option<String> {
        tessel_coordinator::shell::is_agent_id(&agent.0)
            .then(|| self.fork.replace(FORK_AGENT_PLACEHOLDER, &agent.0))
    }

    fn ports<'a>(&'a self, work: &'a Work) -> Ports<'a> {
        Ports::new(&self.git, &self.remote, self.runner.as_ref(), &work.stop)
    }

    fn prepare(
        &self,
        work: &Work,
        agent: &AgentId,
        fork_commit: &CommitId,
        scopes: &[ScopeClaim],
    ) -> CallAnswer {
        let repo = self.repo.to_string();
        let (Some(fork_remote), Ok(commit)) = (self.fork_of(agent), Sha::parse(&fork_commit.0))
        else {
            tracing::info!(
                repo,
                "the agent has no fork or the commit is not a commit id"
            );
            return CallAnswer::Refused {
                refusal: Refusal::CommitNotInFork,
            };
        };
        let pinned = match read_branch(&self.git, &self.remote, MAIN_BRANCH) {
            Ok(Some(main)) => main,
            Ok(None) => {
                tracing::warn!(repo, "the remote has no main branch to land on");
                return CallAnswer::Unknown;
            }
            Err(error) => {
                tracing::warn!(repo, error = %error, "cannot read main before the merge");
                return CallAnswer::Unknown;
            }
        };
        let sources = MergeSources {
            main_remote: self.remote.clone(),
            fork_remote,
            fork_branch: MAIN_BRANCH.to_owned(),
        };
        let (clone_dir, test_dir) = (work.dir.child("main"), work.dir.child("test"));
        let ctx = MergeContext {
            git: &self.git,
            runner: &SystemRunner,
            sources: &sources,
            clone_dir: &clone_dir,
            test_dir: &test_dir,
            pinned_main: &pinned,
        };
        let ports = self.ports(work);
        let request = MergeRequest {
            commit: &commit,
            scopes,
            policy: MergePolicy::Submission,
        };
        match run_merge(&ctx, &ports, &request) {
            Ok(outcome) => merge_answer(&repo, outcome, ports.take_first_failure().as_deref()),
            Err(error) => {
                let note = quote(&error.to_string(), MAX_LOGGED_CHARS);
                tracing::warn!(
                    repo,
                    error = %note,
                    "the merge did not finish; the core retries"
                );
                CallAnswer::Unknown
            }
        }
    }
}

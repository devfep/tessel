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
mod names;
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

use crate::config::{LanderConfig, RepositoryConfig, FORK_AGENT_PLACEHOLDER};
use crate::credential::{check_token_file, is_http_url, url_host, with_askpass, TokenFileError};
use crate::id::RepoId;
use crate::identity::is_server_agent_name;
use crate::paths::{absolute, absolute_location, ResolveError};
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
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error("the lander's git environment: {0}")]
    Git(#[from] CommandError),
    #[error("{0}")]
    TokenFile(#[from] TokenFileError),
    #[error("cannot find this server's executable, which git runs to read the token: {0}")]
    Executable(std::io::Error),
}

/// What a refusal says when an agent's name cannot name a fork.
pub const BAD_AGENT_NAME_MESSAGE: &str =
    "the agent name is not lowercase letters, digits, '.', '_' and '-' of at most 128 bytes, \
     starting with a letter or digit";

/// What `Test` and `Land` are refused with: they are never asked while `Prepare` is the merge.
pub const STAND_IN_MESSAGE: &str = "this lander runs the whole merge in the Prepare call; \
                                    Test and Land are not issued until the staged landing";

/// Answers the calls of one repository against its git remote.
pub struct Landing {
    inner: Arc<Inner>,
}

struct Inner {
    repo: RepoId,
    remote: String,
    fork: String,
    dir: PathBuf,
    git: Git,
    token_file: Option<PathBuf>,
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
        let (remote, fork) = (absolute_location(remote)?, absolute_location(fork)?);
        let token_file = lander.token_file.as_deref().map(absolute).transpose()?;
        let dir = absolute(&lander.work_dir)?
            .join(repo.tenant())
            .join(repo.repo());
        clear_leftovers(&dir)?;
        let git = credentialed_git(
            Git::create_in(&dir.join("git"))?,
            &repo,
            token_file.as_deref(),
            (&remote, &fork),
        )?;
        Ok(Self {
            inner: Arc::new(Inner {
                repo,
                remote,
                fork,
                dir,
                git,
                token_file,
                runner,
                sequence: AtomicU64::new(1),
            }),
        })
    }
}

/// `git` with the askpass hook when a token file is configured, after checking the file. Without
/// one, a repository on a URL is logged once as landing without a credential.
fn credentialed_git(
    git: Git,
    repo: &RepoId,
    token_file: Option<&std::path::Path>,
    (remote, fork): (&str, &str),
) -> Result<Git, LandingError> {
    let Some(token_file) = token_file else {
        if is_http_url(remote) || is_http_url(fork) {
            tracing::warn!(
                repo = %repo,
                "no [lander] token_file: landings on this repository push without a credential"
            );
        }
        return Ok(git);
    };
    check_token_file(token_file)?;
    let program = std::env::current_exe().map_err(LandingError::Executable)?;
    let mut hosts: Vec<String> = Vec::new();
    for host in [remote, fork].into_iter().filter_map(url_host) {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    Ok(with_askpass(git, &program, token_file, &hosts))
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

    fn reads_trunk_head(&self) -> bool {
        true
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
        if let Some(file) = &self.token_file {
            if let Err(error) = check_token_file(file) {
                tracing::error!(repo = %self.repo, error = %error, "cannot use the git token");
                return CallAnswer::Unknown;
            }
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

    /// The fork of `agent`. The name must pass the server's agent-name rule, the one the verifier
    /// uses, checked here whatever the verifier did: it has no separator and no leading dot, so it
    /// can never leave the fork's directory, and it is lowercase, so two names never share a fork
    /// on a filesystem or git host that folds case. Any other name is a rule refusal and no path
    /// is made.
    fn fork_of(&self, agent: &AgentId) -> Result<String, CallAnswer> {
        if is_server_agent_name(&agent.0) {
            return Ok(self.fork.replace(FORK_AGENT_PLACEHOLDER, &agent.0));
        }
        tracing::warn!(repo = %self.repo, "an agent name is not a fork name");
        Err(CallAnswer::Refused {
            refusal: Refusal::Rule {
                rule: RuleKind::ForkMissing,
                message: BAD_AGENT_NAME_MESSAGE.to_string(),
            },
        })
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
        let fork_remote = match self.fork_of(agent) {
            Ok(fork) => fork,
            Err(refused) => return refused,
        };
        let Ok(commit) = Sha::parse(&fork_commit.0) else {
            tracing::info!(repo, "the commit is not a commit id");
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
                if let Some(message) = ports.take_gate_refusal() {
                    tracing::info!(
                        repo,
                        "the trunk's gate cannot be used; the landing is refused"
                    );
                    return CallAnswer::Refused {
                        refusal: Refusal::Rule {
                            rule: RuleKind::GateInvalid,
                            message,
                        },
                    };
                }
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    use tessel_lander::git::GitCommand;
    use tessel_lander::workspace::{clone_main, CloneOutcome};

    use super::git_read::ls_remote_command;
    use super::*;

    const SECRET: &str = "ghs_not-a-real-token-0123456789";

    fn plain_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn values(command: &GitCommand) -> Vec<OsString> {
        let mut all = command.argv.clone();
        all.extend(command.env.values().cloned());
        all
    }

    #[test]
    fn every_command_of_a_landing_carries_the_hook_and_the_token_path_and_never_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        std::fs::write(&token, format!("{SECRET}\n")).unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let main = dir.path().join("main.git");
        std::fs::create_dir(&main).unwrap();
        plain_git(
            &main,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        );
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        plain_git(&work, &["init", "--quiet", "--initial-branch=main"]);
        plain_git(&work, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        plain_git(
            &work,
            &["push", "--quiet", main.to_str().unwrap(), "HEAD:main"],
        );

        let repo = RepoId::new("acme", "demo").unwrap();
        let remote = "https://git.example.invalid/acme/demo.git";
        let fork = "https://forks.example.invalid/acme/fork-a1.git";
        let git = credentialed_git(
            Git::create_in(&dir.path().join("git")).unwrap(),
            &repo,
            Some(&token),
            (remote, fork),
        )
        .unwrap();
        let program = std::env::current_exe().unwrap();

        let local = MergeSources {
            main_remote: main.display().to_string(),
            fork_remote: fork.to_owned(),
        };
        let cloned = clone_main(&git, &SystemRunner, &local, &dir.path().join("clone")).unwrap();
        let workspace = match cloned {
            CloneOutcome::Cloned(workspace) => Some(workspace),
            CloneOutcome::Failed(_) => None,
        }
        .expect("the clone of the local main");
        let remote_sources = MergeSources {
            main_remote: remote.to_owned(),
            ..local.clone()
        };
        let base = Sha::parse(&"a".repeat(40)).unwrap();
        let head = Sha::parse(&"b".repeat(40)).unwrap();
        let commands = [
            git.clone_command(&remote_sources, &dir.path().join("again")),
            git.fetch_fork_command(&workspace, &remote_sources),
            git.push_command(&workspace, &remote_sources, &base, &head),
            ls_remote_command(&git, remote, "refs/heads/main"),
        ];
        for command in commands {
            let shown = format!("{:?}", command.argv);
            let hook = command.env.get("GIT_ASKPASS").map(OsString::as_os_str);
            assert_eq!(hook, Some(program.as_os_str()), "{shown}");
            let file = command.env.get("TESSEL_ASKPASS_TOKEN_FILE");
            assert_eq!(
                file.map(OsString::as_os_str),
                Some(token.as_os_str()),
                "{shown}"
            );
            assert_eq!(
                command.env.get("GIT_TERMINAL_PROMPT"),
                Some(&"0".into()),
                "{shown}"
            );
            let hosts = command.env.get("TESSEL_ASKPASS_HOSTS");
            let expected = OsString::from("git.example.invalid,forks.example.invalid");
            assert_eq!(hosts, Some(&expected), "{shown}");
            for value in values(&command) {
                assert!(!value.to_string_lossy().contains(SECRET), "{shown}");
            }
        }
    }

    #[test]
    fn a_landing_without_a_token_file_has_no_hook() {
        let dir = tempfile::tempdir().unwrap();
        let repo = RepoId::new("acme", "demo").unwrap();
        let urls = (
            "https://git.example.invalid/r.git",
            "/srv/forks/{agent}.git",
        );
        let git = credentialed_git(Git::create_in(dir.path()).unwrap(), &repo, None, urls).unwrap();
        let command = ls_remote_command(&git, urls.0, "refs/heads/main");
        assert_eq!(command.env.get("GIT_ASKPASS"), None);
        assert_eq!(command.env.get("TESSEL_ASKPASS_TOKEN_FILE"), None);
    }

    #[test]
    fn a_relative_token_file_reaches_the_hook_as_an_absolute_path() {
        let here = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let token = here.path().join("token");
        std::fs::write(&token, format!("{SECRET}\n")).unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let relative = Path::new(here.path().file_name().unwrap()).join("token");
        let work = tempfile::tempdir().unwrap();
        let repository = RepositoryConfig {
            tenant: "acme".to_owned(),
            name: "demo".to_owned(),
            run: "t".to_owned(),
            reviewers: Vec::new(),
            sensitive: Vec::new(),
            remote: Some("/srv/main.git".to_owned()),
            fork: Some("/srv/forks/{agent}.git".to_owned()),
        };
        let lander = LanderConfig {
            work_dir: work.path().to_owned(),
            runner_config: "runner.toml".into(),
            token_file: Some(relative),
        };
        let runner = Arc::new(LocalRunnerStub);

        let landing = Landing::new(
            RepoId::new("acme", "demo").unwrap(),
            &repository,
            &lander,
            runner,
        )
        .unwrap();

        let command = ls_remote_command(&landing.inner.git, "/srv/main.git", "refs/heads/main");
        let hook_path = Path::new(command.env.get("TESSEL_ASKPASS_TOKEN_FILE").unwrap());
        assert_eq!(hook_path, token.canonicalize().unwrap());
        assert_eq!(landing.inner.token_file.as_deref(), Some(hook_path));
    }

    struct LocalRunnerStub;

    impl TestRunner for LocalRunnerStub {
        fn run(
            &self,
            _request: &tessel_runner::RunRequest,
        ) -> Result<tessel_runner::RunResult, tessel_runner::RunnerError> {
            unreachable!("no test runs in this test")
        }
    }
}

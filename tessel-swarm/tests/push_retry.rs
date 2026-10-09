//! A push to an agent's fork that the remote answers with a server error is repeated; one it
//! refuses is not. The fork is a local repository and `git` is a wrapper on `PATH` that fails
//! the first pushes to it as a plan file beside the fork says, then runs the real git.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
use std::time::Duration;

use tessel_swarm::conn::{Reconnect, HEARTBEAT_EVERY};
use tessel_swarm::demo;
use tessel_swarm::endpoint::Remote;
use tessel_swarm::guard::ScratchRepo;
use tessel_swarm::local::{LocalServer, LocalSetup};
use tessel_swarm::on::{self, OnConfig, OnResult, Policy, Resolution, REVIEWER};
use tessel_swarm::tasks::{Kind, Task};

const PLAN: &str = "push-plan";

const WRAPPER: &str = r#"#!/bin/sh
pushing=0
target=
for arg in "$@"; do
  [ "$arg" = push ] && pushing=1
  case "$arg" in *.git) target="$arg" ;; esac
done
if [ "$pushing" = 1 ] && [ -n "$target" ]; then
  plan="${target%/*}/push-plan"
  if [ -s "$plan" ]; then
    { read -r left; read -r message; } < "$plan"
    if [ "$left" -gt 0 ]; then
      printf '%s\n%s\n' "$((left - 1))" "$message" > "$plan"
      echo "$message" >&2
      exit 128
    fi
  fi
fi
exec REAL_GIT "$@"
"#;

static WRAPPER_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
static PATH_SET: Once = Once::new();

/// The first `git` on `path` that is an executable file.
fn real_git(path: &std::ffi::OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        let candidate = dir.join("git");
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
            return Some(candidate);
        }
    }
    None
}

fn put_failing_git_first_on_path() {
    PATH_SET.call_once(|| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let Some(real) = real_git(&path) else {
            unreachable!("the test needs an executable git on PATH");
        };
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("git");
        let text = WRAPPER.replace("REAL_GIT", &real.to_string_lossy());
        std::fs::write(&script, text).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![dir.path().to_path_buf()];
        paths.extend(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        WRAPPER_DIR.set(dir).unwrap();
    });
}

fn config() -> OnConfig {
    OnConfig {
        agents: 1,
        policy: Policy::Wait,
        work_ms: 0,
        task_timeout: Duration::from_secs(120),
        trial_wait: Duration::from_secs(60),
        heartbeat_every: HEARTBEAT_EVERY,
        max_denials: 400,
        scripted_reviewer: true,
        reconnect: Reconnect::OFF,
    }
}

/// Runs one task on a fresh local target whose fork fails its first `failures` pushes with
/// `message`. Returns the result and how many failures the plan still held afterwards.
async fn run_with_failing_pushes(failures: u32, message: &str) -> (OnResult, u32) {
    put_failing_git_first_on_path();
    let config = config();
    let scratch = tempfile::tempdir().unwrap();
    let repo = ScratchRepo::parse("swarm-test").unwrap();
    let names = on::principals(config.agents, config.scripted_reviewer);
    let base = demo::base_tree();
    let server = LocalServer::start(LocalSetup {
        repo: &repo,
        scratch: &scratch.path().join("server"),
        base: &base,
        names: &names,
        reviewers: &[REVIEWER.to_string()],
        shadow_enabled: false,
        lease_ms: tessel_swarm::local::LEASE_MS,
    })
    .await
    .unwrap();
    let Remote::Local { forks, .. } = server.endpoint.remote.clone() else {
        unreachable!("the local target has a local remote");
    };
    let plan = forks.join(PLAN);
    std::fs::write(&plan, format!("{failures}\n{message}\n")).unwrap();
    let task = Task {
        id: 1,
        func: "unitPrice".into(),
        kind: Kind::Body,
    };
    let result = on::run_on(
        &server.endpoint,
        &[task],
        &scratch.path().join("agents"),
        &config,
    )
    .await
    .unwrap();
    server.shutdown().await;
    (result, failures_left(&plan))
}

fn failures_left(plan: &Path) -> u32 {
    let text = std::fs::read_to_string(plan).unwrap();
    text.lines().next().unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_push_answered_with_a_server_error_twice_is_repeated_and_the_task_merges() {
    let message = "error: RPC failed; HTTP 503 remote: Service unavailable";
    let (result, left) = run_with_failing_pushes(2, message).await;
    assert_eq!(result.results.len(), 1, "{:?}", result.results);
    let task = &result.results[0];
    assert_eq!(task.result, Resolution::Merged, "{task:?}");
    assert_eq!(task.push_retries, 2, "{task:?}");
    assert_eq!(left, 0, "both planned failures were used");
    assert_eq!(result.summary.merges, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_push_the_remote_refuses_fails_the_task_at_once_without_a_retry() {
    let (result, left) =
        run_with_failing_pushes(5, "fatal: Authentication failed for 'https://x/'").await;
    assert_eq!(result.results.len(), 1, "{:?}", result.results);
    let task = &result.results[0];
    assert_eq!(task.result, Resolution::Failed, "{task:?}");
    assert_eq!(task.push_retries, 0, "{task:?}");
    assert_eq!(left, 4, "the push was tried exactly once");
    let note = task.note.as_deref().unwrap_or_default();
    assert!(note.contains("Authentication failed"), "{note}");
    assert_eq!(result.summary.merges, 0);
}

//! A server started in a directory that is later removed still lands: no git command of the
//! lander depends on the working directory it inherited. This file holds one test because it
//! changes the working directory of its whole process.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

#[expect(dead_code, reason = "this file uses a part of the shared fixtures")]
mod support;

use std::path::Path;

use tessel_coordinator::host::call::{Call, CallAnswer, CallId, DispatchId, Request};
use tessel_coordinator::protocol::{AgentId, ClaimId, CommitId, Mode, Scope, ScopeClaim};
use tessel_server::id::RepoId;
use tessel_server::landing::Landing;
use tessel_server::repo::{now_ms, CallPort};

use support::{capture_logs, passing, Repos};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// `target` as a path relative to `from`, which is a directory directly under `base`, the root of the test.
fn relative_from_child(base: &Path, target: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let below = target.strip_prefix(base)?;
    Ok(Path::new("..").join(below).display().to_string())
}

#[tokio::test]
async fn a_landing_started_with_relative_paths_works_after_its_start_directory_is_removed(
) -> TestResult {
    capture_logs();
    let repos = Repos::new();
    let commit = repos.fork_commit(&[("src/a.rs", "two\n")]);
    let start = repos.root.path().join("start");
    std::fs::create_dir(&start)?;
    std::env::set_current_dir(&start)?;

    let base = repos.root.path();
    let mut repository = repos.repository("removed-cwd");
    repository.remote = Some(relative_from_child(base, &repos.main)?);
    repository.fork = Some(format!(
        "{}/{{agent}}.git",
        relative_from_child(base, &repos.forks)?
    ));
    let mut lander = repos.lander();
    lander.work_dir = Path::new("..").join("lander");
    let landing = Landing::new(
        RepoId::new("acme", "removed-cwd")?,
        &repository,
        &lander,
        passing(),
    )?;
    std::fs::remove_dir(&start)?;

    let answer = landing
        .answer(Call {
            id: CallId(1),
            dispatch_id: DispatchId(1),
            request: Request::Prepare {
                claim: ClaimId(1),
                agent: AgentId("a1".to_string()),
                fork_commit: CommitId(commit),
                scopes: vec![ScopeClaim {
                    scope: Scope::Dir {
                        path: String::new(),
                    },
                    mode: Mode::Create,
                }],
            },
            deadline_ms: now_ms() + 120_000,
        })
        .await;

    assert!(
        matches!(answer, CallAnswer::Prepared { .. }),
        "expected Prepared, got {answer:?}"
    );
    Ok(())
}

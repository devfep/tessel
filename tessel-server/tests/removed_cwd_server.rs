//! A server started with a relative `store_dir` keeps reading its stores after the directory it
//! started in is left: the summary opens the repository file again on every request. This file
//! holds one test because it changes the working directory of its whole process.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tessel_runner::{RunRequest, RunResult, RunnerError};
use tessel_server::config::{Config, RepositoryConfig};
use tessel_server::identity::{self, Ed25519Verifier, Role, TokenRequest};
use tessel_server::keys;
use tessel_server::landing::TestRunner;
use tessel_server::repo::NoLander;
use tessel_server::server::{self, Deps};
use tessel_server::ws::Verifier;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const AUDIENCE: &str = "tessel-test";

struct NoRunner;

impl TestRunner for NoRunner {
    fn run(&self, _request: &RunRequest) -> Result<RunResult, RunnerError> {
        unreachable!("this test lands nothing")
    }
}

async fn summary_status(addr: SocketAddr, token: &str) -> Result<u16, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(addr).await?;
    let request = format!(
        "GET /tenant/acme/repo/demo/summary HTTP/1.1\r\nHost: test\r\n\
         Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    let mut raw = String::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut raw)).await??;
    Ok(raw.split_whitespace().nth(1).unwrap_or("0").parse()?)
}

#[tokio::test]
async fn a_relative_store_dir_still_serves_the_summary_after_the_working_directory_changes(
) -> TestResult {
    let root = tempfile::tempdir()?;
    let key_dir = root.path().join("keys");
    keys::generate(&key_dir, "k1")?;
    let (start, elsewhere) = (root.path().join("start"), root.path().join("elsewhere"));
    std::fs::create_dir(&start)?;
    std::fs::create_dir(&elsewhere)?;
    std::env::set_current_dir(&start)?;
    let config = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        store_dir: "stores".into(),
        identity: None,
        lander: None,
        repositories: vec![RepositoryConfig {
            tenant: "acme".to_owned(),
            name: "demo".to_owned(),
            run: "test".to_owned(),
            reviewers: Vec::new(),
            sensitive: Vec::new(),
            remote: None,
            fork: None,
        }],
    };
    let verifier: Option<Arc<dyn Verifier>> =
        Some(Arc::new(Ed25519Verifier::load(&key_dir, AUDIENCE)?));
    let deps = Deps {
        verifier,
        calls: Arc::new(NoLander),
    };
    let server = server::start_with_runner(&config, deps, Arc::new(NoRunner)).await?;
    std::env::set_current_dir(&elsewhere)?;
    let token = identity::mint(
        &keys::load_signing_key(&key_dir, "k1")?,
        &TokenRequest {
            kid: "k1",
            audience: AUDIENCE,
            tenant: "acme",
            repo: "demo",
            agent: "dashboard",
            role: Role::Agent,
            ttl_ms: 3_600_000,
        },
        identity::now_ms(),
    )?;

    let status = summary_status(server.local_addr(), &token).await?;

    assert_eq!(status, 200);
    server.stop().await;
    Ok(())
}

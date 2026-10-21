//! The real CLI against the real server with identity: keys made by the server's keygen, a token
//! minted by its `admin token`, the server in this process on an ephemeral port, and the
//! `tessel` binary doing `init`, `start`, `claim` and `release` over a WebSocket.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]
#![expect(
    dead_code,
    reason = "each test crate compiles all of support, which the other test crates also use"
)]

mod support;

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use support::{eventually, Agent, REPO};
use tessel_coordinator::protocol::{AgentId, EventKind};
use tessel_server::admin::{self, TokenArgs};
use tessel_server::config::{Config, IdentityConfig, RepositoryConfig};
use tessel_server::id::RepoId;
use tessel_server::identity::{self, Ed25519Verifier};
use tessel_server::keys;
use tessel_server::repo::NoLander;
use tessel_server::server::{self, Deps, RunningServer};
use tessel_server::store::Store;
use tessel_server::ws::Verifier;

const TENANT: &str = "acme";
const AUDIENCE: &str = "tessel-test";
const KID: &str = "k1";
const SHORT: Duration = Duration::from_secs(8);

/// A server with `[identity]` keys, serving `acme/demo`, and the means to mint tokens for it.
struct World {
    dir: tempfile::TempDir,
    identity: IdentityConfig,
    server: RunningServer,
}

impl World {
    async fn start() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let identity = IdentityConfig {
            keys: dir.path().join("keys"),
            audience: AUDIENCE.to_string(),
        };
        keys::generate(&identity.keys, KID)?;
        let verifier: Arc<dyn Verifier> =
            Arc::new(Ed25519Verifier::load(&identity.keys, AUDIENCE)?);
        let config = Config {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            store_dir: dir.path().join("stores"),
            repositories: vec![RepositoryConfig {
                tenant: TENANT.to_string(),
                name: REPO.to_string(),
                run: "local-server-test".to_string(),
                reviewers: Vec::new(),
                sensitive: Vec::new(),
            }],
            identity: Some(identity.clone()),
        };
        let deps = Deps {
            verifier: Some(verifier),
            calls: Arc::new(NoLander),
        };
        let server = server::start(&config, deps).await?;
        Ok(Self {
            dir,
            identity,
            server,
        })
    }

    /// What `TESSEL_COORDINATOR` is for this server: the CLI appends `/repo/<repo>/ws`.
    fn coordinator(&self) -> String {
        format!("ws://{}/tenant/{TENANT}", self.server.local_addr())
    }

    /// `tessel-server admin token` for `agent` on `repo`, through the library.
    fn token(&self, repo: &str, agent: &str) -> Result<String> {
        let args = TokenArgs {
            kid: KID,
            tenant: TENANT,
            repo,
            agent,
            ttl_ms: 3_600_000,
            service: false,
        };
        Ok(admin::token(&self.identity, &args, identity::now_ms())?)
    }

    fn events(&self) -> Result<Vec<EventKind>> {
        let id = RepoId::new(TENANT, REPO)?;
        let store = Store::open_read_only(&id.store_file(&self.dir.path().join("stores")))?;
        Ok(store
            .read_events(0, 1000)?
            .into_iter()
            .map(|event| event.kind)
            .collect())
    }
}

/// Every file under `dir`, `.git` included.
fn files_under(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).with_context(|| next.display().to_string())? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    Ok(found)
}

/// The files under `dir` whose bytes contain `needle`.
fn files_holding(dir: &Path, needle: &str) -> Result<Vec<PathBuf>> {
    let mut holding = Vec::new();
    for path in files_under(dir)? {
        let bytes = std::fs::read(&path).with_context(|| path.display().to_string())?;
        if bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
        {
            holding.push(path);
        }
    }
    Ok(holding)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cli_inits_starts_claims_and_releases_against_a_server_with_identity() -> Result<()> {
    let world = World::start().await?;
    let token = world.token(REPO, "a1")?;
    let agent = Agent::at(&world.coordinator(), "a1", &token)?;

    let init = agent.tessel(&["init"])?;
    assert_eq!(init.code, 0, "{}", init.all());
    let config = agent.root().join(".tessel/config.toml");
    assert_eq!(
        std::fs::metadata(&config)?.permissions().mode() & 0o777,
        0o600
    );
    assert!(std::fs::read_to_string(&config)?.contains(&token));

    let started = agent.tessel(&["start", "local server test"])?;
    assert_eq!(started.code, 0, "{}", started.all());
    let claimed = agent.tessel(&["claim", "src/a.rs"])?;
    assert_eq!(claimed.code, 0, "{}", claimed.all());
    let released = agent.tessel(&["release"])?;
    assert_eq!(released.code, 0, "{}", released.all());

    let seen = eventually(SHORT, || {
        let events = world.events()?;
        let granted = events.iter().any(|kind| {
            matches!(kind, EventKind::ClaimGranted { agent, .. } if *agent == AgentId("a1".into()))
        });
        let released = events
            .iter()
            .any(|kind| matches!(kind, EventKind::ClaimReleased { .. }));
        Ok((granted && released).then_some(events.len()))
    })
    .await
    .context("the server's event log never held the grant and the release")?;
    assert!(
        seen >= 2,
        "the log holds at least the grant and the release"
    );

    let outputs = [init.all(), started.all(), claimed.all(), released.all()].concat();
    assert!(!outputs.contains(&token), "a command printed the token");
    let scanned = files_under(&agent.root())?;
    assert!(scanned.contains(&config), "config.toml was not scanned");
    assert!(
        scanned.contains(&agent.root().join(".tessel/daemon.log")),
        "daemon.log was not scanned"
    );
    let holders = files_holding(&agent.root(), &token)?;
    assert_eq!(
        holders,
        [config],
        "the token is in files other than the config"
    );
    let stores = world.dir.path().join("stores");
    let store_files = files_under(&stores)?;
    assert!(
        store_files
            .iter()
            .any(|file| file.extension().is_some_and(|ext| ext == "sqlite")),
        "the SQLite file was not scanned: {store_files:?}"
    );
    assert!(
        files_holding(&stores, &token)?.is_empty(),
        "the token is in the server's store directory"
    );
    world.server.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_for_another_repository_is_refused_at_start_without_printing_it() -> Result<()> {
    let world = World::start().await?;
    let token = world.token("elsewhere", "a1")?;
    let agent = Agent::at(&world.coordinator(), "a1", &token)?;

    let init = agent.tessel(&["init"])?;
    assert_eq!(init.code, 0, "{}", init.all());
    let started = agent.tessel(&["start", "wrong repository"])?;
    assert_eq!(started.code, 1, "{}", started.all());
    assert!(
        started.stderr.contains(
            "the coordinator refused the connection with HTTP 401 Unauthorized; check \
             TESSEL_TOKEN, TESSEL_REPO and TESSEL_COORDINATOR"
        ),
        "{}",
        started.stderr
    );
    assert!(!started.all().contains(&token), "{}", started.all());
    let holders = files_holding(&agent.root(), &token)?;
    assert_eq!(holders, [agent.root().join(".tessel/config.toml")]);
    assert!(world
        .events()?
        .iter()
        .all(|kind| !matches!(kind, EventKind::AgentConnected { .. })));
    world.server.stop().await;
    Ok(())
}

//! Starting the server: the lock on the store directory, one task per configured repository, and
//! the listener.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use tessel_coordinator::host::HostConfig;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::id::{IdError, RepoId};
use crate::repo::{self, CallPort, RunningRepo};
use crate::store::{StoreDir, StoreError};
use crate::ws::{self, App, Verifier};

/// What the server is wired to: the ports of the identity and of the calls.
pub struct Deps {
    /// `None`: no identity is configured, and every authenticated route answers 401.
    pub verifier: Option<Arc<dyn Verifier>>,
    pub calls: Arc<dyn CallPort>,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("repository {tenant:?}/{name:?}: {source}")]
    Id {
        tenant: String,
        name: String,
        source: IdError,
    },
    #[error("repository {tenant:?}/{name:?} is listed twice")]
    Duplicate { tenant: String, name: String },
    #[error("cannot listen on {addr}: {source}")]
    Listen {
        addr: SocketAddr,
        source: std::io::Error,
    },
}

/// A server that is listening.
pub struct RunningServer {
    local_addr: SocketAddr,
    repos: Vec<RunningRepo>,
    shutdown: oneshot::Sender<()>,
    serving: JoinHandle<()>,
    _dir: StoreDir,
}

impl RunningServer {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop the repositories (which ends their sockets), then the listener, then let go of the
    /// store directory.
    pub async fn stop(self) {
        for repo in self.repos {
            repo.stop().await;
        }
        let _ = self.shutdown.send(());
        let _ = self.serving.await;
    }

    /// Wait until the listener ends, which it does only if it fails.
    pub async fn wait(&mut self) {
        let _ = (&mut self.serving).await;
    }
}

/// Take the lock, open every store, start every repository and listen.
pub async fn start(config: &Config, deps: Deps) -> Result<RunningServer, StartError> {
    let dir = StoreDir::open(&config.store_dir)?;
    let mut repos = Vec::new();
    let mut handles = BTreeMap::new();
    for repository in &config.repositories {
        let id =
            RepoId::new(&repository.tenant, &repository.name).map_err(|source| StartError::Id {
                tenant: repository.tenant.clone(),
                name: repository.name.clone(),
                source,
            })?;
        if handles.contains_key(&id) {
            return Err(StartError::Duplicate {
                tenant: repository.tenant.clone(),
                name: repository.name.clone(),
            });
        }
        let store = dir.open_repo(&id)?;
        let host = HostConfig {
            repo: id.to_string(),
            run: Some(repository.run.clone()),
            reviewers: repository.reviewers_list(),
            sensitive: repository.sensitive_list(),
            ..HostConfig::default()
        };
        let running = repo::spawn(
            id.clone(),
            store,
            dir.repo_file(&id),
            host,
            deps.calls.clone(),
        );
        tracing::info!(repo = %id, "serving");
        handles.insert(id, running.handle.clone());
        repos.push(running);
    }
    if deps.verifier.is_none() {
        tracing::warn!("no identity is configured: every authenticated route answers 401");
    }
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|source| StartError::Listen {
            addr: config.listen,
            source,
        })?;
    let local_addr = listener.local_addr().map_err(|source| StartError::Listen {
        addr: config.listen,
        source,
    })?;
    tracing::info!(addr = %local_addr, "listening");
    let app = Arc::new(App {
        repos: handles,
        verifier: deps.verifier,
    });
    let (shutdown, stopped) = oneshot::channel::<()>();
    let serve = axum::serve(listener, ws::router(app)).with_graceful_shutdown(async {
        let _ = stopped.await;
    });
    let serving = tokio::spawn(async move {
        if let Err(error) = serve.await {
            tracing::error!(error = %error, "the listener failed");
        }
    });
    Ok(RunningServer {
        local_addr,
        repos,
        shutdown,
        serving,
        _dir: dir,
    })
}

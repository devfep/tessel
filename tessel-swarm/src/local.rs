//! The local target: the real coordinator core behind a WebSocket server on 127.0.0.1, and a
//! steward that lands submissions with real git and the repo's real tests.
//!
//! `tessel-cli/tests/support/mod.rs` builds the same kind of fake for the CLI's tests, but it is
//! test-only and tied to that crate's binary, so it cannot be linked from here. This one keeps
//! the pattern (same `shell` functions, same core calls, replies before events as the Durable
//! Object does) without the fault injection. The steward differs on purpose: the CLI's fake
//! always reports a merge, this one cherry-picks the submitted commit onto the trunk, runs the
//! tests, and reports `Conflict` or `TestsFailed` when that is what happened.
//!
//! What the local steward does not do: check the merged diff against the claim (invariant 11);
//! the core already checked the `touched` list the agent submitted.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::coordinator::{Config, Coordinator, MergeDispatch};
use tessel_coordinator::merge::{MergeOutcome, StepExit};
use tessel_coordinator::protocol::{AgentId, CommitId, Event, RunId, ServerMsg};
use tessel_coordinator::shell::{self, Action, Outbound, Session};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

use crate::demo::Tree;
use crate::endpoint::{Endpoint, Remote, Token};
use crate::git::{self, Checks, Git};
use crate::guard::ScratchRepo;

const LEASE_MS: u64 = 30_000;

fn now_ms() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct SocketEntry {
    id: u64,
    bound: Option<AgentId>,
    watching: bool,
    tx: mpsc::UnboundedSender<ServerMsg>,
}

struct State {
    core: Coordinator,
    sockets: Vec<SocketEntry>,
    events: Vec<Event>,
    next_socket: u64,
}

struct Hub {
    state: Mutex<State>,
    /// Token to agent name.
    tokens: HashMap<String, String>,
    path: String,
}

impl State {
    /// Sends `outbound`, then records and streams `events`: the reply first, then the events it
    /// caused, as the Durable Object does.
    fn flush(&mut self, origin: Option<u64>, events: Vec<Event>, outbound: Vec<Outbound>) {
        for item in outbound {
            match item {
                Outbound::Reply(msg) => {
                    if let Some(entry) = self.sockets.iter().find(|s| Some(s.id) == origin) {
                        let _ = entry.tx.send(msg);
                    }
                }
                Outbound::Notify { agent, msg } => {
                    for entry in self
                        .sockets
                        .iter()
                        .filter(|s| s.bound.as_ref() == Some(&agent))
                    {
                        let _ = entry.tx.send(msg.clone());
                    }
                }
            }
        }
        for event in events {
            for entry in self.sockets.iter().filter(|s| s.watching) {
                let _ = entry.tx.send(ServerMsg::Event {
                    event: event.clone(),
                });
            }
            self.events.push(event);
        }
    }
}

/// A running local coordinator and steward. Dropping it without `shutdown` leaves the tasks to
/// end with the runtime; `shutdown` closes the listener and every socket first.
pub struct LocalServer {
    pub endpoint: Endpoint,
    hub: Arc<Hub>,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct LogReader(Arc<Hub>);

impl LogReader {
    pub fn events(&self) -> Vec<Event> {
        lock(&self.0.state).events.clone()
    }
}

/// What the local target is made of on disk, inside `scratch`.
pub struct LocalSetup<'a> {
    pub repo: &'a ScratchRepo,
    pub scratch: &'a Path,
    pub base: &'a Tree,
    /// Every name that needs a token: the agents, the reviewer and the observer.
    pub names: &'a [String],
    pub reviewers: &'a [String],
}

impl LocalServer {
    pub async fn start(setup: LocalSetup<'_>) -> Result<Self> {
        let trunk_dir = setup.scratch.join("trunk");
        let forks_dir = setup.scratch.join("forks");
        std::fs::create_dir_all(&trunk_dir)?;
        std::fs::create_dir_all(&forks_dir)?;
        let trunk = Git::new(&trunk_dir);
        git::init_repo(&trunk, setup.base)?;
        for name in setup.names {
            let fork = forks_dir.join(format!("{name}.git"));
            std::fs::create_dir_all(&fork)?;
            Git::new(&fork).run(&["init", "-q", "--bare", "-b", "main"])?;
        }
        let mut core = Coordinator::new(Config {
            run: RunId("swarm-on".into()),
            lease_ms: LEASE_MS,
            shadow_enabled: false,
        })?;
        core.set_reviewers(setup.reviewers.iter().map(|r| AgentId(r.clone())).collect());
        let mut tokens = HashMap::new();
        let mut issued = HashMap::new();
        for name in setup.names {
            let token = format!("local-{name}");
            tokens.insert(token.clone(), name.clone());
            issued.insert(name.clone(), Token::new(token));
        }
        let path = format!("/repo/{}/ws", setup.repo);
        let hub = Arc::new(Hub {
            state: Mutex::new(State {
                core,
                sockets: Vec::new(),
                events: Vec::new(),
                next_socket: 0,
            }),
            tokens,
            path: path.clone(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (stop, stop_rx) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(accept(listener, Arc::clone(&hub), stop_rx)),
            tokio::spawn(expire_loop(Arc::clone(&hub))),
            tokio::spawn(steward_loop(Arc::clone(&hub), trunk, forks_dir.clone())),
        ];
        Ok(Self {
            hub,
            endpoint: Endpoint {
                ws_url: format!("ws://{addr}{path}"),
                tokens: issued,
                remote: Remote::Local {
                    trunk: trunk_dir,
                    forks: forks_dir,
                },
            },
            stop,
            tasks,
        })
    }

    /// The coordinator's own copy of the event log, for checking what a Watch reader saw.
    pub fn log(&self) -> Vec<Event> {
        self.reader().events()
    }

    /// A handle that reads the log from another task while the server runs.
    pub fn reader(&self) -> LogReader {
        LogReader(Arc::clone(&self.hub))
    }

    pub fn addr(&self) -> Option<SocketAddr> {
        self.endpoint
            .ws_url
            .strip_prefix("ws://")
            .and_then(|rest| rest.split('/').next())
            .and_then(|host| host.parse().ok())
    }

    /// Closes every socket and the listener, and ends the background tasks.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks {
            let _ = task.await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn accept(listener: TcpListener, hub: Arc<Hub>, stop: watch::Receiver<bool>) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(serve(stream, Arc::clone(&hub), stop.clone()));
    }
}

async fn expire_loop(hub: Arc<Hub>) {
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut state = lock(&hub.state);
        let effects = state.core.expire(now_ms());
        let (events, outbound) = shell::split_effects(effects);
        state.flush(None, events, outbound);
    }
}

/// Lands submissions one at a time, the way the steward does: take the next merge from the core,
/// run it with real git, report the outcome.
async fn steward_loop(hub: Arc<Hub>, trunk: Git, forks: PathBuf) {
    loop {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let dispatch = lock(&hub.state).core.begin_merge(now_ms());
        let Some(dispatch) = dispatch else { continue };
        let (repo, dir) = (trunk.clone(), forks.clone());
        let work = dispatch.clone();
        let outcome = tokio::task::spawn_blocking(move || land(&repo, &dir, &work))
            .await
            .unwrap_or(MergeOutcome::GitFailed {});
        let mut state = lock(&hub.state);
        let effects = state.core.merge_outcome(dispatch.claim, &outcome, now_ms());
        let (events, outbound) = shell::split_effects(effects);
        state.flush(None, events, outbound);
    }
}

/// Cherry-picks the submitted commit onto the trunk and runs the tests. A pick that does not
/// apply is a conflict; a pick whose tests fail is rolled back.
fn land(trunk: &Git, forks: &Path, dispatch: &MergeDispatch) -> MergeOutcome {
    try_land(trunk, forks, dispatch).unwrap_or(MergeOutcome::GitFailed {})
}

fn try_land(trunk: &Git, forks: &Path, dispatch: &MergeDispatch) -> Result<MergeOutcome> {
    let fork = forks.join(format!("{}.git", dispatch.agent.0));
    let commit = dispatch.fork_commit.0.as_str();
    let (fetched, _) = trunk.attempt(&["fetch", "-q", &fork.to_string_lossy(), "main"])?;
    let (known, _) = trunk.attempt(&["cat-file", "-e", &format!("{commit}^{{commit}}")])?;
    if !fetched || !known {
        return Ok(MergeOutcome::CommitNotInFork {});
    }
    let base = trunk.run(&["rev-parse", "HEAD"])?;
    let (picked, _) = trunk.attempt(&["cherry-pick", commit])?;
    if !picked {
        let (empty, _) = trunk.attempt(&["diff", "--cached", "--quiet"])?;
        let files: Vec<String> = trunk
            .run(&["diff", "--name-only", "--diff-filter=U"])?
            .lines()
            .map(str::to_string)
            .collect();
        trunk.run(&["cherry-pick", "--abort"])?;
        if files.is_empty() && empty {
            return Ok(MergeOutcome::AlreadyMerged {
                base: CommitId(base),
            });
        }
        return Ok(MergeOutcome::Conflict { files });
    }
    match git::run_checks(&trunk.dir)? {
        Checks::Pass { .. } => {
            let head = trunk.run(&["rev-parse", "HEAD"])?;
            Ok(MergeOutcome::Merged {
                base: CommitId(base),
                head: CommitId(head),
            })
        }
        Checks::BuildFailed | Checks::TestsFailed => {
            trunk.run(&["reset", "-q", "--hard", &base])?;
            Ok(MergeOutcome::TestsFailed {
                result: StepExit { exit_code: 1 },
            })
        }
    }
}

async fn serve(stream: TcpStream, hub: Arc<Hub>, mut stop: watch::Receiver<bool>) {
    let verified: Arc<Mutex<Option<AgentId>>> = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&verified);
    let known = Arc::clone(&hub);
    #[expect(
        clippy::result_large_err,
        reason = "the handshake callback fixes this signature"
    )]
    let check = move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let bearer = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match bearer.and_then(|token| known.tokens.get(token)) {
            Some(agent) if request.uri().path() == known.path => {
                *lock(&seen) = Some(AgentId(agent.clone()));
                Ok(response)
            }
            Some(_) | None => {
                let mut refusal = ErrorResponse::new(Some("unauthorized".to_string()));
                *refusal.status_mut() = StatusCode::UNAUTHORIZED;
                Err(refusal)
            }
        }
    };
    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, check).await else {
        return;
    };
    let Some(agent) = lock(&verified).clone() else {
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    let id = {
        let mut state = lock(&hub.state);
        state.next_socket += 1;
        let id = state.next_socket;
        state.sockets.push(SocketEntry {
            id,
            bound: None,
            watching: false,
            tx,
        });
        id
    };
    let mut session = Session {
        verified: Some(agent),
        ..Session::default()
    };
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            outgoing = rx.recv() => {
                let Some(msg) = outgoing else { break };
                let Ok(text) = serde_json::to_string(&msg) else { break };
                if socket.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    if handle_text(&hub, id, &mut session, &text) {
                        break;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
    }
    close_socket(&hub, id, &session);
}

/// Handles one frame; returns whether the socket must close.
fn handle_text(hub: &Hub, id: u64, session: &mut Session, text: &str) -> bool {
    let mut state = lock(&hub.state);
    let msg = match shell::parse_client_msg(text) {
        Ok(msg) => msg,
        Err(fault) => {
            state.flush(Some(id), Vec::new(), vec![Outbound::Reply(fault.reply())]);
            return false;
        }
    };
    match shell::decide(session, &msg) {
        Action::Reject(reply) => state.flush(Some(id), Vec::new(), vec![Outbound::Reply(reply)]),
        Action::RejectAndClose(reply) => {
            state.flush(Some(id), Vec::new(), vec![Outbound::Reply(reply)]);
            return true;
        }
        Action::Watch { from_seq } => {
            let State {
                events, sockets, ..
            } = &mut *state;
            if let Some(entry) = sockets.iter_mut().find(|s| s.id == id) {
                for event in events.iter().filter(|e| e.seq >= from_seq) {
                    let _ = entry.tx.send(ServerMsg::Event {
                        event: event.clone(),
                    });
                }
                entry.watching = true;
            }
        }
        Action::Call { agent } => {
            let effects = state.core.handle(&agent, msg, now_ms());
            let (events, outbound) = shell::split_effects(effects);
            if let Some(bound) = shell::bind_on_welcome(session, &agent, &outbound) {
                *session = bound;
                if let Some(entry) = state.sockets.iter_mut().find(|s| s.id == id) {
                    entry.bound.clone_from(&session.agent);
                }
            }
            state.flush(Some(id), events, outbound);
        }
    }
    false
}

fn close_socket(hub: &Hub, id: u64, session: &Session) {
    let mut state = lock(&hub.state);
    state.sockets.retain(|s| s.id != id);
    let Some(agent) = session.agent.as_ref() else {
        return;
    };
    if state
        .sockets
        .iter()
        .any(|s| s.bound.as_ref() == Some(agent))
    {
        return;
    }
    let effects = state.core.disconnect(agent, now_ms());
    let (events, outbound) = shell::split_effects(effects);
    state.flush(None, events, outbound);
}

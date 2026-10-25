//! One repository: a task that owns its `Host` and a thread that owns its store. Nothing is
//! shared between two repositories, neither a task, a store nor a log (R49).
//!
//! The task takes every input from one channel, in order: what sockets do, the answers of the
//! storage thread and of calls, and the wake. It runs `Host::step` and carries out each `Output`.
//! While a storage operation is outstanding the host queues what arrives, which is the input gate
//! of the design (R42); the task itself never waits for storage, so a slow disk does not stop it
//! from reading its inbox.
//!
//! The storage thread runs one job at a time in the order they were asked, so the answers reach
//! the host in that order. A commit is answered `Committed` only after its transaction returned,
//! so no `Send` of a batch leaves before the batch is durable (rule 5, C1).

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tessel_coordinator::host::call::{Call, CallAnswer, Refusal};
use tessel_coordinator::host::{BatchId, Host, HostConfig, Input, Output, SocketId};
use tessel_coordinator::merge::RuleKind;
use tessel_coordinator::protocol::AgentId;
use tessel_coordinator::shell::{self, PokeReply, ReplayStep, SummaryTally};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::id::RepoId;
use crate::store::{Store, StoreError};

/// How many messages may wait for one socket's writer before the socket is closed as too slow.
const WRITER_QUEUE: usize = 1_024;

/// How many inputs may wait in the channel to the repository's task. It bounds the channel only:
/// while a storage operation is outstanding the host queues every input it is given, without
/// limit, so a client that floods during a slow commit still grows memory.
const INBOX_QUEUE: usize = 256;

/// The reason `NoLander` gives for refusing every call.
pub const NO_LANDER_MESSAGE: &str = "no lander is configured on this server";

pub fn now_ms() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

/// Where a repository task reads the time: the system clock, or, in a test that built it through
/// `testing::TestClock`, a clock the test moves. `server::start` always passes the system clock,
/// and nothing in the configuration or on the wire can choose another.
#[derive(Clone, Default)]
pub struct Clock(Option<Arc<Shared>>);

/// The clock a test controls: frozen at an instant plus an added skew, and a counter that tells
/// the repository tasks it moved.
pub(crate) struct Shared {
    state: std::sync::Mutex<(Option<u64>, u64)>,
    moved: tokio::sync::watch::Sender<u64>,
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            state: std::sync::Mutex::new((None, 0)),
            moved: tokio::sync::watch::channel(0).0,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (Option<u64>, u64)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn now_ms(&self) -> u64 {
        let (frozen_at, skew) = *self.lock();
        frozen_at.unwrap_or_else(now_ms).saturating_add(skew)
    }

    pub(crate) fn freeze(&self) {
        self.lock().0 = Some(now_ms());
        self.moved.send_modify(|moves| *moves += 1);
    }

    pub(crate) fn advance(&self, ms: u64) {
        self.lock().1 += ms;
        self.moved.send_modify(|moves| *moves += 1);
    }
}

impl Clock {
    pub(crate) fn shared(shared: Arc<Shared>) -> Self {
        Self(Some(shared))
    }

    pub fn now_ms(&self) -> u64 {
        self.0
            .as_ref()
            .map_or_else(now_ms, |shared| shared.now_ms())
    }

    /// A receiver that wakes when a test moves the clock; `None` for the system clock.
    fn subscribe(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.0.as_ref().map(|shared| shared.moved.subscribe())
    }
}

/// Resolves when the test clock moves, never for the system clock.
async fn clock_moved(changes: &mut Option<tokio::sync::watch::Receiver<u64>>) {
    match changes {
        Some(receiver) => {
            if receiver.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// A boxed future, so the port can be a trait object without another dependency.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// The one port the repository task needs for calls (`Prepare`, `Trial`, `ReadHead`, and later
/// `Test`, `Land`, `Reconcile`): given the call, with its request and its deadline, answer it.
/// The task stamps the time and wraps the answer in `Input::Replied`; a panic in the answer reads
/// as `Input::CallLost`. Free text in an answer is untrusted data.
pub trait CallPort: Send + Sync + 'static {
    fn answer(&self, call: Call) -> BoxFuture<CallAnswer>;

    /// Whether this port can read main's head. A repository whose port can wakes once when the
    /// server starts, so a fresh repository knows main's head before its first agent says hello.
    fn reads_trunk_head(&self) -> bool {
        false
    }
}

/// Stands for "no lander configured". Every call is refused with a typed refusal the core reports
/// to the agent as a refused merge. It never claims a landing (rule 6), and it never answers
/// `Unknown` or loses the call, which the core would retry without end.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLander;

impl CallPort for NoLander {
    fn answer(&self, _call: Call) -> BoxFuture<CallAnswer> {
        Box::pin(async {
            CallAnswer::Refused {
                refusal: Refusal::Rule {
                    rule: RuleKind::NoLander,
                    message: NO_LANDER_MESSAGE.to_string(),
                },
            }
        })
    }
}

/// What the task writes to a socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wire {
    Text(String),
    /// A close frame; the socket ends after it.
    Close {
        code: u16,
        reason: String,
    },
}

enum Msg {
    Open {
        sock: SocketId,
        verified: AgentId,
        writer: mpsc::Sender<Wire>,
    },
    Frame {
        sock: SocketId,
        text: String,
    },
    Binary {
        sock: SocketId,
    },
    Closed {
        sock: SocketId,
    },
    Poke,
    /// An answer of the storage thread or of a call.
    Host(Input),
    /// The store cannot be read: the task ends instead of building an empty core over data.
    Unreadable(String),
    Stop,
}

enum StoreJob {
    Load,
    Commit {
        batch: BatchId,
        entries: Vec<(String, String)>,
    },
    /// `live` is false for a socket that is gone: the answer is then an empty, final page.
    ReadEvents {
        sock: SocketId,
        from_seq: u64,
        limit: usize,
        live: bool,
    },
}

/// What the route handlers hold for a repository.
#[derive(Clone)]
pub struct RepoHandle {
    id: RepoId,
    file: Arc<PathBuf>,
    tx: mpsc::Sender<Msg>,
    next_sock: Arc<AtomicU64>,
}

/// A repository's running task.
pub struct RunningRepo {
    pub handle: RepoHandle,
    task: JoinHandle<()>,
}

impl RunningRepo {
    /// Ask the task to end and wait until its store is closed.
    pub async fn stop(self) {
        let _ = self.handle.tx.send(Msg::Stop).await;
        let _ = self.task.await;
    }
}

/// Start the task and the storage thread of `id`.
pub fn spawn(
    id: RepoId,
    store: Store,
    file: PathBuf,
    config: HostConfig,
    calls: Arc<dyn CallPort>,
    clock: Clock,
) -> RunningRepo {
    let (tx, inbox) = mpsc::channel(INBOX_QUEUE);
    let (jobs, queue) = std_mpsc::channel();
    let wake_on_start = holds_state(&id, &store) || calls.reads_trunk_head();
    let thread = spawn_storage(id.clone(), store, queue, tx.clone(), clock.clone());
    let handle = RepoHandle {
        id: id.clone(),
        file: Arc::new(file),
        tx: tx.clone(),
        next_sock: Arc::new(AtomicU64::new(0)),
    };
    let task = RepoTask {
        id,
        host: Host::new(config),
        inbox,
        tx,
        jobs: Some(jobs),
        thread: Some(thread),
        sockets: BTreeMap::new(),
        wake_at: None,
        wake_on_start,
        calls,
        changes: clock.subscribe(),
        clock,
    };
    RunningRepo {
        handle,
        task: tokio::spawn(task.run()),
    }
}

/// Whether `store` holds state. A store that cannot be asked is treated as empty here: the task
/// reports an unreadable store itself when it loads.
fn holds_state(id: &RepoId, store: &Store) -> bool {
    store.has_state().unwrap_or_else(|error| {
        tracing::warn!(repo = %id, error = %error, "cannot tell whether the store holds state");
        false
    })
}

impl RepoHandle {
    pub fn id(&self) -> &RepoId {
        &self.id
    }

    /// Whether the task is still running. It ends when its store cannot be read.
    pub fn is_running(&self) -> bool {
        !self.tx.is_closed()
    }

    /// Register a socket of `verified`, which the adapter authenticated. The writer's receiver
    /// yields what the host sends the socket.
    pub async fn open(&self, verified: AgentId) -> Option<(SocketId, mpsc::Receiver<Wire>)> {
        let sock = SocketId(self.next_sock.fetch_add(1, Ordering::SeqCst));
        let (writer, wire) = mpsc::channel(WRITER_QUEUE);
        let open = Msg::Open {
            sock,
            verified,
            writer,
        };
        self.tx.send(open).await.ok()?;
        Some((sock, wire))
    }

    pub async fn frame(&self, sock: SocketId, text: String) {
        let _ = self.tx.send(Msg::Frame { sock, text }).await;
    }

    pub async fn binary(&self, sock: SocketId) {
        let _ = self.tx.send(Msg::Binary { sock }).await;
    }

    pub async fn closed(&self, sock: SocketId) {
        let _ = self.tx.send(Msg::Closed { sock }).await;
    }

    /// Tell the host main may have moved. A repository with no stored state drops the poke and
    /// answers `UnknownRepo` (R21).
    pub async fn poke(&self) -> Result<PokeReply, StoreError> {
        let file = Arc::clone(&self.file);
        let stored = spawn_read(move || Store::open_read_only(&file)?.has_state()).await?;
        let reply = shell::poke_reply(stored.then_some(""));
        if reply == PokeReply::Recorded {
            let _ = self.tx.send(Msg::Poke).await;
        }
        Ok(reply)
    }

    /// The body of the summary read: counters over the whole stored log, read a page at a time
    /// from a connection of its own, so the repository is not stalled while it runs.
    pub async fn summary(&self) -> Result<String, SummaryError> {
        let file = Arc::clone(&self.file);
        let result = spawn_read(move || Ok(read_summary(&file))).await?;
        result
    }
}

/// Why the summary could not be read.
#[derive(Debug, thiserror::Error)]
pub enum SummaryError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the summary does not encode: {0}")]
    Encode(#[from] serde_json::Error),
}

fn read_summary(file: &std::path::Path) -> Result<String, SummaryError> {
    let store = Store::open_read_only(file)?;
    let mut tally = SummaryTally::default();
    loop {
        let page = store.read_events(tally.next_seq(), shell::REPLAY_PAGE)?;
        let last_seq = page.last().map(|event| event.seq);
        let page_len = page.len();
        tally.add_page(&page);
        match shell::after_page(page_len, last_seq) {
            ReplayStep::Next { .. } => {}
            ReplayStep::Done => break,
        }
    }
    Ok(tally.report()?)
}

async fn spawn_read<T: Send + 'static>(
    read: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    match tokio::task::spawn_blocking(read).await {
        Ok(result) => result,
        Err(error) => Err(StoreError::LockFile {
            path: "read thread".to_string(),
            source: std::io::Error::other(error.to_string()),
        }),
    }
}

fn spawn_storage(
    id: RepoId,
    mut store: Store,
    queue: std_mpsc::Receiver<StoreJob>,
    tx: mpsc::Sender<Msg>,
    clock: Clock,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for job in queue {
            let answer = match job {
                StoreJob::Load => match store.load() {
                    Ok(stored) => Msg::Host(Input::Loaded {
                        now_ms: clock.now_ms(),
                        stored,
                    }),
                    Err(error) => Msg::Unreadable(error.to_string()),
                },
                StoreJob::Commit { batch, entries } => {
                    Msg::Host(committed(&id, &mut store, batch, &entries, &clock))
                }
                StoreJob::ReadEvents {
                    sock,
                    from_seq,
                    limit,
                    live,
                } => Msg::Host(read_page(&id, &store, (sock, from_seq, limit), live)),
            };
            if tx.blocking_send(answer).is_err() {
                return;
            }
        }
    })
}

fn committed(
    id: &RepoId,
    store: &mut Store,
    batch: BatchId,
    entries: &[(String, String)],
    clock: &Clock,
) -> Input {
    let now_ms = clock.now_ms();
    match store.commit(entries) {
        Ok(()) => Input::Committed { now_ms, batch },
        Err(error) => {
            tracing::error!(repo = %id, error = %error, "commit failed");
            Input::CommitFailed { now_ms, batch }
        }
    }
}

/// The page of a replay. A failed read or a gone socket ends the replay with an empty page,
/// because no input reports a failed read and the host would otherwise wait for ever.
fn read_page(
    id: &RepoId,
    store: &Store,
    (sock, from_seq, limit): (SocketId, u64, usize),
    live: bool,
) -> Input {
    let now_ms = now_ms();
    let page = if live {
        store.read_events(from_seq, limit)
    } else {
        Ok(Vec::new())
    };
    match page {
        Ok(events) => Input::ReplayPage {
            now_ms,
            sock,
            events,
            done: !live,
        },
        Err(error) => {
            tracing::error!(repo = %id, error = %error, "reading events failed");
            Input::ReplayPage {
                now_ms,
                sock,
                events: Vec::new(),
                done: true,
            }
        }
    }
}

struct SocketEntry {
    writer: mpsc::Sender<Wire>,
    /// The host closed it: reads for it are answered with an empty page.
    closing: bool,
}

struct RepoTask {
    id: RepoId,
    host: Host,
    inbox: mpsc::Receiver<Msg>,
    tx: mpsc::Sender<Msg>,
    jobs: Option<std_mpsc::Sender<StoreJob>>,
    thread: Option<std::thread::JoinHandle<()>>,
    sockets: BTreeMap<SocketId, SocketEntry>,
    wake_at: Option<u64>,
    /// The store held state when the task started, so a merge or a verification may have been cut
    /// off by the stop that came before and only a wake recovers it; or the repository's port reads
    /// main's head, and a wake reads it.
    wake_on_start: bool,
    calls: Arc<dyn CallPort>,
    clock: Clock,
    /// Wakes the loop when a test moves the clock, so the wake timer is set again.
    changes: Option<tokio::sync::watch::Receiver<u64>>,
}

impl RepoTask {
    async fn run(mut self) {
        if self.wake_on_start {
            self.feed(Input::Wake {
                now_ms: self.clock.now_ms(),
            });
        }
        let timer = tokio::time::sleep(Duration::from_secs(3_600));
        tokio::pin!(timer);
        loop {
            if let Some(at_ms) = self.wake_at {
                let wait = Duration::from_millis(at_ms.saturating_sub(self.clock.now_ms()));
                timer.as_mut().reset(Instant::now() + wait);
            }
            tokio::select! {
                msg = self.inbox.recv() => {
                    let Some(msg) = msg else { break };
                    if !self.handle(msg) {
                        break;
                    }
                }
                () = clock_moved(&mut self.changes) => {}
                () = &mut timer, if self.wake_at.is_some() => {
                    self.wake_at = None;
                    self.feed(Input::Wake { now_ms: self.clock.now_ms() });
                }
            }
        }
        // Closing the inbox first fails a `blocking_send` the storage thread is stuck in, which
        // would otherwise keep the join below waiting for a thread that waits for room in a
        // channel nobody reads any more.
        self.inbox.close();
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        tracing::info!(repo = %self.id, "repository task ended");
    }

    /// Whether the task goes on.
    fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Open {
                sock,
                verified,
                writer,
            } => {
                let entry = SocketEntry {
                    writer,
                    closing: false,
                };
                self.sockets.insert(sock, entry);
                self.feed(Input::Opened {
                    now_ms: self.clock.now_ms(),
                    sock,
                    verified,
                });
            }
            Msg::Frame { sock, text } => self.feed(Input::Frame {
                now_ms: self.clock.now_ms(),
                sock,
                text,
            }),
            Msg::Binary { sock } => self.feed(Input::Binary {
                now_ms: self.clock.now_ms(),
                sock,
            }),
            Msg::Closed { sock } => {
                self.sockets.remove(&sock);
                self.feed(Input::Closed {
                    now_ms: self.clock.now_ms(),
                    sock,
                });
            }
            Msg::Poke => self.feed(Input::TrunkPoked {
                now_ms: self.clock.now_ms(),
            }),
            Msg::Host(input) => self.feed(input),
            Msg::Unreadable(cause) => {
                tracing::error!(
                    repo = %self.id,
                    error = %cause,
                    "the store cannot be read; this repository stops serving until a restart"
                );
                return false;
            }
            Msg::Stop => return false,
        }
        true
    }

    /// Hand `input` to the host and carry out what it asks. Inputs the adapter itself causes, a
    /// send that failed, are fed after the outputs that raised them.
    fn feed(&mut self, input: Input) {
        let mut pending = std::collections::VecDeque::from([input]);
        while let Some(input) = pending.pop_front() {
            let outputs = self.host.step(input);
            for output in outputs {
                self.execute(output, &mut pending);
            }
        }
    }

    fn job(&self, job: StoreJob) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }

    fn execute(&mut self, output: Output, pending: &mut std::collections::VecDeque<Input>) {
        match output {
            Output::Load => self.job(StoreJob::Load),
            Output::Commit { batch, entries } => self.job(StoreJob::Commit { batch, entries }),
            Output::Send { sock, msg } => self.send(sock, &msg, pending),
            Output::Close { sock, code, reason } => self.close(sock, code, reason),
            Output::ReadEvents {
                sock,
                from_seq,
                limit,
            } => {
                let live = self.sockets.get(&sock).is_some_and(|entry| !entry.closing);
                self.job(StoreJob::ReadEvents {
                    sock,
                    from_seq,
                    limit,
                    live,
                });
            }
            Output::SetWake { at_ms } => self.wake_at = at_ms,
            Output::Call(call) => self.call(call),
            Output::Log { line } => tracing::info!(repo = %self.id, line = ?line, "host"),
            // The host's socket table is the only record of a session: no runtime here forgets a
            // socket the way hibernation does. Metrics are M6's.
            Output::SetSession { .. } | Output::Metric { .. } => {}
        }
    }

    fn send(
        &mut self,
        sock: SocketId,
        msg: &tessel_coordinator::protocol::ServerMsg,
        pending: &mut std::collections::VecDeque<Input>,
    ) {
        let Some(entry) = self.sockets.get(&sock) else {
            return;
        };
        let delivered = match serde_json::to_string(msg) {
            Ok(text) => entry.writer.try_send(Wire::Text(text)).is_ok(),
            Err(error) => {
                tracing::error!(repo = %self.id, error = %error, "a message does not encode");
                false
            }
        };
        if !delivered {
            pending.push_back(Input::SendFailed {
                now_ms: self.clock.now_ms(),
                sock,
            });
        }
    }

    /// Close `sock` after what is already queued for it. If its queue is full the connection is
    /// dropped instead, which ends it without a close frame.
    fn close(&mut self, sock: SocketId, code: u16, reason: String) {
        let Some(entry) = self.sockets.get_mut(&sock) else {
            return;
        };
        entry.closing = true;
        if entry.writer.try_send(Wire::Close { code, reason }).is_err() {
            self.sockets.remove(&sock);
        }
    }

    fn call(&self, call: Call) {
        let id = call.id;
        let answer = self.calls.answer(call);
        let tx = self.tx.clone();
        let clock = self.clock.clone();
        tokio::spawn(async move {
            let (input, moved) = match tokio::spawn(answer).await {
                Ok(answer) => {
                    let moved = moves_main(&answer);
                    let replied = Input::Replied {
                        now_ms: clock.now_ms(),
                        call: id,
                        answer,
                    };
                    (replied, moved)
                }
                Err(_) => (
                    Input::CallLost {
                        now_ms: clock.now_ms(),
                        call: id,
                    },
                    false,
                ),
            };
            let _ = tx.send(Msg::Host(input)).await;
            if moved {
                // What the Worker's poke carried when the lander moved main: the host reads
                // main's head, so agents hear of the new head from the host's own read.
                let _ = tx.send(Msg::Poke).await;
            }
        });
    }
}

/// Whether the call that got this answer moved main, so the host should read the head again. A
/// `Prepared` has only put the head on the candidate ref; main moves with the `Landed`.
fn moves_main(answer: &CallAnswer) -> bool {
    match answer {
        CallAnswer::Landed { .. } => true,
        CallAnswer::Prepared { .. }
        | CallAnswer::Tested
        | CallAnswer::Trialled { .. }
        | CallAnswer::Head { .. }
        | CallAnswer::Reconciled { .. }
        | CallAnswer::CandidateMissing
        | CallAnswer::Refused { .. }
        | CallAnswer::Unknown => false,
    }
}

#[cfg(test)]
mod tests {
    use tessel_coordinator::host::call::{CallId, DispatchId, Request};
    use tessel_coordinator::protocol::{ClientMsg, CommitId, PROTOCOL_VERSION};

    use super::*;

    fn commit(text: &str) -> CommitId {
        CommitId(text.to_string())
    }

    fn call_of(request: Request) -> Call {
        Call {
            id: CallId(1),
            dispatch_id: DispatchId(1),
            request,
            deadline_ms: 1,
        }
    }

    #[tokio::test]
    async fn no_lander_refuses_every_kind_of_call_with_a_typed_refusal() {
        let requests = [
            Request::Prepare {
                claim: tessel_coordinator::protocol::ClaimId(1),
                agent: AgentId("a1".to_string()),
                fork_commit: commit("f"),
                scopes: Vec::new(),
            },
            Request::Test {
                base: commit("b"),
                head: commit("h"),
            },
            Request::Land {
                base: commit("b"),
                head: commit("h"),
            },
            Request::Trial {
                agent: AgentId("a1".to_string()),
                commit: None,
                before: commit("o"),
                main: commit("n"),
            },
            Request::ReadHead {
                submitted: Vec::new(),
            },
            Request::Reconcile { head: commit("h") },
        ];
        for request in requests {
            let answer = NoLander.answer(call_of(request.clone())).await;
            assert_eq!(
                answer,
                CallAnswer::Refused {
                    refusal: Refusal::Rule {
                        rule: RuleKind::NoLander,
                        message: NO_LANDER_MESSAGE.to_string()
                    }
                },
                "{request:?}"
            );
        }
    }

    fn hello_text() -> String {
        let hello = ClientMsg::Hello {
            agent: AgentId("a1".to_string()),
            base: commit("ab"),
            protocol: PROTOCOL_VERSION,
        };
        serde_json::to_string(&hello).unwrap()
    }

    fn started(dir: &std::path::Path) -> (RunningRepo, PathBuf) {
        let file = dir.join("s.sqlite");
        let store = Store::open(&file).unwrap();
        let config = HostConfig {
            repo: "acme/demo".to_string(),
            run: Some("test".to_string()),
            ..HostConfig::default()
        };
        let id = RepoId::new("acme", "demo").unwrap();
        let running = spawn(
            id,
            store,
            file.clone(),
            config,
            Arc::new(NoLander),
            Clock::default(),
        );
        (running, file)
    }

    /// A second connection that holds the store's write lock, so a commit waits.
    fn write_lock(file: &std::path::Path) -> rusqlite::Connection {
        let blocker = rusqlite::Connection::open(file).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        blocker
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_batch_is_not_released_before_its_transaction_commits() {
        let dir = tempfile::tempdir().unwrap();
        let (running, file) = started(dir.path());
        let (sock, mut wire) = running
            .handle
            .open(AgentId("a1".to_string()))
            .await
            .unwrap();
        let blocker = write_lock(&file);
        running.handle.frame(sock, hello_text()).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            wire.try_recv().is_err(),
            "nothing leaves while the batch's transaction cannot commit"
        );
        blocker.execute_batch("COMMIT").unwrap();
        let sent = tokio::time::timeout(Duration::from_secs(5), wire.recv())
            .await
            .expect("the welcome arrives once the commit returned");
        assert!(
            matches!(&sent, Some(Wire::Text(text)) if text.contains("welcome")),
            "{sent:?}"
        );
        let events = Store::open_read_only(&file)
            .unwrap()
            .read_events(0, 10)
            .unwrap();
        assert_eq!(events.len(), 1, "the rows were there before the reply");
        running.stop().await;
    }

    /// `stop` with a storage thread stuck sending its answer into an inbox nobody reads. The
    /// runtime is shut down by timeout, so a hang ends the test instead of the process.
    #[test]
    fn stop_returns_with_a_full_inbox() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let outcome = runtime.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let (running, file) = started(dir.path());
            let (sock, _wire) = running
                .handle
                .open(AgentId("a1".to_string()))
                .await
                .unwrap();
            let blocker = write_lock(&file);
            running.handle.frame(sock, hello_text()).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            running.handle.tx.send(Msg::Stop).await.unwrap();
            while running.handle.tx.try_send(Msg::Binary { sock }).is_ok() {}
            blocker.execute_batch("COMMIT").unwrap();
            tokio::time::timeout(Duration::from_secs(5), running.task)
                .await
                .map(|_| ())
                .map_err(|_| "stop did not return with a full inbox".to_string())
        });
        runtime.shutdown_timeout(Duration::from_secs(1));
        assert_eq!(outcome, Ok(()));
    }
}

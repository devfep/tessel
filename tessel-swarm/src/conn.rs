//! One WebSocket connection to a coordinator, speaking the real protocol types.

use std::collections::hash_map::RandomState;
use std::future::Future;
use std::hash::Hasher;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    AgentId, ClientMsg, CommitId, ErrorCode, Event, RequestId, ServerMsg, PROTOCOL_VERSION,
};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::endpoint::Token;
use crate::events;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Most times one task may reopen its connection; the next end of the connection ends the task.
pub const MAX_RESETS_PER_TASK: u32 = 10;
/// The longest pause before a reconnect try, however many tries came before it.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(8);
/// Well inside the 30 s lease, so a claim held through a long wait is renewed.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(8);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The connection to the coordinator ended: it was closed, or it broke. `how` says which, with the
/// close code and reason when the coordinator sent them.
#[derive(Debug)]
pub struct Closed {
    pub how: String,
    /// Reopening the connection was tried, the tries ran out, and nobody should try again.
    pub gave_up: bool,
}

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the coordinator closed the connection ({})", self.how)
    }
}

impl std::error::Error for Closed {}

fn closed(how: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Closed {
        how: how.into(),
        gave_up: false,
    })
}

/// How a connection that ended without the agent asking for it is reopened: `tries` attempts, the
/// first within `first_delay` and each later one within twice the one before, up to eight seconds,
/// each pause a random share of that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconnect {
    pub tries: u32,
    pub first_delay: Duration,
}

impl Reconnect {
    /// Never reconnect: a connection that ends ends the agent.
    pub const OFF: Self = Self {
        tries: 0,
        first_delay: Duration::ZERO,
    };
    /// Five tries within 0.5 s, 1 s, 2 s, 4 s and 8 s.
    pub const STANDARD: Self = Self {
        tries: 5,
        first_delay: Duration::from_millis(500),
    };

    /// The longest pause before try number `attempt`, counting from 0.
    #[must_use]
    pub fn ceiling(self, attempt: u32) -> Duration {
        let doublings = 2_u32.saturating_pow(attempt);
        self.first_delay
            .saturating_mul(doublings)
            .min(MAX_RECONNECT_DELAY)
    }

    /// The pause before try number `attempt`: anywhere from none to the ceiling ("full jitter"),
    /// so that agents cut at the same instant do not all come back at the same instant.
    #[must_use]
    pub fn delay(self, attempt: u32) -> Duration {
        let random = std::hash::BuildHasher::build_hasher(&RandomState::new()).finish();
        let nanos = self.ceiling(attempt).as_nanos() * u128::from(random) / u128::from(u64::MAX);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

pub struct Conn {
    url: String,
    token: Token,
    /// The agent name and base of the last `hello`, which a reconnect repeats.
    identity: Option<(String, String)>,
    policy: Reconnect,
    reconnects: u32,
    /// Reopenings since `begin_task`.
    task_resets: u32,
    socket: Socket,
    next_req: u64,
    heartbeat_every: Duration,
    /// Requests nobody waits on (a release): their answer is not for whoever reads next. A release
    /// that succeeds gets no reply, so its entry stays; the list is bounded by the releases sent.
    unawaited: Vec<RequestId>,
}

/// Installs the TLS provider `wss://` needs. Harmless if one is already installed.
pub fn install_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

impl Conn {
    pub async fn open(url: &str, token: &Token) -> Result<Self> {
        install_tls();
        let mut request = url.into_client_request().context("bad coordinator URL")?;
        let mut bearer = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
            .context("the identity token is not a valid header value")?;
        bearer.set_sensitive(true);
        request.headers_mut().insert(AUTHORIZATION, bearer);
        let attempt =
            tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request));
        // Only the error kind is shown: a handshake error must never echo request headers.
        let (socket, _) = attempt
            .await
            .map_err(|_| anyhow!("no answer from the coordinator within {CONNECT_TIMEOUT:?}"))?
            .map_err(|e| anyhow!("cannot connect to the coordinator: {e}"))?;
        Ok(Self {
            url: url.to_string(),
            token: token.clone(),
            identity: None,
            policy: Reconnect::OFF,
            reconnects: 0,
            task_resets: 0,
            socket,
            next_req: 0,
            heartbeat_every: HEARTBEAT_EVERY,
            unawaited: Vec::new(),
        })
    }

    /// How often `recv` and `keep_alive` send a heartbeat.
    #[must_use]
    pub fn with_heartbeat(mut self, every: Duration) -> Self {
        self.heartbeat_every = every;
        self
    }

    /// Lets this connection reopen itself, as `policy` says, when it ends without being asked to.
    #[must_use]
    pub fn with_reconnect(mut self, policy: Reconnect) -> Self {
        self.policy = policy;
        self
    }

    /// Whether a connection that ended would be reopened.
    #[must_use]
    pub fn can_reconnect(&self) -> bool {
        self.policy.tries > 0 && self.identity.is_some()
    }

    /// Starts counting reopenings for a new unit of work, so that a network that never stops
    /// failing cannot hold one task forever: past [`MAX_RESETS_PER_TASK`] it does not reopen.
    pub fn begin_task(&mut self) {
        self.task_resets = 0;
    }

    /// How many times this connection has been reopened.
    #[must_use]
    pub fn reconnects(&self) -> u32 {
        self.reconnects
    }

    pub fn next_req(&mut self) -> RequestId {
        self.next_req += 1;
        RequestId(self.next_req)
    }

    pub async fn send(&mut self, msg: &ClientMsg) -> Result<()> {
        let text = serde_json::to_string(msg)?;
        self.socket
            .send(Message::text(text))
            .await
            .map_err(|e| closed(format!("while sending: {e}")))
    }

    /// Marks `req` as one whose answer nobody reads. A `StaleFence` for it is dropped, because the
    /// claim it named is already gone, which is what a release is for; any other refusal ends the
    /// next `recv` with an error, so that it is not taken for the answer to a later request.
    pub fn forget(&mut self, req: RequestId) {
        self.unawaited.push(req);
    }

    /// Runs `work` while sending a heartbeat every `heartbeat_every`, so that the claims this
    /// connection holds do not lapse during work that does not read the socket.
    pub async fn keep_alive<T>(&mut self, work: impl Future<Output = T>) -> Result<T> {
        tokio::pin!(work);
        let mut tick = tokio::time::interval(self.heartbeat_every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tokio::select! {
                out = &mut work => return Ok(out),
                _ = tick.tick() => self.beat().await?,
            }
        }
    }

    /// A heartbeat. If the connection is gone and may be reopened, reopens it and beats again, so
    /// the claims it holds are renewed at once; a claim that lapsed meanwhile is found out by the
    /// task's next request, which names its fence.
    async fn beat(&mut self) -> Result<()> {
        let Err(error) = self.send(&ClientMsg::Heartbeat).await else {
            return Ok(());
        };
        if !self.can_reconnect() {
            return Err(error);
        }
        self.reconnect(None).await?;
        self.send(&ClientMsg::Heartbeat).await
    }

    /// The next message, or `None` after `limit`. Sends a heartbeat while it waits.
    pub async fn recv(&mut self, limit: Duration) -> Result<Option<ServerMsg>> {
        let deadline = Instant::now() + limit;
        loop {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            match tokio::time::timeout(left.min(self.heartbeat_every), self.socket.next()).await {
                Err(_) => {
                    if Instant::now() < deadline {
                        self.send(&ClientMsg::Heartbeat).await?;
                    }
                }
                Ok(None) => return Err(closed("without a close frame")),
                Ok(Some(Err(e))) => return Err(closed(e.to_string())),
                Ok(Some(Ok(Message::Close(frame)))) => {
                    let how = frame.map_or_else(
                        || "close frame without a code".to_string(),
                        |f| format!("code {}: {}", u16::from(f.code), f.reason),
                    );
                    return Err(closed(how));
                }
                Ok(Some(Ok(Message::Text(text)))) => {
                    let msg =
                        serde_json::from_str(&text).context("unreadable coordinator message")?;
                    if self.is_late_refusal(&msg)? {
                        continue;
                    }
                    return Ok(Some(msg));
                }
                Ok(Some(Ok(_))) => {}
            }
        }
    }

    /// Whether `msg` refuses an unawaited request because its claim is gone: nothing to act on.
    fn is_late_refusal(&mut self, msg: &ServerMsg) -> Result<bool> {
        let ServerMsg::Error {
            req: Some(req),
            code,
            message,
        } = msg
        else {
            return Ok(false);
        };
        let Some(at) = self.unawaited.iter().position(|r| r == req) else {
            return Ok(false);
        };
        self.unawaited.swap_remove(at);
        if *code != ErrorCode::StaleFence {
            bail!("a request nobody awaited was refused ({code:?}): {message}");
        }
        Ok(true)
    }

    /// `Hello`, then waits for `Welcome`.
    pub async fn hello(&mut self, agent: &str, base: &str) -> Result<()> {
        self.identity = Some((agent.to_string(), base.to_string()));
        let hello = ClientMsg::Hello {
            agent: AgentId(agent.to_string()),
            base: CommitId(base.to_string()),
            protocol: PROTOCOL_VERSION,
        };
        self.send(&hello).await?;
        loop {
            match self.recv(CONNECT_TIMEOUT).await? {
                Some(ServerMsg::Welcome { .. }) => return Ok(()),
                Some(ServerMsg::Error { code, message, .. }) => {
                    bail!("hello for {agent} refused ({code:?}): {message}")
                }
                Some(_) => {}
                None => bail!("no Welcome for {agent} within {CONNECT_TIMEOUT:?}"),
            }
        }
    }
}

impl Conn {
    /// Replaces the ended connection with a new one: waits, opens, and repeats the `hello`, up to
    /// the policy's tries. With `watch_from`, the new connection also watches the log from that
    /// seq, and the events it replays before the `Welcome` are returned: the `Hello` is handled
    /// after the replay, so the connection's own `AgentConnected` marks the end of it, and the
    /// events after that arrive through `recv`. The coordinator withdrew any request that was
    /// queued on the old connection, so nothing is carried over but the request counter.
    ///
    /// Fails with a [`Closed`] that names the tries and the last error.
    pub async fn reconnect(&mut self, watch_from: Option<u64>) -> Result<Vec<Event>> {
        let Some((agent, base)) = self.identity.clone() else {
            return Err(closed("no hello to repeat on a new connection"));
        };
        if self.task_resets >= MAX_RESETS_PER_TASK {
            return Err(anyhow::Error::new(Closed {
                how: format!("reopened {MAX_RESETS_PER_TASK} times for this task already"),
                gave_up: true,
            }));
        }
        let mut last = anyhow!("no try was allowed");
        for attempt in 0..self.policy.tries {
            tokio::time::sleep(self.policy.delay(attempt)).await;
            match self.reopen(&agent, &base, watch_from).await {
                Ok(replayed) => {
                    self.reconnects += 1;
                    self.task_resets += 1;
                    return Ok(replayed);
                }
                Err(error) => last = error,
            }
        }
        Err(anyhow::Error::new(Closed {
            how: format!(
                "gone for good after {} reconnect tries, the last: {last:#}",
                self.policy.tries
            ),
            gave_up: true,
        }))
    }

    async fn reopen(
        &mut self,
        agent: &str,
        base: &str,
        watch_from: Option<u64>,
    ) -> Result<Vec<Event>> {
        let (fresh, replayed) = self.handshake(agent, base, watch_from).await?;
        self.socket = fresh.socket;
        self.unawaited.clear();
        Ok(replayed)
    }

    async fn handshake(
        &self,
        agent: &str,
        base: &str,
        watch_from: Option<u64>,
    ) -> Result<(Self, Vec<Event>)> {
        let mut fresh = Self::open(&self.url, &self.token)
            .await?
            .with_heartbeat(self.heartbeat_every);
        if let Some(from_seq) = watch_from {
            fresh.send(&ClientMsg::Watch { from_seq }).await?;
        }
        let hello = ClientMsg::Hello {
            agent: AgentId(agent.to_string()),
            base: CommitId(base.to_string()),
            protocol: PROTOCOL_VERSION,
        };
        fresh.send(&hello).await?;
        let replayed = fresh.read_replay(agent, watch_from.is_some()).await?;
        Ok((fresh, replayed))
    }

    /// The whole event log as of now, read on a second, short-lived connection for the same agent
    /// that closes when the read ends, so this connection does not become a watcher. The agent's
    /// other connection keeps it connected when this one closes. Fails with a [`Closed`] that gave
    /// up: the caller does not try again.
    pub async fn snapshot(&self) -> Result<Vec<Event>> {
        let Some((agent, base)) = self.identity.clone() else {
            return Err(closed("no hello to repeat on a log connection"));
        };
        match self.handshake(&agent, &base, Some(0)).await {
            Ok((_closed_on_drop, log)) => Ok(log),
            Err(error) => Err(anyhow::Error::new(Closed {
                how: format!("could not read the log after reconnecting: {error:#}"),
                gave_up: true,
            })),
        }
    }

    /// Reads up to the `Welcome` and, when a replay was asked for, up to the connection's own
    /// `AgentConnected` after it.
    async fn read_replay(&mut self, agent: &str, replay: bool) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut welcomed = false;
        loop {
            let Some(msg) = self.recv(CONNECT_TIMEOUT).await? else {
                bail!("no Welcome for {agent} within {CONNECT_TIMEOUT:?}");
            };
            match msg {
                ServerMsg::Welcome { .. } if !replay => return Ok(events),
                ServerMsg::Welcome { .. } => welcomed = true,
                ServerMsg::Event { event } => {
                    let marker = welcomed
                        && events::connected(&event.kind).is_some_and(|who| who.0 == agent);
                    events.push(event);
                    if marker {
                        return Ok(events);
                    }
                }
                ServerMsg::Error { code, message, .. } => {
                    bail!("hello for {agent} refused ({code:?}): {message}")
                }
                ServerMsg::Granted { .. }
                | ServerMsg::Denied { .. }
                | ServerMsg::Shadowed { .. }
                | ServerMsg::Queued { .. }
                | ServerMsg::Accepted { .. }
                | ServerMsg::Merged { .. }
                | ServerMsg::SubmitRejected { .. }
                | ServerMsg::Uncovered { .. }
                | ServerMsg::ReviewRequired { .. }
                | ServerMsg::BaseMoved { .. }
                | ServerMsg::AssumptionChallenged { .. }
                | ServerMsg::LeaseExpired { .. }
                | ServerMsg::RaceOpened { .. }
                | ServerMsg::RaceResult { .. } => {}
            }
        }
    }
}

/// Reads the whole event log on a fresh connection as `observer`. `Watch { from_seq: 0 }` replays
/// the log and then follows it live, so the read ends at the first `AgentConnected` for the
/// observer after the `Welcome`: the `Hello` is handled after the replay, so that event comes
/// after every stored event. A gap in `seq` or a closed socket fails the read.
pub async fn read_log(
    url: &str,
    token: &Token,
    observer: &str,
    limit: Duration,
) -> Result<Vec<Event>> {
    let mut conn = Conn::open(url, token).await?;
    conn.send(&ClientMsg::Watch { from_seq: 0 }).await?;
    let hello = ClientMsg::Hello {
        agent: AgentId(observer.to_string()),
        base: CommitId("observer".into()),
        protocol: PROTOCOL_VERSION,
    };
    conn.send(&hello).await?;
    let deadline = Instant::now() + limit;
    let mut events = Vec::new();
    let mut welcomed = false;
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow!("the event log read did not finish within {limit:?}"))?;
        let Some(msg) = conn.recv(left).await? else {
            bail!("the event log read did not finish within {limit:?}");
        };
        match msg {
            ServerMsg::Welcome { .. } => welcomed = true,
            ServerMsg::Event { event } => {
                let marker = welcomed
                    && events::connected(&event.kind).is_some_and(|agent| agent.0 == observer);
                events.push(event);
                if marker {
                    break;
                }
            }
            ServerMsg::Error { code, message, .. } => {
                bail!("event log read refused ({code:?}): {message}")
            }
            ServerMsg::Granted { .. }
            | ServerMsg::Denied { .. }
            | ServerMsg::Shadowed { .. }
            | ServerMsg::Queued { .. }
            | ServerMsg::Accepted { .. }
            | ServerMsg::Merged { .. }
            | ServerMsg::SubmitRejected { .. }
            | ServerMsg::Uncovered { .. }
            | ServerMsg::ReviewRequired { .. }
            | ServerMsg::BaseMoved { .. }
            | ServerMsg::AssumptionChallenged { .. }
            | ServerMsg::LeaseExpired { .. }
            | ServerMsg::RaceOpened { .. }
            | ServerMsg::RaceResult { .. } => {}
        }
    }
    for (index, event) in events.iter().enumerate() {
        if event.seq != index as u64 {
            bail!(
                "the event log has a gap: position {index} holds seq {}",
                event.seq
            );
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A coordinator that answers every `Hello` with a `Welcome`, and closes the first connection
    /// it is given shortly after, so that the second one is a reconnect.
    async fn dropping_coordinator() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut first = true;
            while let Ok((stream, _)) = listener.accept().await {
                let drop_it = std::mem::take(&mut first);
                tokio::spawn(answer_hellos(stream, drop_it));
            }
        });
        url
    }

    async fn answer_hellos(stream: TcpStream, drop_after_welcome: bool) {
        let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            if !matches!(serde_json::from_str(&text), Ok(ClientMsg::Hello { .. })) {
                continue;
            }
            let welcome = ServerMsg::Welcome {
                head: CommitId("h".into()),
                lease_ms: 30_000,
                protocol: PROTOCOL_VERSION,
            };
            let text = serde_json::to_string(&welcome).unwrap();
            socket.send(Message::text(text)).await.unwrap();
            if drop_after_welcome {
                tokio::time::sleep(Duration::from_millis(60)).await;
                return;
            }
        }
    }

    #[tokio::test]
    async fn work_that_outlives_its_connection_goes_on_over_a_reopened_one() {
        let url = dropping_coordinator().await;
        let token = Token::new("t".into());
        let mut conn = Conn::open(&url, &token)
            .await
            .unwrap()
            .with_heartbeat(Duration::from_millis(20))
            .with_reconnect(Reconnect {
                tries: 3,
                first_delay: Duration::from_millis(5),
            });
        conn.hello("a01", "base").await.unwrap();
        let work = tokio::time::sleep(Duration::from_millis(600));
        conn.keep_alive(work).await.unwrap();
        assert_eq!(conn.reconnects(), 1);
    }

    #[tokio::test]
    async fn a_connection_that_may_not_reconnect_fails_its_work_when_the_socket_ends() {
        let url = dropping_coordinator().await;
        let token = Token::new("t".into());
        let mut conn = Conn::open(&url, &token)
            .await
            .unwrap()
            .with_heartbeat(Duration::from_millis(20));
        conn.hello("a01", "base").await.unwrap();
        let work = tokio::time::sleep(Duration::from_millis(600));
        let error = conn.keep_alive(work).await.unwrap_err();
        assert!(error.downcast_ref::<Closed>().is_some(), "{error:#}");
        assert_eq!(conn.reconnects(), 0);
    }
}

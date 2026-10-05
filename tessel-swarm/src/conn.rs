//! One WebSocket connection to a coordinator, speaking the real protocol types.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    AgentId, ClientMsg, CommitId, Event, EventKind, RequestId, ServerMsg, PROTOCOL_VERSION,
};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::endpoint::Token;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Well inside the 30 s lease, so a claim held through a long wait is renewed.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(8);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Conn {
    socket: Socket,
    next_req: u64,
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
            socket,
            next_req: 0,
        })
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
            .context("the coordinator closed the connection while sending")
    }

    /// The next message, or `None` after `limit`. Sends a heartbeat while it waits.
    pub async fn recv(&mut self, limit: Duration) -> Result<Option<ServerMsg>> {
        let deadline = Instant::now() + limit;
        loop {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            match tokio::time::timeout(left.min(HEARTBEAT_EVERY), self.socket.next()).await {
                Err(_) => {
                    if Instant::now() < deadline {
                        self.send(&ClientMsg::Heartbeat).await?;
                    }
                }
                Ok(None | Some(Err(_))) => bail!("the coordinator closed the connection"),
                Ok(Some(Ok(Message::Text(text)))) => {
                    let msg =
                        serde_json::from_str(&text).context("unreadable coordinator message")?;
                    return Ok(Some(msg));
                }
                Ok(Some(Ok(_))) => {}
            }
        }
    }

    /// `Hello`, then waits for `Welcome`.
    pub async fn hello(&mut self, agent: &str, base: &str) -> Result<()> {
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
                    && matches!(&event.kind, EventKind::AgentConnected { agent } if agent.0 == observer);
                events.push(event);
                if marker {
                    break;
                }
            }
            ServerMsg::Error { code, message, .. } => {
                bail!("event log read refused ({code:?}): {message}")
            }
            _ => {}
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

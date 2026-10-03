//! Tessel coordinator. Day-one toolchain check: a Rust Worker that routes WebSocket connections
//! to one coordinator Durable Object per repo, and round-trips protocol JSON.

mod protocol;

use protocol::{ClientMsg, CommitId, ErrorCode, ServerMsg, PROTOCOL_VERSION};
use worker::*;

/// Route: GET /repo/<name>/ws  (WebSocket upgrade) -> coordinator for <name>.
#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let url = req.url()?;
    let segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
    let repo = match segments.as_slice() {
        ["repo", name, "ws"] if !name.is_empty() => name.to_string(),
        _ => return Response::error("expected /repo/<name>/ws", 404),
    };

    let stub = env
        .durable_object("COORDINATOR")?
        .id_from_name(&repo)?
        .get_stub()?;
    stub.fetch_with_request(req).await
}

#[durable_object]
pub struct Coordinator {
    state: State,
    _env: Env,
}

impl DurableObject for Coordinator {
    fn new(state: State, env: Env) -> Self {
        Self { state, _env: env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        if req.headers().get("Upgrade")?.as_deref() != Some("websocket") {
            return Response::error("expected WebSocket upgrade", 426);
        }
        let pair = WebSocketPair::new()?;
        // Hibernation API: the runtime holds the socket, so an idle
        // coordinator costs nothing while agents stay connected.
        self.state.accept_web_socket(&pair.server);
        Response::from_websocket(pair.client)
    }

    async fn websocket_message(
        &self,
        ws: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> Result<()> {
        let reply = match message {
            WebSocketIncomingMessage::String(text) => handle(&text),
            WebSocketIncomingMessage::Binary(_) => error(ErrorCode::Malformed, "binary frames not supported"),
        };
        ws.send_with_str(serde_json::to_string(&reply)?)
    }

    async fn websocket_close(
        &self,
        _ws: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }
}

/// Toolchain check only: answers Hello, rejects everything else.
/// The real claim index, wait queue and leases replace this next.
fn handle(text: &str) -> ServerMsg {
    match serde_json::from_str::<ClientMsg>(text) {
        Ok(ClientMsg::Hello { protocol, .. }) if protocol != PROTOCOL_VERSION => error(
            ErrorCode::UnsupportedProtocol,
            &format!("client speaks v{protocol}, coordinator speaks v{PROTOCOL_VERSION}"),
        ),
        Ok(ClientMsg::Hello { base, .. }) => ServerMsg::Welcome {
            head: CommitId(base.0),
            lease_ms: 30_000,
            protocol: PROTOCOL_VERSION,
        },
        Ok(other) => error(
            ErrorCode::Malformed,
            &format!("not implemented yet: {other:?}"),
        ),
        Err(e) => error(ErrorCode::Malformed, &e.to_string()),
    }
}

fn error(code: ErrorCode, message: &str) -> ServerMsg {
    ServerMsg::Error { req: None, code, message: message.to_string() }
}

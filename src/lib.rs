//! Tessel coordinator Worker: routes WebSocket connections to one Durable Object per repo. The
//! Durable Object is a thin shell around the pure core in `coordinator`: it loads and stores
//! the core, binds sockets to agents, delivers messages and sets the lease alarm. Decisions that
//! need no runtime live in `shell`; storage access lives in `store`.
//!
//! Persist before send (CLAUDE.md rule 6): `apply` runs the core and returns an `Applied`;
//! `persist` stores its state and events in one transaction and returns a `store::Persisted`;
//! `deliver` and `settle` require that token, so sending before the write does not type-check.
//! If the write fails the cached core is dropped, because it is ahead of storage, and the next
//! call reloads from storage. So is a core whose state or event is too large to store.
//!
//! Authentication: the Worker serves `/repo/<name>/ws` only to a request that carries
//! `Authorization: Bearer <COORDINATOR_TOKEN>`; anything else is refused before the Durable
//! Object is reached. `agent` in `hello` is then the only identity the core trusts.

mod coordinator;
mod protocol;
mod shell;
mod store;

use std::cell::RefCell;
use std::fmt::Display;

use coordinator::{Coordinator as Core, Effect};
use protocol::{AgentId, ClientMsg, ServerMsg};
use shell::EntriesError;
use shell::{keep_first, Action, ReplayStep, Session, Target};
use store::{Applied, Persisted};
use worker::*;

/// The fixed body of the refusal of a request without the coordinator token.
const UNAUTHORIZED_BODY: &str = "unauthorized";

/// What `apply` made of a call: something to store and send, or a refusal for the sender.
enum Prepared {
    Ready(Applied),
    Refused(ServerMsg),
}

/// Route: GET /repo/<name>/ws  (WebSocket upgrade) -> coordinator for <name>, for a request
/// that carries the coordinator token.
#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let url = req.url()?;
    let segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
    let repo = match segments.as_slice() {
        ["repo", name, "ws"] if !name.is_empty() => name.to_string(),
        _ => return Response::error("expected /repo/<name>/ws", 404),
    };
    if !authorized(&req, &env)? {
        return Response::error(UNAUTHORIZED_BODY, 401);
    }

    let stub = env
        .durable_object("COORDINATOR")?
        .id_from_name(&repo)?
        .get_stub()?;
    stub.fetch_with_request(req).await
}

/// Whether the request carries the `COORDINATOR_TOKEN` secret. A missing secret refuses every
/// request. The token is never logged or echoed.
fn authorized(req: &Request, env: &Env) -> Result<bool> {
    let expected = env
        .secret("COORDINATOR_TOKEN")
        .ok()
        .map(|secret| secret.to_string());
    let presented = req.headers().get("Authorization")?;
    Ok(shell::is_authorized(
        expected.as_deref(),
        presented.as_deref(),
    ))
}

#[durable_object]
pub struct Coordinator {
    state: State,
    env: Env,
    /// Loaded on first use, and again after a failed write or hibernation.
    core: RefCell<Option<Core>>,
}

impl DurableObject for Coordinator {
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            core: RefCell::new(None),
        }
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
        let result = self.dispatch(&ws, message).await;
        if let Err(e) = &result {
            console_error!("coordinator {}: closing socket: {e}", self.repo());
            self.close_socket(&ws, "coordinator error");
        }
        result
    }

    async fn websocket_close(
        &self,
        ws: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        self.withdraw(&ws).await
    }

    async fn websocket_error(&self, ws: WebSocket, _error: Error) -> Result<()> {
        self.withdraw(&ws).await
    }

    async fn alarm(&self) -> Result<Response> {
        self.ensure_loaded().await?;
        let now_ms = now_ms();
        let prepared = self.apply(|core| core.expire(now_ms))?;
        let applied = self.ready(prepared, "expire leases")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await?;
        Response::ok("")
    }
}

impl Coordinator {
    fn repo(&self) -> String {
        self.state
            .id()
            .name()
            .unwrap_or_else(|| "<unnamed>".to_string())
    }

    fn fail(&self, operation: &str, cause: impl Display) -> Error {
        Error::RustError(format!(
            "coordinator {}: {operation} failed: {cause}",
            self.repo()
        ))
    }

    fn close_socket(&self, ws: &WebSocket, reason: &str) {
        if let Err(e) = ws.close(Some(1011), Some(reason)) {
            console_error!("coordinator {}: close failed: {e}", self.repo());
        }
    }

    async fn dispatch(&self, ws: &WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        let text = match message {
            WebSocketIncomingMessage::String(text) => text,
            WebSocketIncomingMessage::Binary(_) => return send(ws, &shell::binary_rejection()),
        };
        let msg = match shell::parse_client_msg(&text) {
            Ok(msg) => msg,
            Err(fault) => return send(ws, &fault.reply()),
        };
        let session = self.read_session(ws)?;
        match shell::decide(&session, &msg) {
            Action::Reject(reply) => send(ws, &reply),
            Action::Watch { from_seq } => self.watch(ws, session, from_seq).await,
            Action::Call { agent } => self.call(ws, &session, agent, msg).await,
        }
    }

    /// Load the core from storage, or create it for the configured run. Fails loudly on corrupt
    /// state or a bad config; never starts empty over stored data. `RUN` and `SHADOW_ENABLED`
    /// matter only when nothing is stored yet.
    async fn ensure_loaded(&self) -> Result<()> {
        if self.core.borrow().is_some() {
            return Ok(());
        }
        let stored: Option<String> = self
            .state
            .storage()
            .get(shell::STATE_KEY)
            .await
            .map_err(|e| self.fail("load state", e))?;
        let run = self.env.var("RUN").ok().map(|var| var.to_string());
        let shadow = self
            .env
            .var("SHADOW_ENABLED")
            .ok()
            .map(|var| var.to_string());
        let core = shell::load_core(stored.as_deref(), run.as_deref(), shadow.as_deref())
            .map_err(|e| self.fail("load state", e))?;
        let mut slot = self.core.borrow_mut();
        if slot.is_none() {
            *slot = Some(core);
        }
        Ok(())
    }

    /// Run `step` on the core and serialize the result. Nothing is stored or sent yet. If
    /// serialization fails the core is dropped, because it has moved on from storage. If the
    /// state or an event is too large to store, the core is dropped too and the call is refused.
    fn apply(&self, step: impl FnOnce(&mut Core) -> Vec<Effect>) -> Result<Prepared> {
        let mut slot = self.core.borrow_mut();
        let Some(core) = slot.as_mut() else {
            return Err(self.fail("apply", "core is not loaded"));
        };
        let effects = step(core);
        let next_expiry_ms = core.next_expiry_ms();
        let (events, outbound) = shell::split_effects(effects);
        match shell::persist_entries(core, &events) {
            Ok(entries) => Ok(Prepared::Ready(Applied {
                entries,
                events,
                outbound,
                next_expiry_ms,
            })),
            Err(EntriesError::TooLarge) => {
                *slot = None;
                console_error!(
                    "coordinator {}: {}",
                    self.repo(),
                    shell::STATE_LIMIT_MESSAGE
                );
                Ok(Prepared::Refused(shell::state_limit_reply()))
            }
            Err(e @ EntriesError::Serialize(_)) => {
                *slot = None;
                Err(self.fail("serialize state", e))
            }
        }
    }

    /// The `Applied` of a call that has no sender to refuse (the alarm, a closing socket): a
    /// refusal is an error to log.
    fn ready(&self, prepared: Prepared, operation: &str) -> Result<Applied> {
        match prepared {
            Prepared::Ready(applied) => Ok(applied),
            Prepared::Refused(_) => Err(self.fail(operation, shell::STATE_LIMIT_MESSAGE)),
        }
    }

    /// A socket closed or failed. If it was bound and no other open socket is bound to the same
    /// agent, withdraw the agent's queued request: nobody is left to receive its grant.
    async fn withdraw(&self, closing: &WebSocket) -> Result<()> {
        let session = self.read_session(closing)?;
        let open: Vec<WebSocket> = self
            .state
            .get_websockets()
            .into_iter()
            .filter(|socket| socket != closing)
            .collect();
        let others = self.read_sessions(&open);
        let Some(agent) = shell::agent_to_withdraw(&session, &others) else {
            return Ok(());
        };
        self.ensure_loaded().await?;
        let now_ms = now_ms();
        let prepared = self.apply(|core| core.disconnect(&agent, now_ms))?;
        let applied = self.ready(prepared, "withdraw queued request")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await
    }

    /// Store the call's state and events in one transaction. On failure the cached core is
    /// dropped and the caller must send nothing: there is no `Persisted` to send with.
    async fn persist(&self, applied: Applied) -> Result<Persisted> {
        let written = store::write(&self.state.storage(), applied).await;
        match written {
            Ok(persisted) => Ok(persisted),
            Err(e) => {
                *self.core.borrow_mut() = None;
                Err(self.fail("persist state and events", e))
            }
        }
    }

    async fn call(
        &self,
        ws: &WebSocket,
        session: &Session,
        agent: AgentId,
        msg: ClientMsg,
    ) -> Result<()> {
        self.ensure_loaded().await?;
        let prepared = self.apply(|core| core.handle(&agent, msg, now_ms()))?;
        let applied = match prepared {
            Prepared::Ready(applied) => applied,
            Prepared::Refused(reply) => return send(ws, &reply),
        };
        let persisted = self.persist(applied).await?;
        let bound = self.bind(ws, session, &agent, &persisted);
        let settled = self.settle(&persisted, Some(ws)).await;
        bound?;
        settled
    }

    /// Bind the socket to `agent` if the core welcomed it. Runs after the persist.
    fn bind(
        &self,
        ws: &WebSocket,
        session: &Session,
        agent: &AgentId,
        persisted: &Persisted,
    ) -> Result<()> {
        let Some(bound) = shell::bind_on_welcome(session, agent, persisted.outbound()) else {
            return Ok(());
        };
        self.write_session(ws, &bound)
    }

    /// Deliver a persisted call, then reschedule the alarm even if the delivery failed. Returns
    /// the first error.
    async fn settle(&self, persisted: &Persisted, reply_to: Option<&WebSocket>) -> Result<()> {
        let delivered = self.deliver(persisted, reply_to);
        let rescheduled = self.reschedule(persisted.next_expiry_ms()).await;
        delivered?;
        rescheduled
    }

    /// Send what `shell::plan_delivery` plans. Only a failed send to the socket that sent the
    /// message is an error. A failed send to any other socket closes that socket and delivery
    /// goes on, so one dead watcher cannot close the sender or make an alarm retry.
    fn deliver(&self, persisted: &Persisted, reply_to: Option<&WebSocket>) -> Result<()> {
        let sockets = if shell::needs_sessions(persisted.outbound(), persisted.events()) {
            self.state.get_websockets()
        } else {
            Vec::new()
        };
        let sessions = self.read_sessions(&sockets);
        let mut sender_error = None;
        for (target, msg) in
            shell::plan_delivery(persisted.outbound(), persisted.events(), &sessions)
        {
            match target {
                Target::Sender => {
                    let sent = match reply_to {
                        Some(ws) => send(ws, &msg),
                        None => Err(self.fail("deliver", "a reply has no sender socket")),
                    };
                    keep_first(&mut sender_error, sent);
                }
                Target::Socket(index) => {
                    let Some(ws) = sockets.get(index) else {
                        let missing = self.fail("deliver", "plan names a socket that is not open");
                        keep_first(&mut sender_error, Err(missing));
                        continue;
                    };
                    if let Err(e) = send(ws, &msg) {
                        console_error!("coordinator {}: send to a socket failed: {e}", self.repo());
                        self.close_socket(ws, "send failed");
                    }
                }
            }
        }
        sender_error.map_or(Ok(()), Err)
    }

    /// The session of each open socket. A socket whose attachment cannot be read is closed and
    /// counts as unbound, so it receives nothing.
    fn read_sessions(&self, sockets: &[WebSocket]) -> Vec<Session> {
        let mut sessions = Vec::with_capacity(sockets.len());
        for socket in sockets {
            match self.read_session(socket) {
                Ok(session) => sessions.push(session),
                Err(e) => {
                    console_error!("coordinator {}: {e}", self.repo());
                    self.close_socket(socket, "unreadable session");
                    sessions.push(Session::default());
                }
            }
        }
        sessions
    }

    async fn reschedule(&self, next_expiry_ms: Option<u64>) -> Result<()> {
        let storage = self.state.storage();
        let result = match shell::alarm_at_ms(next_expiry_ms) {
            Some(at_ms) => {
                let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at_ms));
                storage.set_alarm(ScheduledTime::new(date)).await
            }
            None => storage.delete_alarm().await,
        };
        result.map_err(|e| self.fail("set alarm", e))
    }

    /// Replay the stored events with `seq >= from_seq` a page at a time, then mark the socket a
    /// watcher.
    ///
    /// An event is either in the replay or sent live, never both and never neither, because of
    /// the Durable Object's input gates: while a storage operation is in flight no other event
    /// (message or alarm) is delivered, and `deliver` runs in the same turn that its write
    /// completes. Every await in this function is a storage read for that reason. Do not add
    /// an await of any other kind (a `fetch`, a timer) between the first read and the mark.
    async fn watch(&self, ws: &WebSocket, mut session: Session, from_seq: u64) -> Result<()> {
        let storage = self.state.storage();
        let mut start_seq = from_seq;
        loop {
            let page = store::read_events(&storage, start_seq, shell::REPLAY_PAGE)
                .await
                .map_err(|e| self.fail("read events", e))?;
            let last_seq = page.last().map(|event| event.seq);
            let page_len = page.len();
            for event in page {
                send(ws, &ServerMsg::Event { event })?;
            }
            match shell::after_page(page_len, last_seq) {
                ReplayStep::Next { start_seq: next } => start_seq = next,
                ReplayStep::Done => break,
            }
        }
        session.watch_from = Some(from_seq);
        self.write_session(ws, &session)
    }

    fn read_session(&self, ws: &WebSocket) -> Result<Session> {
        let attached = ws
            .deserialize_attachment::<Session>()
            .map_err(|e| self.fail("read socket session", e))?;
        Ok(attached.unwrap_or_default())
    }

    fn write_session(&self, ws: &WebSocket, session: &Session) -> Result<()> {
        ws.serialize_attachment(session)
            .map_err(|e| self.fail("write socket session", e))
    }
}

fn now_ms() -> u64 {
    Date::now().as_millis()
}

fn send(ws: &WebSocket, msg: &ServerMsg) -> Result<()> {
    ws.send_with_str(serde_json::to_string(msg)?)
}

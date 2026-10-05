//! Tessel coordinator Worker: routes WebSocket connections to one Durable Object per repo. The
//! Durable Object is a thin shell around the pure core in `coordinator`: it loads and stores
//! the core, binds sockets to agents, delivers messages and sets the lease alarm. Decisions that
//! need no runtime live in `shell`.
//!
//! Persist before send (CLAUDE.md rule 6): `apply` runs the core and returns an `Applied`; the
//! only way to deliver from it is after `persist` has stored its state and events in one
//! transaction. If the write fails the cached core is dropped, because it is ahead of storage,
//! and the next call reloads from storage.

mod coordinator;
mod protocol;
mod shell;

use std::cell::RefCell;
use std::fmt::Display;

use coordinator::{Coordinator as Core, Effect};
use protocol::{AgentId, ClientMsg, Event, ServerMsg};
use shell::{Action, Outbound, Session};
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

/// What one call into the core produced, ready to store and then send.
struct Applied {
    /// (key, JSON) pairs for one transaction: the core state, then each event.
    entries: Vec<(String, String)>,
    events: Vec<Event>,
    outbound: Vec<Outbound>,
    next_expiry_ms: Option<u64>,
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
            if let Err(close_error) = ws.close(Some(1011), Some("coordinator error")) {
                console_error!("coordinator {}: close failed: {close_error}", self.repo());
            }
        }
        result
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

    async fn alarm(&self) -> Result<Response> {
        self.ensure_loaded().await?;
        let now_ms = now_ms();
        let applied = self.apply(|core| core.expire(now_ms))?;
        self.persist(&applied).await?;
        self.settle(None, &applied, now_ms).await?;
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

    async fn dispatch(&self, ws: &WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        let text = match message {
            WebSocketIncomingMessage::String(text) => text,
            WebSocketIncomingMessage::Binary(_) => return send(ws, &shell::binary_rejection()),
        };
        let Some(msg) = shell::parse_client_msg(&text) else {
            return send(ws, &shell::malformed_reply());
        };
        let session = self.read_session(ws)?;
        match shell::decide(&session, &msg) {
            Action::Reject(reply) => send(ws, &reply),
            Action::Watch { from_seq } => self.watch(ws, session, from_seq).await,
            Action::Call { agent } => self.call(ws, &session, agent, msg).await,
        }
    }

    /// Load the core from storage, or create it for the configured run. Fails loudly on corrupt
    /// state or a bad config; never starts empty over stored data.
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
        let core = shell::load_core(stored.as_deref(), run.as_deref())
            .map_err(|e| self.fail("load state", e))?;
        let mut slot = self.core.borrow_mut();
        if slot.is_none() {
            *slot = Some(core);
        }
        Ok(())
    }

    /// Run `step` on the core and serialize the result. Nothing is stored or sent yet. If
    /// serialization fails the core is dropped, because it has moved on from storage.
    fn apply(&self, step: impl FnOnce(&mut Core) -> Vec<Effect>) -> Result<Applied> {
        let mut slot = self.core.borrow_mut();
        let Some(core) = slot.as_mut() else {
            return Err(self.fail("apply", "core is not loaded"));
        };
        let effects = step(core);
        let next_expiry_ms = core.next_expiry_ms();
        let (events, outbound) = shell::split_effects(effects);
        match shell::persist_entries(core, &events) {
            Ok(entries) => Ok(Applied {
                entries,
                events,
                outbound,
                next_expiry_ms,
            }),
            Err(e) => {
                *slot = None;
                Err(self.fail("serialize state", e))
            }
        }
    }

    /// Store the call's state and events in one transaction. On failure the cached core is
    /// dropped and the caller must send nothing.
    async fn persist(&self, applied: &Applied) -> Result<()> {
        let entries = applied.entries.clone();
        let written = self
            .state
            .storage()
            .transaction(move |txn| async move {
                for (key, json) in entries {
                    txn.put(&key, json).await?;
                }
                Ok(())
            })
            .await;
        if let Err(e) = written {
            *self.core.borrow_mut() = None;
            return Err(self.fail("persist state and events", e));
        }
        Ok(())
    }

    async fn call(
        &self,
        ws: &WebSocket,
        session: &Session,
        agent: AgentId,
        msg: ClientMsg,
    ) -> Result<()> {
        self.ensure_loaded().await?;
        let now_ms = now_ms();
        let applied = self.apply(|core| core.handle(&agent, msg, now_ms))?;
        self.persist(&applied).await?;
        let bound = self.bind(ws, session, &agent, &applied);
        let settled = self.settle(Some(ws), &applied, now_ms).await;
        bound?;
        settled
    }

    /// Bind the socket to `agent` if the core welcomed it. Runs after the persist.
    fn bind(
        &self,
        ws: &WebSocket,
        session: &Session,
        agent: &AgentId,
        applied: &Applied,
    ) -> Result<()> {
        let Some(bound) = shell::bind_on_welcome(session, agent, &applied.outbound) else {
            return Ok(());
        };
        self.write_session(ws, &bound)
    }

    /// Deliver a persisted call, then reschedule the alarm even if a send failed. Returns the
    /// first error.
    async fn settle(
        &self,
        reply_to: Option<&WebSocket>,
        applied: &Applied,
        now_ms: u64,
    ) -> Result<()> {
        let delivered = self.deliver(reply_to, applied);
        let rescheduled = self.reschedule(applied.next_expiry_ms, now_ms).await;
        delivered?;
        rescheduled
    }

    /// Send replies to `reply_to`, notifications to the sockets bound to the named agent (none
    /// open means dropped), and every new event to the watchers. Tries every send.
    fn deliver(&self, reply_to: Option<&WebSocket>, applied: &Applied) -> Result<()> {
        let sockets = self.state.get_websockets();
        let mut sessions = Vec::with_capacity(sockets.len());
        for socket in &sockets {
            sessions.push(self.read_session(socket)?);
        }
        let mut first_error = None;
        for item in &applied.outbound {
            match item {
                Outbound::Reply(msg) => {
                    if let Some(ws) = reply_to {
                        keep_first(&mut first_error, send(ws, msg));
                    }
                }
                Outbound::Notify { agent, msg } => {
                    for index in shell::bound_indexes(agent, &sessions) {
                        keep_first(&mut first_error, send(&sockets[index], msg));
                    }
                }
            }
        }
        for event in &applied.events {
            let msg = ServerMsg::Event {
                event: event.clone(),
            };
            for index in shell::watcher_indexes(&sessions) {
                keep_first(&mut first_error, send(&sockets[index], &msg));
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn reschedule(&self, next_expiry_ms: Option<u64>, now_ms: u64) -> Result<()> {
        let storage = self.state.storage();
        let result = match shell::alarm_offset_ms(next_expiry_ms, now_ms) {
            Some(offset_ms) => storage.set_alarm(offset_ms).await,
            None => storage.delete_alarm().await,
        };
        result.map_err(|e| self.fail("set alarm", e))
    }

    /// Replay every stored event with `seq >= from_seq`, then mark the socket a watcher. The
    /// replay and the marking have no yielding await between the storage read and the mark, and
    /// a call's events are sent in the same turn that its write completes, so an event is
    /// either in the replay or sent live, never both and never neither.
    async fn watch(&self, ws: &WebSocket, mut session: Session, from_seq: u64) -> Result<()> {
        let first_key = shell::event_key(from_seq);
        let options = ListOptions::new()
            .prefix(shell::EVENT_PREFIX)
            .start(&first_key);
        let stored = self
            .state
            .storage()
            .list_with_options(options)
            .await
            .map_err(|e| self.fail("read events", e))?;
        let mut first_error = None;
        for entry in stored.values() {
            let json = entry
                .map_err(|e| self.fail("read events", format!("{e:?}")))?
                .as_string()
                .ok_or_else(|| self.fail("read events", "stored event is not a string"))?;
            let event: Event =
                serde_json::from_str(&json).map_err(|e| self.fail("parse stored event", e))?;
            keep_first(&mut first_error, send(ws, &ServerMsg::Event { event }));
        }
        session.watcher = true;
        self.write_session(ws, &session)?;
        first_error.map_or(Ok(()), Err)
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

fn keep_first(slot: &mut Option<Error>, result: Result<()>) {
    if let Err(e) = result {
        if slot.is_none() {
            *slot = Some(e);
        }
    }
}

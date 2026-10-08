//! Tessel coordinator Worker: routes WebSocket connections to one Durable Object per repo. The
//! Durable Object is a thin shell around the pure core in `coordinator`: it loads and stores
//! the core, binds sockets to agents, delivers messages and sets the lease alarm. Decisions that
//! need no runtime live in `shell`; storage access lives in `store`.
//!
//! Merges: a submission that passed the core's checks is merged by the steward, reached through the
//! `STEWARD` service binding. The alarm drives it: `drive_merges` stores the in-flight marker, then
//! calls the steward with the dispatch the stored call carries, then stores the answer. The marker
//! is stored first, so a restart re-dispatches the same claim instead of starting a second merge.
//!
//! Verifications: after a merge that challenged assumptions, `run_one_verification` asks the
//! steward's `/trial` to test each assuming agent's work on the new main, the same way: the marker
//! is stored first, the answer is stored before anyone is told, and it runs only when no merge is
//! due. A trial never merges or pushes.
//!
//! Persist before send (CLAUDE.md rule 6): `apply` runs the core and returns an `Applied`;
//! `persist` stores its state and events in one transaction and returns a `store::Persisted`;
//! `deliver` and `settle` require that token, so sending before the write does not type-check.
//! If the write fails the cached core is dropped, because it is ahead of storage, and the next
//! call reloads from storage. So is a core whose state or event is too large to store.
//!
//! Authentication: the Worker serves `/repo/<name>/ws` only to a request that carries
//! `Authorization: Bearer <token>`, a token the steward signed for this repo and one agent (format
//! in `identity`); anything else is refused before the Durable Object is reached. The Worker
//! hands the verified agent to the Durable Object in a header it sets itself, the Durable Object
//! stores it in the socket's attachment, and a `hello` for any other agent is refused with
//! `NotOwner` and the socket is closed.
//!
//! Summary: `GET /repo/<name>/summary` answers `{"summary": {...}, "head_seq": n}`, the counters of
//! `Summary::from_events` over the repo's whole stored log and the `seq` of its last event
//! (`null` for an empty log). It takes the same identity token as `ws`, and any agent of the repo
//! may read it; the refusals are the same (401 for every token fault, 403 for a repo the
//! deployment does not serve), plus 405 for any method but `GET`. It is a plain read of stored
//! events: no socket, no event appended, no alarm, and it never waits for a merge.
//!
//! Trunk moves: `POST /repo/<name>/trunk-moved` (the steward's identity token only) is the
//! steward's poke that main moved without the coordinator, as an admin merge does. It carries
//! nothing the Durable Object reads: it is counted (a repo with no stored state is ignored, 204),
//! and the alarm reads the trunk head from the steward's `/head` (`run_head_sync`), applied by
//! `Core::trunk_read` as a compare-and-set.

use std::cell::{Cell, RefCell};
use std::fmt::Display;
use std::future::{poll_fn, Future};
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use crate::coordinator::{
    head_request_body, parse_head_response, Coordinator as Core, Effect, MergeDispatch,
    VerifyDispatch,
};
use crate::merge::{self, MergeOutcome, TrialOutcome, TrialReport};
use crate::protocol::{AgentId, ClaimId, ClientMsg, CommitId, ServerMsg};
use crate::shell::{
    self, keep_first, Action, Denied, Inbound, PokeReply, ReplayStep, Route, Session,
    StoreDecision, StoredSize, SummaryTally, Target, Work,
};
use crate::store::{self, Applied, Dispatch, Persisted};
use worker::*;

/// WebSocket close code for a policy violation.
const CLOSE_POLICY_VIOLATION: u16 = 1008;

/// The fixed body of the refusal of a request without a valid identity token.
const UNAUTHORIZED_BODY: &str = "unauthorized";

/// The header the Worker uses to tell the Durable Object which agent the token proved. The
/// Worker overwrites it on every request, and the Durable Object is reachable only through the
/// Worker, so a caller cannot choose its value.
const VERIFIED_AGENT_HEADER: &str = "X-Tessel-Verified-Agent";

/// The service binding to the steward's `MergeService` entrypoint (wrangler.toml).
const STEWARD_BINDING: &str = "STEWARD";

/// The URL the steward is asked at. A service binding ignores the host; only the path and method
/// matter to `MergeService`.
const STEWARD_MERGE_URL: &str = "https://steward.internal/merge";

/// The URL of the steward's trial: test a fork's commit on main without merging it.
const STEWARD_TRIAL_URL: &str = "https://steward.internal/trial";

/// The URL of the steward's `/head`: read the head of the trunk's main.
const STEWARD_HEAD_URL: &str = "https://steward.internal/head";

/// How long the coordinator waits for the steward to read a head: one ref, not a test run.
const HEAD_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the coordinator waits for the steward (see `merge::STEWARD_CALL_TIMEOUT_MS`).
const STEWARD_CALL_TIMEOUT: Duration = Duration::from_millis(merge::STEWARD_CALL_TIMEOUT_MS);

/// What `apply` made of a call: something to store and send, or a client call that is refused
/// because it would grow the state too far.
enum Prepared {
    Ready(Applied),
    Refused,
}

/// Routes: GET /repo/<name>/ws (WebSocket upgrade), GET /repo/<name>/summary (evidence summary)
/// and POST /repo/<name>/trunk-moved (the steward's poke) -> coordinator for <name>, for a request
/// that carries an identity token for <name>.
/// `shell::authorize` decides; this only reads the request and answers its refusals.
#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let url = req.url()?;
    let segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
    let prefix = env
        .var("ALLOWED_REPO_PREFIX")
        .ok()
        .map(|var| var.to_string());
    let key = env
        .secret("IDENTITY_SIGNING_KEY")
        .ok()
        .map(|secret| secret.to_string());
    let authorization = req.headers().get("Authorization")?;
    let method = req.method().to_string();
    let inbound = Inbound {
        method: &method,
        segments: &segments,
        allowed_prefix: prefix.as_deref(),
        signing_key: key.as_deref(),
        authorization: authorization.as_deref(),
        now_ms: now_ms(),
    };
    let (route, agent) = match shell::authorize(&inbound) {
        Ok(admitted) => admitted,
        Err(denied) => return refuse(denied, url.path()),
    };
    let repo = match route {
        Route::Ws { repo } | Route::Summary { repo } | Route::TrunkMoved { repo } => repo,
    };

    let mut forwarded = req.clone_mut()?;
    let headers = forwarded.headers_mut()?;
    headers.delete("Authorization")?;
    headers.set(VERIFIED_AGENT_HEADER, &agent.0)?;
    let stub = env
        .durable_object("COORDINATOR")?
        .id_from_name(repo)?
        .get_stub()?;
    stub.fetch_with_request(forwarded).await
}

/// The response for a refused request. A 401 is logged with its reason and the path, never the
/// token or the key.
fn refuse(denied: Denied, path: &str) -> Result<Response> {
    match denied {
        Denied::NotFound => Response::error(
            "expected /repo/<name>/ws, /repo/<name>/summary or /repo/<name>/trunk-moved",
            404,
        ),
        Denied::MethodNotAllowed { allow } => {
            let response = Response::error("method not allowed", 405)?;
            let headers = Headers::new();
            headers.set("Allow", allow)?;
            Ok(response.with_headers(headers))
        }
        Denied::Forbidden => Response::error("this deployment does not serve that repo", 403),
        Denied::NotSteward => {
            console_error!("coordinator: refused {path}: not the steward");
            Response::error("only the steward may report a trunk move", 403)
        }
        Denied::Unauthorized(reason) => {
            console_error!("coordinator: refused {path}: {reason:?}");
            Response::error(UNAUTHORIZED_BODY, 401)
        }
    }
}

#[durable_object]
pub struct Coordinator {
    state: State,
    env: Env,
    /// Loaded on first use, and again after a failed write or hibernation.
    core: RefCell<Option<Core>>,
    /// The size of the state as last stored, for the soft limit.
    stored: Cell<StoredSize>,
    /// The claim whose merge this instance is waiting on. Memory only: a restart forgets it, which
    /// is how the alarm tells a merge cut off by a restart from one still running. While it is
    /// set, `apply` schedules only lease expiry, so a client message cannot start a second
    /// dispatch or a hot loop of alarms.
    merging: Cell<Option<ClaimId>>,
    /// The verification this instance is waiting on, as `merging` is for a merge.
    verifying: Cell<Option<u64>>,
    /// The events counted for the summary route so far. Memory only: a restart starts it over.
    summary_tally: RefCell<SummaryTally>,
}

impl DurableObject for Coordinator {
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            core: RefCell::new(None),
            stored: Cell::new(StoredSize::default()),
            merging: Cell::new(None),
            verifying: Cell::new(None),
            summary_tally: RefCell::new(SummaryTally::default()),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        let segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
        if let Some(Route::Summary { .. }) = shell::parse_route(&segments) {
            if req.headers().get(VERIFIED_AGENT_HEADER)?.is_none() {
                return Response::error(UNAUTHORIZED_BODY, 401);
            }
            return self.summary().await;
        }
        if let Some(Route::TrunkMoved { .. }) = shell::parse_route(&segments) {
            if req.headers().get(VERIFIED_AGENT_HEADER)?.is_none() {
                return Response::error(UNAUTHORIZED_BODY, 401);
            }
            return self.trunk_moved().await;
        }
        if req.headers().get("Upgrade")?.as_deref() != Some("websocket") {
            return Response::error("expected WebSocket upgrade", 426);
        }
        let Some(verified) = req.headers().get(VERIFIED_AGENT_HEADER)? else {
            return Response::error(UNAUTHORIZED_BODY, 401);
        };
        let pair = WebSocketPair::new()?;
        // Hibernation API: the runtime holds the socket, so an idle
        // coordinator costs nothing while agents stay connected.
        self.state.accept_web_socket(&pair.server);
        let session = Session {
            verified: Some(AgentId(verified)),
            ..Session::default()
        };
        self.write_session(&pair.server, &session)?;
        console_log!(
            "coordinator {}: socket accepted: agent={}",
            self.repo(),
            shell::log_agent(session.verified.as_ref())
        );
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
            self.withdraw_closed(std::slice::from_ref(&ws)).await;
        }
        result
    }

    async fn websocket_close(
        &self,
        ws: WebSocket,
        code: usize,
        reason: String,
        was_clean: bool,
    ) -> Result<()> {
        let agent = self.log_agent_of(&ws);
        let line = shell::close_log_line(code, was_clean, &reason, agent.as_ref());
        console_log!("coordinator {}: {line}", self.repo());
        self.withdraw(std::slice::from_ref(&ws)).await
    }

    async fn websocket_error(&self, ws: WebSocket, error: Error) -> Result<()> {
        let agent = self.log_agent_of(&ws);
        let line = shell::error_log_line(&error.to_string(), agent.as_ref());
        console_error!("coordinator {}: {line}", self.repo());
        self.withdraw(std::slice::from_ref(&ws)).await
    }

    async fn alarm(&self) -> Result<Response> {
        self.run_alarm().await?;
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
        self.close_with(ws, 1011, reason);
    }

    /// Close a socket from here and mark its session closed first, so the runtime still listing
    /// it as open shields no agent from a withdrawal.
    fn close_with(&self, ws: &WebSocket, code: u16, reason: &str) {
        if let Ok(session) = self.read_session(ws) {
            let closed = Session {
                closed: true,
                ..session
            };
            if let Err(e) = self.write_session(ws, &closed) {
                console_error!("coordinator {}: {e}", self.repo());
            }
        }
        if let Err(e) = ws.close(Some(code), Some(reason)) {
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
            Action::RejectAndClose(reply) => {
                let sent = send(ws, &reply);
                self.close_with(ws, CLOSE_POLICY_VIOLATION, "identity mismatch");
                self.withdraw_closed(std::slice::from_ref(ws)).await;
                sent
            }
            Action::Watch { from_seq } => self.watch(ws, session, from_seq).await,
            Action::Call { agent } => self.call(ws, &session, agent, msg).await,
        }
    }

    /// Load the core from storage, or create it for the configured run. Fails loudly on corrupt
    /// state or a bad config; never starts empty over stored data. `RUN` and `SHADOW_ENABLED`
    /// matter only when nothing is stored yet; `REVIEWERS` is applied on every load, and a bad
    /// one leaves nobody able to review.
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
        let reviewers = self.env.var("REVIEWERS").ok().map(|var| var.to_string());
        let reviewers = shell::reviewers_or_none(reviewers.as_deref(), |e| {
            console_error!("coordinator {}: {e}; nobody can review", self.repo());
        });
        let core = shell::load_core(
            stored.as_deref(),
            run.as_deref(),
            shadow.as_deref(),
            reviewers,
        )
        .map_err(|e| self.fail("load state", e))?;
        let mut slot = self.core.borrow_mut();
        if slot.is_none() {
            *slot = Some(core);
            self.stored.set(StoredSize::on_load(stored.as_deref()));
        }
        Ok(())
    }

    /// Run `step` on the core and serialize the result. Nothing is stored or sent yet. If
    /// serialization fails the core is dropped, because it has moved on from storage. A client
    /// call that would grow the state or log an event past the soft limit is refused and the core
    /// is dropped too. Any work past the hard limit is an error and drops the core.
    fn apply(&self, work: Work, step: impl FnOnce(&mut Core) -> Vec<Effect>) -> Result<Prepared> {
        let mut slot = self.core.borrow_mut();
        let Some(core) = slot.as_mut() else {
            return Err(self.fail("apply", "core is not loaded"));
        };
        let effects = step(core);
        let next_alarm_ms = core.next_wake_ms(
            self.merging.get().is_some(),
            self.verifying.get().is_some(),
            now_ms(),
        );
        let (events, outbound) = shell::split_effects(effects);
        let entries = match shell::persist_entries(core, &events) {
            Ok(entries) => entries,
            Err(e) => {
                *slot = None;
                return Err(self.fail("serialize state", e));
            }
        };
        match self.stored.get().judge(work, &entries) {
            StoreDecision::Store => Ok(Prepared::Ready(Applied {
                entries,
                events,
                outbound,
                next_alarm_ms,
                dispatch: None,
            })),
            StoreDecision::Refuse => {
                *slot = None;
                console_error!(
                    "coordinator {}: {}",
                    self.repo(),
                    shell::STATE_LIMIT_MESSAGE
                );
                Ok(Prepared::Refused)
            }
            StoreDecision::OverHard => {
                *slot = None;
                Err(self.fail("store state", "state is over the storage limit"))
            }
        }
    }

    /// The alarm: expire leases, recover a merge or verification a restart cut off, run at most
    /// one merge, then at most one verification (the core offers none while a merge is due). One
    /// of each per alarm keeps each invocation short and lets lease expiry run first every time.
    /// Each step stores its result under the hard limit and delivers it. The head read waits for
    /// the steward for up to `HEAD_CALL_TIMEOUT`, and it runs inside the alarm, so it can delay the
    /// next lease expiry by that long when a poke arrives.
    async fn run_alarm(&self) -> Result<()> {
        self.ensure_loaded().await?;
        self.expire_at(now_ms()).await?;
        self.recover_cut_off_merge().await?;
        self.recover_cut_off_verification().await?;
        self.run_one_merge().await?;
        self.run_one_verification().await?;
        self.run_head_sync().await?;
        self.ensure_alarm().await
    }

    /// The steward's poke: count it and schedule the alarm. Nothing in the request is read, so a
    /// caller cannot choose the head. Answers 202: the head is read by the alarm. A repo with no
    /// stored state is not one this coordinator serves: answers 204 and loads, stores and
    /// schedules nothing, so a push to any other repo cannot create a Durable Object's state.
    async fn trunk_moved(&self) -> Result<Response> {
        let stored: Option<String> = self
            .state
            .storage()
            .get(shell::STATE_KEY)
            .await
            .map_err(|e| self.fail("read state for a trunk poke", e))?;
        if shell::poke_reply(stored.as_deref()) == PokeReply::UnknownRepo {
            return Response::empty().map(|response| response.with_status(204));
        }
        self.ensure_loaded().await?;
        let now_ms = now_ms();
        let prepared = self.apply(Work::Plain, |core| core.trunk_moved(now_ms))?;
        let applied = self.ready(prepared, "record trunk move")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await?;
        Response::empty().map(|response| response.with_status(202))
    }

    /// Read the trunk head from the steward if the core says one is due, then offer it to the
    /// core. A failed call or an unusable answer is logged and offered as `None`, which clears
    /// the flag (see `coordinator::trunk`). The answer is stored before anyone is told.
    async fn run_head_sync(&self) -> Result<()> {
        let read = self
            .core
            .borrow()
            .as_ref()
            .and_then(|core| core.begin_head_read());
        let Some(read) = read else {
            return Ok(());
        };
        let answer = self.ask_steward_for_head().await;
        let prepared = self.apply(Work::Plain, |core| core.trunk_read(&read, answer, now_ms()))?;
        let applied = self.ready(prepared, "apply trunk head")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await
    }

    /// Ask the steward for the head of the trunk's main. `None` for a failed call, a timeout, a
    /// non-200 and an answer without a head.
    async fn ask_steward_for_head(&self) -> Option<CommitId> {
        let call = self.call_steward_for(STEWARD_HEAD_URL, head_request_body(&self.repo()));
        match with_timeout(call, HEAD_CALL_TIMEOUT).await {
            Some(Ok((status, body))) => {
                let head = parse_head_response(status, &body);
                if head.is_none() {
                    console_error!(
                        "coordinator {}: steward /head gave no head ({status})",
                        self.repo()
                    );
                }
                head
            }
            Some(Err(e)) => {
                console_error!("coordinator {}: steward /head failed: {e}", self.repo());
                None
            }
            None => {
                console_error!("coordinator {}: steward /head timed out", self.repo());
                None
            }
        }
    }

    /// A verification marked in flight while this instance is not waiting on the steward was cut
    /// off by a restart: the core counts it as a failed attempt, as for a merge.
    async fn recover_cut_off_verification(&self) -> Result<()> {
        let in_flight = self
            .core
            .borrow()
            .as_ref()
            .is_some_and(|core| core.has_verification_in_flight());
        if !shell::merge_cut_off(self.verifying.get().is_some(), in_flight) {
            return Ok(());
        }
        let now_ms = now_ms();
        let prepared = self.apply(Work::Plain, |core| core.recover_verification(now_ms))?;
        let applied = self.ready(prepared, "recover cut-off verification")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await
    }

    /// Run the next verification the core offers, if one is due. The in-flight marker is stored
    /// before the steward is called; the answer is stored before it is logged to anyone.
    async fn run_one_verification(&self) -> Result<()> {
        if self.verifying.get().is_some() || self.merging.get().is_some() {
            return Ok(());
        }
        let started_ms = now_ms();
        let mut dispatch = None;
        let prepared = self.apply(Work::Plain, |core| {
            dispatch = core.begin_verification(started_ms);
            Vec::new()
        })?;
        let Some(dispatch) = dispatch else {
            return Ok(());
        };
        let mut applied = self.ready(prepared, "start verification")?;
        applied.next_alarm_ms = self
            .core
            .borrow()
            .as_ref()
            .and_then(|core| core.next_wake_ms(false, true, started_ms));
        applied.dispatch = Some(Dispatch::Verify(dispatch));
        let persisted = self.persist(applied).await?;
        let Some(Dispatch::Verify(dispatch)) = persisted.dispatch() else {
            return Err(self.fail(
                "start verification",
                "the stored call lost its verification",
            ));
        };
        self.verifying.set(Some(dispatch.id));
        self.settle(&persisted, None).await?;
        let outcome = self.ask_steward_to_try(dispatch).await;
        self.verifying.set(None);
        let id = dispatch.id;
        let prepared = self.apply(Work::Plain, |core| {
            core.verification_outcome(id, &outcome, now_ms())
        })?;
        let applied = self.ready(prepared, "apply verification outcome")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await
    }

    /// Ask the steward to try the work. A failed call, a non-200 and an answer that is not a
    /// known outcome are all `ServiceUnavailable`: infrastructure, retried with backoff.
    async fn ask_steward_to_try(&self, dispatch: &VerifyDispatch) -> TrialReport {
        let call = self.call_steward_for(
            STEWARD_TRIAL_URL,
            merge::trial_request_body(
                &self.repo(),
                &dispatch.agent,
                &dispatch.before,
                &dispatch.main,
                dispatch.commit.as_ref(),
            ),
        );
        match with_timeout(call, STEWARD_CALL_TIMEOUT).await {
            Some(Ok((status, body))) => TrialReport::from_response(status, &body),
            Some(Err(e)) => {
                console_error!("coordinator {}: steward trial failed: {e}", self.repo());
                TrialReport::stopped(TrialOutcome::ServiceUnavailable)
            }
            None => {
                console_error!("coordinator {}: steward trial timed out", self.repo());
                TrialReport::stopped(TrialOutcome::ServiceUnavailable)
            }
        }
    }

    /// A merge marked in flight while this instance is not waiting on the steward was cut off by a
    /// restart: its answer is lost. The core counts it as an attempt, so a commit that keeps
    /// killing the merge is eventually rejected.
    async fn recover_cut_off_merge(&self) -> Result<()> {
        let in_flight = self
            .core
            .borrow()
            .as_ref()
            .is_some_and(|core| core.has_merge_in_flight());
        if !shell::merge_cut_off(self.merging.get().is_some(), in_flight) {
            return Ok(());
        }
        let now_ms = now_ms();
        let prepared = self.apply(Work::Plain, |core| core.recover_merge(now_ms))?;
        let applied = self.ready(prepared, "recover cut-off merge")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await
    }

    /// Run the next merge the core offers, if one is due. The in-flight marker is stored before
    /// the steward is called; the answer is stored before anyone is told.
    async fn run_one_merge(&self) -> Result<()> {
        if self.merging.get().is_some() {
            return Ok(());
        }
        let started_ms = now_ms();
        let mut dispatch = None;
        let prepared = self.apply(Work::Plain, |core| {
            dispatch = core.begin_merge(started_ms);
            Vec::new()
        })?;
        let Some(dispatch) = dispatch else {
            return Ok(());
        };
        let mut applied = self.ready(prepared, "start merge")?;
        // The answer is not in yet: schedule the lease expiry and the watchdog, as for a merge
        // this instance is waiting on.
        applied.next_alarm_ms = self
            .core
            .borrow()
            .as_ref()
            .and_then(|core| core.next_wake_ms(true, false, started_ms));
        applied.dispatch = Some(Dispatch::Merge(dispatch));
        let persisted = self.persist(applied).await?;
        let Some(Dispatch::Merge(dispatch)) = persisted.dispatch() else {
            return Err(self.fail("start merge", "the stored call lost its dispatch"));
        };
        self.merging.set(Some(dispatch.claim));
        self.settle(&persisted, None).await?;
        let outcome = self.ask_steward(dispatch).await;
        self.merging.set(None);
        let claim = dispatch.claim;
        let prepared = self.apply(Work::Plain, |core| {
            core.merge_outcome(claim, &outcome, now_ms())
        })?;
        let applied = self.ready(prepared, "apply merge outcome")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await
    }

    /// Ask the steward to merge. A failed call, a non-200 and an answer that is not a known
    /// outcome are all `ServiceUnavailable`: infrastructure, retried with backoff by the core.
    async fn ask_steward(&self, dispatch: &MergeDispatch) -> MergeOutcome {
        let body = merge::request_body(
            &self.repo(),
            &dispatch.agent,
            &dispatch.fork_commit,
            &dispatch.scopes,
        );
        let call = self.call_steward_for(STEWARD_MERGE_URL, body);
        match with_timeout(call, STEWARD_CALL_TIMEOUT).await {
            Some(Ok((status, body))) => MergeOutcome::from_response(status, &body),
            Some(Err(e)) => {
                console_error!("coordinator {}: steward call failed: {e}", self.repo());
                MergeOutcome::ServiceUnavailable
            }
            None => {
                console_error!("coordinator {}: steward call timed out", self.repo());
                MergeOutcome::ServiceUnavailable
            }
        }
    }

    /// POST `body` to the steward at `url`; the status and the text of the answer.
    async fn call_steward_for(&self, url: &str, body: String) -> Result<(u16, String)> {
        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(wasm_bindgen::JsValue::from_str(&body)));
        let request = Request::new_with_init(url, &init)?;
        let steward = self.env.service(STEWARD_BINDING)?;
        let mut response = steward.fetch_request(request).await?;
        Ok((response.status_code(), response.text().await?))
    }

    /// The expiry step that runs before a client message, so the message is judged against the
    /// state after expiry: a message that frees a lapsed claim and adds as much cannot look like
    /// no growth. Nothing is written when no lease is due.
    async fn expire_due(&self, now_ms: u64) -> Result<()> {
        let due = self
            .core
            .borrow()
            .as_ref()
            .is_some_and(|core| core.has_due_expiry(now_ms));
        if !due {
            return Ok(());
        }
        self.expire_at(now_ms).await
    }

    async fn expire_at(&self, now_ms: u64) -> Result<()> {
        let prepared = self.apply(Work::Plain, |core| core.expire(now_ms))?;
        let applied = self.ready(prepared, "expire leases")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await
    }

    /// The `Applied` of a call that has no sender to refuse (the alarm, a closing socket): a
    /// refusal is an error to log.
    fn ready(&self, prepared: Prepared, operation: &str) -> Result<Applied> {
        match prepared {
            Prepared::Ready(applied) => Ok(applied),
            Prepared::Refused => Err(self.fail(operation, shell::STATE_LIMIT_MESSAGE)),
        }
    }

    /// The agent a socket speaks for (see `shell::session_agent`), if its attachment can be read.
    fn log_agent_of(&self, ws: &WebSocket) -> Option<AgentId> {
        let session = self.read_session(ws).ok()?;
        shell::session_agent(&session).cloned()
    }

    /// `withdraw` for sockets this Durable Object closed itself, where no handler returns the
    /// error: log it, so it is not lost.
    async fn withdraw_closed(&self, closed: &[WebSocket]) {
        if let Err(e) = self.withdraw(closed).await {
            console_error!(
                "coordinator {}: withdraw after close failed: {e}",
                self.repo()
            );
        }
    }

    /// Sockets closed or failed. Each agent bound to one of them, with no other open socket
    /// bound to it, loses its queued request: nobody is left to receive its grant. `closing`
    /// is excluded from the open sockets, because a socket closed here may still be listed.
    /// Each withdrawal is applied, then persisted, then settled, as any other call.
    async fn withdraw(&self, closing: &[WebSocket]) -> Result<()> {
        let mut sessions = Vec::with_capacity(closing.len());
        for socket in closing {
            sessions.push(self.read_session(socket)?);
        }
        let open: Vec<WebSocket> = self
            .state
            .get_websockets()
            .into_iter()
            .filter(|socket| !closing.contains(socket))
            .collect();
        let others = self.read_sessions(&open);
        if sessions.iter().all(|session| session.agent.is_none()) {
            return Ok(());
        }
        self.ensure_loaded().await?;
        let queued = |agent: &AgentId| {
            let slot = self.core.borrow();
            slot.as_ref()
                .is_some_and(|core| core.has_queued_request(agent))
        };
        let mut first_error = None;
        for agent in shell::agents_to_withdraw(&sessions, &others, queued) {
            let withdrawn = self.withdraw_agent(&agent).await;
            if let Err(e) = &withdrawn {
                console_error!("coordinator {}: withdraw failed: {e}", self.repo());
            }
            keep_first(&mut first_error, withdrawn);
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn withdraw_agent(&self, agent: &AgentId) -> Result<()> {
        self.ensure_loaded().await?;
        let now_ms = now_ms();
        let prepared = self.apply(Work::Plain, |core| core.disconnect(agent, now_ms))?;
        let applied = self.ready(prepared, "withdraw queued request")?;
        let persisted = self.persist(applied).await?;
        self.settle(&persisted, None).await
    }

    /// Store the call's state and events in one transaction. On failure the cached core is
    /// dropped and the caller must send nothing: there is no `Persisted` to send with.
    async fn persist(&self, applied: Applied) -> Result<Persisted> {
        let written = store::write(&self.state.storage(), applied).await;
        match written {
            Ok(persisted) => {
                self.stored
                    .set(self.stored.get().on_write(persisted.entries()));
                Ok(persisted)
            }
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
        let now_ms = now_ms();
        self.expire_due(now_ms).await?;
        let req = shell::req_of(&msg);
        let work = shell::work_of(&msg);
        let prepared = self.apply(work, |core| core.handle(&agent, msg, now_ms))?;
        let applied = match prepared {
            Prepared::Ready(applied) => applied,
            Prepared::Refused => return send(ws, &shell::state_limit_reply(req)),
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
    /// the delivery error. A failed alarm write is logged and not returned: the call is already
    /// stored and answered, and an error here would close the client's socket. The next call
    /// reschedules, and the alarm handler ends by rescheduling with its error returned.
    async fn settle(&self, persisted: &Persisted, reply_to: Option<&WebSocket>) -> Result<()> {
        let (delivered, dead) = self.deliver(persisted, reply_to);
        if let Err(e) = self.reschedule(persisted.next_alarm_ms()).await {
            console_error!("coordinator {}: {e}", self.repo());
        }
        if !dead.is_empty() {
            Box::pin(self.withdraw_closed(&dead)).await;
        }
        delivered
    }

    /// Set the alarm from the core as it is now. An error is for the alarm handler to return, so
    /// the runtime runs the alarm again.
    async fn ensure_alarm(&self) -> Result<()> {
        let next = {
            let slot = self.core.borrow();
            let merging_here = self.merging.get().is_some();
            let verifying_here = self.verifying.get().is_some();
            slot.as_ref()
                .and_then(|core| core.next_wake_ms(merging_here, verifying_here, now_ms()))
        };
        self.reschedule(next).await
    }

    /// Send what `shell::plan_delivery` plans. Only a failed send to the socket that sent the
    /// message is an error. A failed send to any other socket closes that socket and delivery
    /// goes on, so one dead watcher cannot close the sender or make an alarm retry; the sockets it
    /// closed are returned, for `settle` to withdraw their agents.
    fn deliver(
        &self,
        persisted: &Persisted,
        reply_to: Option<&WebSocket>,
    ) -> (Result<()>, Vec<WebSocket>) {
        let sockets = if shell::needs_sessions(persisted.outbound(), persisted.events()) {
            self.state.get_websockets()
        } else {
            Vec::new()
        };
        let sessions = self.read_sessions(&sockets);
        let mut sender_error = None;
        let mut dead = Vec::new();
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
                        let agent = sessions.get(index).and_then(shell::session_agent);
                        console_error!(
                            "coordinator {}: send to a socket failed: agent={} {e}",
                            self.repo(),
                            shell::log_agent(agent)
                        );
                        self.close_socket(ws, "send failed");
                        dead.push(ws.clone());
                    }
                }
            }
        }
        (sender_error.map_or(Ok(()), Err), dead)
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

    async fn reschedule(&self, next_alarm_ms: Option<u64>) -> Result<()> {
        let storage = self.state.storage();
        let result = match shell::alarm_at_ms(next_alarm_ms, now_ms()) {
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
    ///
    /// The CLI relies on this. After `Watch { from_seq: 0 }` it sends `Hello` on the same socket
    /// and treats the first event after the `Welcome` as the end of the replay: the `Hello` is a
    /// queued message that this loop's input gate holds back until the whole replay is sent. If
    /// an await of another kind let that `Hello` run mid-replay, its `AgentConnected` event
    /// would reach the CLI before the later pages, the CLI would take a prefix of the log for
    /// the whole log, and it could adopt a claim whose release is still to come.
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

    /// The summary of the whole stored log as JSON. A plain read of storage: it loads no core,
    /// appends no event, sets no alarm and touches no socket. The tally of the events already
    /// counted is kept in memory, so a read costs only the events stored since the last one; the
    /// first read after a wake scans the whole log, a page at a time with no events kept. That scan
    /// runs behind the input gate like any storage read loop, so it delays other messages and the
    /// alarm until it ends, in proportion to the log's length. Events are never rewritten or
    /// removed, so the counted prefix stays valid.
    async fn summary(&self) -> Result<Response> {
        let storage = self.state.storage();
        let mut tally = self.summary_tally.borrow().clone();
        loop {
            let page = store::read_events(&storage, tally.next_seq(), shell::REPLAY_PAGE)
                .await
                .map_err(|e| self.fail("read events", e))?;
            let last_seq = page.last().map(|event| event.seq);
            let page_len = page.len();
            tally.add_page(&page);
            match shell::after_page(page_len, last_seq) {
                ReplayStep::Next { .. } => {}
                ReplayStep::Done => break,
            }
        }
        let body = tally.report().map_err(|e| self.fail("encode summary", e))?;
        *self.summary_tally.borrow_mut() = tally;
        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;
        headers.set("Cache-Control", "no-store")?;
        Ok(Response::ok(body)?.with_headers(headers))
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

/// `fut`'s output, or `None` if it is not ready within `limit`.
async fn with_timeout<T>(fut: impl Future<Output = T>, limit: Duration) -> Option<T> {
    let mut fut = pin!(fut);
    let mut timer = pin!(Delay::from(limit));
    poll_fn(|cx| {
        if let Poll::Ready(value) = fut.as_mut().poll(cx) {
            return Poll::Ready(Some(value));
        }
        if timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

fn now_ms() -> u64 {
    Date::now().as_millis()
}

fn send(ws: &WebSocket, msg: &ServerMsg) -> Result<()> {
    ws.send_with_str(serde_json::to_string(msg)?)
}

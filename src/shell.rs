//! Pure helpers for the Durable Object shell in `runtime.rs`: everything that can be decided without
//! a runtime. The shell only reads storage and sockets, calls these, and does what they say.
//!
//! Decisions made here:
//! - A `Notify` for an agent with no open socket is dropped. The core keeps the state that
//!   matters (claims, queue); a reconnecting agent learns the rest from its next message.
//! - Event keys are zero-padded so storage's key order is `seq` order.
//! - Stored state and events are JSON strings, so their shape is serde's, not the JS bridge's.

use serde::{Deserialize, Serialize};

use crate::coordinator::{Config, Coordinator as Core, Effect, InvalidConfig};
use crate::protocol::{AgentId, ClientMsg, ErrorCode, Event, RequestId, RunId, ServerMsg};

/// Lease length for every claim. A fixed value: nothing needs to tune it yet.
pub const LEASE_MS: u64 = 30_000;

/// The longest agent id a `Hello` may carry. A socket attachment holds at most 2 KiB, and the id
/// is stored in it.
pub const MAX_AGENT_ID_BYTES: usize = 128;

/// The longest text frame the shell parses. Larger frames are refused with `Malformed`.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// The size past which a client call may not grow the stored state, and the largest single event
/// a client call may log.
pub const SOFT_ENTRY_BYTES: usize = 1024 * 1024;

/// The largest value stored in one entry. Durable Object storage rejects values over 2 MiB; the
/// margin keeps the expiry that frees a repo from being rejected.
pub const HARD_ENTRY_BYTES: usize = 2 * 1024 * 1024 - 64 * 1024;

/// The largest integer a JavaScript number holds exactly. A socket attachment cannot store a
/// larger `u64`, and no event will ever have a higher `seq`.
pub const MAX_SAFE_SEQ: u64 = (1 << 53) - 1;

/// Storage key of the serialized core.
pub const STATE_KEY: &str = "core";

/// Storage key prefix of the event log.
pub const EVENT_PREFIX: &str = "ev:";

/// What a socket remembers across hibernation, kept in its attachment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The agent the Worker verified at the upgrade, set before the first message. A `Hello` for
    /// anyone else is refused. `None` (an attachment from before verification) refuses every
    /// `Hello`.
    #[serde(default)]
    pub verified: Option<AgentId>,
    /// Set by the first `Hello` the core answered with `Welcome`.
    #[serde(default)]
    pub agent: Option<AgentId>,
    /// Set once a `Watch` replay has finished: live events below this `seq` are not sent.
    /// `None` means the socket is not watching.
    #[serde(default)]
    pub watch_from: Option<u64>,
}

/// What the shell does with one parsed client message.
#[derive(Debug)]
pub enum Action {
    /// Answer the sender directly; the core is not called.
    Reject(ServerMsg),
    /// Answer the sender directly, then close the socket; the core is not called.
    RejectAndClose(ServerMsg),
    /// Replay the stored log from `from_seq`, then follow live.
    Watch { from_seq: u64 },
    /// Call the core with `agent` as the identity bound to the connection.
    Call { agent: AgentId },
}

/// One message to send once the call's state and events are stored.
#[derive(Debug, Clone)]
pub enum Outbound {
    /// To the socket that sent the message.
    Reply(ServerMsg),
    /// To every open socket bound to `agent`.
    Notify { agent: AgentId, msg: ServerMsg },
}

/// Why the core could not be built or loaded. Never carries stored or client text.
#[derive(Debug, PartialEq, Eq)]
pub enum LoadError {
    MissingRun,
    InvalidShadowEnabled,
    InvalidConfig(InvalidConfig),
    /// Stored state that does not parse. Only the position is kept: the state holds intents.
    Corrupt {
        line: usize,
        column: usize,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::MissingRun => {
                write!(
                    f,
                    "the RUN variable is missing or empty; set it in wrangler.toml"
                )
            }
            LoadError::InvalidShadowEnabled => write!(
                f,
                "the SHADOW_ENABLED variable must be exactly \"true\" or \"false\""
            ),
            LoadError::InvalidConfig(e) => write!(f, "{e}"),
            LoadError::Corrupt { line, column } => write!(
                f,
                "stored coordinator state is corrupt (line {line}, column {column}); \
                 refusing to start empty"
            ),
        }
    }
}

/// The one place the core's `Config` is assembled from the Worker's variables.
pub fn config_from_vars(run: Option<&str>, shadow: Option<&str>) -> Result<Config, LoadError> {
    let Some(run) = run.filter(|run| !run.is_empty()) else {
        return Err(LoadError::MissingRun);
    };
    Ok(Config {
        run: RunId(run.to_string()),
        lease_ms: LEASE_MS,
        shadow_enabled: parse_shadow_enabled(shadow)?,
    })
}

/// `SHADOW_ENABLED`: exactly "true" or "false"; unset means false. Anything else is an error.
fn parse_shadow_enabled(shadow: Option<&str>) -> Result<bool, LoadError> {
    match shadow {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(LoadError::InvalidShadowEnabled),
    }
}

/// Restore the core from its stored state, or create it when nothing is stored yet. The variables
/// are read only in the second case: a repo keeps the config it was created with.
pub fn load_core(
    stored: Option<&str>,
    run: Option<&str>,
    shadow: Option<&str>,
) -> Result<Core, LoadError> {
    let Some(stored) = stored else {
        let config = config_from_vars(run, shadow)?;
        return Core::new(config).map_err(LoadError::InvalidConfig);
    };
    serde_json::from_str(stored).map_err(|e| LoadError::Corrupt {
        line: e.line(),
        column: e.column(),
    })
}

/// Why a text frame is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameFault {
    /// Over `MAX_FRAME_BYTES`; refused before it is parsed.
    TooLarge,
    NotAMessage,
}

impl FrameFault {
    /// The reply to send. A fixed text, never the input.
    pub fn reply(self) -> ServerMsg {
        match self {
            FrameFault::TooLarge => {
                let message = format!("frame is larger than {MAX_FRAME_BYTES} bytes");
                error_msg(ErrorCode::Malformed, &message)
            }
            FrameFault::NotAMessage => malformed_reply(),
        }
    }
}

/// Parse one text frame. The parse error is dropped on purpose: it quotes the input.
pub fn parse_client_msg(text: &str) -> Result<ClientMsg, FrameFault> {
    if text.len() > MAX_FRAME_BYTES {
        return Err(FrameFault::TooLarge);
    }
    serde_json::from_str(text).map_err(|_| FrameFault::NotAMessage)
}

/// The reply to a text frame that is not a valid message. A fixed text, never the input.
pub fn malformed_reply() -> ServerMsg {
    error_msg(ErrorCode::Malformed, "not a valid message")
}

/// The reply to a binary frame.
pub fn binary_rejection() -> ServerMsg {
    error_msg(ErrorCode::Malformed, "binary frames are not supported")
}

fn error_msg(code: ErrorCode, message: &str) -> ServerMsg {
    ServerMsg::Error {
        req: None,
        code,
        message: message.to_string(),
    }
}

/// Decide what to do with `msg` on a socket in `session`.
///
/// Before the socket is bound only `Hello` and `Watch` are served. A `Hello` must name the agent
/// the Worker verified at the upgrade; any other is refused with `NotOwner` and the socket is
/// closed. On a bound socket a `Hello` is run as the bound agent.
pub fn decide(session: &Session, msg: &ClientMsg) -> Action {
    match msg {
        ClientMsg::Watch { .. } if session.watch_from.is_some() => Action::Reject(error_msg(
            ErrorCode::Malformed,
            "this socket is already watching",
        )),
        ClientMsg::Watch { from_seq } => Action::Watch {
            from_seq: clamp_watch_from(*from_seq),
        },
        ClientMsg::Hello { agent, .. } => {
            if let Some(rejection) = check_agent_id(agent) {
                return Action::Reject(rejection);
            }
            if session.verified.as_ref() != Some(agent) {
                return Action::RejectAndClose(error_msg(
                    ErrorCode::NotOwner,
                    "hello names an agent this connection's token was not issued for",
                ));
            }
            let agent = session.agent.clone().unwrap_or_else(|| agent.clone());
            Action::Call { agent }
        }
        ClientMsg::Claim { .. }
        | ClientMsg::Amend { .. }
        | ClientMsg::Heartbeat
        | ClientMsg::Release { .. }
        | ClientMsg::Submit { .. }
        | ClientMsg::OpenRace { .. }
        | ClientMsg::JoinRace { .. }
        | ClientMsg::PickWinner { .. }
        | ClientMsg::Review { .. } => match &session.agent {
            Some(agent) => Action::Call {
                agent: agent.clone(),
            },
            None => Action::Reject(ServerMsg::Error {
                req: req_of(msg),
                code: ErrorCode::NoHello,
                message: "send hello before any other message".to_string(),
            }),
        },
    }
}

/// The stored watch start: `from_seq` limited to `MAX_SAFE_SEQ`, so the attachment can hold it.
pub fn clamp_watch_from(from_seq: u64) -> u64 {
    from_seq.min(MAX_SAFE_SEQ)
}

/// `Malformed` for an agent id that is empty or too long to store in a socket attachment.
fn check_agent_id(agent: &AgentId) -> Option<ServerMsg> {
    if agent.0.is_empty() {
        return Some(error_msg(ErrorCode::Malformed, "agent id is empty"));
    }
    if agent.0.len() > MAX_AGENT_ID_BYTES {
        let message = format!("agent id is longer than {MAX_AGENT_ID_BYTES} bytes");
        return Some(error_msg(ErrorCode::Malformed, &message));
    }
    None
}

/// Split one call's effects into the events to store and the messages to send, each in order.
pub fn split_effects(effects: Vec<Effect>) -> (Vec<Event>, Vec<Outbound>) {
    let mut events = Vec::new();
    let mut outbound = Vec::new();
    for effect in effects {
        match effect {
            Effect::Log(event) => events.push(event),
            Effect::Reply(msg) => outbound.push(Outbound::Reply(msg)),
            Effect::Notify { agent, msg } => outbound.push(Outbound::Notify { agent, msg }),
        }
    }
    (events, outbound)
}

/// The session after a call: bound to `agent` if the core answered `Welcome`, else unchanged.
pub fn bind_on_welcome(
    session: &Session,
    agent: &AgentId,
    outbound: &[Outbound],
) -> Option<Session> {
    for item in outbound {
        if let Outbound::Reply(ServerMsg::Welcome { .. }) = item {
            return Some(Session {
                agent: Some(agent.clone()),
                ..session.clone()
            });
        }
    }
    None
}

/// Indexes of the sessions bound to `agent`.
pub fn bound_indexes(agent: &AgentId, sessions: &[Session]) -> Vec<usize> {
    let mut found = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        if session.agent.as_ref() == Some(agent) {
            found.push(index);
        }
    }
    found
}

/// Indexes of the sessions that follow the event log and want the event with this `seq`.
pub fn watcher_indexes(sessions: &[Session], seq: u64) -> Vec<usize> {
    let mut found = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        let Some(from) = session.watch_from else {
            continue;
        };
        if seq >= from {
            found.push(index);
        }
    }
    found
}

/// The largest time a JavaScript `Date` can hold, in milliseconds since the epoch.
pub const MAX_DATE_MS: f64 = 8.64e15;

/// The absolute time the alarm should fire, in milliseconds since the epoch, or `None` to clear
/// it. The core picks the earliest of a lease expiry and a merge dispatch. This is the one place
/// the time is clamped: `setAlarm` refuses a time that is not after 0, so it is at least
/// `now_ms + 1` (it fires at once), and at most the largest valid `Date`.
pub fn alarm_at_ms(next_alarm_ms: Option<u64>, now_ms: u64) -> Option<f64> {
    let next = next_alarm_ms?.max(now_ms.saturating_add(1));
    Some((next as f64).min(MAX_DATE_MS))
}

/// Whether a stored in-flight merge was cut off by a restart. It was not if this instance is
/// itself waiting on the steward: recovery must then do nothing.
pub fn merge_cut_off(merging_here: bool, in_flight: bool) -> bool {
    in_flight && !merging_here
}

/// Remember the first error of a series of attempts: record `result` in `slot` unless an earlier
/// one is already there. Delivery uses it to try every send and still report a failure.
pub fn keep_first<E>(slot: &mut Option<E>, result: Result<(), E>) {
    if let Err(e) = result {
        if slot.is_none() {
            *slot = Some(e);
        }
    }
}

/// How many events one replay read returns.
pub const REPLAY_PAGE: usize = 100;

/// What a `Watch` replay does after reading one page.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplayStep {
    /// Read the next page, starting at this `seq`.
    Next {
        start_seq: u64,
    },
    Done,
}

/// Decide whether the replay is over. A page shorter than `REPLAY_PAGE` was the last one.
pub fn after_page(page_len: usize, last_seq: Option<u64>) -> ReplayStep {
    let Some(last_seq) = last_seq else {
        return ReplayStep::Done;
    };
    if page_len < REPLAY_PAGE {
        return ReplayStep::Done;
    }
    match last_seq.checked_add(1) {
        Some(start_seq) => ReplayStep::Next { start_seq },
        None => ReplayStep::Done,
    }
}

/// Where one message of a delivery plan goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The socket that sent the message being handled.
    Sender,
    /// The open socket at this index of the sessions the plan was made from.
    Socket(usize),
}

/// Turn one stored call into the sends to make, in order: replies to the sender and
/// notifications to every socket bound to the named agent (dropped if none is open), then each
/// new event, in `seq` order, to every watcher. A sender that is also a watcher gets its reply
/// first and the events after.
pub fn plan_delivery(
    outbound: &[Outbound],
    events: &[Event],
    sessions: &[Session],
) -> Vec<(Target, ServerMsg)> {
    let mut plan = Vec::new();
    for item in outbound {
        match item {
            Outbound::Reply(msg) => plan.push((Target::Sender, msg.clone())),
            Outbound::Notify { agent, msg } => {
                for index in bound_indexes(agent, sessions) {
                    plan.push((Target::Socket(index), msg.clone()));
                }
            }
        }
    }
    for event in events {
        for index in watcher_indexes(sessions, event.seq) {
            let msg = ServerMsg::Event {
                event: event.clone(),
            };
            plan.push((Target::Socket(index), msg));
        }
    }
    plan
}

/// Storage key of the event with this `seq`. Keys sort in `seq` order.
pub fn event_key(seq: u64) -> String {
    format!("{EVENT_PREFIX}{seq:020}")
}

/// The text of the `Malformed` reply when a client call would grow the stored state too far.
pub const STATE_LIMIT_MESSAGE: &str = "repo state limit reached";

/// The reply to the sender of a client call that was refused for size. It carries the message's
/// `req` so the client can match it.
pub fn state_limit_reply(req: Option<RequestId>) -> ServerMsg {
    ServerMsg::Error {
        req,
        code: ErrorCode::Malformed,
        message: STATE_LIMIT_MESSAGE.to_string(),
    }
}

/// The `req` a client message carries, if any. `Release` carries an optional one.
pub fn req_of(msg: &ClientMsg) -> Option<RequestId> {
    match msg {
        ClientMsg::Claim { req, .. }
        | ClientMsg::Amend { req, .. }
        | ClientMsg::Submit { req, .. }
        | ClientMsg::OpenRace { req, .. }
        | ClientMsg::JoinRace { req, .. }
        | ClientMsg::PickWinner { req, .. }
        | ClientMsg::Review { req, .. } => Some(*req),
        ClientMsg::Release { req, .. } => *req,
        ClientMsg::Hello { .. } | ClientMsg::Heartbeat | ClientMsg::Watch { .. } => None,
    }
}

/// What kind of work produced the result that is about to be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    /// A message that adds content (`Claim`, `Amend`, `Submit`): the only work that may be
    /// refused for growing the state past `SOFT_ENTRY_BYTES`.
    Content,
    /// Everything else: the other client messages, the alarm's expiry, a withdrawal on close and
    /// the expiry that follows a refusal. Never refused for size below `HARD_ENTRY_BYTES`, so a
    /// repo over the soft limit can always make progress. No tolerance is added: it would let
    /// small steps ratchet past the hard limit.
    Plain,
}

/// The kind of work a client message does.
pub fn work_of(msg: &ClientMsg) -> Work {
    match msg {
        ClientMsg::Claim { .. } | ClientMsg::Amend { .. } | ClientMsg::Submit { .. } => {
            Work::Content
        }
        ClientMsg::Hello { .. }
        | ClientMsg::Heartbeat
        | ClientMsg::Release { .. }
        | ClientMsg::OpenRace { .. }
        | ClientMsg::JoinRace { .. }
        | ClientMsg::PickWinner { .. }
        | ClientMsg::Review { .. }
        | ClientMsg::Watch { .. } => Work::Plain,
    }
}

/// The sizes, in bytes, of one call's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizes {
    /// The state as last stored.
    pub previous_state: usize,
    pub state: usize,
    /// The largest single event entry of the call, 0 if it logged none.
    pub largest_event: usize,
}

/// What to do with a call's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreDecision {
    Store,
    /// A client call that would grow the state or log an event past `SOFT_ENTRY_BYTES`.
    Refuse,
    /// Past `HARD_ENTRY_BYTES`: storage would reject it.
    OverHard,
}

/// Store or refuse. A client call is refused when the new state is over `SOFT_ENTRY_BYTES` and
/// larger than the stored one (anything that does not grow the state, a `release` for one, goes
/// through), or when one event is over it. Past `HARD_ENTRY_BYTES`, the largest value storage
/// accepts less a margin, nothing is stored.
pub fn decide_store(work: Work, sizes: Sizes) -> StoreDecision {
    if work == Work::Content {
        let grown = sizes.state > SOFT_ENTRY_BYTES && sizes.state > sizes.previous_state;
        if grown || sizes.largest_event > SOFT_ENTRY_BYTES {
            return StoreDecision::Refuse;
        }
    }
    if sizes.state > HARD_ENTRY_BYTES || sizes.largest_event > HARD_ENTRY_BYTES {
        return StoreDecision::OverHard;
    }
    StoreDecision::Store
}

/// The sizes of `entries` (the state first, then one per event) against the stored state.
pub fn entry_sizes(entries: &[(String, String)], previous_state: usize) -> Sizes {
    let state = entries.first().map_or(0, |(_, json)| json.len());
    let mut largest_event = 0;
    for (_, json) in entries.iter().skip(1) {
        largest_event = largest_event.max(json.len());
    }
    Sizes {
        previous_state,
        state,
        largest_event,
    }
}

/// The size of the state as last stored, which the soft limit compares growth against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoredSize {
    bytes: usize,
}

impl StoredSize {
    /// The size of the stored state just read, or 0 if nothing is stored yet.
    pub fn on_load(stored: Option<&str>) -> Self {
        Self {
            bytes: stored.map_or(0, str::len),
        }
    }

    /// The size after `entries` (the state first) were written.
    pub fn on_write(self, entries: &[(String, String)]) -> Self {
        match entries.first() {
            Some((_, json)) => Self { bytes: json.len() },
            None => self,
        }
    }

    /// Store or refuse the result `entries` of `work`. Judging changes nothing: the size moves
    /// only when a write completes.
    pub fn judge(self, work: Work, entries: &[(String, String)]) -> StoreDecision {
        decide_store(work, entry_sizes(entries, self.bytes))
    }
}

/// Everything one call writes, as (key, JSON) pairs: the core state first, then each event.
pub fn persist_entries(
    core: &Core,
    events: &[Event],
) -> Result<Vec<(String, String)>, serde_json::Error> {
    let mut entries = Vec::with_capacity(events.len() + 1);
    entries.push((STATE_KEY.to_string(), serde_json::to_string(core)?));
    for event in events {
        entries.push((event_key(event.seq), serde_json::to_string(event)?));
    }
    Ok(entries)
}

/// Whether delivery must read the other sockets' sessions: only a notification or an event can
/// go to a socket other than the sender.
pub fn needs_sessions(outbound: &[Outbound], events: &[Event]) -> bool {
    if !events.is_empty() {
        return true;
    }
    for item in outbound {
        match item {
            Outbound::Reply(_) => {}
            Outbound::Notify { .. } => return true,
        }
    }
    false
}

/// The agent whose queued request to withdraw when a socket in `closing` goes away: the bound
/// agent, unless another open socket (`others`, which excludes the closing one) is bound to it,
/// and only if it has a queued request (`has_queued`), so a close that changes nothing is not
/// stored.
pub fn agent_to_withdraw(
    closing: &Session,
    others: &[Session],
    has_queued: impl Fn(&AgentId) -> bool,
) -> Option<AgentId> {
    let agent = closing.agent.as_ref()?;
    if bound_indexes(agent, others).is_empty() && has_queued(agent) {
        return Some(agent.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ClaimId, EventKind, Fence, Intent, Mode, OnConflict, Scope, ScopeClaim};

    const NOW: u64 = 1_000;

    fn agent(name: &str) -> AgentId {
        AgentId(name.to_string())
    }

    fn bound(name: &str) -> Session {
        Session {
            verified: Some(agent(name)),
            agent: Some(agent(name)),
            watch_from: None,
        }
    }

    fn verified(name: &str) -> Session {
        Session {
            verified: Some(agent(name)),
            ..Session::default()
        }
    }

    fn msg(json: &str) -> ClientMsg {
        serde_json::from_str(json).unwrap()
    }

    fn hello(name: &str) -> ClientMsg {
        msg(&format!(
            r#"{{"type":"hello","agent":"{name}","base":"abc"}}"#
        ))
    }

    fn claim(on_conflict: &str, mode: &str) -> ClientMsg {
        msg(&format!(
            r#"{{"type":"claim","req":1,"intent":{{"summary":"s","task_ref":null}},
            "scopes":[{{"scope":{{"kind":"symbol","path":"a.rs","qualified_name":"f"}},
            "mode":"{mode}"}}],"on_conflict":"{on_conflict}"}}"#
        ))
    }

    fn run(core: &mut Core, session: &Session, message: ClientMsg) -> (AgentId, Vec<Effect>) {
        let Action::Call { agent } = decide(session, &message) else {
            panic!("expected the core to be called");
        };
        let effects = core.handle(&agent, message, NOW);
        (agent, effects)
    }

    fn new_core() -> Core {
        load_core(None, Some("test"), None).unwrap()
    }

    fn reject_code(action: Action) -> ErrorCode {
        let Action::Reject(ServerMsg::Error { code, .. }) = action else {
            panic!("expected a rejection, got {action:?}");
        };
        code
    }

    #[test]
    fn session_survives_the_attachment_round_trip() {
        let session = Session {
            verified: Some(agent("a1")),
            agent: Some(agent("a1")),
            watch_from: Some(0),
        };
        let json = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), session);
    }

    #[test]
    fn empty_attachment_is_an_unbound_non_watcher() {
        assert_eq!(
            serde_json::from_str::<Session>("{}").unwrap(),
            Session::default()
        );
    }

    #[test]
    fn unbound_socket_gets_no_hello_for_everything_but_hello_and_watch() {
        let session = Session::default();
        let heartbeat = msg(r#"{"type":"heartbeat"}"#);
        let release = msg(r#"{"type":"release","claim":1,"fence":1}"#);
        for message in [claim("fail", "depend"), heartbeat, release] {
            assert_eq!(reject_code(decide(&session, &message)), ErrorCode::NoHello);
        }
    }

    #[test]
    fn no_hello_rejection_echoes_the_req_of_every_request_carrying_message() {
        let session = Session::default();
        let release = msg(r#"{"type":"release","claim":1,"fence":1,"req":8}"#);
        let old_release = msg(r#"{"type":"release","claim":1,"fence":1}"#);
        let claim_msg = claim("fail", "depend");
        for (message, expected) in [
            (release, Some(RequestId(8))),
            (old_release, None),
            (claim_msg.clone(), req_of(&claim_msg)),
        ] {
            let Action::Reject(ServerMsg::Error { req, code, .. }) = decide(&session, &message)
            else {
                panic!("expected a rejection");
            };
            assert_eq!((req, code), (expected, ErrorCode::NoHello));
        }
        assert!(req_of(&claim_msg).is_some(), "the claim carries a req");
    }

    #[test]
    fn unbound_hello_runs_as_the_verified_agent() {
        let Action::Call { agent: who } = decide(&verified("a1"), &hello("a1")) else {
            panic!("expected a core call");
        };
        assert_eq!(who, agent("a1"));
    }

    fn close_code(action: Action) -> ErrorCode {
        let Action::RejectAndClose(ServerMsg::Error { code, .. }) = action else {
            panic!("expected a rejection that closes the socket, got {action:?}");
        };
        code
    }

    #[test]
    fn hello_naming_another_agent_is_not_owner_and_closes_the_socket() {
        for session in [verified("a1"), bound("a1")] {
            let action = decide(&session, &hello("a2"));
            assert_eq!(close_code(action), ErrorCode::NotOwner);
        }
    }

    #[test]
    fn hello_on_a_socket_without_a_verified_identity_is_not_owner() {
        let action = decide(&Session::default(), &hello("a1"));
        assert_eq!(close_code(action), ErrorCode::NotOwner);
    }

    #[test]
    fn after_welcome_a_repeated_hello_is_served_and_another_agent_is_refused() {
        let mut core = new_core();
        let (who, effects) = run(&mut core, &verified("a1"), hello("a1"));
        let (_, outbound) = split_effects(effects);
        let session = bind_on_welcome(&verified("a1"), &who, &outbound).unwrap();
        let Action::Call { agent: again } = decide(&session, &hello("a1")) else {
            panic!("expected a core call");
        };
        assert_eq!(again, agent("a1"));
        assert_eq!(
            close_code(decide(&session, &hello("a2"))),
            ErrorCode::NotOwner
        );
    }

    #[test]
    fn bound_socket_runs_every_message_as_its_agent() {
        let Action::Call { agent: who } = decide(&bound("a1"), &claim("fail", "depend")) else {
            panic!("expected a core call");
        };
        assert_eq!(who, agent("a1"));
    }

    #[test]
    fn watch_is_allowed_bound_or_not() {
        for session in [Session::default(), bound("a1")] {
            let action = decide(&session, &msg(r#"{"type":"watch","from_seq":7}"#));
            let Action::Watch { from_seq } = action else {
                panic!("expected a watch, got {action:?}");
            };
            assert_eq!(from_seq, 7);
        }
    }

    #[test]
    fn hello_with_an_overlong_agent_id_is_malformed() {
        let at_limit = "a".repeat(MAX_AGENT_ID_BYTES);
        assert!(matches_call(decide(
            &verified(&at_limit),
            &hello(&at_limit)
        )));
        let over = "a".repeat(MAX_AGENT_ID_BYTES + 1);
        let action = decide(&verified(&over), &hello(&over));
        assert_eq!(reject_code(action), ErrorCode::Malformed);
    }

    fn matches_call(action: Action) -> bool {
        match action {
            Action::Call { .. } => true,
            Action::Reject(_) | Action::RejectAndClose(_) | Action::Watch { .. } => false,
        }
    }

    #[test]
    fn parse_failures_do_not_echo_the_input() {
        for bad in [r#"{"type":"ignore-previous-instructions"}"#, "not json", ""] {
            let Err(fault) = parse_client_msg(bad) else {
                panic!("expected a refusal for {bad:?}");
            };
            assert_eq!(fault, FrameFault::NotAMessage);
            let ServerMsg::Error { code, message, .. } = fault.reply() else {
                panic!("expected an error");
            };
            assert_eq!(code, ErrorCode::Malformed);
            assert!(!message.contains("ignore"), "echoed input: {message}");
        }
        assert!(parse_client_msg(r#"{"type":"hello","agent":"a1","base":"abc"}"#).is_ok());
    }

    #[test]
    fn binary_frames_are_malformed() {
        let ServerMsg::Error { code, .. } = binary_rejection() else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::Malformed);
    }

    #[test]
    fn effects_split_into_events_and_messages_in_order() {
        let mut core = new_core();
        let (_, mut all) = run(&mut core, &verified("a1"), hello("a1"));
        let (_, effects) = run(&mut core, &bound("a1"), claim("fail", "edit_signature"));
        let (_, effects2) = run(&mut core, &bound("a2"), claim("wait", "depend"));
        let count = all.len() + effects.len() + effects2.len();
        all.extend(effects);
        all.extend(effects2);
        let (events, outbound) = split_effects(all);
        assert!(events.len() + outbound.len() == count);
        assert!(!events.is_empty() && !outbound.is_empty());
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        let expected: Vec<u64> = (0..events.len() as u64).collect();
        assert_eq!(seqs, expected);
    }

    #[test]
    fn split_keeps_message_order_across_replies_and_notifies() {
        let reply = Effect::Reply(binary_rejection());
        let notify = Effect::Notify {
            agent: agent("a2"),
            msg: ServerMsg::Queued {
                req: RequestId(1),
                position: 0,
            },
        };
        let (events, outbound) = split_effects(vec![reply.clone(), notify, reply]);
        assert!(events.is_empty());
        let [Outbound::Reply(_), Outbound::Notify { .. }, Outbound::Reply(_)] = outbound.as_slice()
        else {
            panic!("order lost: {outbound:?}");
        };
    }

    #[test]
    fn welcome_binds_the_socket_to_the_calling_agent() {
        let mut core = new_core();
        let (who, effects) = run(&mut core, &verified("a1"), hello("a1"));
        let (_, outbound) = split_effects(effects);
        let bound = bind_on_welcome(&Session::default(), &who, &outbound).unwrap();
        assert_eq!(bound.agent, Some(agent("a1")));
    }

    #[test]
    fn a_refused_hello_does_not_bind() {
        let mut core = new_core();
        let message = msg(r#"{"type":"hello","agent":"a1","base":"abc","protocol":999}"#);
        let (who, effects) = run(&mut core, &verified("a1"), message);
        let (_, outbound) = split_effects(effects);
        assert!(bind_on_welcome(&Session::default(), &who, &outbound).is_none());
    }

    #[test]
    fn a_welcome_sent_to_someone_else_does_not_bind_this_socket() {
        let outbound = [Outbound::Notify {
            agent: agent("a2"),
            msg: ServerMsg::Welcome {
                head: crate::protocol::CommitId("x".into()),
                lease_ms: 1,
                protocol: 1,
            },
        }];
        assert!(bind_on_welcome(&Session::default(), &agent("a1"), &outbound).is_none());
    }

    #[test]
    fn rebinding_keeps_the_watcher_flag_and_from_seq() {
        let session = Session {
            verified: Some(agent("a1")),
            agent: None,
            watch_from: Some(7),
        };
        let mut core = new_core();
        let (who, effects) = run(&mut core, &session, hello("a1"));
        let (_, outbound) = split_effects(effects);
        let rebound = bind_on_welcome(&session, &who, &outbound).unwrap();
        assert_eq!(rebound.watch_from, Some(7));
    }

    #[test]
    fn notify_targets_every_socket_of_the_named_agent_and_nobody_else() {
        let sessions = [bound("a1"), bound("a2"), Session::default(), bound("a2")];
        assert_eq!(bound_indexes(&agent("a2"), &sessions), vec![1, 3]);
        assert_eq!(bound_indexes(&agent("a1"), &sessions), vec![0]);
        assert!(bound_indexes(&agent("a3"), &sessions).is_empty());
        assert!(bound_indexes(&agent("a1"), &[]).is_empty());
    }

    #[test]
    fn released_claim_wakes_the_waiter_on_its_own_socket_not_the_releaser() {
        let mut core = new_core();
        let a1 = bound("a1");
        let a2 = bound("a2");
        let (_, granted) = run(&mut core, &a1, claim("fail", "edit_signature"));
        let Some((claim, fence)) = granted_claim(&granted) else {
            panic!("a1 was not granted: {granted:?}");
        };
        run(&mut core, &a2, claim_msg_wait());
        let release = ClientMsg::Release {
            claim,
            fence,
            req: None,
        };
        let (_, effects) = run(&mut core, &a1, release);
        let (_, outbound) = split_effects(effects);
        let sessions = [a1, a2];
        let mut woken = Vec::new();
        for item in &outbound {
            if let Outbound::Notify {
                agent,
                msg: ServerMsg::Granted { .. },
            } = item
            {
                woken.extend(bound_indexes(agent, &sessions));
            }
        }
        assert_eq!(woken, vec![1]);
    }

    fn granted_claim(effects: &[Effect]) -> Option<(ClaimId, Fence)> {
        for effect in effects {
            if let Effect::Reply(ServerMsg::Granted { claim, fence, .. }) = effect {
                return Some((*claim, *fence));
            }
        }
        None
    }

    fn was_denied(effects: &[Effect]) -> bool {
        for effect in effects {
            if let Effect::Reply(ServerMsg::Denied { .. }) = effect {
                return true;
            }
        }
        false
    }

    fn claim_msg_wait() -> ClientMsg {
        claim("wait", "depend")
    }

    #[test]
    fn watchers_are_found_by_their_flag() {
        let watcher = Session {
            verified: None,
            agent: None,
            watch_from: Some(0),
        };
        let sessions = [Session::default(), watcher.clone(), bound("a1"), watcher];
        assert_eq!(watcher_indexes(&sessions, 0), vec![1, 3]);
        assert!(watcher_indexes(&[], 0).is_empty());
    }

    #[test]
    fn a_stored_merge_is_cut_off_only_when_this_instance_is_not_running_it() {
        assert!(merge_cut_off(false, true));
        assert!(
            !merge_cut_off(true, true),
            "recovery is a no-op while merging"
        );
        assert!(!merge_cut_off(false, false));
        assert!(!merge_cut_off(true, false));
    }

    #[test]
    fn alarm_is_cleared_without_an_expiry() {
        assert_eq!(alarm_at_ms(None, 5), None);
    }

    #[test]
    fn alarm_time_is_the_absolute_expiry() {
        assert_eq!(
            alarm_at_ms(Some(1_791_180_000_000), 1_791_000_000_000),
            Some(1_791_180_000_000.0)
        );
    }

    #[test]
    fn a_due_or_past_alarm_is_set_just_after_now_never_at_or_before_zero() {
        let now = 1_791_000_000_000;
        assert_eq!(alarm_at_ms(Some(0), now), Some(now as f64 + 1.0));
        assert_eq!(alarm_at_ms(Some(now), now), Some(now as f64 + 1.0));
        assert_eq!(alarm_at_ms(Some(now - 5), now), Some(now as f64 + 1.0));
        assert_eq!(alarm_at_ms(Some(0), 0), Some(1.0));
    }

    #[test]
    fn an_absurdly_distant_expiry_is_clamped_to_a_valid_date() {
        assert_eq!(alarm_at_ms(Some(u64::MAX), 5), Some(MAX_DATE_MS));
        assert_eq!(
            alarm_at_ms(Some(MAX_DATE_MS as u64 + 1), 5),
            Some(MAX_DATE_MS)
        );
        assert_eq!(alarm_at_ms(Some(MAX_DATE_MS as u64), 5), Some(MAX_DATE_MS));
        assert_eq!(alarm_at_ms(Some(10), u64::MAX), Some(MAX_DATE_MS));
    }

    #[test]
    fn event_keys_sort_in_seq_order() {
        let seqs = [0, 1, 9, 10, 99, 100, 12_345, u64::MAX];
        let keys: Vec<String> = seqs.iter().map(|s| event_key(*s)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert!(keys.iter().all(|k| k.starts_with(EVENT_PREFIX)));
        assert_eq!(event_key(5), "ev:00000000000000000005");
        assert_eq!(event_key(u64::MAX), "ev:18446744073709551615");
    }

    #[test]
    fn persist_entries_put_state_first_then_events_in_order() {
        let mut core = new_core();
        run(&mut core, &verified("a1"), hello("a1"));
        let (_, effects) = run(&mut core, &bound("a1"), claim("fail", "edit_signature"));
        let (events, _) = split_effects(effects);
        assert!(!events.is_empty());
        let entries = persist_entries(&core, &events).unwrap();
        assert_eq!(entries.len(), events.len() + 1);
        assert_eq!(entries[0].0, STATE_KEY);
        for (entry, event) in entries[1..].iter().zip(&events) {
            assert_eq!(entry.0, event_key(event.seq));
            let stored: Event = serde_json::from_str(&entry.1).unwrap();
            assert_eq!(stored.seq, event.seq);
        }
    }

    #[test]
    fn stored_state_restores_the_core_and_continues_the_seq() {
        let mut core = new_core();
        run(&mut core, &verified("a1"), hello("a1"));
        let (_, first) = run(&mut core, &bound("a1"), claim("fail", "edit_signature"));
        let last = split_effects(first).0.last().map(|e| e.seq).unwrap();
        let stored = serde_json::to_string(&core).unwrap();

        let mut restored = load_core(Some(&stored), Some("ignored"), Some("junk")).unwrap();
        let (_, denied) = run(&mut restored, &bound("a2"), claim("fail", "depend"));
        assert!(
            was_denied(&denied),
            "restored core forgot a1's claim: {denied:?}"
        );
        let (_, next) = run(&mut restored, &bound("a2"), claim("fail", "depend"));
        let seqs: Vec<u64> = split_effects(next).0.iter().map(|e| e.seq).collect();
        assert!(seqs.iter().all(|seq| *seq > last), "seq reused: {seqs:?}");
    }

    #[test]
    fn no_stored_state_creates_a_core_for_the_run() {
        let core = load_core(None, Some("dogfood"), None).unwrap();
        let json = serde_json::to_string(&core).unwrap();
        assert!(json.contains(r#""run":"dogfood""#), "{json}");
        assert!(
            json.contains(&format!(r#""lease_ms":{LEASE_MS}"#)),
            "{json}"
        );
    }

    #[test]
    fn missing_or_empty_run_fails_loudly() {
        assert_eq!(
            load_core(None, None, None).unwrap_err(),
            LoadError::MissingRun
        );
        assert_eq!(
            load_core(None, Some(""), None).unwrap_err(),
            LoadError::MissingRun
        );
        assert!(config_from_vars(None, None).is_err());
    }

    #[test]
    fn corrupt_stored_state_fails_without_starting_empty_or_echoing_it() {
        let err =
            load_core(Some(r#"{"claims":"secret-intent-text""#), Some("r"), None).unwrap_err();
        let LoadError::Corrupt { .. } = err else {
            panic!("expected Corrupt, got {err:?}");
        };
        assert!(!err.to_string().contains("secret"));
        assert!(err.to_string().contains("refusing to start empty"));
        assert!(load_core(Some(""), Some("r"), None).is_err());
        assert!(load_core(Some("{}"), Some("r"), None).is_err());
    }

    #[test]
    fn a_log_event_carries_the_configured_run() {
        let mut core = new_core();
        let (_, effects) = run(&mut core, &verified("a1"), hello("a1"));
        let (events, _) = split_effects(effects);
        let [Event {
            kind: EventKind::AgentConnected { .. },
            run,
            ..
        }] = events.as_slice()
        else {
            panic!("expected one AgentConnected event, got {events:?}");
        };
        assert_eq!(run, &RunId("test".into()));
    }

    fn shadow_of(core: &Core) -> bool {
        let json: serde_json::Value = serde_json::to_value(core).unwrap();
        json["config"]["shadow_enabled"].as_bool().unwrap()
    }

    fn core_with_shadow(shadow: Option<&str>) -> Result<Core, LoadError> {
        load_core(None, Some("r"), shadow)
    }

    #[test]
    fn shadow_is_off_when_the_variable_is_unset() {
        assert!(!shadow_of(&core_with_shadow(None).unwrap()));
    }

    #[test]
    fn shadow_is_on_for_exactly_true() {
        assert!(shadow_of(&core_with_shadow(Some("true")).unwrap()));
    }

    #[test]
    fn shadow_is_off_for_exactly_false() {
        assert!(!shadow_of(&core_with_shadow(Some("false")).unwrap()));
    }

    #[test]
    fn any_other_shadow_value_fails_loudly_without_echoing_it() {
        for bad in ["TRUE", "True", "1", "yes", "", " true"] {
            let err = core_with_shadow(Some(bad)).unwrap_err();
            assert_eq!(err, LoadError::InvalidShadowEnabled, "{bad:?}");
            assert!(err.to_string().contains("SHADOW_ENABLED"));
        }
    }

    #[test]
    fn an_empty_agent_id_is_malformed_and_never_called() {
        let action = decide(&Session::default(), &hello(""));
        assert_eq!(reject_code(action), ErrorCode::Malformed);
    }

    #[test]
    fn a_second_watch_on_a_watcher_is_refused_but_the_first_is_served() {
        let watcher = Session {
            verified: None,
            agent: None,
            watch_from: Some(0),
        };
        let again = decide(&watcher, &msg(r#"{"type":"watch","from_seq":0}"#));
        assert_eq!(reject_code(again), ErrorCode::Malformed);
        let first = decide(
            &Session::default(),
            &msg(r#"{"type":"watch","from_seq":0}"#),
        );
        assert!(matches_watch(first));
    }

    fn matches_watch(action: Action) -> bool {
        match action {
            Action::Watch { .. } => true,
            Action::Reject(_) | Action::RejectAndClose(_) | Action::Call { .. } => false,
        }
    }

    #[test]
    fn an_empty_or_short_replay_page_ends_the_replay() {
        assert_eq!(after_page(0, None), ReplayStep::Done);
        assert_eq!(after_page(REPLAY_PAGE - 1, Some(40)), ReplayStep::Done);
    }

    #[test]
    fn a_full_replay_page_continues_after_its_last_seq() {
        assert_eq!(
            after_page(REPLAY_PAGE, Some(99)),
            ReplayStep::Next { start_seq: 100 }
        );
        assert_eq!(after_page(REPLAY_PAGE, Some(u64::MAX)), ReplayStep::Done);
    }

    fn event_at(seq: u64) -> Event {
        let kind = EventKind::AgentConnected { agent: agent("a1") };
        Event {
            seq,
            at_ms: 0,
            run: RunId("test".into()),
            kind,
        }
    }

    fn queued(agent_name: &str) -> Outbound {
        Outbound::Notify {
            agent: agent(agent_name),
            msg: ServerMsg::Queued {
                req: RequestId(1),
                position: 0,
            },
        }
    }

    fn seq_of(msg: &ServerMsg) -> Option<u64> {
        if let ServerMsg::Event { event } = msg {
            return Some(event.seq);
        }
        None
    }

    fn watcher() -> Session {
        Session {
            verified: None,
            agent: None,
            watch_from: Some(0),
        }
    }

    #[test]
    fn delivery_sends_replies_then_notifications_then_events() {
        let outbound = [Outbound::Reply(binary_rejection()), queued("a2")];
        let plan = plan_delivery(&outbound, &[event_at(0)], &[bound("a2"), watcher()]);
        let targets: Vec<Target> = plan.iter().map(|(t, _)| *t).collect();
        assert_eq!(
            targets,
            vec![Target::Sender, Target::Socket(0), Target::Socket(1)]
        );
    }

    #[test]
    fn a_notify_reaches_every_socket_of_an_agent_with_two() {
        let plan = plan_delivery(
            &[queued("a2")],
            &[],
            &[bound("a2"), bound("a1"), bound("a2")],
        );
        let targets: Vec<Target> = plan.iter().map(|(t, _)| *t).collect();
        assert_eq!(targets, vec![Target::Socket(0), Target::Socket(2)]);
    }

    #[test]
    fn a_notify_for_an_agent_with_no_open_socket_is_dropped() {
        let plan = plan_delivery(&[queued("a3")], &[], &[bound("a1"), Session::default()]);
        assert!(plan.is_empty(), "{plan:?}");
        assert!(plan_delivery(&[queued("a3")], &[], &[]).is_empty());
    }

    #[test]
    fn a_notify_never_goes_to_the_sender() {
        let plan = plan_delivery(&[queued("a2")], &[], &[bound("a1"), bound("a2")]);
        for (target, _) in &plan {
            assert_ne!(*target, Target::Sender);
        }
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn each_watcher_gets_every_event_once_in_seq_order() {
        let events = [event_at(4), event_at(5), event_at(6)];
        let sessions = [watcher(), bound("a1"), watcher()];
        let plan = plan_delivery(&[], &events, &sessions);
        assert_eq!(plan.len(), 6);
        for index in [0, 2] {
            let mut seqs = Vec::new();
            for (target, msg) in &plan {
                if *target == Target::Socket(index) {
                    seqs.extend(seq_of(msg));
                }
            }
            assert_eq!(seqs, vec![4, 5, 6], "watcher {index}");
        }
    }

    #[test]
    fn no_watchers_means_no_event_sends() {
        let plan = plan_delivery(&[], &[event_at(0), event_at(1)], &[bound("a1")]);
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn a_sender_that_is_also_a_watcher_gets_its_reply_then_the_events() {
        let sender = Session {
            verified: None,
            agent: Some(agent("a1")),
            watch_from: Some(0),
        };
        let outbound = [Outbound::Reply(binary_rejection())];
        let plan = plan_delivery(&outbound, &[event_at(0), event_at(1)], &[sender]);
        let targets: Vec<Target> = plan.iter().map(|(t, _)| *t).collect();
        assert_eq!(
            targets,
            vec![Target::Sender, Target::Socket(0), Target::Socket(0)]
        );
        assert_eq!(seq_of(&plan[1].1), Some(0));
        assert_eq!(seq_of(&plan[2].1), Some(1));
    }

    fn watcher_from(watch_from: u64) -> Session {
        Session {
            verified: None,
            agent: None,
            watch_from: Some(watch_from),
        }
    }

    fn event_seqs(plan: &[(Target, ServerMsg)], index: usize) -> Vec<u64> {
        let mut seqs = Vec::new();
        for (target, msg) in plan {
            if *target == Target::Socket(index) {
                seqs.extend(seq_of(msg));
            }
        }
        seqs
    }

    #[test]
    fn a_watcher_ahead_of_the_log_receives_nothing_until_the_log_reaches_it() {
        let sessions = [watcher_from(5)];
        let behind = [event_at(0), event_at(1), event_at(4)];
        assert!(plan_delivery(&[], &behind, &sessions).is_empty());
        let plan = plan_delivery(&[], &[event_at(5), event_at(6)], &sessions);
        assert_eq!(event_seqs(&plan, 0), vec![5, 6]);
    }

    #[test]
    fn a_watcher_gets_each_event_from_its_from_seq_exactly_once() {
        let events = [event_at(3), event_at(4), event_at(5), event_at(6)];
        let plan = plan_delivery(&[], &events, &[watcher_from(5)]);
        assert_eq!(event_seqs(&plan, 0), vec![5, 6]);
    }

    #[test]
    fn watchers_with_different_from_seq_each_get_their_own_range() {
        let events = [event_at(1), event_at(2), event_at(3)];
        let sessions = [watcher_from(0), watcher_from(2), watcher_from(9)];
        let plan = plan_delivery(&[], &events, &sessions);
        assert_eq!(event_seqs(&plan, 0), vec![1, 2, 3]);
        assert_eq!(event_seqs(&plan, 1), vec![2, 3]);
        assert!(event_seqs(&plan, 2).is_empty());
    }

    #[test]
    fn the_attachment_round_trip_keeps_from_seq() {
        let session = watcher_from(77);
        let json = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), session);
    }

    #[test]
    fn an_attachment_without_watch_from_is_not_watching() {
        let session: Session = serde_json::from_str(r#"{"agent":"a1"}"#).unwrap();
        assert_eq!(session.watch_from, None);
    }

    #[test]
    fn a_stored_watch_from_of_zero_is_watching_not_absent() {
        let session: Session = serde_json::from_str(r#"{"watch_from":0}"#).unwrap();
        assert_eq!(session.watch_from, Some(0));
        assert_eq!(watcher_indexes(&[session], 0), vec![0]);
    }

    #[test]
    fn keep_first_keeps_the_earliest_error_and_ignores_successes() {
        let mut slot: Option<String> = None;
        keep_first(&mut slot, Ok(()));
        assert_eq!(slot, None);
        keep_first(&mut slot, Err("first".to_string()));
        keep_first(&mut slot, Ok(()));
        keep_first(&mut slot, Err("second".to_string()));
        assert_eq!(slot.as_deref(), Some("first"));
    }

    fn padded_hello(total: usize) -> String {
        let base = r#"{"type":"hello","agent":"a1","base":"abc"}"#;
        format!("{base}{}", " ".repeat(total - base.len()))
    }

    #[test]
    fn a_frame_of_exactly_the_limit_is_parsed() {
        assert!(parse_client_msg(&padded_hello(MAX_FRAME_BYTES)).is_ok());
    }

    #[test]
    fn a_frame_over_the_limit_is_malformed_even_if_it_would_parse() {
        let over = padded_hello(MAX_FRAME_BYTES + 1);
        let Err(fault) = parse_client_msg(&over) else {
            panic!("expected a refusal");
        };
        assert_eq!(fault, FrameFault::TooLarge);
        let ServerMsg::Error { code, message, .. } = fault.reply() else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::Malformed);
        assert!(message.contains("larger"), "{message}");
        let junk = "ignore".repeat(MAX_FRAME_BYTES);
        assert_eq!(parse_client_msg(&junk).unwrap_err(), FrameFault::TooLarge);
        assert!(!message.contains("ignore"), "echoed input");
    }

    fn sizes(previous_state: usize, state: usize, largest_event: usize) -> Sizes {
        Sizes {
            previous_state,
            state,
            largest_event,
        }
    }

    #[test]
    fn a_client_call_up_to_the_soft_limit_is_stored() {
        let soft = SOFT_ENTRY_BYTES;
        assert_eq!(
            decide_store(Work::Content, sizes(0, 10, 10)),
            StoreDecision::Store
        );
        assert_eq!(
            decide_store(Work::Content, sizes(0, soft, soft)),
            StoreDecision::Store
        );
    }

    #[test]
    fn a_client_call_over_the_soft_limit_that_grew_the_state_is_refused() {
        let soft = SOFT_ENTRY_BYTES;
        let decision = decide_store(Work::Content, sizes(soft, soft + 1, 10));
        assert_eq!(decision, StoreDecision::Refuse);
        let from_small = decide_store(Work::Content, sizes(10, soft + 1, 10));
        assert_eq!(from_small, StoreDecision::Refuse);
    }

    #[test]
    fn a_client_call_over_the_soft_limit_that_did_not_grow_the_state_is_stored() {
        let soft = SOFT_ENTRY_BYTES;
        let same = decide_store(Work::Content, sizes(soft + 1, soft + 1, 10));
        assert_eq!(same, StoreDecision::Store);
        let shrunk = decide_store(Work::Content, sizes(soft + 500, soft + 1, 10));
        assert_eq!(shrunk, StoreDecision::Store);
    }

    #[test]
    fn expiry_only_work_is_stored_over_the_soft_limit_up_to_the_hard_limit() {
        let (soft, hard) = (SOFT_ENTRY_BYTES, HARD_ENTRY_BYTES);
        let grown = sizes(soft, hard, soft + 1);
        assert_eq!(decide_store(Work::Plain, grown), StoreDecision::Store);
        let at_hard = sizes(0, hard, hard);
        assert_eq!(decide_store(Work::Plain, at_hard), StoreDecision::Store);
    }

    #[test]
    fn nothing_is_stored_over_the_hard_limit() {
        let hard = HARD_ENTRY_BYTES;
        assert!(hard < 2 * 1024 * 1024);
        let state = decide_store(Work::Plain, sizes(0, hard + 1, 10));
        assert_eq!(state, StoreDecision::OverHard);
        let event = decide_store(Work::Plain, sizes(0, 10, hard + 1));
        assert_eq!(event, StoreDecision::OverHard);
        let kept_big = decide_store(Work::Content, sizes(hard + 9, hard + 1, 10));
        assert_eq!(kept_big, StoreDecision::OverHard);
        let grown = decide_store(Work::Content, sizes(0, hard + 1, 10));
        assert_eq!(grown, StoreDecision::Refuse);
    }

    fn claim_with_summary(summary: &str, on_conflict: &str) -> ClientMsg {
        msg(&format!(
            r#"{{"type":"claim","req":1,"intent":{{"summary":"{summary}","task_ref":null}},
            "scopes":[{{"scope":{{"kind":"symbol","path":"a.rs","qualified_name":"f"}},
            "mode":"edit_signature"}}],"on_conflict":"{on_conflict}"}}"#
        ))
    }

    #[test]
    fn one_event_over_the_soft_limit_refuses_a_client_call_though_the_state_is_small() {
        let mut core = new_core();
        run(&mut core, &bound("a1"), claim("fail", "edit_signature"));
        let huge = "x".repeat(SOFT_ENTRY_BYTES + 1);
        let (_, effects) = run(&mut core, &bound("a2"), claim_with_summary(&huge, "fail"));
        let (events, _) = split_effects(effects);
        let entries = persist_entries(&core, &events).unwrap();
        let measured = entry_sizes(&entries, entries[0].1.len());
        assert!(measured.state < SOFT_ENTRY_BYTES);
        assert!(measured.largest_event > SOFT_ENTRY_BYTES);
        assert_eq!(decide_store(Work::Content, measured), StoreDecision::Refuse);
        assert_eq!(decide_store(Work::Plain, measured), StoreDecision::Store);
    }

    #[test]
    fn entry_sizes_take_the_state_first_and_the_largest_event() {
        let entry = |len: usize| ("k".to_string(), "x".repeat(len));
        assert_eq!(entry_sizes(&[], 5), sizes(5, 0, 0));
        assert_eq!(entry_sizes(&[entry(7)], 5), sizes(5, 7, 0));
        let many = [entry(100), entry(3), entry(9), entry(4)];
        assert_eq!(entry_sizes(&many, 0), sizes(0, 100, 9));
    }

    #[test]
    fn the_state_limit_reply_is_a_fixed_malformed_error_with_the_req() {
        let ServerMsg::Error { req, code, message } = state_limit_reply(Some(RequestId(116)))
        else {
            panic!("expected an error");
        };
        assert_eq!((req, code), (Some(RequestId(116)), ErrorCode::Malformed));
        assert_eq!(message, "repo state limit reached");
        let ServerMsg::Error { req, .. } = state_limit_reply(None) else {
            panic!("expected an error");
        };
        assert_eq!(req, None);
    }

    #[test]
    fn every_client_message_with_a_req_reports_it() {
        let with_req = [
            (claim("fail", "depend"), Some(1)),
            (
                msg(r#"{"type":"amend","req":3,"claim":1,"fence":1,"add":[]}"#),
                Some(3),
            ),
            (
                msg(r#"{"type":"submit","req":4,"claim":1,"fence":1,
                    "fork_commit":"c","touched":[]}"#),
                Some(4),
            ),
            (
                msg(
                    r#"{"type":"open_race","req":5,"intent":{"summary":"s","task_ref":null},
                "scopes":[],"max_entrants":1,"deadline_ms":1,"criteria":[]}"#,
                ),
                Some(5),
            ),
            (msg(r#"{"type":"join_race","req":6,"race":1}"#), Some(6)),
            (
                msg(r#"{"type":"pick_winner","req":7,"race":1,"claim":1}"#),
                Some(7),
            ),
            (
                msg(r#"{"type":"review","req":8,"claim":1,"approve":true,"note":null}"#),
                Some(8),
            ),
            (hello("a1"), None),
            (msg(r#"{"type":"heartbeat"}"#), None),
            (msg(r#"{"type":"release","claim":1,"fence":1}"#), None),
            (msg(r#"{"type":"watch","from_seq":0}"#), None),
        ];
        for (message, expected) in with_req {
            assert_eq!(req_of(&message), expected.map(RequestId), "{message:?}");
        }
    }

    fn waiting_msg(req: u64, path: &str, summary: String) -> ClientMsg {
        ClientMsg::Claim {
            req: RequestId(req),
            intent: Intent {
                summary,
                task_ref: None,
                assumptions: vec![],
            },
            scopes: vec![ScopeClaim {
                scope: Scope::File { path: path.into() },
                mode: Mode::EditBody,
            }],
            on_conflict: OnConflict::Wait,
        }
    }

    fn measured(core: &Core, effects: Vec<Effect>, previous: usize) -> Sizes {
        let (events, _) = split_effects(effects);
        entry_sizes(&persist_entries(core, &events).unwrap(), previous)
    }

    /// A core one root claim holds against 50 waiters, filled to 50 bytes under the soft limit,
    /// and its stored JSON. Letting the root claim go grants every waiter, which grows the state.
    fn near_soft_core() -> (Core, String) {
        let mut core = new_core();
        let root = ScopeClaim {
            scope: Scope::Dir {
                path: String::new(),
            },
            mode: Mode::EditBody,
        };
        let holder = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent {
                summary: "holds the root".into(),
                task_ref: None,
                assumptions: vec![],
            },
            scopes: vec![root],
            on_conflict: OnConflict::Fail,
        };
        core.handle(&agent("a"), holder, NOW);
        for index in 0..50 {
            let waiter = waiting_msg(10 + index, &format!("f{index}.rs"), "w".into());
            core.handle(&agent(&format!("w{index}")), waiter, NOW);
        }
        let mut probe = core.clone();
        probe.handle(
            &agent("fill"),
            waiting_msg(99, "fill.rs", String::new()),
            NOW,
        );
        let base = serde_json::to_string(&probe).unwrap().len();
        let filler = "x".repeat(SOFT_ENTRY_BYTES - 50 - base);
        core.handle(&agent("fill"), waiting_msg(99, "fill.rs", filler), NOW);
        let stored = serde_json::to_string(&core).unwrap();
        assert_eq!(stored.len(), SOFT_ENTRY_BYTES - 50);
        (core, stored)
    }

    #[test]
    fn a_release_whose_grants_grow_the_state_over_soft_is_stored_but_a_claim_is_not() {
        let (mut core, stored) = near_soft_core();
        let release = ClientMsg::Release {
            claim: ClaimId(1),
            fence: Fence(1),
            req: None,
        };
        assert_eq!(work_of(&release), Work::Plain);
        let effects = core.handle(&agent("a"), release, NOW);
        let grown = measured(&core, effects, stored.len());
        assert!(
            grown.state > SOFT_ENTRY_BYTES && grown.state > stored.len(),
            "{grown:?}"
        );
        assert_eq!(
            decide_store(work_of(&ClientMsg::Heartbeat), grown),
            StoreDecision::Store
        );
        assert_eq!(decide_store(Work::Content, grown), StoreDecision::Refuse);
    }

    #[test]
    fn expiry_that_grows_the_state_past_the_soft_limit_never_wedges_the_repo() {
        let (core, stored) = near_soft_core();
        let late = NOW + LEASE_MS;
        let mut client = core.clone();
        let claim = waiting_msg(500, "zz.rs", "z".into());
        assert_eq!(work_of(&claim), Work::Content);
        let effects = client.handle(&agent("zz"), claim, late);
        let refused = measured(&client, effects, stored.len());
        assert!(refused.state > SOFT_ENTRY_BYTES, "{refused:?}");
        assert_eq!(decide_store(Work::Content, refused), StoreDecision::Refuse);

        let mut recovered = load_core(Some(&stored), None, None).unwrap();
        let effects = recovered.expire(late);
        let freed = measured(&recovered, effects, stored.len());
        assert_eq!(decide_store(Work::Plain, freed), StoreDecision::Store);
        let after_expiry = serde_json::to_string(&recovered).unwrap();
        assert_eq!(after_expiry.len(), freed.state);

        let release = ClientMsg::Release {
            claim: ClaimId(2),
            fence: Fence(2),
            req: None,
        };
        let effects = recovered.handle(&agent("w0"), release, late);
        let after_release = measured(&recovered, effects, after_expiry.len());
        assert!(after_release.state < after_expiry.len());
        assert_eq!(
            decide_store(Work::Content, after_release),
            StoreDecision::Store
        );
        assert_eq!(
            decide_store(Work::Plain, after_release),
            StoreDecision::Store
        );
        let effects = recovered.expire(late + 1);
        let next = measured(&recovered, effects, after_release.state);
        assert_eq!(decide_store(Work::Plain, next), StoreDecision::Store);
    }

    #[test]
    fn a_content_message_that_refills_what_expiry_freed_is_refused_after_the_expiry_step() {
        const LEN: usize = 2_000;
        let mut core = new_core();
        let held = |summary: String, path: &str| waiting_msg(1, path, summary);
        core.handle(&agent("a"), held("x".repeat(LEN), "x.rs"), NOW);
        let mut probe = core.clone();
        probe.handle(&agent("f"), held(String::new(), "f.rs"), NOW + 1);
        let base = serde_json::to_string(&probe).unwrap().len();
        let filler = "f".repeat(SOFT_ENTRY_BYTES + 100 - base);
        core.handle(&agent("f"), held(filler, "f.rs"), NOW + 1);
        core.handle(&agent("f"), ClientMsg::Heartbeat, NOW + 1);
        let stored = serde_json::to_string(&core).unwrap();
        assert_eq!(stored.len(), SOFT_ENTRY_BYTES + 100);

        let late = NOW + LEASE_MS;
        let refill = held("w".repeat(LEN - 2), "y.rs");
        let size = StoredSize::on_load(Some(&stored));

        let mut together = core.clone();
        let effects = together.handle(&agent("w"), refill.clone(), late);
        let (events, _) = split_effects(effects);
        let entries = persist_entries(&together, &events).unwrap();
        assert!(
            entries[0].1.len() <= stored.len(),
            "the lapsed claim is refilled"
        );
        let joint = size.judge(work_of(&refill), &entries);
        assert_eq!(
            joint,
            StoreDecision::Store,
            "judged with its own expiry it looks like no growth"
        );

        let mut stepped = core.clone();
        assert!(stepped.has_due_expiry(late));
        let effects = stepped.expire(late);
        let (events, _) = split_effects(effects);
        let expiry = persist_entries(&stepped, &events).unwrap();
        assert_eq!(size.judge(Work::Plain, &expiry), StoreDecision::Store);
        let size = size.on_write(&expiry);
        assert!(
            !stepped.has_due_expiry(late),
            "the message's own lazy expiry is a no-op"
        );
        let effects = stepped.handle(&agent("w"), refill.clone(), late);
        let (events, _) = split_effects(effects);
        let message = persist_entries(&stepped, &events).unwrap();
        assert!(message[0].1.len() > SOFT_ENTRY_BYTES);
        assert_eq!(
            size.judge(work_of(&refill), &message),
            StoreDecision::Refuse
        );
    }

    #[test]
    fn only_messages_that_add_content_are_judged_by_the_soft_limit() {
        let content = [
            claim("fail", "depend"),
            msg(r#"{"type":"amend","req":3,"claim":1,"fence":1,"add":[]}"#),
            msg(r#"{"type":"submit","req":4,"claim":1,"fence":1,
                "fork_commit":"c","touched":[]}"#),
        ];
        for message in &content {
            assert_eq!(work_of(message), Work::Content, "{message:?}");
        }
        let plain = [
            hello("a1"),
            msg(r#"{"type":"heartbeat"}"#),
            msg(r#"{"type":"release","claim":1,"fence":1}"#),
            msg(
                r#"{"type":"open_race","req":5,"intent":{"summary":"s","task_ref":null},
                "scopes":[],"max_entrants":1,"deadline_ms":1,"criteria":[]}"#,
            ),
            msg(r#"{"type":"join_race","req":6,"race":1}"#),
            msg(r#"{"type":"pick_winner","req":7,"race":1,"claim":1}"#),
            msg(r#"{"type":"review","req":8,"claim":1,"approve":true,"note":null}"#),
            msg(r#"{"type":"watch","from_seq":0}"#),
        ];
        for message in &plain {
            assert_eq!(work_of(message), Work::Plain, "{message:?}");
        }
    }

    #[test]
    fn over_the_soft_limit_a_hello_that_grows_the_state_by_a_byte_is_stored() {
        let soft = SOFT_ENTRY_BYTES;
        let grown = sizes(soft + 5, soft + 6, 10);
        assert_eq!(
            decide_store(work_of(&hello("a1")), grown),
            StoreDecision::Store
        );
        for message in [
            claim("fail", "depend"),
            msg(r#"{"type":"amend","req":3,"claim":1,"fence":1,"add":[]}"#),
            msg(r#"{"type":"submit","req":4,"claim":1,"fence":1,
                "fork_commit":"c","touched":[]}"#),
        ] {
            assert_eq!(
                decide_store(work_of(&message), grown),
                StoreDecision::Refuse
            );
        }
    }

    #[test]
    fn a_content_message_that_does_not_grow_the_state_is_stored_over_the_soft_limit() {
        let soft = SOFT_ENTRY_BYTES;
        let same = sizes(soft + 5, soft + 5, 10);
        assert_eq!(
            decide_store(work_of(&claim("fail", "depend")), same),
            StoreDecision::Store
        );
    }

    #[test]
    fn every_kind_of_work_is_refused_or_failed_over_the_hard_limit() {
        let hard = HARD_ENTRY_BYTES;
        for work in [Work::Content, Work::Plain] {
            let grown = decide_store(work, sizes(0, hard + 1, 10));
            assert_ne!(grown, StoreDecision::Store, "{work:?}");
            let event = decide_store(work, sizes(0, 10, hard + 1));
            assert_ne!(event, StoreDecision::Store, "{work:?}");
        }
    }

    fn state_entries(len: usize) -> Vec<(String, String)> {
        vec![(STATE_KEY.to_string(), "x".repeat(len))]
    }

    #[test]
    fn with_nothing_stored_the_size_is_zero_and_a_small_first_write_is_stored() {
        let size = StoredSize::on_load(None);
        assert_eq!(size, StoredSize::default());
        assert_eq!(
            size.judge(Work::Content, &state_entries(10)),
            StoreDecision::Store
        );
        let over = state_entries(SOFT_ENTRY_BYTES + 1);
        assert_eq!(size.judge(Work::Content, &over), StoreDecision::Refuse);
    }

    #[test]
    fn a_loaded_state_sets_the_size_growth_is_compared_with() {
        let stored = "x".repeat(SOFT_ENTRY_BYTES + 10);
        let size = StoredSize::on_load(Some(&stored));
        let same = state_entries(SOFT_ENTRY_BYTES + 10);
        assert_eq!(size.judge(Work::Content, &same), StoreDecision::Store);
        let grown = state_entries(SOFT_ENTRY_BYTES + 11);
        assert_eq!(size.judge(Work::Content, &grown), StoreDecision::Refuse);
    }

    #[test]
    fn after_a_write_the_next_decision_compares_with_the_new_size() {
        let size = StoredSize::on_load(None);
        let big = state_entries(SOFT_ENTRY_BYTES + 100);
        let size = size.on_write(&big);
        assert_eq!(size.judge(Work::Content, &big), StoreDecision::Store);
        let grown = state_entries(SOFT_ENTRY_BYTES + 101);
        assert_eq!(size.judge(Work::Content, &grown), StoreDecision::Refuse);
        let shrunk = size.on_write(&state_entries(10));
        let regrown = state_entries(SOFT_ENTRY_BYTES + 1);
        assert_eq!(shrunk.judge(Work::Content, &regrown), StoreDecision::Refuse);
        assert_eq!(size.on_write(&[]), size, "an empty write changes nothing");
    }

    #[test]
    fn a_refusal_leaves_the_size_unchanged() {
        let size = StoredSize::on_load(Some("abc"));
        let over = state_entries(SOFT_ENTRY_BYTES + 1);
        assert_eq!(size.judge(Work::Content, &over), StoreDecision::Refuse);
        assert_eq!(size, StoredSize::on_load(Some("abc")));
        assert_eq!(size.judge(Work::Content, &over), StoreDecision::Refuse);
    }

    fn multibyte_hello(bytes: usize) -> String {
        let base = r#"{"type":"hello","agent":"é","base":"abc"}"#;
        format!("{base}{}", " ".repeat(bytes - base.len()))
    }

    #[test]
    fn the_frame_limit_counts_bytes_not_characters() {
        let at = multibyte_hello(MAX_FRAME_BYTES);
        assert_eq!(at.len(), MAX_FRAME_BYTES);
        assert!(at.chars().count() < MAX_FRAME_BYTES);
        assert!(parse_client_msg(&at).is_ok());
        let two_byte = "é".repeat(MAX_FRAME_BYTES / 2 + 1);
        assert!(two_byte.chars().count() < MAX_FRAME_BYTES);
        assert_eq!(
            parse_client_msg(&two_byte).unwrap_err(),
            FrameFault::TooLarge
        );
    }

    #[test]
    fn a_watch_start_is_kept_up_to_the_largest_exact_integer() {
        assert_eq!(MAX_SAFE_SEQ, 9_007_199_254_740_991);
        assert_eq!(clamp_watch_from(0), 0);
        assert_eq!(clamp_watch_from(MAX_SAFE_SEQ - 1), MAX_SAFE_SEQ - 1);
        assert_eq!(clamp_watch_from(MAX_SAFE_SEQ), MAX_SAFE_SEQ);
        assert_eq!(clamp_watch_from(MAX_SAFE_SEQ + 1), MAX_SAFE_SEQ);
        assert_eq!(clamp_watch_from(u64::MAX), MAX_SAFE_SEQ);
    }

    #[test]
    fn a_watch_with_a_huge_from_seq_is_served_from_the_clamped_start() {
        let huge = msg(&format!(r#"{{"type":"watch","from_seq":{}}}"#, u64::MAX));
        let Action::Watch { from_seq } = decide(&Session::default(), &huge) else {
            panic!("expected a watch");
        };
        assert_eq!(from_seq, MAX_SAFE_SEQ);
    }

    #[test]
    fn delivery_reads_sessions_only_for_notifications_and_events() {
        let reply = Outbound::Reply(binary_rejection());
        assert!(!needs_sessions(&[], &[]));
        assert!(!needs_sessions(&[reply.clone(), reply.clone()], &[]));
        assert!(needs_sessions(&[reply.clone(), queued("a2")], &[]));
        assert!(needs_sessions(&[reply], &[event_at(0)]));
        assert!(needs_sessions(&[], &[event_at(0)]));
    }

    #[test]
    fn a_closing_socket_withdraws_its_agent_only_when_it_was_the_last_one() {
        let closing = bound("a1");
        let queued = |_: &AgentId| true;
        assert_eq!(agent_to_withdraw(&closing, &[], queued), Some(agent("a1")));
        let elsewhere = [bound("a2"), watcher(), Session::default()];
        let last = agent_to_withdraw(&closing, &elsewhere, queued);
        assert_eq!(last, Some(agent("a1")));
        let twin = [bound("a2"), bound("a1")];
        assert_eq!(agent_to_withdraw(&closing, &twin, queued), None);
    }

    #[test]
    fn closing_an_unbound_socket_withdraws_nobody() {
        let queued = |_: &AgentId| true;
        assert_eq!(agent_to_withdraw(&Session::default(), &[], queued), None);
        assert_eq!(agent_to_withdraw(&watcher(), &[bound("a1")], queued), None);
    }

    #[test]
    fn a_close_of_an_agent_with_no_queued_request_withdraws_nobody() {
        let queued_only_a2 = |who: &AgentId| *who == agent("a2");
        assert_eq!(agent_to_withdraw(&bound("a1"), &[], queued_only_a2), None);
        let to_withdraw = agent_to_withdraw(&bound("a2"), &[], queued_only_a2);
        assert_eq!(to_withdraw, Some(agent("a2")));
    }
}

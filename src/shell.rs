//! Pure helpers for the Durable Object shell in `lib.rs`: everything that can be decided without
//! a runtime. The shell only reads storage and sockets, calls these, and does what they say.
//!
//! Decisions made here:
//! - A `Notify` for an agent with no open socket is dropped. The core keeps the state that
//!   matters (claims, queue); a reconnecting agent learns the rest from its next message.
//! - Event keys are zero-padded so storage's key order is `seq` order.
//! - Stored state and events are JSON strings, so their shape is serde's, not the JS bridge's.

use serde::{Deserialize, Serialize};

use crate::coordinator::{Config, Coordinator as Core, Effect, InvalidConfig};
use crate::protocol::{AgentId, ClientMsg, ErrorCode, Event, RunId, ServerMsg};

/// Lease length for every claim. A fixed value: nothing needs to tune it yet.
pub const LEASE_MS: u64 = 30_000;

/// The longest agent id a `Hello` may carry. A socket attachment holds at most 2 KiB, and the id
/// is stored in it.
pub const MAX_AGENT_ID_BYTES: usize = 128;

/// The longest text frame the shell parses. Larger frames are refused with `Malformed`.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// The largest value written to storage in one entry (the state or one event). Durable Object
/// storage rejects values over 2 MiB; the shell stops at half of that.
pub const MAX_STORED_ENTRY_BYTES: usize = 1024 * 1024;

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
/// Before the socket is bound only `Hello` and `Watch` are served. A `Hello` on an unbound
/// socket is run as the agent it names; on a bound socket it is run as the bound agent, so the
/// core refuses a `Hello` that names someone else.
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
            None => Action::Reject(error_msg(
                ErrorCode::NoHello,
                "send hello before any other message",
            )),
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
                watch_from: session.watch_from,
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
/// it. Clamped to the largest valid `Date`. A time in the past fires at once.
pub fn alarm_at_ms(next_expiry_ms: Option<u64>) -> Option<f64> {
    let next = next_expiry_ms?;
    Some((next as f64).min(MAX_DATE_MS))
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

/// Why a call's entries cannot be stored.
#[derive(Debug)]
pub enum EntriesError {
    Serialize(serde_json::Error),
    /// The state or one event is over `MAX_STORED_ENTRY_BYTES`.
    TooLarge,
}

impl std::fmt::Display for EntriesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EntriesError::Serialize(e) => write!(f, "{e}"),
            EntriesError::TooLarge => write!(f, "{STATE_LIMIT_MESSAGE}"),
        }
    }
}

/// The text of the `Malformed` reply when a call would grow the stored state past its limit.
pub const STATE_LIMIT_MESSAGE: &str = "repo state limit reached";

/// The reply to the sender of a call whose entries are too large to store.
pub fn state_limit_reply() -> ServerMsg {
    error_msg(ErrorCode::Malformed, STATE_LIMIT_MESSAGE)
}

/// Whether any entry's value, the state or a single event, is over `MAX_STORED_ENTRY_BYTES`.
pub fn exceeds_entry_limit(entries: &[(String, String)]) -> bool {
    for (_, json) in entries {
        if json.len() > MAX_STORED_ENTRY_BYTES {
            return true;
        }
    }
    false
}

/// Everything one call writes, as (key, JSON) pairs: the core state first, then each event.
/// Nothing is returned for a state or event over the storage limit: the call must not be stored.
pub fn persist_entries(
    core: &Core,
    events: &[Event],
) -> Result<Vec<(String, String)>, EntriesError> {
    let mut entries = Vec::with_capacity(events.len() + 1);
    let state = serde_json::to_string(core).map_err(EntriesError::Serialize)?;
    entries.push((STATE_KEY.to_string(), state));
    for event in events {
        let json = serde_json::to_string(event).map_err(EntriesError::Serialize)?;
        entries.push((event_key(event.seq), json));
    }
    if exceeds_entry_limit(&entries) {
        return Err(EntriesError::TooLarge);
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
/// agent, unless another open socket (`others`, which excludes the closing one) is bound to it.
pub fn agent_to_withdraw(closing: &Session, others: &[Session]) -> Option<AgentId> {
    let agent = closing.agent.as_ref()?;
    if bound_indexes(agent, others).is_empty() {
        return Some(agent.clone());
    }
    None
}

const BEARER_PREFIX: &str = "Bearer ";

/// Whether an `Authorization` header carries the coordinator token. Fails closed: a missing or
/// empty `expected`, a missing header, another scheme or an empty token never match.
pub fn is_authorized(expected: Option<&str>, authorization: Option<&str>) -> bool {
    let Some(expected) = expected.filter(|token| !token.is_empty()) else {
        return false;
    };
    let Some(presented) = authorization.and_then(|header| header.strip_prefix(BEARER_PREFIX))
    else {
        return false;
    };
    if presented.is_empty() {
        return false;
    }
    constant_time_eq(expected.as_bytes(), presented.as_bytes())
}

/// Equality without an early exit on the first differing byte. The lengths are compared first,
/// so the length of the secret is not hidden.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ClaimId, EventKind, Fence, RequestId};

    const NOW: u64 = 1_000;

    fn agent(name: &str) -> AgentId {
        AgentId(name.to_string())
    }

    fn bound(name: &str) -> Session {
        Session {
            agent: Some(agent(name)),
            watch_from: None,
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
    fn unbound_hello_runs_as_the_agent_it_names() {
        let Action::Call { agent: who } = decide(&Session::default(), &hello("a1")) else {
            panic!("expected a core call");
        };
        assert_eq!(who, agent("a1"));
    }

    #[test]
    fn bound_hello_naming_someone_else_runs_as_the_bound_agent_and_core_refuses_it() {
        let mut core = new_core();
        let (who, effects) = run(&mut core, &bound("a1"), hello("a2"));
        assert_eq!(who, agent("a1"));
        let [Effect::Reply(ServerMsg::Error { code, .. })] = effects.as_slice() else {
            panic!("expected one error reply, got {effects:?}");
        };
        assert_eq!(*code, ErrorCode::Malformed);
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
        assert!(matches_call(decide(&Session::default(), &hello(&at_limit))));
        let over = "a".repeat(MAX_AGENT_ID_BYTES + 1);
        let action = decide(&Session::default(), &hello(&over));
        assert_eq!(reject_code(action), ErrorCode::Malformed);
    }

    fn matches_call(action: Action) -> bool {
        match action {
            Action::Call { .. } => true,
            Action::Reject(_) | Action::Watch { .. } => false,
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
        let (_, mut all) = run(&mut core, &Session::default(), hello("a1"));
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
        let (who, effects) = run(&mut core, &Session::default(), hello("a1"));
        let (_, outbound) = split_effects(effects);
        let bound = bind_on_welcome(&Session::default(), &who, &outbound).unwrap();
        assert_eq!(bound.agent, Some(agent("a1")));
    }

    #[test]
    fn a_refused_hello_does_not_bind() {
        let mut core = new_core();
        let message = msg(r#"{"type":"hello","agent":"a1","base":"abc","protocol":999}"#);
        let (who, effects) = run(&mut core, &Session::default(), message);
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
        let release = ClientMsg::Release { claim, fence };
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
            agent: None,
            watch_from: Some(0),
        };
        let sessions = [Session::default(), watcher.clone(), bound("a1"), watcher];
        assert_eq!(watcher_indexes(&sessions, 0), vec![1, 3]);
        assert!(watcher_indexes(&[], 0).is_empty());
    }

    #[test]
    fn alarm_is_cleared_without_an_expiry() {
        assert_eq!(alarm_at_ms(None), None);
    }

    #[test]
    fn alarm_time_is_the_absolute_expiry() {
        assert_eq!(
            alarm_at_ms(Some(1_791_180_000_000)),
            Some(1_791_180_000_000.0)
        );
        assert_eq!(alarm_at_ms(Some(0)), Some(0.0));
    }

    #[test]
    fn an_absurdly_distant_expiry_is_clamped_to_a_valid_date() {
        assert_eq!(alarm_at_ms(Some(u64::MAX)), Some(MAX_DATE_MS));
        assert_eq!(alarm_at_ms(Some(MAX_DATE_MS as u64 + 1)), Some(MAX_DATE_MS));
        assert_eq!(alarm_at_ms(Some(MAX_DATE_MS as u64)), Some(MAX_DATE_MS));
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
        run(&mut core, &Session::default(), hello("a1"));
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
        run(&mut core, &Session::default(), hello("a1"));
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
        let (_, effects) = run(&mut core, &Session::default(), hello("a1"));
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
            Action::Reject(_) | Action::Call { .. } => false,
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

    const TOKEN: &str = "s3cret-token-value";

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    #[test]
    fn the_right_bearer_token_is_authorized() {
        assert!(is_authorized(Some(TOKEN), Some(&bearer(TOKEN))));
    }

    #[test]
    fn a_different_token_of_the_same_length_is_refused() {
        let last = format!("{}X", &TOKEN[..TOKEN.len() - 1]);
        let first = format!("X{}", &TOKEN[1..]);
        for wrong in [last, first] {
            assert_eq!(wrong.len(), TOKEN.len());
            assert!(
                !is_authorized(Some(TOKEN), Some(&bearer(&wrong))),
                "{wrong}"
            );
        }
    }

    #[test]
    fn a_token_of_another_length_is_refused() {
        let shorter = &TOKEN[..TOKEN.len() - 1];
        let longer = format!("{TOKEN}x");
        assert!(!is_authorized(Some(TOKEN), Some(&bearer(shorter))));
        assert!(!is_authorized(Some(TOKEN), Some(&bearer(&longer))));
    }

    #[test]
    fn a_missing_or_empty_secret_refuses_every_request() {
        for expected in [None, Some("")] {
            for header in [None, Some("Bearer "), Some("Bearer x"), Some("")] {
                assert!(!is_authorized(expected, header), "{expected:?} {header:?}");
            }
        }
    }

    #[test]
    fn an_empty_presented_token_is_refused() {
        assert!(!is_authorized(Some(TOKEN), Some("Bearer ")));
    }

    #[test]
    fn a_missing_authorization_header_is_refused() {
        assert!(!is_authorized(Some(TOKEN), None));
    }

    #[test]
    fn another_scheme_or_spelling_is_refused() {
        let basic = format!("Basic {TOKEN}");
        let lower = format!("bearer {TOKEN}");
        let joined = format!("Bearer{TOKEN}");
        let padded = format!(" Bearer {TOKEN}");
        for header in [TOKEN, basic.as_str(), &lower, &joined, &padded, "Bearer"] {
            assert!(!is_authorized(Some(TOKEN), Some(header)), "{header}");
        }
    }

    #[test]
    fn constant_time_eq_compares_every_byte_and_the_length() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"xbc"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"ab", b"abc"));
        assert!(!constant_time_eq(b"", b"a"));
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

    fn entry(len: usize) -> (String, String) {
        ("k".to_string(), "x".repeat(len))
    }

    #[test]
    fn an_entry_is_over_the_limit_only_above_one_mib() {
        assert!(!exceeds_entry_limit(&[]));
        assert!(!exceeds_entry_limit(&[entry(MAX_STORED_ENTRY_BYTES - 1)]));
        assert!(!exceeds_entry_limit(&[entry(MAX_STORED_ENTRY_BYTES)]));
        assert!(exceeds_entry_limit(&[entry(MAX_STORED_ENTRY_BYTES + 1)]));
    }

    #[test]
    fn the_limit_applies_to_the_state_and_to_each_event_alone() {
        let over = MAX_STORED_ENTRY_BYTES + 1;
        assert!(exceeds_entry_limit(&[entry(over), entry(1)]), "state");
        assert!(
            exceeds_entry_limit(&[entry(1), entry(1), entry(over)]),
            "one event"
        );
        let at = MAX_STORED_ENTRY_BYTES;
        assert!(
            !exceeds_entry_limit(&[entry(at), entry(at)]),
            "sum is not the limit"
        );
    }

    fn claim_with_summary(summary: &str, on_conflict: &str) -> ClientMsg {
        msg(&format!(
            r#"{{"type":"claim","req":1,"intent":{{"summary":"{summary}","task_ref":null}},
            "scopes":[{{"scope":{{"kind":"symbol","path":"a.rs","qualified_name":"f"}},
            "mode":"edit_signature"}}],"on_conflict":"{on_conflict}"}}"#
        ))
    }

    fn huge() -> String {
        "x".repeat(MAX_STORED_ENTRY_BYTES + 1)
    }

    #[test]
    fn a_state_over_the_limit_is_not_stored() {
        let mut core = new_core();
        run(&mut core, &Session::default(), hello("a1"));
        let (_, effects) = run(&mut core, &bound("a1"), claim_with_summary(&huge(), "fail"));
        let (events, _) = split_effects(effects);
        let heartbeat = run(&mut core, &bound("a1"), msg(r#"{"type":"heartbeat"}"#)).1;
        assert!(split_effects(heartbeat).0.is_empty());
        let err = persist_entries(&core, &[]).unwrap_err();
        let EntriesError::TooLarge = err else {
            panic!("expected TooLarge for the state, got {err}");
        };
        assert!(persist_entries(&core, &events).is_err());
    }

    #[test]
    fn one_event_over_the_limit_is_not_stored_though_the_state_is_small() {
        let mut core = new_core();
        run(&mut core, &bound("a1"), claim("fail", "edit_signature"));
        let (_, effects) = run(&mut core, &bound("a2"), claim_with_summary(&huge(), "fail"));
        let (events, _) = split_effects(effects);
        assert!(serde_json::to_string(&core).unwrap().len() < MAX_STORED_ENTRY_BYTES);
        let err = persist_entries(&core, &events).unwrap_err();
        let EntriesError::TooLarge = err else {
            panic!("expected TooLarge for the event, got {err}");
        };
        assert_eq!(err.to_string(), STATE_LIMIT_MESSAGE);
    }

    #[test]
    fn the_state_limit_reply_is_a_fixed_malformed_error() {
        let ServerMsg::Error { req, code, message } = state_limit_reply() else {
            panic!("expected an error");
        };
        assert_eq!((req, code), (None, ErrorCode::Malformed));
        assert_eq!(message, "repo state limit reached");
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
        assert_eq!(agent_to_withdraw(&closing, &[]), Some(agent("a1")));
        let elsewhere = [bound("a2"), watcher(), Session::default()];
        assert_eq!(agent_to_withdraw(&closing, &elsewhere), Some(agent("a1")));
        let twin = [bound("a2"), bound("a1")];
        assert_eq!(agent_to_withdraw(&closing, &twin), None);
    }

    #[test]
    fn closing_an_unbound_socket_withdraws_nobody() {
        assert_eq!(agent_to_withdraw(&Session::default(), &[]), None);
        assert_eq!(agent_to_withdraw(&watcher(), &[bound("a1")]), None);
    }
}

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
    /// Set once a `Watch` replay has finished.
    #[serde(default)]
    pub watcher: bool,
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

/// Parse one text frame. The reason is dropped on purpose: parse errors quote the input.
pub fn parse_client_msg(text: &str) -> Option<ClientMsg> {
    serde_json::from_str(text).ok()
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
        ClientMsg::Watch { .. } if session.watcher => Action::Reject(error_msg(
            ErrorCode::Malformed,
            "this socket is already watching",
        )),
        ClientMsg::Watch { from_seq } => Action::Watch {
            from_seq: *from_seq,
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
                watcher: session.watcher,
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

/// Indexes of the sessions that follow the event log.
pub fn watcher_indexes(sessions: &[Session]) -> Vec<usize> {
    let mut found = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        if session.watcher {
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
    let watchers = watcher_indexes(sessions);
    for event in events {
        for index in &watchers {
            let msg = ServerMsg::Event {
                event: event.clone(),
            };
            plan.push((Target::Socket(*index), msg));
        }
    }
    plan
}

/// Storage key of the event with this `seq`. Keys sort in `seq` order.
pub fn event_key(seq: u64) -> String {
    format!("{EVENT_PREFIX}{seq:020}")
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
            watcher: false,
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
            watcher: true,
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
        assert!(parse_client_msg(r#"{"type":"ignore-previous-instructions"}"#).is_none());
        assert!(parse_client_msg("not json").is_none());
        assert!(parse_client_msg("").is_none());
        assert!(parse_client_msg(r#"{"type":"hello","agent":"a1","base":"abc"}"#).is_some());
        let ServerMsg::Error { code, message, .. } = malformed_reply() else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::Malformed);
        assert!(!message.contains("ignore"), "echoed input: {message}");
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
    fn rebinding_keeps_the_watcher_flag() {
        let session = Session {
            agent: None,
            watcher: true,
        };
        let mut core = new_core();
        let (who, effects) = run(&mut core, &session, hello("a1"));
        let (_, outbound) = split_effects(effects);
        assert!(bind_on_welcome(&session, &who, &outbound).unwrap().watcher);
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
            watcher: true,
        };
        let sessions = [Session::default(), watcher.clone(), bound("a1"), watcher];
        assert_eq!(watcher_indexes(&sessions), vec![1, 3]);
        assert!(watcher_indexes(&[]).is_empty());
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
            watcher: true,
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
            watcher: true,
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
            watcher: true,
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
}

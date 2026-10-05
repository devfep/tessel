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
pub fn config_from_vars(run: Option<&str>) -> Result<Config, LoadError> {
    let Some(run) = run.filter(|run| !run.is_empty()) else {
        return Err(LoadError::MissingRun);
    };
    Ok(Config {
        run: RunId(run.to_string()),
        lease_ms: LEASE_MS,
    })
}

/// Restore the core from its stored state, or create it when nothing is stored yet.
pub fn load_core(stored: Option<&str>, run: Option<&str>) -> Result<Core, LoadError> {
    let Some(stored) = stored else {
        let config = config_from_vars(run)?;
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
        ClientMsg::Watch { from_seq } => Action::Watch {
            from_seq: *from_seq,
        },
        ClientMsg::Hello { agent, .. } => {
            if agent.0.len() > MAX_AGENT_ID_BYTES {
                let message = format!("agent id is longer than {MAX_AGENT_ID_BYTES} bytes");
                return Action::Reject(error_msg(ErrorCode::Malformed, &message));
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

/// Milliseconds from now until the alarm should fire, or `None` to clear it. An expiry that is
/// already due fires at once.
pub fn alarm_offset_ms(next_expiry_ms: Option<u64>, now_ms: u64) -> Option<i64> {
    let next = next_expiry_ms?;
    let offset = next.saturating_sub(now_ms);
    Some(i64::try_from(offset).unwrap_or(i64::MAX))
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
        load_core(None, Some("test")).unwrap()
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
        run(&mut core, &Session::default(), hello("a1"));
        let (_, effects) = run(&mut core, &bound("a1"), claim("fail", "edit_signature"));
        let (_, effects2) = run(&mut core, &bound("a2"), claim("wait", "depend"));
        let count = effects.len() + effects2.len();
        let mut all = effects;
        all.extend(effects2);
        let (events, outbound) = split_effects(all);
        assert!(events.len() + outbound.len() == count);
        assert!(!events.is_empty() && !outbound.is_empty());
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted);
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
    fn alarm_is_cleared_without_an_expiry_and_offset_otherwise() {
        assert_eq!(alarm_offset_ms(None, NOW), None);
        assert_eq!(alarm_offset_ms(Some(1_500), NOW), Some(500));
    }

    #[test]
    fn a_due_or_past_expiry_fires_immediately() {
        assert_eq!(alarm_offset_ms(Some(NOW), NOW), Some(0));
        assert_eq!(alarm_offset_ms(Some(NOW - 1), NOW), Some(0));
        assert_eq!(alarm_offset_ms(Some(0), u64::MAX), Some(0));
    }

    #[test]
    fn an_absurdly_distant_expiry_does_not_overflow() {
        assert_eq!(alarm_offset_ms(Some(u64::MAX), 0), Some(i64::MAX));
    }

    #[test]
    fn event_keys_sort_in_seq_order() {
        let seqs = [0, 1, 9, 10, 99, 100, 12_345, u64::MAX];
        let keys: Vec<String> = seqs.iter().map(|s| event_key(*s)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert!(keys.iter().all(|k| k.starts_with(EVENT_PREFIX)));
        assert!(event_key(5) >= event_key(0) && event_key(4) < event_key(5));
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

        let mut restored = load_core(Some(&stored), Some("ignored")).unwrap();
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
        let core = load_core(None, Some("dogfood")).unwrap();
        let json = serde_json::to_string(&core).unwrap();
        assert!(json.contains(r#""run":"dogfood""#), "{json}");
        assert!(
            json.contains(&format!(r#""lease_ms":{LEASE_MS}"#)),
            "{json}"
        );
    }

    #[test]
    fn missing_or_empty_run_fails_loudly() {
        assert_eq!(load_core(None, None).unwrap_err(), LoadError::MissingRun);
        assert_eq!(
            load_core(None, Some("")).unwrap_err(),
            LoadError::MissingRun
        );
        assert!(config_from_vars(None).is_err());
    }

    #[test]
    fn corrupt_stored_state_fails_without_starting_empty_or_echoing_it() {
        let err = load_core(Some(r#"{"claims":"secret-intent-text""#), Some("r")).unwrap_err();
        let LoadError::Corrupt { .. } = err else {
            panic!("expected Corrupt, got {err:?}");
        };
        assert!(!err.to_string().contains("secret"));
        assert!(err.to_string().contains("refusing to start empty"));
        assert!(load_core(Some(""), Some("r")).is_err());
        assert!(load_core(Some("{}"), Some("r")).is_err());
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
}

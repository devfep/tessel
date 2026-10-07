//! Keeping `head` current when main moves without the coordinator: an admin merge goes around the
//! merge queue, so only the steward knows the new trunk head.
//!
//! The steward's push event reaches the coordinator as a bare poke (`trunk_moved`) that carries no
//! data, so nothing in the request can set the head: it only raises `head_sync_due`, a persisted
//! flag, and the alarm does the rest. The shell asks the steward for the trunk head through its
//! binding-only merge service, then offers the answer to `trunk_read`.
//!
//! Decisions made here:
//! - The read is a compare-and-set. The Durable Object handles other events while it waits for the
//!   steward, so `trunk_read` applies the answer only if no merge is in flight and `head` is still
//!   what it was when the read began. Otherwise nothing changes and the flag stays, so the next
//!   alarm reads again. A merge that is in flight is not waited for: its own landing sets `head`,
//!   and the flag then makes the alarm check that the trunk has not moved past it.
//! - An answer that is the current head, or no usable head (the call failed, or the answer was
//!   empty or all zeros), changes nothing but clears the flag. Keeping it would retry a failing
//!   steward in a hot loop; the next push to main raises the flag again.
//! - The move is logged as `BaseMoved { by: "steward", notified: [] }`. Nobody is notified: the
//!   touched scopes of an admin merge are unknown to the coordinator, and claims are not touched.

use super::{Coordinator, Effect};
use crate::protocol::{AgentId, CommitId, EventKind};

/// The agent a trunk move is attributed to: the steward is the only writer of main.
const STEWARD_AGENT: &str = "steward";

/// A head read the shell should make now, with the head it was made against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadRead {
    /// `head` when the read began. `trunk_read` applies the answer only if it is still this.
    pub head_at_start: Option<CommitId>,
}

/// What the coordinator sends the steward's `/head`: read the head of `repo`'s main.
pub fn head_request_body(repo: &str) -> String {
    serde_json::json!({ "repo": repo }).to_string()
}

/// The head a steward `/head` response carries: `{ "head": "<sha>" }` with status 200. Any other
/// status or shape is `None`. A body is never kept or quoted.
pub fn parse_head_response(status: u16, body: &str) -> Option<CommitId> {
    if status != 200 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let head = value.get("head")?.as_str()?;
    Some(CommitId(head.to_string()))
}

/// Whether `head` names no commit: empty, or all zeros (the null sha).
fn is_null_head(head: &CommitId) -> bool {
    head.0.bytes().all(|byte| byte == b'0')
}

impl Coordinator {
    /// The steward says main moved: raise the flag. The request carries nothing the core uses.
    /// Persisted with the rest of the state by the caller.
    pub fn trunk_moved(&mut self, now_ms: u64) -> Vec<Effect> {
        self.advance_clock(now_ms);
        self.state.head_sync_due = true;
        Vec::new()
    }

    /// The head read to make now, if the flag is raised and no merge is in flight.
    pub fn begin_head_read(&self) -> Option<HeadRead> {
        if !self.state.head_sync_due || self.state.merge_in_flight.is_some() {
            return None;
        }
        Some(HeadRead {
            head_at_start: self.state.head.clone(),
        })
    }

    /// The alarm time of the head read: now, while one is due.
    pub fn next_head_read_ms(&self, now_ms: u64) -> Option<u64> {
        self.begin_head_read().map(|_| now_ms)
    }

    /// Apply the trunk head the steward answered for `read`. `None` is an unusable answer.
    pub fn trunk_read(
        &mut self,
        read: &HeadRead,
        answer: Option<CommitId>,
        now_ms: u64,
    ) -> Vec<Effect> {
        let now_ms = self.advance_clock(now_ms);
        if !self.state.head_sync_due {
            return Vec::new();
        }
        if self.state.merge_in_flight.is_some() || self.state.head != read.head_at_start {
            return Vec::new();
        }
        self.state.head_sync_due = false;
        let Some(head) = answer else {
            return Vec::new();
        };
        if is_null_head(&head) || self.state.head.as_ref() == Some(&head) {
            return Vec::new();
        }
        self.state.head = Some(head.clone());
        vec![self.event(
            now_ms,
            EventKind::BaseMoved {
                head,
                by: AgentId(STEWARD_AGENT.to_string()),
                notified: Vec::new(),
            },
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::Config;
    use crate::protocol::{
        ClientMsg, DecisionRecord, Intent, Mode, OnConflict, RequestId, RunId, Scope, ScopeClaim,
        ServerMsg,
    };

    const NOW: u64 = 1_000;
    const OLD: &str = "e894fbe000000000000000000000000000000000";
    const TRUNK: &str = "7d7f4c9000000000000000000000000000000000";
    const NEWER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn core() -> Coordinator {
        Coordinator::new(Config {
            run: RunId("test".into()),
            lease_ms: 30_000,
            shadow_enabled: false,
        })
        .unwrap()
    }

    fn agent(name: &str) -> AgentId {
        AgentId(name.into())
    }

    fn sha(text: &str) -> CommitId {
        CommitId(text.into())
    }

    fn hello(c: &mut Coordinator, who: &str, base: &str) -> CommitId {
        let msg = ClientMsg::Hello {
            agent: agent(who),
            base: sha(base),
            protocol: 1,
        };
        for effect in c.handle(&agent(who), msg, NOW) {
            if let Effect::Reply(ServerMsg::Welcome { head, .. }) = effect {
                return head;
            }
        }
        panic!("hello was not welcomed");
    }

    fn logged(effects: &[Effect]) -> Vec<&EventKind> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Log(event) => Some(&event.kind),
                Effect::Reply(_) | Effect::Notify { .. } => None,
            })
            .collect()
    }

    /// A core that has welcomed an agent at `OLD` and been poked.
    fn poked() -> (Coordinator, HeadRead) {
        let mut c = core();
        hello(&mut c, "a1", OLD);
        assert!(c.trunk_moved(NOW).is_empty());
        let read = c.begin_head_read().expect("a poke makes a read due");
        (c, read)
    }

    fn head_of(c: &Coordinator) -> Option<CommitId> {
        c.state.head.clone()
    }

    /// Submits a claim by `who` and dispatches its merge, leaving it in flight.
    fn merge_in_flight(c: &mut Coordinator, who: &str) {
        let scope = ScopeClaim {
            scope: Scope::File {
                path: "src/a.rs".into(),
            },
            mode: Mode::EditBody,
        };
        let msg = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent {
                summary: "s".into(),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes: vec![scope.clone()],
            on_conflict: OnConflict::Fail,
        };
        let mut granted = None;
        for effect in c.handle(&agent(who), msg, NOW) {
            if let Effect::Reply(ServerMsg::Granted { claim, fence, .. }) = effect {
                granted = Some((claim, fence));
            }
        }
        let (claim, fence) = granted.expect("granted");
        let submit = ClientMsg::Submit {
            req: RequestId(2),
            claim,
            fence,
            fork_commit: sha("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            touched: vec![scope],
            decisions: DecisionRecord {
                evidence: vec!["tests passed".into()],
                ..DecisionRecord::default()
            },
        };
        c.handle(&agent(who), submit, NOW);
        assert!(c.begin_merge(NOW).is_some());
    }

    #[test]
    fn idle_read_moves_head_and_logs_one_base_moved() {
        let (mut c, read) = poked();
        let effects = c.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        assert_eq!(head_of(&c), Some(sha(TRUNK)));
        let [EventKind::BaseMoved { head, by, notified }] = logged(&effects)[..] else {
            panic!("expected exactly one BaseMoved, got {effects:?}");
        };
        assert_eq!(*head, sha(TRUNK));
        assert_eq!(*by, agent("steward"));
        assert!(notified.is_empty());
        assert_eq!(effects.len(), 1, "nothing is sent to anyone: {effects:?}");
        assert!(!c.state.head_sync_due);
        assert_eq!(c.begin_head_read(), None);
    }

    #[test]
    fn read_with_a_merge_in_flight_changes_nothing_and_keeps_the_flag() {
        let (mut c, read) = poked();
        merge_in_flight(&mut c, "a1");
        let effects = c.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(head_of(&c), Some(sha(OLD)));
        assert!(c.state.head_sync_due);
    }

    #[test]
    fn no_read_is_due_while_a_merge_is_in_flight() {
        let (mut c, _) = poked();
        merge_in_flight(&mut c, "a1");
        assert_eq!(c.begin_head_read(), None);
        assert_eq!(c.next_head_read_ms(NOW), None);
    }

    #[test]
    fn read_after_the_head_changed_meanwhile_changes_nothing_and_keeps_the_flag() {
        let mut c = core();
        let read = {
            c.trunk_moved(NOW);
            c.begin_head_read().expect("due")
        };
        assert_eq!(read.head_at_start, None);
        hello(&mut c, "a1", OLD);
        let effects = c.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(head_of(&c), Some(sha(OLD)));
        assert!(c.state.head_sync_due);
        let again = c.begin_head_read().expect("still due");
        assert_eq!(again.head_at_start, Some(sha(OLD)));
    }

    #[test]
    fn the_same_head_logs_nothing_and_clears_the_flag() {
        let (mut c, read) = poked();
        let effects = c.trunk_read(&read, Some(sha(OLD)), NOW + 1);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(head_of(&c), Some(sha(OLD)));
        assert!(!c.state.head_sync_due);
    }

    #[test]
    fn empty_zero_and_missing_answers_are_ignored() {
        for answer in [
            Some(sha("")),
            Some(sha(&"0".repeat(40))),
            Some(sha("0")),
            None,
        ] {
            let (mut c, read) = poked();
            let effects = c.trunk_read(&read, answer.clone(), NOW + 1);
            assert!(effects.is_empty(), "{answer:?}: {effects:?}");
            assert_eq!(head_of(&c), Some(sha(OLD)), "{answer:?}");
            assert!(!c.state.head_sync_due, "{answer:?}");
        }
    }

    #[test]
    fn an_answer_with_no_flag_raised_changes_nothing() {
        let mut c = core();
        hello(&mut c, "a1", OLD);
        let read = HeadRead {
            head_at_start: Some(sha(OLD)),
        };
        assert!(c.trunk_read(&read, Some(sha(TRUNK)), NOW).is_empty());
        assert_eq!(head_of(&c), Some(sha(OLD)));
    }

    #[test]
    fn the_next_welcome_carries_the_new_head() {
        let (mut c, read) = poked();
        c.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        assert_eq!(hello(&mut c, "a2", NEWER), sha(TRUNK));
    }

    #[test]
    fn a_poke_alone_never_moves_the_head() {
        let (c, _) = poked();
        assert_eq!(head_of(&c), Some(sha(OLD)));
        assert_eq!(c.next_head_read_ms(NOW + 5), Some(NOW + 5));
    }

    #[test]
    fn the_flag_survives_save_and_reload() {
        let (c, _) = poked();
        let saved = serde_json::to_string(&c).unwrap();
        let mut reloaded: Coordinator = serde_json::from_str(&saved).unwrap();
        let read = reloaded.begin_head_read().expect("flag persisted");
        assert_eq!(read.head_at_start, Some(sha(OLD)));
        reloaded.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        let saved = serde_json::to_string(&reloaded).unwrap();
        let again: Coordinator = serde_json::from_str(&saved).unwrap();
        assert_eq!(again.begin_head_read(), None);
        assert_eq!(head_of(&again), Some(sha(TRUNK)));
    }

    #[test]
    fn a_state_stored_before_the_flag_existed_loads_with_it_down() {
        let c = core();
        let mut state: serde_json::Value = serde_json::to_value(&c).unwrap();
        state.as_object_mut().unwrap().remove("head_sync_due");
        let loaded: Coordinator = serde_json::from_value(state).unwrap();
        assert_eq!(loaded.begin_head_read(), None);
    }

    #[test]
    fn head_request_names_the_repo() {
        assert_eq!(head_request_body("demo"), r#"{"repo":"demo"}"#);
    }

    #[test]
    fn head_response_parses_only_a_200_with_a_head() {
        assert_eq!(
            parse_head_response(200, r#"{"head":"abc"}"#),
            Some(sha("abc"))
        );
        assert_eq!(parse_head_response(502, r#"{"head":"abc"}"#), None);
        assert_eq!(parse_head_response(200, r#"{"head":7}"#), None);
        assert_eq!(parse_head_response(200, r#"{"other":"abc"}"#), None);
        assert_eq!(parse_head_response(200, "not json"), None);
    }

    #[test]
    fn the_read_is_due_in_the_wake_time_while_flagged() {
        let (c, _) = poked();
        assert_eq!(c.next_wake_ms(false, false, NOW + 9), Some(NOW + 9));
        let idle = core();
        assert_eq!(idle.next_wake_ms(false, false, NOW), None);
    }
}

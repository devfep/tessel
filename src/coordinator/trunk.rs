//! Keeping `head` current when main moves without the coordinator: an admin merge goes around the
//! merge queue, so only the steward knows the new trunk head.
//!
//! The steward's push event reaches the coordinator as a bare poke (`trunk_moved`) that carries no
//! data, so nothing in the request can set the head: it only counts (`head_pokes`, persisted), and
//! the alarm does the rest. The shell asks the steward for the trunk head through its binding-only
//! merge service, then offers the answer to `trunk_read`.
//!
//! Decisions made here:
//! - A poke is a count, not a flag, so a poke that arrives while a read is awaiting the steward is
//!   not lost: a read acknowledges only the pokes counted when it began (`head_synced`), and any
//!   later poke leaves another read due.
//! - The read is a compare-and-set. The host handles other inputs while it waits for the
//!   steward, so `trunk_read` applies the answer only if no merge is in flight and `head` is still
//!   what it was when the read began. Otherwise nothing changes and the read stays due, so the
//!   next alarm reads again. A merge that is in flight is not waited for: its own landing sets
//!   `head`, and the pending pokes then make the alarm check that the trunk has not moved past it.
//! - An answer that is the current head acknowledges the pokes and changes nothing else.
//! - No usable answer (the call failed, or the answer was empty or all zeros) leaves the pokes
//!   pending but is no reason to wake (`head_tried`): a failing steward is not retried in a hot
//!   loop. The next poke, or any alarm that runs anyway (a merge, a lease), reads again.
//! - With no head yet, a read is due without a poke, so the head is main's before the first
//!   `Hello`. A `Hello` that arrives first sets the head itself (`head_from_hello`), but that base
//!   was never read from main. The answer of a read that began with no head replaces it, whatever
//!   the poke count: the answer is the truth about main. It is logged as a `BaseMoved`, because
//!   the agent was welcomed with the old head. A head a landing or an earlier read set is never
//!   replaced this way, and the compare-and-set still drops a read that began before it.
//! - A read that fails while the head is missing or adopted from a `Hello` is offered again after
//!   `infra_backoff_ms` (10 s, 30 s, 90 s), at most `MAX_INFRA_RETRIES` times, then no wake is set
//!   for it. Any alarm that runs anyway still offers it while the head is missing, and a poke
//!   starts the count again. Agents already welcomed learn nothing new: the next `Welcome` and
//!   the next dispatch carry the head.
//! - The move is logged as `BaseMoved { by: "steward", notified: [] }`. Nobody is notified: the
//!   touched scopes of an admin merge are unknown to the coordinator, and claims are not touched.

use super::{Coordinator, Effect};
use crate::merge::{infra_backoff_ms, MAX_INFRA_RETRIES};
use crate::protocol::{AgentId, CommitId, EventKind};

/// The agent a trunk move is attributed to: the steward is the only writer of main.
const STEWARD_AGENT: &str = "steward";

/// A head read the shell should make now, with the head it was made against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadRead {
    /// `head` when the read began. `trunk_read` applies the answer only if it is still this.
    pub head_at_start: Option<CommitId>,
    /// The pokes counted when the read began: the most it acknowledges.
    pub pokes: u64,
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
    /// The steward says main moved: count it. The request carries nothing the core uses.
    /// Persisted with the rest of the state by the caller.
    pub fn trunk_moved(&mut self, now_ms: u64) -> Vec<Effect> {
        self.advance_clock(now_ms);
        self.state.head_pokes = self.state.head_pokes.saturating_add(1);
        self.state.head_start_failures = 0;
        Vec::new()
    }

    /// The head read to make now, if a poke is unanswered or the coordinator has no head yet, and
    /// no merge is in flight. It is also offered after a failed read, so any alarm that runs
    /// anyway retries it.
    pub fn begin_head_read(&self) -> Option<HeadRead> {
        if !self.head_read_due() || self.state.merge_in_flight.is_some() {
            return None;
        }
        Some(HeadRead {
            head_at_start: self.state.head.clone(),
            pokes: self.state.head_pokes,
        })
    }

    /// A poke is unanswered, or no head is known: the first `Hello` would otherwise make an
    /// agent's declared base the head, unchecked.
    fn head_read_due(&self) -> bool {
        self.state.head_pokes > self.state.head_synced
            || self.state.head.is_none()
            || self.start_retry_owed()
    }

    /// `head` is the base a `Hello` declared, not something read from main or landed.
    fn head_is_adopted(&self) -> bool {
        self.state.head.is_some() && self.state.head == self.state.head_from_hello
    }

    /// A start read failed, the retries are not used up, and the head is still unchecked.
    fn start_retry_owed(&self) -> bool {
        let failures = self.state.head_start_failures;
        self.head_is_adopted() && (1..=MAX_INFRA_RETRIES).contains(&failures)
    }

    /// The alarm time of the head read: now, while a poke has not been tried yet, or the end of
    /// the backoff after a failed start read. A poke whose read failed is pending but not a
    /// reason to wake.
    pub fn next_head_read_ms(&self, now_ms: u64) -> Option<u64> {
        self.begin_head_read()?;
        if self.state.head_pokes > self.state.head_tried {
            return Some(now_ms);
        }
        let failures = self.state.head_start_failures;
        if (1..=MAX_INFRA_RETRIES).contains(&failures) {
            return Some(self.state.head_start_retry_ms.max(now_ms));
        }
        None
    }

    /// A read failed while the head was missing or adopted: wait before the next try.
    fn note_start_read_failed(&mut self, now_ms: u64) {
        if self.state.head.is_some() && !self.head_is_adopted() {
            return;
        }
        let failures = self.state.head_start_failures.saturating_add(1);
        self.state.head_start_failures = failures;
        let wait = infra_backoff_ms(failures.min(MAX_INFRA_RETRIES));
        self.state.head_start_retry_ms = now_ms.saturating_add(wait);
    }

    /// Apply the trunk head the steward answered for `read`. `None` is an unusable answer.
    pub fn trunk_read(
        &mut self,
        read: &HeadRead,
        answer: Option<CommitId>,
        now_ms: u64,
    ) -> Vec<Effect> {
        let now_ms = self.advance_clock(now_ms);
        let began_blind = read.head_at_start.is_none() && self.head_is_adopted();
        if !began_blind && !self.head_read_due() {
            return Vec::new();
        }
        if self.state.merge_in_flight.is_some() {
            return Vec::new();
        }
        if !began_blind && self.state.head != read.head_at_start {
            return Vec::new();
        }
        self.state.head_tried = self.state.head_tried.max(read.pokes);
        let head = match answer {
            Some(head) if !is_null_head(&head) => head,
            Some(_) | None => {
                self.note_start_read_failed(now_ms);
                return Vec::new();
            }
        };
        self.state.head_synced = self.state.head_synced.max(read.pokes);
        self.state.head_start_failures = 0;
        self.state.head_from_hello = None;
        if self.state.head.as_ref() == Some(&head) {
            return Vec::new();
        }
        let first = self.state.head.replace(head.clone()).is_none();
        if first {
            // The first head is no move: nobody had a base to be moved from.
            return Vec::new();
        }
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
    use crate::merge::{infra_backoff_ms, MergeOutcome, MAX_INFRA_RETRIES};
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

    /// A core whose head `OLD` was read from main, that has welcomed an agent at it and been
    /// poked.
    fn poked() -> (Coordinator, HeadRead) {
        let mut c = core();
        let first = c.begin_head_read().expect("no head: a read is due");
        c.trunk_read(&first, Some(sha(OLD)), NOW);
        hello(&mut c, "a1", OLD);
        assert!(c.trunk_moved(NOW).is_empty());
        let read = c.begin_head_read().expect("a poke makes a read due");
        (c, read)
    }

    /// A poke has not been answered by a usable read.
    fn unanswered(c: &Coordinator) -> bool {
        c.state.head_pokes > c.state.head_synced
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
        assert!(!unanswered(&c));
        assert_eq!(c.begin_head_read(), None);
    }

    #[test]
    fn read_with_a_merge_in_flight_changes_nothing_and_keeps_the_flag() {
        let (mut c, read) = poked();
        merge_in_flight(&mut c, "a1");
        let effects = c.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(head_of(&c), Some(sha(OLD)));
        assert!(unanswered(&c));
    }

    #[test]
    fn no_read_is_due_while_a_merge_is_in_flight() {
        let (mut c, _) = poked();
        merge_in_flight(&mut c, "a1");
        assert_eq!(c.begin_head_read(), None);
        assert_eq!(c.next_head_read_ms(NOW), None);
    }

    #[test]
    fn read_after_a_read_head_changed_meanwhile_changes_nothing_and_keeps_the_flag() {
        let (mut c, read) = poked();
        let other = HeadRead {
            head_at_start: Some(sha(NEWER)),
            pokes: read.pokes,
        };
        let effects = c.trunk_read(&other, Some(sha(TRUNK)), NOW + 1);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(head_of(&c), Some(sha(OLD)));
        assert!(unanswered(&c));
        assert_eq!(c.next_head_read_ms(NOW + 2), Some(NOW + 2));
        let again = c.begin_head_read().expect("still due");
        assert_eq!(again.head_at_start, Some(sha(OLD)));
    }

    #[test]
    fn a_poked_start_read_replaces_the_head_a_hello_set_meanwhile() {
        let mut c = core();
        c.trunk_moved(NOW);
        let read = c.begin_head_read().expect("due");
        assert_eq!(read.head_at_start, None);
        hello(&mut c, "a1", OLD);
        let effects = c.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert_eq!(head_of(&c), Some(sha(TRUNK)));
        assert!(!unanswered(&c));
    }

    #[test]
    fn the_same_head_logs_nothing_and_clears_the_flag() {
        let (mut c, read) = poked();
        let effects = c.trunk_read(&read, Some(sha(OLD)), NOW + 1);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(head_of(&c), Some(sha(OLD)));
        assert!(!unanswered(&c));
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
            assert!(unanswered(&c), "{answer:?}: the move is not dropped");
            assert_eq!(
                c.next_head_read_ms(NOW + 2),
                None,
                "{answer:?}: no wake of its own"
            );
        }
    }

    #[test]
    fn a_failed_read_is_retried_by_the_next_poke_or_the_next_alarm_that_runs_anyway() {
        let (mut c, read) = poked();
        c.trunk_read(&read, None, NOW + 1);
        assert_eq!(c.next_head_read_ms(NOW + 2), None);
        let retry = c
            .begin_head_read()
            .expect("still pending for an alarm that runs anyway");
        assert_eq!(c.next_wake_ms(false, false, NOW + 2), None);
        c.trunk_moved(NOW + 3);
        assert_eq!(c.next_head_read_ms(NOW + 4), Some(NOW + 4));
        let effects = c.trunk_read(&retry, Some(sha(TRUNK)), NOW + 5);
        assert_eq!(head_of(&c), Some(sha(TRUNK)));
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert!(
            unanswered(&c),
            "the poke made after the read began is still pending"
        );
    }

    #[test]
    fn a_poke_that_arrives_during_a_read_leaves_another_read_due() {
        let (mut c, read) = poked();
        c.trunk_moved(NOW + 1);
        let effects = c.trunk_read(&read, Some(sha(TRUNK)), NOW + 2);
        assert_eq!(
            effects.len(),
            1,
            "the head the read saw is applied: {effects:?}"
        );
        assert!(unanswered(&c), "the second poke is not acknowledged");
        let next = c.begin_head_read().expect("another read is due");
        assert_eq!(next.head_at_start, Some(sha(TRUNK)));
        assert_eq!(c.next_head_read_ms(NOW + 3), Some(NOW + 3));
        let effects = c.trunk_read(&next, Some(sha(NEWER)), NOW + 4);
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert_eq!(head_of(&c), Some(sha(NEWER)));
        assert!(!unanswered(&c));
    }

    #[test]
    fn an_answer_with_no_flag_raised_changes_nothing() {
        let mut c = core();
        hello(&mut c, "a1", OLD);
        let read = HeadRead {
            head_at_start: Some(sha(OLD)),
            pokes: 0,
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
        let mut c = core();
        hello(&mut c, "a1", OLD);
        let mut state: serde_json::Value = serde_json::to_value(&c).unwrap();
        let fields = state.as_object_mut().unwrap();
        for field in ["head_pokes", "head_synced", "head_tried"] {
            fields.remove(field);
        }
        let loaded: Coordinator = serde_json::from_value(state).unwrap();
        assert_eq!(loaded.begin_head_read(), None);
    }

    #[test]
    fn a_core_with_no_head_offers_a_read_and_the_answer_is_its_first_head() {
        let mut c = core();
        let read = c.begin_head_read().expect("no head: a read is due");
        assert_eq!(read.head_at_start, None);
        assert_eq!(
            c.next_head_read_ms(NOW),
            None,
            "due, but no wake is set for it"
        );
        assert!(
            c.trunk_read(&read, Some(sha(TRUNK)), NOW).is_empty(),
            "no move is logged"
        );
        assert_eq!(head_of(&c), Some(sha(TRUNK)));
        assert_eq!(c.begin_head_read(), None);
    }

    #[test]
    fn a_hello_after_the_read_keeps_the_read_head_and_one_before_it_is_replaced_by_it() {
        let mut after = core();
        let read = after.begin_head_read().expect("due");
        after.trunk_read(&read, Some(sha(TRUNK)), NOW);
        hello(&mut after, "a1", OLD);
        assert_eq!(head_of(&after), Some(sha(TRUNK)));

        let mut before = core();
        let read = before.begin_head_read().expect("due");
        hello(&mut before, "a1", OLD);
        before.trunk_read(&read, Some(sha(TRUNK)), NOW);
        assert_eq!(
            head_of(&before),
            Some(sha(TRUNK)),
            "the answer is the truth about main, not the base the agent declared"
        );
    }

    #[test]
    fn a_read_with_no_usable_answer_leaves_no_head_and_is_offered_again() {
        let mut c = core();
        let read = c.begin_head_read().expect("due");
        for answer in [None, Some(sha("0000000000000000000000000000000000000000"))] {
            assert!(c.trunk_read(&read, answer, NOW).is_empty());
            assert_eq!(head_of(&c), None);
            assert!(c.begin_head_read().is_some());
        }
    }

    /// The main a merge dispatch was baselined on.
    fn dispatched_main(c: &Coordinator) -> Option<CommitId> {
        c.state
            .merge_in_flight
            .as_ref()
            .and_then(|flight| flight.main.clone())
    }

    #[test]
    fn a_hello_before_the_start_read_answers_leaves_the_read_head_for_the_next_dispatch() {
        let mut c = core();
        let read = c.begin_head_read().expect("no head: a read is due");
        assert_eq!(hello(&mut c, "a1", OLD), sha(OLD));
        let effects = c.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        assert_eq!(head_of(&c), Some(sha(TRUNK)));
        let [EventKind::BaseMoved { head, by, notified }] = logged(&effects)[..] else {
            panic!("the agent was told OLD, so the change is logged: {effects:?}");
        };
        assert_eq!((head, by), (&sha(TRUNK), &agent("steward")));
        assert!(notified.is_empty());
        assert_eq!(c.begin_head_read(), None, "the read is answered");
        merge_in_flight(&mut c, "a1");
        assert_eq!(dispatched_main(&c), Some(sha(TRUNK)));
        assert_eq!(hello(&mut c, "a2", NEWER), sha(TRUNK));
    }

    #[test]
    fn a_start_read_answering_the_head_a_hello_set_keeps_it_and_logs_nothing() {
        let mut c = core();
        let read = c.begin_head_read().expect("due");
        hello(&mut c, "a1", OLD);
        assert!(c.trunk_read(&read, Some(sha(OLD)), NOW + 1).is_empty());
        assert_eq!(head_of(&c), Some(sha(OLD)));
        assert_eq!(c.begin_head_read(), None);
    }

    #[test]
    fn the_head_a_hello_set_is_remembered_across_save_and_reload() {
        let mut c = core();
        let read = c.begin_head_read().expect("due");
        hello(&mut c, "a1", OLD);
        let saved = serde_json::to_string(&c).unwrap();
        let mut reloaded: Coordinator = serde_json::from_str(&saved).unwrap();
        reloaded.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        assert_eq!(head_of(&reloaded), Some(sha(TRUNK)));
    }

    #[test]
    fn a_landing_after_a_hello_is_not_replaced_by_the_stale_start_read() {
        let mut c = core();
        let read = c.begin_head_read().expect("due");
        hello(&mut c, "a1", OLD);
        merge_in_flight(&mut c, "a1");
        assert!(c.trunk_read(&read, Some(sha(TRUNK)), NOW + 1).is_empty());
        assert_eq!(head_of(&c), Some(sha(OLD)), "no read applies mid-merge");
        let dispatch = c
            .state
            .merge_in_flight
            .as_ref()
            .expect("in flight")
            .dispatch;
        let merged = MergeOutcome::Merged {
            base: sha(OLD),
            head: sha(NEWER),
        };
        c.merge_outcome(dispatch, &merged, NOW + 2);
        assert_eq!(head_of(&c), Some(sha(NEWER)));
        assert!(c.trunk_read(&read, Some(sha(TRUNK)), NOW + 3).is_empty());
        assert_eq!(
            head_of(&c),
            Some(sha(NEWER)),
            "the landing is the newer truth"
        );
    }

    #[test]
    fn a_head_from_an_earlier_read_is_not_replaced_by_a_blind_start_read() {
        let mut c = core();
        let first = c.begin_head_read().expect("due");
        let stale = c.begin_head_read().expect("due again before the answer");
        c.trunk_read(&first, Some(sha(TRUNK)), NOW);
        assert!(c.trunk_read(&stale, Some(sha(NEWER)), NOW + 1).is_empty());
        assert_eq!(head_of(&c), Some(sha(TRUNK)));
    }

    #[test]
    fn a_failed_start_read_is_offered_again_after_the_backoff_and_not_before() {
        let mut c = core();
        let read = c.begin_head_read().expect("due");
        c.trunk_read(&read, None, NOW);
        let first = NOW + infra_backoff_ms(1);
        for early in [NOW, NOW + 1, first - 1] {
            assert_eq!(
                c.next_head_read_ms(early),
                Some(first),
                "not woken at {early}"
            );
        }
        assert_eq!(c.next_head_read_ms(first), Some(first));
        assert_eq!(c.next_head_read_ms(first + 5), Some(first + 5));
        let retry = c.begin_head_read().expect("still due");
        c.trunk_read(&retry, None, first);
        let second = first + infra_backoff_ms(2);
        assert_eq!(c.next_head_read_ms(first + 1), Some(second));
        assert!(second - first > first - NOW, "the wait grows");
    }

    #[test]
    fn a_failed_start_read_stops_waking_after_the_infrastructure_retries() {
        let mut c = core();
        let mut now = NOW;
        for failure in 1..=MAX_INFRA_RETRIES + 1 {
            let read = c
                .begin_head_read()
                .expect("a head read is always owed with no head");
            c.trunk_read(&read, None, now);
            let wake = c.next_head_read_ms(now);
            if failure <= MAX_INFRA_RETRIES {
                now = wake.expect("a retry is scheduled");
            } else {
                assert_eq!(wake, None, "no wake after {failure} failures");
            }
        }
        assert!(
            c.begin_head_read().is_some(),
            "an alarm that runs anyway still reads"
        );
        c.trunk_moved(now);
        assert_eq!(
            c.next_head_read_ms(now),
            Some(now),
            "a poke is a new reason"
        );
    }

    #[test]
    fn a_hello_after_a_failed_start_read_leaves_the_retry_due_and_its_answer_applies() {
        let mut c = core();
        let read = c.begin_head_read().expect("due");
        c.trunk_read(&read, None, NOW);
        hello(&mut c, "a1", OLD);
        let retry_at = NOW + infra_backoff_ms(1);
        assert_eq!(c.next_head_read_ms(NOW + 1), Some(retry_at));
        let retry = c
            .begin_head_read()
            .expect("the base a hello declared is unchecked");
        assert_eq!(retry.head_at_start, Some(sha(OLD)));
        c.trunk_read(&retry, Some(sha(TRUNK)), retry_at);
        assert_eq!(head_of(&c), Some(sha(TRUNK)));
        assert_eq!(c.begin_head_read(), None);
        assert_eq!(c.next_head_read_ms(retry_at + 1), None);
    }

    #[test]
    fn a_read_that_works_ends_the_retries() {
        let mut c = core();
        let read = c.begin_head_read().expect("due");
        c.trunk_read(&read, None, NOW);
        let retry = c.begin_head_read().expect("due");
        c.trunk_read(&retry, Some(sha(TRUNK)), NOW + 1);
        assert_eq!(c.next_head_read_ms(NOW + 2), None);
        assert_eq!(c.begin_head_read(), None);
    }

    #[test]
    fn a_poke_starts_the_start_read_retries_again() {
        let mut c = core();
        let mut now = NOW;
        for _ in 0..=MAX_INFRA_RETRIES {
            let read = c.begin_head_read().expect("a read is owed with no head");
            c.trunk_read(&read, None, now);
            now += 1;
        }
        c.trunk_moved(now);
        let read = c.begin_head_read().expect("due");
        c.trunk_read(&read, None, now);
        assert_eq!(c.next_head_read_ms(now), Some(now + infra_backoff_ms(1)));
    }

    #[test]
    fn an_answer_that_confirms_the_hello_head_ends_its_replaceability() {
        let mut c = core();
        let first = c.begin_head_read().expect("due");
        let second = c.begin_head_read().expect("due again before an answer");
        hello(&mut c, "a1", OLD);
        assert!(c.trunk_read(&first, Some(sha(OLD)), NOW + 1).is_empty());
        assert!(c.trunk_read(&second, Some(sha(TRUNK)), NOW + 2).is_empty());
        assert_eq!(head_of(&c), Some(sha(OLD)));
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

    #[test]
    fn a_restart_while_the_start_read_is_out_reads_main_again_without_a_poke() {
        let mut c = core();
        let lost = c.begin_head_read().expect("no head: a read is due");
        assert_eq!(lost.head_at_start, None);
        hello(&mut c, "a1", OLD);
        let saved = serde_json::to_string(&c).unwrap();
        let mut restored: Coordinator = serde_json::from_str(&saved).unwrap();
        let read = restored
            .begin_head_read()
            .expect("the head is still the one a Hello adopted: main is read again");
        let effects = restored.trunk_read(&read, Some(sha(TRUNK)), NOW + 1);
        assert_eq!(head_of(&restored), Some(sha(TRUNK)));
        assert_eq!(effects.len(), 1, "the move is logged: {effects:?}");
    }
}

//! Checkers for the merge path: I3, I6, I7, I11 and I12 of `docs/proposals/phase-1.md`
//! ("Invariants and the simulator checks that prove them"), and COORD-HEAD-FROM-AGENT's check that
//! no dispatch uses a head an agent declared.
//!
//! Their evidence and advisory clauses wait for the rows that add evidence records and advisory
//! mode (M6): I3 does not count evidence records per landed commit, I11 does not compare a
//! landing's evidence head with the head the lander tested, and none of the five reads advisory
//! releases.

use std::collections::BTreeMap;

use crate::protocol::{Event, Summary};
use crate::sim::driver::{Delivery, Dispatch, SlotNote, StaleAnswer};
use crate::sim::lander::Landing;

/// How many merges may be dispatched in a row past a ready verification
/// (`coordinator::verifying::VERIFY_SLOT_AFTER`, private there).
pub const VERIFY_SLOT_AFTER: usize = 4;

pub fn i3(_events: &[Event], _transcript: &[Delivery], _history: &[Landing]) -> Vec<String> {
    Vec::new()
}

/// What I6 compares the log with.
pub struct MergePath<'a> {
    /// Calls the host took as not finished, by fork commit.
    pub cutoffs: &'a BTreeMap<String, u32>,
    /// The fork commits the lander cuts off at one stage, every time.
    pub poisoned: &'a [String],
    /// Failures the lander did not cause: restarts and failed commits.
    pub other_failures: u64,
    /// Main's first-parent history.
    pub history: &'a [Landing],
}

pub fn i6(_events: &[Event], _path: &MergePath<'_>) -> Vec<String> {
    Vec::new()
}

pub fn i7(_stale: &[StaleAnswer]) -> Vec<String> {
    Vec::new()
}

pub fn i11(_events: &[Event], _counters: &Summary) -> Vec<String> {
    Vec::new()
}

pub fn i12(_notes: &[SlotNote], _events: &[Event]) -> Vec<String> {
    Vec::new()
}

pub fn agent_base(_dispatches: &[Dispatch], _declared: &str) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::host::call::Request;
    use crate::host::SocketId;
    use crate::protocol::{
        AgentId, ClaimId, DecisionRecord, EventKind, ReleaseReason, RunId, ServerMsg,
    };
    use crate::sim::driver::{SlotCall, BASE};
    use crate::sim::lander::MAIN0;

    fn event(seq: usize, kind: EventKind) -> Event {
        Event {
            seq: seq as u64,
            at_ms: 0,
            run: RunId("r".to_string()),
            kind,
        }
    }

    fn log(kinds: Vec<EventKind>) -> Vec<Event> {
        kinds
            .into_iter()
            .enumerate()
            .map(|(seq, kind)| event(seq, kind))
            .collect()
    }

    fn merged(claim: u64, head: &str) -> EventKind {
        EventKind::Merged {
            claim: ClaimId(claim),
            head: crate::protocol::CommitId(head.to_string()),
            base: None,
            unverified: Vec::new(),
            untried_shadows: Vec::new(),
        }
    }

    fn submitted(claim: u64, fork: &str) -> EventKind {
        EventKind::Submitted {
            claim: ClaimId(claim),
            fork_commit: crate::protocol::CommitId(fork.to_string()),
            touched: Vec::new(),
            decisions: DecisionRecord {
                rejected: Vec::new(),
                evidence: Vec::new(),
                transcript: None,
            },
        }
    }

    fn rejected(claim: u64, reason: &str) -> EventKind {
        EventKind::SubmitRejected {
            claim: ClaimId(claim),
            reason: reason.to_string(),
        }
    }

    const INFRA: &str = "the merge could not be completed after 4 tries: infrastructure, not a code failure; submit again";

    fn landing(head: &str, fork: &str) -> Landing {
        Landing {
            head: head.to_string(),
            fork: Some(fork.to_string()),
        }
    }

    fn told_merged(sock: u64, claim: u64) -> Delivery {
        Delivery {
            sock: SocketId(sock),
            agent: AgentId("a".to_string()),
            msg: ServerMsg::Merged {
                claim: ClaimId(claim),
                head: crate::protocol::CommitId("h1".to_string()),
                base: None,
            },
        }
    }

    // ---- I3 ----

    #[test]
    fn i3_is_silent_for_one_merge_per_claim_on_a_history_with_each_submission_once() {
        let events = log(vec![merged(1, "h1"), merged(2, "h2")]);
        let history = [landing("h1", "f1"), landing("h2", "f2")];
        assert!(i3(&events, &[told_merged(0, 1)], &history).is_empty());
    }

    #[test]
    fn i3_flags_a_claim_merged_twice_in_the_log() {
        let events = log(vec![merged(1, "h1"), merged(1, "h1")]);
        let found = i3(&events, &[], &[landing("h1", "f1")]);
        assert_eq!(found, ["claim 1 was merged 2 times"]);
    }

    #[test]
    fn i3_flags_a_claim_a_client_was_told_merged_twice() {
        let found = i3(&[], &[told_merged(3, 1), told_merged(3, 1)], &[]);
        assert_eq!(found, ["socket 3 was told claim 1 merged 2 times"]);
        assert!(i3(&[], &[told_merged(3, 1), told_merged(4, 1)], &[]).is_empty());
    }

    #[test]
    fn i3_flags_a_submission_on_main_twice_and_a_merge_that_is_not_on_main() {
        let history = [landing("h1", "f1"), landing("h2", "f1")];
        let found = i3(&[], &[], &history);
        assert_eq!(found, ["the submission f1 landed on main twice"]);
        let found = i3(&[], &[], &[landing("h1", "f1"), landing("h1", "f2")]);
        assert_eq!(found, ["h1 is on main twice"]);
        let events = log(vec![merged(1, "elsewhere")]);
        let found = i3(&events, &[], &[landing("h1", "f1")]);
        assert_eq!(found, ["claim 1 merged at elsewhere, which is not on main"]);
    }

    // ---- I6 ----

    fn path<'a>(
        cutoffs: &'a BTreeMap<String, u32>,
        poisoned: &'a [String],
        history: &'a [Landing],
    ) -> MergePath<'a> {
        MergePath {
            cutoffs,
            poisoned,
            other_failures: 0,
            history,
        }
    }

    #[test]
    fn i6_is_silent_while_a_poisoned_commit_has_tries_left_and_after_it_was_rejected() {
        let poisoned = ["f1".to_string()];
        let cutoffs = BTreeMap::from([("f1".to_string(), 3)]);
        let events = log(vec![submitted(1, "f1")]);
        assert!(i6(&events, &path(&cutoffs, &poisoned, &[])).is_empty());
        let cutoffs = BTreeMap::from([("f1".to_string(), 4)]);
        let events = log(vec![submitted(1, "f1"), rejected(1, INFRA)]);
        assert!(i6(&events, &path(&cutoffs, &poisoned, &[])).is_empty());
    }

    #[test]
    fn i6_flags_a_poisoned_commit_dispatched_past_its_tries() {
        let poisoned = ["f1".to_string()];
        let cutoffs = BTreeMap::from([("f1".to_string(), 5)]);
        let events = log(vec![submitted(1, "f1"), rejected(1, INFRA)]);
        let found = i6(&events, &path(&cutoffs, &poisoned, &[]));
        assert_eq!(found, ["f1 was dispatched 5 times past its 4 tries"]);
    }

    #[test]
    fn i6_flags_a_poisoned_commit_whose_tries_are_spent_and_whose_claim_was_not_rejected() {
        let poisoned = ["f1".to_string()];
        let cutoffs = BTreeMap::from([("f1".to_string(), 4)]);
        let events = log(vec![submitted(1, "f1")]);
        let found = i6(&events, &path(&cutoffs, &poisoned, &[]));
        assert_eq!(
            found,
            ["claim 1: every try of f1 is spent and it was not rejected"]
        );
        let released = log(vec![
            submitted(1, "f1"),
            EventKind::ClaimReleased {
                claim: ClaimId(1),
                reason: ReleaseReason::Agent,
            },
        ]);
        assert!(i6(&released, &path(&cutoffs, &poisoned, &[])).is_empty());
    }

    #[test]
    fn i6_flags_a_poisoned_commit_that_merged() {
        let poisoned = ["f1".to_string()];
        let events = log(vec![submitted(1, "f1"), merged(1, "h1")]);
        let found = i6(&events, &path(&BTreeMap::new(), &poisoned, &[]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("merged though its commit f1"),
            "{found:?}"
        );
    }

    #[test]
    fn i6_flags_a_claim_rejected_for_infrastructure_after_too_few_cut_off_calls() {
        let events = log(vec![submitted(2, "f2"), rejected(2, INFRA)]);
        let cutoffs = BTreeMap::from([("f2".to_string(), 3)]);
        let found = i6(&events, &path(&cutoffs, &[], &[]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("after only 3 cut-off calls"), "{found:?}");
        let cutoffs = BTreeMap::from([("f2".to_string(), 4)]);
        assert!(i6(&events, &path(&cutoffs, &[], &[])).is_empty());
        let three = BTreeMap::from([("f2".to_string(), 3)]);
        let restarts = MergePath {
            other_failures: 1,
            ..path(&three, &[], &[])
        };
        assert!(i6(&events, &restarts).is_empty(), "a restart is a failure");
    }

    #[test]
    fn i6_flags_work_that_is_on_main_rejected_as_an_infrastructure_failure() {
        let events = log(vec![submitted(2, "f2"), rejected(2, INFRA)]);
        let cutoffs = BTreeMap::from([("f2".to_string(), 4)]);
        let history = [landing("h2", "f2")];
        let found = i6(&events, &path(&cutoffs, &[], &history));
        assert_eq!(
            found,
            ["claim 2 was rejected as an infrastructure failure though its work is on main"]
        );
        let other = log(vec![submitted(2, "f3"), rejected(2, INFRA)]);
        let cutoffs = BTreeMap::from([("f3".to_string(), 4)]);
        assert!(i6(&other, &path(&cutoffs, &[], &history)).is_empty());
    }

    #[test]
    fn i6_does_not_count_a_rejection_for_failing_tests() {
        let events = log(vec![
            submitted(2, "f2"),
            rejected(2, "tests failed (exit code 1)"),
        ]);
        assert!(i6(&events, &path(&BTreeMap::new(), &[], &[landing("h", "f2")])).is_empty());
    }

    // ---- I7 ----

    #[test]
    fn i7_flags_an_answer_that_changed_what_the_host_stores() {
        let quiet = StaleAnswer {
            call: 1,
            dispatch: 2,
            changed: None,
        };
        let loud = StaleAnswer {
            call: 3,
            dispatch: 4,
            changed: Some("changed the stored state".to_string()),
        };
        assert!(i7(std::slice::from_ref(&quiet)).is_empty());
        assert_eq!(
            i7(&[quiet, loud]),
            ["the answer to call 3 for dispatch 4, not in flight, changed the stored state"]
        );
    }

    // ---- I11 ----

    #[test]
    fn i11_compares_the_counters_with_the_log() {
        let events = log(vec![merged(1, "h1"), merged(2, "h2")]);
        let right = Summary::from_events(&events);
        assert!(i11(&events, &right).is_empty());
        let wrong = Summary {
            merges: right.merges + 1,
            ..right.clone()
        };
        assert_eq!(i11(&events, &wrong).len(), 1);
        let short = Summary {
            merges: right.merges - 1,
            ..right
        };
        assert_eq!(i11(&events, &short).len(), 1);
    }

    // ---- I12 ----

    fn note(events_len: usize, call: SlotCall, ready: bool) -> SlotNote {
        SlotNote {
            events_len,
            call,
            verification_ready: ready,
        }
    }

    /// A log with `merges` landings, one event each.
    fn landings_log(merges: usize) -> Vec<Event> {
        log((0..merges).map(|n| merged(n as u64, "h")).collect())
    }

    #[test]
    fn i12_allows_up_to_the_slot_landings_past_a_ready_verification() {
        let events = landings_log(6);
        let mut notes = vec![note(0, SlotCall::MergeStart, true)];
        for len in 1..=VERIFY_SLOT_AFTER {
            notes.push(note(len, SlotCall::MergeStart, true));
        }
        assert!(
            i12(&notes, &events).is_empty(),
            "{:?}",
            i12(&notes, &events)
        );
    }

    #[test]
    fn i12_flags_a_merge_dispatched_after_too_many_landings_with_a_verification_ready() {
        let events = landings_log(6);
        let mut notes = vec![note(0, SlotCall::MergeStart, true)];
        for len in 1..=VERIFY_SLOT_AFTER + 1 {
            notes.push(note(len, SlotCall::MergeStart, true));
        }
        let found = i12(&notes, &events);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("after 5 landings"), "{found:?}");
    }

    #[test]
    fn i12_starts_over_after_a_trial_or_a_merge_dispatched_with_none_ready() {
        let events = landings_log(12);
        let mut notes = vec![note(0, SlotCall::MergeStart, true)];
        notes.push(note(4, SlotCall::Trial, false));
        notes.push(note(4, SlotCall::MergeStart, true));
        notes.push(note(8, SlotCall::MergeStart, true));
        assert!(i12(&notes, &events).is_empty());
        let events = landings_log(12);
        let notes = [
            note(0, SlotCall::MergeStart, true),
            note(4, SlotCall::MergeStart, false),
            note(5, SlotCall::MergeStart, true),
            note(9, SlotCall::MergeStart, true),
        ];
        assert!(i12(&notes, &events).is_empty());
    }

    #[test]
    fn i12_ignores_the_other_calls_of_a_merge() {
        let events = landings_log(8);
        let notes = [
            note(0, SlotCall::MergeStart, true),
            note(6, SlotCall::Other, true),
        ];
        assert!(i12(&notes, &events).is_empty());
    }

    // ---- COORD-HEAD-FROM-AGENT ----

    fn dispatch(request: Request) -> Dispatch {
        Dispatch {
            request,
            events_len: 0,
        }
    }

    fn commit(text: &str) -> crate::protocol::CommitId {
        crate::protocol::CommitId(text.to_string())
    }

    #[test]
    fn the_agent_base_check_flags_each_dispatch_built_on_the_declared_head() {
        let lander_made = [
            dispatch(Request::Test {
                base: commit(MAIN0),
                head: commit("h"),
            }),
            dispatch(Request::Land {
                base: commit(MAIN0),
                head: commit("h"),
            }),
            dispatch(Request::ReadHead {
                submitted: vec![commit(BASE)],
            }),
        ];
        assert!(agent_base(&lander_made, BASE).is_empty());
        let named = |request| agent_base(&[dispatch(request)], BASE);
        let land = Request::Land {
            base: commit(BASE),
            head: commit("h"),
        };
        assert_eq!(
            named(land),
            ["dispatch 0 (land) names the head an agent declared"]
        );
        let trial = Request::Trial {
            agent: AgentId("a".to_string()),
            commit: Some(commit("f")),
            before: commit(MAIN0),
            main: commit(BASE),
        };
        assert_eq!(
            named(trial),
            ["dispatch 0 (trial) names the head an agent declared"]
        );
        let test = Request::Test {
            base: commit(MAIN0),
            head: commit(BASE),
        };
        assert_eq!(named(test).len(), 1);
    }
}

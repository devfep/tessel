//! Checkers for the merge path: I3, I6, I7, I11 and I12 of `docs/proposals/phase-1.md`
//! ("Invariants and the simulator checks that prove them"), and COORD-HEAD-FROM-AGENT's check that
//! no dispatch uses a head an agent declared.
//!
//! Their evidence and advisory clauses wait for the rows that add evidence records and advisory
//! mode (M6): I3 does not count evidence records per landed commit, I11 does not compare a
//! landing's evidence head with the head the lander tested, and none of the five reads advisory
//! releases.

use std::collections::{BTreeMap, BTreeSet};

use crate::host::call::Request;
use crate::merge::MAX_INFRA_RETRIES;
use crate::protocol::{ClaimId, Event, EventKind, ServerMsg, Summary};
use crate::sim::driver::{Delivery, Dispatch, SlotCall, SlotNote, StaleAnswer};
use crate::sim::lander::{Landing, MAIN0};

/// How many merges may be dispatched in a row past a ready verification
/// (`coordinator::verifying::VERIFY_SLOT_AFTER`, private there, so repeated here; no test pins the
/// two together).
pub const VERIFY_SLOT_AFTER: usize = 4;

/// I3: at most one `Merged` per claim, in the log and among the messages clients received; each
/// submission at most once on main's first-parent history; every landing the log reports is on
/// that history.
pub fn i3(events: &[Event], transcript: &[Delivery], history: &[Landing]) -> Vec<String> {
    let mut found = Vec::new();
    let forks = forks_by_claim(events);
    let mut merged: BTreeMap<u64, usize> = BTreeMap::new();
    for event in events {
        if let EventKind::Merged { claim, head, .. } = &event.kind {
            *merged.entry(claim.0).or_insert(0) += 1;
            if !merged_on_main(&forks, claim.0, &head.0, history) {
                found.push(format!(
                    "claim {} merged at {}, which is not on main",
                    claim.0, head.0
                ));
            }
        }
    }
    for (claim, count) in merged.iter().filter(|(_, count)| **count > 1) {
        found.push(format!("claim {claim} was merged {count} times"));
    }
    let mut told: BTreeMap<(u64, u64), usize> = BTreeMap::new();
    for delivery in transcript {
        if let ServerMsg::Merged { claim, .. } = &delivery.msg {
            *told.entry((delivery.sock.0, claim.0)).or_insert(0) += 1;
        }
    }
    for ((sock, claim), count) in told.iter().filter(|(_, count)| **count > 1) {
        found.push(format!(
            "socket {sock} was told claim {claim} merged {count} times"
        ));
    }
    found.extend(repeated_on_main(history));
    found.extend(broken_chain(history));
    found
}

/// The claim a `Merged` event is about.
pub fn merged_claim(kind: &EventKind) -> Option<ClaimId> {
    if let EventKind::Merged { claim, .. } = kind {
        return Some(*claim);
    }
    None
}

/// The claim a rejection or a release ended without a merge.
fn ended_without_merge(kind: &EventKind) -> Option<ClaimId> {
    if let EventKind::SubmitRejected { claim, .. } | EventKind::ClaimReleased { claim, .. } = kind {
        return Some(*claim);
    }
    None
}

/// Whether a claim's `Merged` at `head` is main: either the landing of one of the claim's own
/// submissions, or, when one of them landed earlier (a retry answered `already merged`), a head on
/// main at or after that landing.
fn merged_on_main(
    forks: &BTreeMap<u64, Vec<String>>,
    claim: u64,
    head: &str,
    history: &[Landing],
) -> bool {
    let own = |landing: &Landing| {
        landing
            .fork
            .as_ref()
            .is_some_and(|fork| forks.get(&claim).is_some_and(|mine| mine.contains(fork)))
    };
    if history
        .iter()
        .any(|landing| landing.head == head && own(landing))
    {
        return true;
    }
    let Some(landed) = history.iter().position(own) else {
        return false;
    };
    history[landed..].iter().any(|landing| landing.head == head)
}

/// A landing whose parent is not the main before it: a head put on main that was not made on it,
/// which drops what landed in between.
fn broken_chain(history: &[Landing]) -> Vec<String> {
    let mut found = Vec::new();
    let mut main = MAIN0;
    for landing in history {
        if landing.parent != main {
            found.push(format!(
                "{} landed on {main} but was made on {}",
                landing.head, landing.parent
            ));
        }
        main = landing.head.as_str();
    }
    found
}

/// A fork commit or a head that appears more than once on main's first-parent history.
fn repeated_on_main(history: &[Landing]) -> Vec<String> {
    let mut found = Vec::new();
    let mut heads = BTreeSet::new();
    let mut forks = BTreeSet::new();
    for landing in history {
        if !heads.insert(landing.head.as_str()) {
            found.push(format!("{} is on main twice", landing.head));
        }
        let fork = landing.fork.as_deref();
        if fork.is_some_and(|fork| !forks.insert(fork)) {
            found.push(format!(
                "the submission {} landed on main twice",
                fork.unwrap_or_default()
            ));
        }
    }
    found
}

/// Whether `reason` is the one the core gives a submission it could not merge for infrastructure
/// reasons (`merging::infra_rejection`).
pub fn is_infra_rejection(reason: &str) -> bool {
    reason.contains("infrastructure, not a code failure")
}

/// The fork commits each claim submitted, by claim.
fn forks_by_claim(events: &[Event]) -> BTreeMap<u64, Vec<String>> {
    let mut forks: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    for event in events {
        if let EventKind::Submitted {
            claim, fork_commit, ..
        } = &event.kind
        {
            forks
                .entry(claim.0)
                .or_default()
                .push(fork_commit.0.clone());
        }
    }
    forks
}

/// What I6 compares the log with.
pub struct MergePath<'a> {
    /// Calls the host took as not finished, by fork commit.
    pub cutoffs: &'a BTreeMap<String, u32>,
    /// The fork commits the lander cuts off at one stage, every time.
    pub poisoned: &'a [String],
    /// Restarts and failed commits that struck while a merge of the fork commit was in flight,
    /// each of which the core can count against it, or lose a count of.
    pub strikes: &'a BTreeMap<String, u64>,
    /// Main's first-parent history.
    pub history: &'a [Landing],
}

/// I6: a commit that cuts the merge path off every time is dispatched at most
/// `MAX_INFRA_RETRIES + 1` times (and once more for each restart or failed commit that struck
/// during one of its merges, which can lose a count), then its claim is rejected and never merged.
/// A claim is rejected as an infrastructure failure only after that many of its own calls were cut
/// off or such failures struck, so no claim is rejected because of another's commit, and work that
/// is on main is never rejected.
pub fn i6(events: &[Event], path: &MergePath<'_>) -> Vec<String> {
    let mut found = Vec::new();
    let by_claim = forks_by_claim(events);
    for fork in path.poisoned {
        let allowed =
            u64::from(MAX_INFRA_RETRIES) + 1 + path.strikes.get(fork).copied().unwrap_or(0);
        let seen = u64::from(path.cutoffs.get(fork).copied().unwrap_or(0));
        if seen > allowed {
            found.push(format!(
                "{fork} was dispatched {seen} times past its {allowed} tries"
            ));
        }
        found.extend(poisoned_outcome(events, &by_claim, fork, seen >= allowed));
    }
    found.extend(infra_rejections(events, &by_claim, path));
    found
}

/// Each rejection as an infrastructure failure: charged enough, and not for landed work.
fn infra_rejections(
    events: &[Event],
    by_claim: &BTreeMap<u64, Vec<String>>,
    path: &MergePath<'_>,
) -> Vec<String> {
    let mut found = Vec::new();
    let mut latest: BTreeMap<u64, &str> = BTreeMap::new();
    for event in events {
        if let EventKind::Submitted {
            claim, fork_commit, ..
        } = &event.kind
        {
            latest.insert(claim.0, fork_commit.0.as_str());
        }
        let EventKind::SubmitRejected { claim, reason } = &event.kind else {
            continue;
        };
        if !is_infra_rejection(reason) {
            continue;
        }
        let charged: u64 = by_claim
            .get(&claim.0)
            .into_iter()
            .flatten()
            .map(|fork| {
                u64::from(path.cutoffs.get(fork).copied().unwrap_or(0))
                    + path.strikes.get(fork).copied().unwrap_or(0)
            })
            .sum();
        if charged <= u64::from(MAX_INFRA_RETRIES) {
            found.push(format!(
                "claim {} was rejected as an infrastructure failure after only {charged} failures",
                claim.0
            ));
        }
        let landed = latest.get(&claim.0).is_some_and(|fork| {
            path.history
                .iter()
                .any(|landing| landing.fork.as_deref() == Some(*fork))
        });
        if landed {
            found.push(format!(
                "claim {} was rejected as an infrastructure failure though its work is on main",
                claim.0
            ));
        }
    }
    found
}

/// What became of the claim that submitted a poisoned commit: it must not merge, and once every
/// try is spent it must have been rejected (or have ended some other way).
fn poisoned_outcome(
    events: &[Event],
    by_claim: &BTreeMap<u64, Vec<String>>,
    fork: &str,
    spent: bool,
) -> Vec<String> {
    let mut found = Vec::new();
    let Some((claim, _)) = by_claim
        .iter()
        .find(|(_, forks)| forks.iter().any(|f| f == fork))
    else {
        return found;
    };
    let ended_by = |wanted: fn(&EventKind, ClaimId) -> bool| {
        events
            .iter()
            .any(|event| wanted(&event.kind, ClaimId(*claim)))
    };
    if ended_by(|kind, claim| merged_claim(kind) == Some(claim)) {
        found.push(format!(
            "claim {claim} merged though its commit {fork} cuts the merge path off every time"
        ));
    }
    let gone = ended_by(|kind, claim| ended_without_merge(kind) == Some(claim));
    if spent && !gone {
        found.push(format!(
            "claim {claim}: every try of {fork} is spent and it was not rejected"
        ));
    }
    found
}

/// I7: an answer for a dispatch that was not in flight changes nothing the host stores.
pub fn i7(stale: &[StaleAnswer]) -> Vec<String> {
    stale
        .iter()
        .filter_map(|answer| {
            let changed = answer.changed.as_ref()?;
            Some(format!(
                "the answer to call {} for dispatch {}, not in flight, {changed}",
                answer.call, answer.dispatch
            ))
        })
        .collect()
}

/// I11 (without its evidence clause): the counters the host keeps as it goes equal
/// `Summary::from_events` over the log.
pub fn i11(events: &[Event], counters: &Summary) -> Vec<String> {
    let expected = Summary::from_events(events);
    if expected == *counters {
        return Vec::new();
    }
    vec![format!(
        "the counters {counters:?} differ from the log's {expected:?}"
    )]
}

/// I12 (without advisory mode): a verification that is ready is dispatched within a bounded number
/// of landings. Counted in landings, not dispatches, because a merge in flight is never
/// interrupted and a `Land` in doubt holds the verification back; the bound is one more than the
/// core's `VERIFY_SLOT_AFTER` for the merge that was already out when the verification became
/// ready.
///
/// Unproven at the default seeds: a seed lands 0 to 2 merges, so the bound is never reached there.
/// Only the hand-built notes in this file exercise it; a continuous-load run is not built.
pub fn i12(notes: &[SlotNote], events: &[Event]) -> Vec<String> {
    let mut found = Vec::new();
    let mut ready_since: Option<usize> = None;
    for note in notes {
        match (note.call, note.verification_ready) {
            (SlotCall::Trial, _) | (SlotCall::MergeStart, false) => ready_since = None,
            (SlotCall::MergeStart, true) => {
                let since = *ready_since.get_or_insert(note.events_len);
                let landed = landings(events, since, note.events_len);
                if landed > VERIFY_SLOT_AFTER {
                    found.push(format!(
                        "a merge was dispatched after {landed} landings with a verification ready"
                    ));
                }
            }
            (SlotCall::Other, _) => {}
        }
    }
    found
}

/// How many `Merged` events the log holds with `from <= seq < to`, as positions in the log.
fn landings(events: &[Event], from: usize, to: usize) -> usize {
    events
        .iter()
        .skip(from)
        .take(to.saturating_sub(from))
        .filter(|event| merged_claim(&event.kind).is_some())
        .count()
}

/// COORD-HEAD-FROM-AGENT: no dispatch names `declared`, the head agents say they are on. The core
/// reads main's head from the lander; a call built on an agent's word is built on a commit main
/// may never have held.
pub fn agent_base(dispatches: &[Dispatch], declared: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (index, dispatch) in dispatches.iter().enumerate() {
        let names = match &dispatch.request {
            Request::Test { base, head } | Request::Land { base, head } => {
                vec![base.0.as_str(), head.0.as_str()]
            }
            Request::Trial {
                commit,
                before,
                main,
                ..
            } => {
                let mut names = vec![before.0.as_str(), main.0.as_str()];
                names.extend(commit.iter().map(|c| c.0.as_str()));
                names
            }
            Request::Reconcile { head } => vec![head.0.as_str()],
            Request::Prepare { .. } | Request::ReadHead { .. } => Vec::new(),
        };
        if names.contains(&declared) {
            found.push(format!(
                "dispatch {index} ({}) names the head an agent declared",
                request_name(&dispatch.request)
            ));
        }
    }
    found
}

fn request_name(request: &Request) -> &'static str {
    match request {
        Request::Prepare { .. } => "prepare",
        Request::Test { .. } => "test",
        Request::Land { .. } => "land",
        Request::Trial { .. } => "trial",
        Request::ReadHead { .. } => "read head",
        Request::Reconcile { .. } => "reconcile",
    }
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

    const INFRA: &str =
        "the merge could not be completed after 4 tries: infrastructure, not a code \
                         failure; submit again";

    fn landing(head: &str, fork: &str) -> Landing {
        landing_on(MAIN0, head, fork)
    }

    fn landing_on(parent: &str, head: &str, fork: &str) -> Landing {
        Landing {
            head: head.to_string(),
            parent: parent.to_string(),
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
        let events = log(vec![
            submitted(1, "f1"),
            submitted(2, "f2"),
            merged(1, "h1"),
            merged(2, "h2"),
        ]);
        let history = [landing("h1", "f1"), landing_on("h1", "h2", "f2")];
        assert!(i3(&events, &[told_merged(0, 1)], &history).is_empty());
    }

    #[test]
    fn i3_flags_a_claim_merged_twice_in_the_log() {
        let events = log(vec![submitted(1, "f1"), merged(1, "h1"), merged(1, "h1")]);
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
    fn i3_flags_a_submission_on_main_twice() {
        let history = [landing("h1", "f1"), landing_on("h1", "h2", "f1")];
        let found = i3(&[], &[], &history);
        assert_eq!(found, ["the submission f1 landed on main twice"]);
        let found = i3(
            &[],
            &[],
            &[landing("h1", "f1"), landing_on("h1", "h1", "f2")],
        );
        assert_eq!(found, ["h1 is on main twice"]);
    }

    #[test]
    fn i3_flags_a_merge_that_is_not_the_claims_own_landing() {
        let history = [landing("h1", "f1"), landing_on("h1", "h2", "f2")];
        let elsewhere = log(vec![submitted(1, "f1"), merged(1, "elsewhere")]);
        let found = i3(&elsewhere, &[], &history);
        assert_eq!(found, ["claim 1 merged at elsewhere, which is not on main"]);
        let others = log(vec![submitted(1, "f9"), merged(1, "h1")]);
        let found = i3(&others, &[], &history);
        assert_eq!(found, ["claim 1 merged at h1, which is not on main"]);
        let retry = log(vec![submitted(1, "f1"), merged(1, "h2")]);
        assert!(
            i3(&retry, &[], &history).is_empty(),
            "an already-merged retry"
        );
    }

    #[test]
    fn i3_flags_a_retry_merge_at_a_head_from_before_the_claims_own_landing() {
        let history = [landing("h1", "f1"), landing_on("h1", "h2", "f2")];
        let before = log(vec![submitted(2, "f2"), merged(2, "h1")]);
        let found = i3(&before, &[], &history);
        assert_eq!(found, ["claim 2 merged at h1, which is not on main"]);
        let at = log(vec![submitted(2, "f2"), merged(2, "h2")]);
        assert!(i3(&at, &[], &history).is_empty());
    }

    #[test]
    fn i3_flags_a_landing_that_was_not_made_on_the_main_before_it() {
        let history = [landing("h1", "f1"), landing_on(MAIN0, "h2", "f2")];
        let found = i3(&[], &[], &history);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("but was made on"), "{found:?}");
        assert!(i3(&[], &[], &[landing_on("zzz", "h1", "f1")]).len() == 1);
    }

    // ---- I6 ----

    static NO_STRIKES: BTreeMap<String, u64> = BTreeMap::new();

    fn path<'a>(
        cutoffs: &'a BTreeMap<String, u32>,
        poisoned: &'a [String],
        history: &'a [Landing],
    ) -> MergePath<'a> {
        MergePath {
            cutoffs,
            poisoned,
            strikes: &NO_STRIKES,
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
        assert!(found[0].contains("after only 3 failures"), "{found:?}");
        let cutoffs = BTreeMap::from([("f2".to_string(), 4)]);
        assert!(i6(&events, &path(&cutoffs, &[], &[])).is_empty());
        let three = BTreeMap::from([("f2".to_string(), 3)]);
        let struck = BTreeMap::from([("f2".to_string(), 1)]);
        let restarts = MergePath {
            strikes: &struck,
            ..path(&three, &[], &[])
        };
        assert!(i6(&events, &restarts).is_empty(), "a restart is a failure");
        let elsewhere = BTreeMap::from([("f9".to_string(), 7)]);
        let other = MergePath {
            strikes: &elsewhere,
            ..path(&three, &[], &[])
        };
        assert_eq!(
            i6(&events, &other).len(),
            1,
            "another commit's restarts do not count"
        );
    }

    #[test]
    fn i6_allows_a_poisoned_commit_one_more_try_for_each_restart_that_struck_it() {
        let poisoned = ["f1".to_string()];
        let cutoffs = BTreeMap::from([("f1".to_string(), 5)]);
        let events = log(vec![submitted(1, "f1"), rejected(1, INFRA)]);
        let struck = BTreeMap::from([("f1".to_string(), 1)]);
        let path = MergePath {
            strikes: &struck,
            ..path(&cutoffs, &poisoned, &[])
        };
        assert!(i6(&events, &path).is_empty());
        let elsewhere = BTreeMap::from([("f2".to_string(), 9)]);
        let other = MergePath {
            strikes: &elsewhere,
            ..path
        };
        assert_eq!(
            i6(&events, &other).len(),
            1,
            "other commits' restarts excuse nothing"
        );
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

//! Host tests of the call path: each is named for the rule of `docs/reference/runtime-rules.md`
//! it pins. They drive the host through the testkit, so they read transcripts and outputs, not
//! the host's fields.

use serde_json::{json, Value};

use super::batch::{Held, Pending};
use super::call::{Call, CallAnswer, CallId, DispatchId, Request};
use super::call::{Refusal, TrialResult};
use super::testkit::Testkit;
use super::wake;
use super::{BatchId, HostConfig, Input, Output, SocketId};
use crate::coordinator::{Coordinator as Core, Effect};
use crate::merge::{MergeOutcome, MERGE_WATCHDOG_MS};
use crate::protocol::{
    AgentId, Assumption, ClaimId, ClientMsg, CommitId, DecisionRecord, ErrorCode, Fence, Intent,
    Mode, OnConflict, RequestId, Scope, ScopeClaim, ServerMsg,
};
use crate::shell::{self, Outbound, HARD_ENTRY_BYTES, SOFT_ENTRY_BYTES, STATE_LIMIT_MESSAGE};

const NOW_MS: u64 = 1_000;

fn config() -> HostConfig {
    HostConfig {
        repo: "repo-1".to_string(),
        run: Some("test".to_string()),
        ..HostConfig::default()
    }
}

fn kit() -> Testkit {
    Testkit::new(config())
}

fn hello(name: &str) -> ClientMsg {
    let hello = json!({"type": "hello", "agent": name, "base": "abc"});
    serde_json::from_value(hello).unwrap()
}

fn edit_file(path: &str) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::File { path: path.into() },
        mode: Mode::EditBody,
    }
}

fn claim(req: u64, path: &str, summary: &str) -> ClientMsg {
    ClientMsg::Claim {
        req: RequestId(req),
        intent: Intent {
            summary: summary.to_string(),
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes: vec![edit_file(path)],
        on_conflict: OnConflict::Fail,
    }
}

fn label(output: &Output) -> &'static str {
    match output {
        Output::Load => "Load",
        Output::Commit { .. } => "Commit",
        Output::Send { .. } => "Send",
        Output::SetSession { .. } => "SetSession",
        Output::Close { .. } => "Close",
        Output::ReadEvents { .. } => "ReadEvents",
        Output::SetWake { .. } => "SetWake",
        Output::Call(_) => "Call",
        Output::Log { .. } => "Log",
        Output::Metric { .. } => "Metric",
    }
}

/// What the host produced after `mark` outputs, by kind.
fn labels_since(kit: &Testkit, mark: usize) -> Vec<&'static str> {
    kit.history()[mark..].iter().map(label).collect()
}

fn count(kit: &Testkit, wanted: &str) -> usize {
    kit.history()
        .iter()
        .filter(|output| label(output) == wanted)
        .count()
}

/// Open a socket for `name`, say hello and let the host finish.
fn welcomed(kit: &mut Testkit, name: &str) -> SocketId {
    let sock = kit.open(name);
    kit.send(sock, &hello(name));
    kit.settle();
    sock
}

fn granted(kit: &Testkit, sock: SocketId) -> (ClaimId, Fence) {
    for msg in kit.received(sock) {
        if let ServerMsg::Granted { claim, fence, .. } = msg {
            return (*claim, *fence);
        }
    }
    panic!("no grant in {:?}", kit.received(sock));
}

/// The answer of a merge that landed `new` on top of `old`.
fn landed() -> CallAnswer {
    CallAnswer::Prepared {
        base: CommitId("old".into()),
        head: CommitId("new".into()),
    }
}

fn state(kit: &Testkit) -> Value {
    serde_json::from_str(kit.stored_state().expect("a stored state")).unwrap()
}

/// Run the wake that is due now until it waits for a call or ends.
fn wake(kit: &mut Testkit) {
    kit.wake_now();
    kit.settle();
}

/// A wake suspended at a head read: a poke is counted and the wake asks for the head.
fn head_read_outstanding(kit: &mut Testkit) -> Call {
    kit.poke();
    kit.settle();
    wake(kit);
    let call = kit.calls().last().expect("the wake asked for the head");
    assert!(matches!(call.request, Request::ReadHead { .. }), "{call:?}");
    call.clone()
}

/// The stored core of a repo with enough 10 KB claims to be at least `at_least` bytes.
fn core_json_of_size(at_least: usize) -> String {
    let mut core = shell::load_core(None, Some("test"), None, Vec::new(), Vec::new()).unwrap();
    let summary = "x".repeat(10_000);
    let mut made = 0;
    loop {
        let json = serde_json::to_string(&core).unwrap();
        if json.len() >= at_least {
            return json;
        }
        let missing = (at_least - json.len()) / 11_000;
        for _ in 0..missing.max(1) {
            made += 1;
            let agent = AgentId(format!("filler-{made}"));
            core.handle(&agent, claim(1, &format!("f{made}.rs"), &summary), 1_000);
        }
    }
}

#[test]
fn host_a_load_builds_the_core_from_the_stored_state_and_the_deployment() {
    let mut fresh = Testkit::new(HostConfig {
        run: Some("fresh".to_string()),
        reviewers: Some("ana".to_string()),
        sensitive: Some("a/".to_string()),
        ..config()
    });
    welcomed(&mut fresh, "a1");
    assert_eq!(label(&fresh.history()[1]), "Load", "the first call loads");
    let created = state(&fresh);
    assert_eq!(created["config"]["run"], json!("fresh"));
    assert_eq!(created["reviewers"], json!(["ana"]));
    assert_eq!(created["sensitive"], json!(["a/"]));

    let mut reloaded = Testkit::new(HostConfig {
        run: Some("other".to_string()),
        reviewers: Some("bo".to_string()),
        sensitive: Some("b/".to_string()),
        ..config()
    });
    reloaded.seed("core", fresh.stored_state().unwrap().to_string());
    welcomed(&mut reloaded, "a1");
    let state = state(&reloaded);
    assert_eq!(
        state["config"]["run"],
        json!("fresh"),
        "a stored run is kept"
    );
    assert_eq!(state["reviewers"], json!(["bo"]));
    assert_eq!(state["sensitive"], json!(["b/"]));
}

#[test]
fn host_bad_deployment_lists_fail_closed_and_are_logged_without_their_text() {
    let mut kit = Testkit::new(HostConfig {
        reviewers: Some("ana, not valid".to_string()),
        sensitive: Some("a/, /abs".to_string()),
        ..config()
    });
    welcomed(&mut kit, "a1");
    assert_eq!(state(&kit)["reviewers"], json!([]), "nobody can review");
    assert_eq!(state(&kit)["sensitive"], json!([""]), "everything is held");
    let logs = kit.logs().join("\n");
    assert!(logs.contains("nobody can review"), "{logs}");
    assert!(logs.contains("every submission is held"), "{logs}");
    assert!(logs.contains("repo-1"), "{logs}");
    assert!(
        !logs.contains("not valid") && !logs.contains("/abs"),
        "{logs}"
    );
}

#[test]
fn host_corrupt_stored_state_fails_the_call_and_never_starts_empty() {
    let mut kit = kit();
    kit.seed("core", r#"{"claims":"intent-text""#.to_string());
    let sock = kit.open("a1");
    kit.send(sock, &hello("a1"));
    kit.settle();

    assert_eq!(
        count(&kit, "Commit"),
        0,
        "nothing is stored over the corrupt state"
    );
    assert_eq!(kit.stored_state(), Some(r#"{"claims":"intent-text""#));
    assert!(kit.received(sock).is_empty());
    let logs = kit.logs().join("\n");
    assert!(logs.contains("load state failed"), "{logs}");
    assert!(
        !logs.contains("intent-text"),
        "stored text is not echoed: {logs}"
    );

    kit.send(sock, &hello("a1"));
    assert_eq!(count(&kit, "Load"), 2, "the next call loads again");
}

#[test]
fn host_a_load_that_finishes_second_keeps_the_core_already_loaded() {
    let mut kit = kit();
    welcomed(&mut kit, "a1");
    let other = shell::load_core(None, Some("other"), None, Vec::new(), Vec::new()).unwrap();
    let stray = Input::Loaded {
        now_ms: kit.now_ms(),
        stored: Some(serde_json::to_string(&other).unwrap()),
    };
    kit.input(stray);
    kit.settle();

    let sock = kit.open("a2");
    kit.send(sock, &hello("a2"));
    kit.settle();
    assert_eq!(count(&kit, "Load"), 1, "a loaded core is not loaded again");
    assert_eq!(
        state(&kit)["config"]["run"],
        json!("test"),
        "the first core stays"
    );
}

#[test]
fn host_the_wake_is_computed_after_each_step_with_this_instances_waits() {
    let mut kit = kit();
    let (sock, _, _) = submitted(&mut kit);
    assert_eq!(kit.wake(), Some(kit.now_ms() + 1), "the merge is due now");

    wake(&mut kit);
    assert_eq!(kit.calls().len(), 1);

    kit.send(sock, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(
        kit.wake(),
        Some(kit.now_ms() + MERGE_WATCHDOG_MS),
        "a merge waiting here leaves only the watchdog"
    );

    kit.reply(CallId(1), landed());
    kit.settle();
    assert_ne!(
        kit.wake(),
        Some(kit.now_ms() + MERGE_WATCHDOG_MS),
        "the answered call no longer holds the wake back"
    );
}

#[test]
fn host_a_refused_or_oversized_call_drops_the_core() {
    let mut soft = kit();
    soft.seed("core", core_json_of_size(SOFT_ENTRY_BYTES - 30_000));
    let sock = welcomed(&mut soft, "a1");
    let mark = soft.history().len();
    soft.send(sock, &claim(7, "big.rs", &"y".repeat(60_000)));
    assert_eq!(
        labels_since(&soft, mark),
        ["Log", "Send"],
        "refused at once"
    );
    let ServerMsg::Error { req, code, message } = soft.received(sock).last().unwrap() else {
        panic!("the sender is told");
    };
    assert_eq!(*req, Some(RequestId(7)));
    assert_eq!(*code, ErrorCode::Malformed);
    assert_eq!(message, STATE_LIMIT_MESSAGE);
    let mark = soft.history().len();
    soft.send(sock, &ClientMsg::Heartbeat);
    assert_eq!(labels_since(&soft, mark), ["Load"], "the core was dropped");
    soft.settle();
    assert!(
        !soft.stored_state().unwrap().contains("big.rs"),
        "nothing was stored"
    );

    let mut hard = kit();
    hard.seed("core", core_json_of_size(HARD_ENTRY_BYTES + 1));
    let sock = hard.open("a1");
    hard.send(sock, &hello("a1"));
    hard.settle();
    assert_eq!(
        count(&hard, "Commit"),
        0,
        "nothing is stored over the hard limit"
    );
    assert!(hard.received(sock).is_empty());
    assert!(hard.logs().join("\n").contains("store state failed"));
    let mark = hard.history().len();
    hard.send(sock, &hello("a1"));
    assert_eq!(labels_since(&hard, mark), ["Load"], "the core was dropped");
}

#[test]
fn host_the_soft_limit_judges_growth_against_the_size_last_written() {
    let mut json = core_json_of_size(SOFT_ENTRY_BYTES + 3_000);
    json.push_str(&" ".repeat(100_000));
    let mut kit = kit();
    kit.seed("core", json);
    let sock = welcomed(&mut kit, "a1");

    kit.send(sock, &claim(1, "a.rs", "small"));
    kit.settle();
    assert!(
        matches!(kit.received(sock).last(), Some(ServerMsg::Error { .. })),
        "the size is the one just written, not the padded one that was read, so this grows it"
    );
}

#[test]
fn host_the_core_is_reloaded_after_a_steward_call_before_the_answer_is_applied() {
    let mut kit = kit();
    welcomed(&mut kit, "a1");
    let call = head_read_outstanding(&mut kit);

    kit.fail_next_commit();
    kit.poke();
    kit.settle();
    let mark = kit.history().len();
    kit.reply(call.id, CallAnswer::Unknown);
    assert_eq!(
        labels_since(&kit, mark),
        ["Load"],
        "the dropped core is reloaded first"
    );
    kit.settle();
    assert_eq!(
        labels_since(&kit, mark),
        ["Load", "Log", "Commit", "SetWake", "SetWake"],
        "then the answer is applied and stored, a head that is not one is logged, the wake ends"
    );
    assert!(
        !kit.logs().iter().any(|line| line.contains("not loaded")),
        "{:?}",
        kit.logs()
    );
}

#[test]
fn an_answer_for_a_call_the_host_does_not_wait_on_changes_nothing() {
    let mut kit = kit();
    welcomed(&mut kit, "a1");
    let before = kit.stored_state().unwrap().to_string();
    let mark = kit.history().len();
    kit.reply(CallId(99), CallAnswer::Tested);
    assert_eq!(labels_since(&kit, mark), ["Log"]);
    assert!(kit.logs().last().unwrap().contains("no such call"));
    assert_eq!(kit.stored_state(), Some(before.as_str()));
}

#[test]
fn a_commit_answer_for_another_batch_is_ignored() {
    let mut kit = kit();
    let sock = kit.open("a1");
    kit.send(sock, &hello("a1"));
    kit.deliver_next();
    let mark = kit.history().len();
    let wrong = Input::Committed {
        now_ms: kit.now_ms(),
        batch: BatchId(99),
    };
    kit.input(wrong);
    assert_eq!(labels_since(&kit, mark), ["Log"], "nothing is released");
    assert!(kit.received(sock).is_empty());
    kit.settle();
    assert_eq!(
        kit.received(sock).len(),
        1,
        "its own batch releases the Welcome"
    );
}

#[test]
fn host_due_leases_expire_in_their_own_write_before_a_client_message() {
    let mut kit = kit();
    let first = welcomed(&mut kit, "a1");
    kit.send(first, &claim(1, "x.rs", "s"));
    kit.settle();
    let commits = count(&kit, "Commit");

    kit.send(first, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(count(&kit, "Commit"), commits + 1, "nothing due, one write");

    kit.advance(shell::LEASE_MS - 1);
    kit.send(first, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(
        count(&kit, "Commit"),
        commits + 2,
        "a lease not yet due is not expired"
    );

    kit.advance(shell::LEASE_MS + 1);
    let second = kit.open("a2");
    let mark = kit.history().len();
    kit.send(second, &hello("a2"));
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit"],
        "the expiry is a write of its own"
    );
    assert!(kit.received(second).is_empty(), "the message waits for it");
    kit.deliver_next();
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit", "Send", "SetWake", "Commit"],
        "the owner is told, then the message runs"
    );
    assert!(matches!(
        kit.received(first).last(),
        Some(ServerMsg::LeaseExpired { .. })
    ));
    kit.settle();
    kit.send(second, &claim(2, "x.rs", "s"));
    kit.settle();
    assert!(matches!(
        kit.received(second).last(),
        Some(ServerMsg::Granted { .. })
    ));
}

#[test]
fn host_a_client_call_persists_then_binds_then_settles() {
    let mut kit = kit();
    let sock = kit.open("a1");
    kit.send(sock, &hello("a1"));
    assert_eq!(labels_since(&kit, 0), ["SetSession", "Load"]);
    kit.deliver_next();
    assert_eq!(labels_since(&kit, 0), ["SetSession", "Load", "Commit"]);
    assert!(kit
        .session(sock)
        .is_some_and(|session| session.agent.is_none()));
    assert!(kit.received(sock).is_empty());
    kit.deliver_next();
    assert_eq!(
        labels_since(&kit, 0),
        [
            "SetSession",
            "Load",
            "Commit",
            "SetSession",
            "Send",
            "SetWake"
        ],
        "persist, then bind, then settle with the sender"
    );
    assert_eq!(
        kit.session(sock).and_then(|session| session.agent.clone()),
        Some(AgentId("a1".to_string()))
    );
    assert!(matches!(kit.received(sock)[0], ServerMsg::Welcome { .. }));

    let mark = kit.history().len();
    kit.send(sock, &claim(1, "x.rs", "s"));
    kit.settle();
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit", "Send", "SetWake"],
        "a grant binds nothing"
    );
}

#[test]
fn host_a_failed_commit_drops_the_core_and_sends_nothing() {
    let mut kit = kit();
    let sock = welcomed(&mut kit, "a1");
    let before = kit.stored_state().unwrap().to_string();
    let received = kit.received(sock).len();

    kit.fail_next_commit();
    kit.send(sock, &claim(1, "x.rs", "s"));
    kit.settle();
    assert_eq!(kit.closed(sock).map(|(code, _)| *code), Some(1011));
    assert_eq!(kit.received(sock).len(), received, "nothing is sent");
    assert_eq!(kit.stored_state(), Some(before.as_str()));
    assert!(kit
        .logs()
        .last()
        .unwrap()
        .contains("persist state and events failed"));

    assert_eq!(
        count(&kit, "Load"),
        2,
        "the core is reloaded from storage, for the withdrawal of the closed socket"
    );
}

/// Release a stored batch with these outbound messages and no sender, with a1 connected and no
/// socket for "ghost", and say what the host produced.
fn release_with(outbound: Vec<Outbound>) -> (Testkit, SocketId, Vec<&'static str>) {
    let mut kit = kit();
    let sock = welcomed(&mut kit, "a1");
    let mark = kit.history().len();
    let held = Held {
        outbound,
        events: Vec::new(),
        reply_to: None,
        bind: None,
        next_wake_ms: None,
        calls: Vec::new(),
    };
    let committed = Pending::new(BatchId(99), Vec::new(), held, None).commit();
    let mut out = Vec::new();
    kit.host.release(kit.now_ms(), committed, &mut out);
    kit.execute(out);
    let labels = labels_since(&kit, mark);
    (kit, sock, labels)
}

fn welcome(lease_ms: u64) -> ServerMsg {
    ServerMsg::Welcome {
        head: CommitId("ab".repeat(20)),
        lease_ms,
        protocol: 1,
    }
}

fn mixed_outbound() -> Vec<Outbound> {
    vec![
        Outbound::Reply(welcome(1)),
        Outbound::Notify {
            agent: AgentId("ghost".to_string()),
            msg: welcome(2),
        },
        Outbound::Notify {
            agent: AgentId("a1".to_string()),
            msg: welcome(77),
        },
    ]
}

#[test]
fn host_settle_delivers_then_reschedules_even_when_delivery_failed() {
    let (kit, sock, labels) = release_with(mixed_outbound());
    assert_eq!(labels, ["Log", "Send", "SetWake"]);
    assert!(
        matches!(
            kit.received(sock).last(),
            Some(ServerMsg::Welcome { lease_ms: 77, .. })
        ),
        "delivery went on after the failure"
    );
}

#[test]
fn host_only_a_failed_send_to_the_sender_is_an_error() {
    let (kit, sock, labels) = release_with(mixed_outbound());
    assert_eq!(labels.iter().filter(|label| **label == "Log").count(), 1);
    assert!(kit.logs().last().unwrap().contains("deliver failed"));
    let welcomes = kit
        .received(sock)
        .iter()
        .filter(|msg| matches!(msg, ServerMsg::Welcome { .. }))
        .count();
    assert_eq!(
        welcomes, 2,
        "the hello's own and the notification; no reply, no ghost"
    );
}

#[test]
fn host_a_frame_during_an_outstanding_commit_is_applied_only_after_committed() {
    let mut kit = kit();
    let first = welcomed(&mut kit, "a1");
    let second = welcomed(&mut kit, "a2");
    let (first_before, second_before) = (kit.received(first).len(), kit.received(second).len());

    let mark = kit.history().len();
    kit.send(first, &claim(1, "x.rs", "s"));
    assert_eq!(labels_since(&kit, mark), ["Commit"]);
    kit.send(second, &claim(1, "y.rs", "s"));
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit"],
        "the second frame waits for the commit"
    );

    kit.deliver_next();
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit", "Send", "SetWake", "Commit"],
        "the first is settled, then the queued frame runs"
    );
    assert_eq!(kit.received(first).len(), first_before + 1);
    assert_eq!(
        kit.received(second).len(),
        second_before,
        "its reply waits for its own commit"
    );
    kit.settle();
    assert_eq!(kit.received(second).len(), second_before + 1);
}

#[test]
fn host_a_frame_during_an_outstanding_call_is_applied_at_once() {
    let mut kit = kit();
    let sock = welcomed(&mut kit, "a1");
    head_read_outstanding(&mut kit);
    assert_eq!(kit.calls().len(), 1, "a call is outstanding");

    let mark = kit.history().len();
    kit.send(sock, &ClientMsg::Heartbeat);
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit"],
        "the frame does not wait for the call"
    );
}

#[test]
fn host_inputs_queue_behind_a_commit_not_behind_a_call() {
    let mut kit = kit();
    let sock = welcomed(&mut kit, "a1");
    let call = head_read_outstanding(&mut kit);

    let mark = kit.history().len();
    kit.send(sock, &ClientMsg::Heartbeat);
    kit.send(sock, &ClientMsg::Heartbeat);
    assert_eq!(labels_since(&kit, mark), ["Commit"], "one commit at a time");
    kit.reply(call.id, CallAnswer::Unknown);
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit"],
        "an answer waits behind a commit as any input does"
    );
    kit.settle();
    let commits = labels_since(&kit, mark)
        .iter()
        .filter(|l| **l == "Commit")
        .count();
    assert_eq!(commits, 3, "every input ran, in order");
}

fn senders_since(kit: &Testkit, mark: usize) -> Vec<SocketId> {
    let mut socks = Vec::new();
    for output in &kit.history()[mark..] {
        if let Output::Send { sock, .. } = output {
            socks.push(*sock);
        }
    }
    socks
}

#[test]
fn host_queued_frames_run_in_the_order_they_arrived() {
    let mut kit = kit();
    let first = welcomed(&mut kit, "a1");
    let second = welcomed(&mut kit, "a2");
    let third = welcomed(&mut kit, "a3");

    let mark = kit.history().len();
    kit.send(first, &claim(1, "x.rs", "s"));
    kit.send(third, &claim(1, "z.rs", "s"));
    kit.send(second, &claim(1, "y.rs", "s"));
    kit.settle();
    assert_eq!(
        senders_since(&kit, mark),
        [first, third, second],
        "replies follow the order the frames were queued in"
    );
}

#[test]
fn host_inputs_the_host_does_not_serve_yet_are_logged_not_dropped_silently() {
    let mut kit = kit();
    welcomed(&mut kit, "a1");
    let now_ms = kit.now_ms();
    let mark = kit.history().len();
    kit.input(Input::ReplayPage {
        now_ms,
        sock: SocketId(0),
        events: Vec::new(),
        done: true,
    });
    assert_eq!(labels_since(&kit, mark), ["Log"]);
    let line = kit.logs().last().unwrap();
    assert!(line.contains("repo-1"), "{line}");
    assert!(
        line.contains("a replay page is not served by this host yet"),
        "{line}"
    );
}

#[test]
fn a_commit_answer_for_an_earlier_batch_is_ignored() {
    let mut kit = kit();
    let sock = welcomed(&mut kit, "a1");
    kit.send(sock, &claim(1, "x.rs", "s"));
    let received = kit.received(sock).len();
    let mark = kit.history().len();
    let stale = Input::Committed {
        now_ms: kit.now_ms(),
        batch: BatchId(0),
    };
    kit.input(stale);
    assert_eq!(labels_since(&kit, mark), ["Log"], "nothing is released");
    assert_eq!(kit.received(sock).len(), received);
    kit.settle();
    assert_eq!(
        kit.received(sock).len(),
        received + 1,
        "its own batch releases"
    );
}

#[test]
fn host_a_failed_commit_of_a_batch_holding_a_call_hands_out_nothing() {
    let mut kit = kit();
    submitted(&mut kit);
    kit.wake_now();
    kit.fail_next_commit();
    let mark = kit.history().len();
    kit.settle();
    assert_eq!(
        labels_since(&kit, mark),
        ["SetWake", "Commit", "Log", "SetWake"],
        "the expiry is stored; the marker's batch fails and hands out no Call"
    );
    assert!(kit.calls().is_empty());
    assert!(
        !state(&kit)["merge_in_flight"].is_object(),
        "no marker is stored"
    );
    kit.reply(CallId(1), landed());
    assert!(kit.logs().last().unwrap().contains("no such call"));
}

fn grant_in(
    core: &mut Core,
    who: &str,
    path: &str,
    assumptions: Vec<Assumption>,
) -> (ClaimId, Fence) {
    let msg = ClientMsg::Claim {
        req: RequestId(1),
        intent: Intent {
            summary: "s".to_string(),
            task_ref: None,
            assumptions,
        },
        scopes: vec![edit_file(path)],
        on_conflict: OnConflict::Fail,
    };
    for effect in core.handle(&AgentId(who.to_string()), msg, NOW_MS) {
        if let Effect::Reply(ServerMsg::Granted { claim, fence, .. }) = effect {
            return (claim, fence);
        }
    }
    panic!("{who} was not granted {path}");
}

/// Submit the claim of `who` and have the reviewer approve it if the core asks for a review.
fn submit_in(core: &mut Core, who: &str, (claim, fence): (ClaimId, Fence), path: &str) {
    let submit = ClientMsg::Submit {
        req: RequestId(2),
        claim,
        fence,
        fork_commit: CommitId("ab".repeat(20)),
        touched: vec![edit_file(path)],
        decisions: DecisionRecord {
            evidence: vec!["tests passed".into()],
            ..DecisionRecord::default()
        },
    };
    for effect in core.handle(&AgentId(who.to_string()), submit, NOW_MS) {
        if let Effect::Reply(ServerMsg::ReviewRequired { claim, .. }) = effect {
            let approve = ClientMsg::Review {
                req: RequestId(3),
                claim,
                approve: true,
                note: None,
            };
            core.handle(&AgentId("felix".to_string()), approve, NOW_MS);
        }
    }
}

/// A core in which a1's claim on b.rs assumes a.rs and a2's edit of a.rs is submitted and due to
/// merge. Landing it queues the verification of the assumption.
fn core_with_a_challenger_submitted() -> (Core, (ClaimId, Fence)) {
    let reviewers = shell::reviewers_or_none(Some("felix"), |_| {});
    let mut core = shell::load_core(None, Some("test"), None, reviewers, Vec::new()).unwrap();
    let assumption = Assumption {
        scope: Scope::File {
            path: "a.rs".into(),
        },
        statement: "f returns Some".to_string(),
    };
    grant_in(&mut core, "a1", "b.rs", vec![assumption]);
    let challenger = grant_in(&mut core, "a2", "a.rs", Vec::new());
    submit_in(&mut core, "a2", challenger, "a.rs");
    (core, challenger)
}

/// A stored core in which a2's edit of a.rs has merged and the verification of a1's assumption is
/// queued.
fn core_json_with_a_queued_verification() -> String {
    let (mut core, challenger) = core_with_a_challenger_submitted();
    core.begin_merge(NOW_MS)
        .expect("the challenger is due to merge");
    let landed = MergeOutcome::Merged {
        base: CommitId("old".into()),
        head: CommitId("new".into()),
    };
    core.merge_outcome(core.merge_flight_id(challenger.0), &landed, NOW_MS);
    serde_json::to_string(&core).unwrap()
}

#[test]
fn host_a_trial_counts_as_this_instance_verifying() {
    let mut kit = Testkit::new(HostConfig {
        reviewers: Some("felix".to_string()),
        ..config()
    });
    kit.seed("core", core_json_with_a_queued_verification());
    let sock = welcomed(&mut kit, "a1");
    assert_eq!(
        kit.wake(),
        Some(kit.now_ms() + 1),
        "the verification is due now"
    );

    wake(&mut kit);
    assert_eq!(kit.calls().len(), 1);
    assert!(matches!(kit.calls()[0].request, Request::Trial { .. }));

    kit.send(sock, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(
        kit.wake(),
        Some(kit.now_ms() + shell::LEASE_MS),
        "a verification waiting here leaves only the watchdog, behind the next lease expiry"
    );
}

#[test]
fn host_an_open_queued_behind_a_commit_does_not_strand_the_frame_after_it() {
    let mut kit = kit();
    let first = welcomed(&mut kit, "a1");
    kit.send(first, &claim(1, "x.rs", "s"));
    let second = kit.open("a2");
    kit.send(second, &hello("a2"));
    assert!(kit.received(second).is_empty(), "both wait for the commit");
    kit.settle();
    assert!(matches!(
        kit.received(second).first(),
        Some(ServerMsg::Welcome { .. })
    ));
}

#[test]
fn a_frame_on_a_socket_the_host_does_not_know_is_logged() {
    let mut kit = kit();
    let mark = kit.history().len();
    kit.send(SocketId(99), &ClientMsg::Heartbeat);
    assert_eq!(labels_since(&kit, mark), ["Log"]);
    let line = kit.logs().last().unwrap();
    assert!(line.contains("handle frame failed"), "{line}");
    assert!(line.contains("the socket is not open"), "{line}");
}

/// A claim of a1 on x.rs, submitted and waiting to merge.
fn submitted(kit: &mut Testkit) -> (SocketId, ClaimId, CommitId) {
    let sock = welcomed(kit, "a1");
    kit.send(sock, &claim(1, "x.rs", "s"));
    kit.settle();
    let (claim_id, fence) = granted(kit, sock);
    let fork_commit = CommitId("ab".repeat(20));
    let submit = ClientMsg::Submit {
        req: RequestId(2),
        claim: claim_id,
        fence,
        fork_commit: fork_commit.clone(),
        touched: vec![edit_file("x.rs")],
        decisions: DecisionRecord {
            evidence: vec!["tests passed".into()],
            ..DecisionRecord::default()
        },
    };
    kit.send(sock, &submit);
    kit.settle();
    (sock, claim_id, fork_commit)
}

#[test]
fn host_only_a_merge_call_holds_the_merge_wake_back() {
    let mut kit = kit();
    let wait = |kit: &mut Testkit, request: Request| {
        kit.host.calls.clear();
        let call = Call {
            id: CallId(1),
            dispatch_id: DispatchId(1),
            request,
            deadline_ms: 0,
        };
        kit.host.calls.insert(call.id, call);
        let waits = kit.host.waits();
        (waits.merging, waits.verifying)
    };
    let head = Request::ReadHead {
        submitted: Vec::new(),
    };
    let reconcile = Request::Reconcile {
        head: CommitId("a".into()),
    };
    let (base, head_commit) = (CommitId("a".into()), CommitId("b".into()));
    let test = Request::Test {
        base: base.clone(),
        head: head_commit.clone(),
    };
    let land = Request::Land {
        base,
        head: head_commit,
    };
    let trial = Request::Trial {
        agent: AgentId("a1".to_string()),
        commit: None,
        before: CommitId("a".into()),
        main: CommitId("b".into()),
    };
    let prepare = Request::Prepare {
        claim: ClaimId(1),
        agent: AgentId("a1".to_string()),
        fork_commit: CommitId("a".into()),
        scopes: Vec::new(),
    };
    assert_eq!(
        wait(&mut kit, head),
        (false, false),
        "a head read waits for no merge"
    );
    assert_eq!(wait(&mut kit, reconcile), (false, false));
    assert_eq!(wait(&mut kit, prepare), (true, false));
    assert_eq!(wait(&mut kit, test), (true, false));
    assert_eq!(wait(&mut kit, land), (true, false));
    assert_eq!(wait(&mut kit, trial), (false, true));
    kit.host.calls.clear();
    let none = kit.host.waits();
    assert_eq!((none.merging, none.verifying), (false, false));
}

#[test]
fn host_a_lost_call_leaves_the_table_and_no_longer_holds_the_wake_back() {
    let mut kit = kit();
    let (sock, _, _) = submitted(&mut kit);
    wake(&mut kit);
    kit.send(sock, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(kit.wake(), Some(kit.now_ms() + MERGE_WATCHDOG_MS));

    kit.lose(CallId(1));
    kit.settle();
    assert_ne!(
        kit.wake(),
        Some(kit.now_ms() + MERGE_WATCHDOG_MS),
        "the wait is over"
    );

    kit.reply(CallId(1), CallAnswer::Unknown);
    assert!(kit.logs().last().unwrap().contains("no such call"));
}

// ---------- Sockets and sessions (R6 to R13, R31, R32) ----------

/// A stored core over the hard limit that no expiry can bring under it: the first claim, early's,
/// expires `LEASE_MS` after the testkit starts, and the claims that fill the state far outlive it.
fn core_json_over_the_hard_limit_with_one_lease_due() -> String {
    let mut core = shell::load_core(None, Some("test"), None, Vec::new(), Vec::new()).unwrap();
    core.handle(
        &AgentId("early".to_string()),
        claim(1, "early.rs", "s"),
        NOW_MS,
    );
    let summary = "x".repeat(10_000);
    let mut made = 0;
    while serde_json::to_string(&core).unwrap().len() <= HARD_ENTRY_BYTES + 20_000 {
        made += 1;
        let agent = AgentId(format!("filler-{made}"));
        core.handle(&agent, claim(1, &format!("f{made}.rs"), &summary), NOW_MS);
    }
    let mut stored = serde_json::to_value(&core).unwrap();
    let claims = stored["claims"].as_object_mut().unwrap();
    for (_, filler) in claims.iter_mut().filter(|(id, _)| id.as_str() != "1") {
        filler["expires_at_ms"] = json!(4_000_000_000_000_u64);
    }
    stored.to_string()
}

fn waiting_claim(req: u64, paths: &[&str]) -> ClientMsg {
    ClientMsg::Claim {
        req: RequestId(req),
        intent: Intent {
            summary: "s".to_string(),
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes: paths.iter().map(|path| edit_file(path)).collect(),
        on_conflict: OnConflict::Wait,
    }
}

/// Whether the stored core has a queued request of `agent`.
fn queued(kit: &Testkit, agent: &str) -> bool {
    let stored = kit.stored_state();
    let core = shell::load_core(stored, Some("test"), None, Vec::new(), Vec::new()).unwrap();
    core.has_queued_request(&AgentId(agent.to_string()))
}

/// a1 holds x.rs and a2 waits behind it. Returns their sockets.
fn a2_waits_behind_a1(kit: &mut Testkit) -> (SocketId, SocketId) {
    let a1 = welcomed(kit, "a1");
    kit.send(a1, &claim(1, "x.rs", "s"));
    let a2 = welcomed(kit, "a2");
    kit.send(a2, &waiting_claim(2, &["x.rs"]));
    kit.settle();
    assert!(queued(kit, "a2"), "a2 waits");
    (a1, a2)
}

fn release_of(kit: &Testkit, sock: SocketId) -> ClientMsg {
    let (claim, fence) = granted(kit, sock);
    ClientMsg::Release {
        claim,
        fence,
        req: None,
    }
}

fn error_of(msg: &ServerMsg) -> (Option<RequestId>, ErrorCode, &str) {
    let ServerMsg::Error { req, code, message } = msg else {
        panic!("expected an error, got {msg:?}");
    };
    (*req, *code, message)
}

fn close_of(kit: &Testkit, sock: SocketId) -> Option<(u16, &str)> {
    kit.closed(sock)
        .map(|(code, reason)| (*code, reason.as_str()))
}

#[test]
fn host_an_opened_socket_holds_its_verified_agent_before_its_first_frame() {
    let mut kit = kit();
    let sock = kit.open("a1");
    assert_eq!(labels_since(&kit, 0), ["SetSession"]);
    let session = kit.session(sock).expect("the session is recorded at once");
    assert_eq!(session.verified, Some(AgentId("a1".to_string())));
    assert_eq!(session.agent, None, "nothing is bound before a welcome");
    assert!(!session.closed);
}

#[test]
fn host_a_binary_frame_is_answered_and_the_socket_stays_open() {
    let mut kit = kit();
    let sock = welcomed(&mut kit, "a1");
    let mark = kit.history().len();
    kit.send_binary(sock);
    assert_eq!(labels_since(&kit, mark), ["Send"]);
    let (req, code, message) = error_of(kit.received(sock).last().unwrap());
    assert_eq!(req, None);
    assert_eq!(code, ErrorCode::Malformed);
    assert_eq!(message, "binary frames are not supported");
    assert!(kit.closed(sock).is_none());

    kit.send(sock, &claim(1, "x.rs", "s"));
    kit.settle();
    granted(&kit, sock);
}

#[test]
fn host_a_malformed_or_oversized_frame_gets_a_fault_reply_and_nothing_else() {
    let mut kit = kit();
    let sock = welcomed(&mut kit, "a1");
    let mark = kit.history().len();
    kit.send_text(sock, r#"{"type":"secret-text""#.to_string());
    assert_eq!(labels_since(&kit, mark), ["Send"]);
    let (req, code, message) = error_of(kit.received(sock).last().unwrap());
    assert_eq!((req, code), (None, ErrorCode::Malformed));
    assert_eq!(message, "not a valid message", "the input is not echoed");

    let mark = kit.history().len();
    kit.send_text(sock, "y".repeat(shell::MAX_FRAME_BYTES + 1));
    assert_eq!(labels_since(&kit, mark), ["Send"]);
    let (req, code, message) = error_of(kit.received(sock).last().unwrap());
    assert_eq!((req, code), (None, ErrorCode::Malformed));
    assert_eq!(message, "frame is larger than 65536 bytes");
    assert!(kit.closed(sock).is_none());
}

#[test]
fn host_a_frame_before_hello_is_refused_with_no_hello() {
    let mut kit = kit();
    let sock = kit.open("a1");
    let mark = kit.history().len();
    kit.send(sock, &claim(5, "x.rs", "s"));
    assert_eq!(labels_since(&kit, mark), ["Send"], "the core is not called");
    let (req, code, _) = error_of(kit.received(sock).last().unwrap());
    assert_eq!((req, code), (Some(RequestId(5)), ErrorCode::NoHello));
    assert!(kit.closed(sock).is_none());
}

#[test]
fn host_a_hello_for_another_agent_closes_1008_and_withdraws() {
    let mut kit = kit();
    let (_, a2) = a2_waits_behind_a1(&mut kit);
    let mark = kit.history().len();
    kit.send(a2, &hello("b"));
    assert_eq!(
        labels_since(&kit, mark),
        ["Send", "SetSession", "Close", "Commit"],
        "the reply, the close, then the withdrawal is stored"
    );
    let (_, code, _) = error_of(kit.received(a2).last().unwrap());
    assert_eq!(code, ErrorCode::NotOwner);
    assert_eq!(close_of(&kit, a2), Some((1008, "identity mismatch")));
    kit.settle();
    assert!(
        !queued(&kit, "a2"),
        "the bound agent's request is withdrawn"
    );
}

#[test]
fn host_an_error_handling_a_frame_closes_1011_and_withdraws() {
    let mut kit = kit();
    let (_, a2) = a2_waits_behind_a1(&mut kit);
    let received = kit.received(a2).len();
    kit.fail_next_commit();
    kit.send(a2, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(close_of(&kit, a2), Some((1011, "coordinator error")));
    assert_eq!(kit.received(a2).len(), received, "nothing else is sent");
    assert!(
        !queued(&kit, "a2"),
        "the closed socket's request is withdrawn"
    );
}

#[test]
fn an_error_loading_the_state_closes_the_sender_with_1011() {
    let mut kit = kit();
    kit.seed("core", r#"{"claims":"intent-text""#.to_string());
    let sock = kit.open("a1");
    kit.send(sock, &hello("a1"));
    kit.settle();
    assert_eq!(close_of(&kit, sock), Some((1011, "coordinator error")));
}

#[test]
fn an_error_applying_a_call_closes_the_sender_with_1011() {
    let mut kit = kit();
    kit.seed("core", core_json_of_size(HARD_ENTRY_BYTES + 1));
    let sock = kit.open("a1");
    kit.send(sock, &hello("a1"));
    kit.settle();
    assert_eq!(close_of(&kit, sock), Some((1011, "coordinator error")));
}

#[test]
fn an_error_in_the_expiry_before_a_call_closes_the_sender_with_1011() {
    let mut over = kit();
    over.seed("core", core_json_over_the_hard_limit_with_one_lease_due());
    over.advance(shell::LEASE_MS + 1);
    let sock = over.open("a1");
    over.send(sock, &hello("a1"));
    over.settle();
    assert!(over.logs().join("\n").contains("store state failed"));
    assert_eq!(close_of(&over, sock), Some((1011, "coordinator error")));

    let mut failed = kit();
    let first = welcomed(&mut failed, "a1");
    failed.send(first, &claim(1, "x.rs", "s"));
    failed.settle();
    failed.advance(shell::LEASE_MS + 1);
    let second = failed.open("a2");
    failed.fail_next_commit();
    failed.send(second, &hello("a2"));
    failed.settle();
    assert_eq!(close_of(&failed, second), Some((1011, "coordinator error")));
    assert!(failed.received(second).is_empty());
}

#[test]
fn host_a_closed_or_failed_socket_withdraws_its_agents() {
    let mut kit = kit();
    let (a1, a2) = a2_waits_behind_a1(&mut kit);
    let mark = kit.history().len();
    kit.close(a2);
    assert_eq!(labels_since(&kit, mark), ["Commit"]);
    kit.settle();
    assert!(!queued(&kit, "a2"));

    let mark = kit.history().len();
    kit.close(a1);
    assert!(
        labels_since(&kit, mark).is_empty(),
        "an agent with no queued request is not withdrawn, so nothing is stored"
    );

    let mut idle = Testkit::new(config());
    let unbound = idle.open("a1");
    idle.close(unbound);
    assert_eq!(
        count(&idle, "Load"),
        0,
        "a socket that never said hello loads nothing"
    );

    kit.close(SocketId(99));
    let line = kit.logs().last().unwrap();
    assert!(line.contains("handle close failed"), "{line}");
    assert!(line.contains("the socket is not open"), "{line}");
}

#[test]
fn host_a_socket_closed_by_the_host_is_marked_closed_before_it_is_closed() {
    let mut kit = kit();
    let (_, a2) = a2_waits_behind_a1(&mut kit);
    let twin = welcomed(&mut kit, "a2");
    let mark = kit.history().len();
    kit.fail_next_commit();
    kit.send(a2, &ClientMsg::Heartbeat);
    kit.settle();

    let history = &kit.history()[mark..];
    let close = history
        .iter()
        .position(|output| matches!(output, Output::Close { sock, .. } if *sock == a2))
        .expect("the host closed the socket");
    let Output::SetSession { sock, session } = &history[close - 1] else {
        panic!(
            "the session is recorded just before the close: {:?}",
            history[close - 1]
        );
    };
    assert_eq!((*sock, session.closed), (a2, true));
    assert!(kit.session(a2).is_some_and(|session| session.closed));
    assert!(
        queued(&kit, "a2"),
        "the twin socket is open and shields the agent"
    );

    kit.close(twin);
    kit.settle();
    assert!(
        !queued(&kit, "a2"),
        "the socket the host closed is still listed but shields nobody"
    );
}

#[test]
fn host_only_an_agent_with_no_other_open_socket_and_a_queued_request_is_withdrawn() {
    let mut kit = kit();
    let (_, a2) = a2_waits_behind_a1(&mut kit);
    let twin = welcomed(&mut kit, "a2");

    let mark = kit.history().len();
    kit.close(a2);
    assert!(
        labels_since(&kit, mark).is_empty(),
        "another open socket of a2 shields it: nothing is stored"
    );
    assert!(queued(&kit, "a2"));

    kit.close(twin);
    kit.settle();
    assert!(
        !queued(&kit, "a2"),
        "the last socket of a2 took its request with it"
    );
}

#[test]
fn host_a_withdrawal_is_applied_persisted_then_settled() {
    let mut kit = kit();
    let a1 = welcomed(&mut kit, "a1");
    kit.send(a1, &claim(1, "x.rs", "s"));
    let e1 = welcomed(&mut kit, "e1");
    kit.send(e1, &claim(1, "y.rs", "s"));
    let b = welcomed(&mut kit, "b");
    kit.send(b, &waiting_claim(2, &["y.rs", "x.rs"]));
    let c = welcomed(&mut kit, "c");
    kit.send(c, &waiting_claim(3, &["x.rs"]));
    kit.settle();
    kit.send(a1, &release_of(&kit, a1));
    kit.settle();
    assert!(
        queued(&kit, "b") && queued(&kit, "c"),
        "b waits on e1, c behind b"
    );

    let received = kit.received(c).len();
    let mark = kit.history().len();
    kit.close(b);
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit"],
        "applied, then persisted"
    );
    assert!(
        kit.received(c).len() == received,
        "nothing is sent before the commit"
    );
    kit.deliver_next();
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit", "Send", "SetWake"],
        "then settled"
    );
    assert!(matches!(
        kit.received(c).last(),
        Some(ServerMsg::Granted { .. })
    ));
}

#[test]
fn host_a_failed_send_to_another_socket_closes_it_1011_and_delivery_goes_on() {
    let mut kit = kit();
    let (a1, a2) = a2_waits_behind_a1(&mut kit);
    let twin = welcomed(&mut kit, "a2");
    kit.fail_sends_to(twin);
    kit.send(a1, &release_of(&kit, a1));
    kit.settle();

    assert!(matches!(
        kit.received(a2).last(),
        Some(ServerMsg::Granted { .. })
    ));
    assert_eq!(close_of(&kit, twin), Some((1011, "send failed")));
    assert!(kit.closed(a2).is_none() && kit.closed(a1).is_none());
    let logs = kit.logs().join("\n");
    assert!(logs.contains("send to a socket failed: agent=a2"), "{logs}");
}

#[test]
fn host_a_socket_whose_send_failed_is_closed_once_and_its_agent_withdrawn() {
    let mut kit = kit();
    let a1 = welcomed(&mut kit, "a1");
    kit.send(a1, &claim(1, "x.rs", "s"));
    let a2 = welcomed(&mut kit, "a2");
    kit.fail_sends_to(a2);
    kit.send(a2, &waiting_claim(2, &["x.rs"]));
    kit.settle();
    assert_eq!(close_of(&kit, a2), Some((1011, "send failed")));
    assert!(
        !queued(&kit, "a2"),
        "the dead socket's request is withdrawn"
    );

    let closes = count(&kit, "Close");
    let now_ms = kit.now_ms();
    kit.input(Input::SendFailed { now_ms, sock: a2 });
    assert_eq!(
        count(&kit, "Close"),
        closes,
        "a closed socket is not closed twice"
    );

    kit.input(Input::SendFailed {
        now_ms,
        sock: SocketId(99),
    });
    let line = kit.logs().last().unwrap();
    assert!(line.contains("handle send failure failed"), "{line}");
    assert!(line.contains("the socket is not open"), "{line}");
}

#[test]
fn host_a_failed_withdrawal_is_logged_and_closes_nothing_and_sends_nothing() {
    let mut kit = kit();
    let (_, a2) = a2_waits_behind_a1(&mut kit);
    let received = kit.received(a2).len();
    let mark = kit.history().len();
    kit.fail_next_commit();
    kit.close(a2);
    kit.settle();
    assert_eq!(labels_since(&kit, mark), ["Commit", "Log"]);
    let line = kit.logs().last().unwrap();
    assert!(line.contains("persist state and events failed"), "{line}");
    assert_eq!(kit.received(a2).len(), received);
    assert!(queued(&kit, "a2"), "nothing was stored");
}

// ---------- The wake and the calls it issues (R20 to R27, R38, R41, R43, R47) ----------

/// The in-flight marker of the merge, if the stored core holds one.
fn merge_marker_stored(kit: &Testkit) -> bool {
    state(kit)["merge_in_flight"].is_object()
}

fn infra_failures(kit: &Testkit, claim: &str) -> u64 {
    state(kit)["claims"][claim]["work"]["infra_failures"]
        .as_u64()
        .expect("the claim holds its work")
}

fn setwakes_since(kit: &Testkit, mark: usize) -> Vec<Option<u64>> {
    let mut wakes = Vec::new();
    for output in &kit.history()[mark..] {
        if let Output::SetWake { at_ms } = output {
            wakes.push(*at_ms);
        }
    }
    wakes
}

/// The stored core state of every commit the host handed out, in order.
fn committed_states(kit: &Testkit) -> Vec<Value> {
    let mut states = Vec::new();
    for output in kit.history() {
        let Output::Commit { entries, .. } = output else {
            continue;
        };
        for (key, json) in entries {
            if key == shell::STATE_KEY {
                states.push(serde_json::from_str(json).unwrap());
            }
        }
    }
    states
}

fn lease_of(state: &Value, claim: &str) -> bool {
    state["claims"][claim].is_object()
}

#[test]
fn host_a_wake_runs_its_steps_in_order_each_as_its_own_write() {
    let mut kit = kit();
    submitted(&mut kit);
    let other = welcomed(&mut kit, "a2");
    kit.send(other, &claim(1, "y.rs", "s"));
    kit.settle();
    wake(&mut kit);
    assert_eq!(kit.calls().len(), 1, "the merge is out");
    kit.restart();
    kit.advance(shell::LEASE_MS + 1);
    let before = committed_states(&kit).len();

    wake(&mut kit);
    let states = committed_states(&kit);
    assert_eq!(states.len() - before, 2, "expiry, then merge recovery");
    let (expiry, recovery) = (&states[before], &states[before + 1]);
    assert!(
        !lease_of(expiry, "2"),
        "the first write expires the lapsed lease"
    );
    assert!(
        expiry["merge_in_flight"].is_object(),
        "the cut-off merge is not touched yet"
    );
    assert!(
        recovery["merge_in_flight"].is_null(),
        "the second write recovers it"
    );
    assert_eq!(
        recovery["claims"]["1"]["work"]["infra_failures"],
        json!(1),
        "the cut-off counts as an attempt"
    );
    assert_eq!(
        kit.calls().len(),
        1,
        "it is not dispatched again: it is in backoff"
    );
}

#[test]
fn host_a_wake_that_dispatched_a_merge_runs_its_verification_step_only_after_replied() {
    let mut kit = kit();
    let (core, _) = core_with_a_challenger_submitted();
    kit.seed("core", serde_json::to_string(&core).unwrap());
    welcomed(&mut kit, "a3");

    wake(&mut kit);
    assert_eq!(kit.calls().len(), 1, "one call: the merge");
    assert!(matches!(kit.calls()[0].request, Request::Prepare { .. }));
    let mark = kit.history().len();
    kit.reply(CallId(1), landed());
    kit.settle();
    assert_eq!(kit.calls().len(), 2, "the landing queued a verification");
    assert!(
        matches!(kit.calls()[1].request, Request::Trial { .. }),
        "the same wake dispatches it once the merge is answered: {:?}",
        kit.calls()
    );
    assert_eq!(
        labels_since(&kit, mark)
            .iter()
            .filter(|l| **l == "Commit")
            .count(),
        2,
        "the outcome is one write and the verification's marker another"
    );
}

#[test]
fn host_a_merge_marker_is_stored_before_its_call_and_its_outcome_before_anyone_is_told() {
    let mut kit = kit();
    let (sock, _, _) = submitted(&mut kit);
    kit.wake_now();
    let mut stored_before_the_call = false;
    while kit.calls().is_empty() {
        stored_before_the_call |= merge_marker_stored(&kit);
        assert!(kit.deliver_next(), "the host asks for nothing more");
    }
    assert!(
        stored_before_the_call,
        "the marker is stored before the call goes out"
    );

    let told = kit.received(sock).len();
    kit.reply(CallId(1), landed());
    assert!(!merge_marker_stored(&kit), "the outcome is stored");
    assert_eq!(kit.received(sock).len(), told, "and nobody is told yet");
    kit.settle();
    assert!(
        matches!(kit.received(sock).last(), Some(ServerMsg::Merged { .. })),
        "{:?}",
        kit.received(sock)
    );
}

#[test]
fn host_a_wake_while_a_call_is_outstanding_dispatches_nothing_and_sets_the_watchdog() {
    let mut kit = kit();
    submitted(&mut kit);
    wake(&mut kit);
    let mark = kit.history().len();

    kit.wake_now();
    assert_eq!(labels_since(&kit, mark), ["SetWake"]);
    assert_eq!(kit.wake(), Some(kit.now_ms() + MERGE_WATCHDOG_MS));
    assert_eq!(kit.calls().len(), 1, "no second dispatch");
}

/// The state of a repo whose verification is queued and a1 holds a claim: a wake dispatches the
/// trial.
fn trial_outstanding() -> (Testkit, SocketId, Call) {
    let mut kit = Testkit::new(HostConfig {
        reviewers: Some("felix".to_string()),
        ..config()
    });
    kit.seed("core", core_json_with_a_queued_verification());
    let sock = welcomed(&mut kit, "a1");
    wake(&mut kit);
    let call = kit.calls().last().expect("the trial is out").clone();
    assert!(matches!(call.request, Request::Trial { .. }));
    (kit, sock, call)
}

#[test]
fn host_a_verification_marker_is_stored_before_its_call_and_its_outcome_before_anyone_is_told() {
    let mut kit = Testkit::new(HostConfig {
        reviewers: Some("felix".to_string()),
        ..config()
    });
    kit.seed("core", core_json_with_a_queued_verification());
    let sock = welcomed(&mut kit, "a1");
    kit.wake_now();
    let mut stored_before_the_call = false;
    while kit.calls().is_empty() {
        stored_before_the_call |= state(&kit)["verification_in_flight"].is_object();
        assert!(kit.deliver_next(), "the host asks for nothing more");
    }
    assert!(
        stored_before_the_call,
        "the marker is stored before the call goes out"
    );

    let told = kit.received(sock).len();
    let mark = kit.history().len();
    let broken = CallAnswer::Trialled {
        before: Some(TrialResult::Clean),
        after: Some(TrialResult::TestsFailed),
    };
    kit.reply(CallId(1), broken);
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit"],
        "nothing is delivered or scheduled before the outcome commits"
    );
    assert!(
        state(&kit)["verification_in_flight"].is_null(),
        "the outcome is stored"
    );
    kit.settle();
    assert_eq!(kit.received(sock).len(), told);
    assert_eq!(
        count(&kit, "Call"),
        1,
        "the verification is not dispatched again"
    );
}

#[test]
fn host_a_wake_while_a_verification_is_in_flight_is_pending_and_in_the_future() {
    let (mut kit, sock, _) = trial_outstanding();
    kit.send(sock, &claim(9, "late.rs", "s"));
    kit.settle();
    for _ in 0..3 {
        let at = kit.wake().expect("a wake is pending");
        assert!(at > kit.now_ms(), "the wake is in the future");
        kit.advance(at - kit.now_ms());
        kit.wake_now();
        kit.settle();
        if kit.wake() == Some(kit.now_ms() + MERGE_WATCHDOG_MS) {
            return;
        }
    }
    panic!(
        "the wake kept firing while the verification waited: {:?}",
        kit.wake()
    );
}

#[test]
fn host_a_poke_for_an_unknown_repo_stores_nothing() {
    let mut kit = kit();
    kit.poke();
    assert_eq!(labels_since(&kit, 0), ["Load"]);
    kit.settle();
    assert_eq!(
        labels_since(&kit, 0),
        ["Load"],
        "nothing is stored or scheduled"
    );
    assert_eq!(kit.stored_state(), None);
    assert_eq!(kit.wake(), None);
    assert!(kit.logs().is_empty());

    let sock = welcomed(&mut kit, "a1");
    kit.poke();
    kit.settle();
    assert_eq!(
        state(&kit)["head_pokes"],
        json!(1),
        "a repo with state counts it"
    );
    assert_eq!(
        kit.wake(),
        Some(kit.now_ms() + 1),
        "and the head read is due"
    );
    assert!(kit.received(sock).len() == 1);
}

#[test]
fn host_a_head_read_runs_only_when_offered_and_is_stored_before_anyone_is_told() {
    let mut kit = kit();
    welcomed(&mut kit, "a1");
    wake(&mut kit);
    assert!(kit.calls().is_empty(), "no poke, no head read");
    assert_eq!(kit.wake(), None);

    let call = head_read_outstanding(&mut kit);
    assert_eq!(
        call.request,
        Request::ReadHead {
            submitted: Vec::new()
        }
    );
    assert_eq!(state(&kit)["head"], json!("abc"));
    let mark = kit.history().len();
    let moved = CallAnswer::Head {
        main: Some(CommitId("new".into())),
        landed: Vec::new(),
    };
    kit.reply(call.id, moved);
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit"],
        "the answer is stored first"
    );
    assert_eq!(state(&kit)["head"], json!("new"));
    kit.settle();
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit", "SetWake", "SetWake"],
        "then the wake is set, and the wake ends"
    );
    assert_eq!(kit.wake(), None, "main moved and nothing else is due");
}

#[test]
fn host_a_failed_or_timed_out_head_call_is_none_and_logged() {
    let ends_in_none: [(&str, Option<CallAnswer>); 5] = [
        ("a failed call", Some(CallAnswer::Unknown)),
        ("a timeout", None),
        (
            "a head of none",
            Some(CallAnswer::Head {
                main: None,
                landed: Vec::new(),
            }),
        ),
        (
            "a refusal",
            Some(CallAnswer::Refused {
                refusal: Refusal::CommitNotInFork,
            }),
        ),
        ("another answer", Some(CallAnswer::Tested)),
    ];
    for (what, answer) in ends_in_none {
        let mut kit = kit();
        welcomed(&mut kit, "a1");
        let call = head_read_outstanding(&mut kit);
        match answer {
            Some(answer) => kit.reply(call.id, answer),
            None => kit.lose(call.id),
        }
        kit.settle();
        let line = kit
            .logs()
            .last()
            .unwrap_or_else(|| panic!("{what} is logged"));
        assert!(
            line.contains("steward head call failed: no head in the answer"),
            "{what}: {line}"
        );
        assert_eq!(state(&kit)["head"], json!("abc"), "{what} moves nothing");
        assert_eq!(state(&kit)["head_tried"], json!(1), "{what} is tried");
        assert_eq!(kit.wake(), None, "{what} is no reason to wake");
    }
}

#[test]
fn host_a_marker_with_no_call_waiting_is_a_cut_off() {
    let mut kit = kit();
    submitted(&mut kit);
    wake(&mut kit);
    assert!(merge_marker_stored(&kit));

    kit.restart();
    wake(&mut kit);
    assert!(!merge_marker_stored(&kit), "the cut-off is recovered");
    assert_eq!(infra_failures(&kit, "1"), 1, "and counted as an attempt");
    assert_eq!(
        kit.calls().len(),
        1,
        "nothing is dispatched: the claim is in backoff"
    );
}

#[test]
fn host_a_verification_marker_with_no_call_waiting_is_a_cut_off() {
    let (mut kit, _, _) = trial_outstanding();
    assert!(state(&kit)["verification_in_flight"].is_object());

    kit.restart();
    wake(&mut kit);
    let state = state(&kit);
    assert!(
        state["verification_in_flight"].is_null(),
        "the cut-off is recovered"
    );
    assert_eq!(
        state["verifications"][0]["infra_failures"],
        json!(1),
        "and counted"
    );
    assert_eq!(kit.calls().len(), 1, "the retry waits for its backoff");
}

#[test]
fn host_a_marker_with_a_call_waiting_is_not_a_cut_off() {
    let mut kit = kit();
    submitted(&mut kit);
    wake(&mut kit);
    kit.wake_now();
    kit.settle();
    assert!(merge_marker_stored(&kit), "the merge is still running");
    assert_eq!(infra_failures(&kit, "1"), 0);
}

#[test]
fn host_a_failed_or_timed_out_steward_call_is_service_unavailable() {
    let unusable: [(&str, Option<CallAnswer>); 4] = [
        ("a failed call", Some(CallAnswer::Unknown)),
        ("a timeout", None),
        ("an answer of another kind", Some(CallAnswer::Tested)),
        (
            "a stale landing",
            Some(CallAnswer::Landed {
                head: CommitId("new".into()),
            }),
        ),
    ];
    for (what, answer) in unusable {
        let mut kit = kit();
        submitted(&mut kit);
        wake(&mut kit);
        match answer {
            Some(answer) => kit.reply(CallId(1), answer),
            None => kit.lose(CallId(1)),
        }
        kit.settle();
        assert_eq!(infra_failures(&kit, "1"), 1, "{what} is infrastructure");
        assert!(
            state(&kit)["claims"]["1"].is_object(),
            "{what} rejects nothing"
        );
        let line = kit.logs().last().unwrap();
        assert!(
            line.contains("steward merge call failed: no usable answer"),
            "{what}: {line}"
        );
    }

    let (mut kit, _, call) = trial_outstanding();
    kit.reply(call.id, CallAnswer::Unknown);
    kit.settle();
    let state = state(&kit);
    assert_eq!(
        state["verifications"][0]["infra_failures"],
        json!(1),
        "a trial too"
    );
    let line = kit.logs().last().unwrap();
    assert!(
        line.contains("steward trial call failed: no usable answer"),
        "{line}"
    );
}

#[test]
fn host_a_refusal_is_a_fact_about_the_work_not_the_service() {
    let mut kit = kit();
    let (sock, _, _) = submitted(&mut kit);
    wake(&mut kit);
    let conflict = Refusal::Conflict {
        files: vec!["x.rs".to_string()],
    };
    kit.reply(CallId(1), CallAnswer::Refused { refusal: conflict });
    kit.settle();
    assert!(
        state(&kit)["claims"]["1"]["work"].is_null(),
        "the submission is rejected, not retried"
    );
    assert!(
        matches!(
            kit.received(sock).last(),
            Some(ServerMsg::SubmitRejected { .. })
        ),
        "{:?}",
        kit.received(sock)
    );
}

#[test]
fn host_a_head_call_waits_30_seconds_and_a_steward_call_13_minutes() {
    let mut kit = kit();
    submitted(&mut kit);
    wake(&mut kit);
    let merge = kit.calls().last().unwrap().clone();
    assert_eq!(merge.deadline_ms, kit.now_ms() + 780_000);

    let (kit, _, trial) = trial_outstanding();
    assert_eq!(trial.deadline_ms, kit.now_ms() + 780_000);

    let mut kit = self::kit();
    welcomed(&mut kit, "a1");
    let head = head_read_outstanding(&mut kit);
    assert_eq!(head.deadline_ms, kit.now_ms() + 30_000);
}

#[test]
fn host_calls_are_numbered_in_the_order_they_are_issued() {
    let mut kit = kit();
    let (core, _) = core_with_a_challenger_submitted();
    kit.seed("core", serde_json::to_string(&core).unwrap());
    welcomed(&mut kit, "a3");
    wake(&mut kit);
    kit.reply(CallId(1), landed());
    kit.settle();
    let ids: Vec<CallId> = kit.calls().iter().map(|call| call.id).collect();
    assert_eq!(ids, [CallId(1), CallId(2)]);
    let dispatches: Vec<DispatchId> = kit.calls().iter().map(|call| call.dispatch_id).collect();
    assert_eq!(
        dispatches,
        [DispatchId(1), DispatchId(2)],
        "the core's dispatch ids"
    );
}

#[test]
fn host_a_failed_step_leaves_one_future_wake() {
    // A commit fails: the core is dropped and the wake is set again after the retry delay.
    let mut kit = kit();
    submitted(&mut kit);
    kit.wake_now();
    kit.fail_next_commit();
    let mark = kit.history().len();
    kit.settle();
    let retry = kit.now_ms() + wake::RETRY_MS;
    assert_eq!(setwakes_since(&kit, mark).last(), Some(&Some(retry)));
    assert_eq!(kit.wake(), Some(retry));
    assert!(kit.calls().is_empty());

    kit.advance(wake::RETRY_MS);
    wake(&mut kit);
    assert_eq!(
        kit.calls().len(),
        1,
        "the retried wake dispatches the merge"
    );

    // A load fails: nothing else is set.
    let mut kit = self::kit();
    kit.seed("core", r#"{"claims":"garbled""#.to_string());
    let mark = kit.history().len();
    wake(&mut kit);
    let retry = kit.now_ms() + wake::RETRY_MS;
    assert_eq!(
        setwakes_since(&kit, mark),
        [Some(retry)],
        "one wake, in the future"
    );

    // The commit of an answer fails: the marker stays, and the retry recovers it as a cut-off.
    let mut kit = self::kit();
    submitted(&mut kit);
    wake(&mut kit);
    kit.fail_next_commit();
    kit.reply(CallId(1), landed());
    let mark = kit.history().len();
    kit.settle();
    let retry = kit.now_ms() + wake::RETRY_MS;
    assert_eq!(setwakes_since(&kit, mark), [Some(retry)]);
    assert!(merge_marker_stored(&kit));
    kit.advance(wake::RETRY_MS);
    wake(&mut kit);
    assert_eq!(
        infra_failures(&kit, "1"),
        1,
        "the lost outcome counts as an attempt"
    );

    // A wake that only sets the wake, while a call is out, fails to load.
    let mut kit = self::kit();
    welcomed(&mut kit, "a1");
    head_read_outstanding(&mut kit);
    kit.fail_next_commit();
    kit.poke();
    kit.settle();
    kit.seed("core", r#"{"claims":"garbled""#.to_string());
    let mark = kit.history().len();
    kit.wake_now();
    kit.settle();
    let retry = kit.now_ms() + wake::RETRY_MS;
    assert_eq!(setwakes_since(&kit, mark), [Some(retry)]);
}

#[test]
fn host_a_restarted_host_starts_with_nothing_awaited_and_reloads_on_first_use() {
    let mut kit = kit();
    let (sock, _, _) = submitted(&mut kit);
    wake(&mut kit);
    assert_eq!(kit.calls().len(), 1);

    kit.restart();
    let mark = kit.history().len();
    kit.reply(CallId(1), landed());
    assert_eq!(
        labels_since(&kit, mark),
        ["Log"],
        "the old call is not awaited"
    );
    assert!(kit.logs().last().unwrap().contains("no such call"));
    assert!(merge_marker_stored(&kit));

    kit.wake_now();
    assert_eq!(
        labels_since(&kit, mark)[1..],
        ["Load"],
        "the first use loads the core"
    );
    kit.settle();
    assert!(
        !merge_marker_stored(&kit),
        "the marker is a cut-off of the old process"
    );
    assert_eq!(infra_failures(&kit, "1"), 1);
    assert_eq!(kit.received(sock).len(), 3, "and nobody was told it merged");
}

#[test]
fn host_an_answer_for_another_dispatch_changes_nothing() {
    let mut kit = kit();
    let (sock, _, _) = submitted(&mut kit);
    wake(&mut kit);
    kit.fail_next_commit();
    kit.poke();
    kit.settle();

    let mut moved = state(&kit);
    moved["merge_in_flight"]["dispatch"] = json!(99);
    kit.seed("core", moved.to_string());
    let told = kit.received(sock).len();
    kit.reply(CallId(1), landed());
    kit.settle();
    let after = state(&kit);
    assert_eq!(
        after["merge_in_flight"]["dispatch"],
        json!(99),
        "the dispatch in flight stays"
    );
    assert!(after["claims"]["1"].is_object(), "the claim did not land");
    assert_eq!(kit.received(sock).len(), told);
}

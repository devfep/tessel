//! Host tests of the call path: each is named for the rule of `docs/reference/runtime-rules.md`
//! it pins. They drive the host through the testkit, so they read transcripts and outputs, not
//! the host's fields.

use serde_json::{json, Value};

use super::call::{Call, CallAnswer, CallId, DispatchId, Request};
use super::testkit::Testkit;
use super::{BatchId, HostConfig, Input, MarkerStep, Output, SocketId, StartJob};
use crate::coordinator::{Coordinator as Core, Effect};
use crate::merge::{MergeOutcome, MERGE_WATCHDOG_MS};
use crate::protocol::{
    AgentId, Assumption, ClaimId, ClientMsg, CommitId, DecisionRecord, ErrorCode, Fence, Intent,
    Mode, OnConflict, RequestId, Scope, ScopeClaim, ServerMsg,
};
use crate::shell::{self, HARD_ENTRY_BYTES, SOFT_ENTRY_BYTES, STATE_LIMIT_MESSAGE};

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

fn state(kit: &Testkit) -> Value {
    serde_json::from_str(kit.stored_state().expect("a stored state")).unwrap()
}

fn call_with(id: u64, request: Request) -> Call {
    Call {
        id: CallId(id),
        dispatch_id: DispatchId(id),
        request,
        deadline_ms: 0,
    }
}

fn read_head(id: u64) -> Call {
    call_with(
        id,
        Request::ReadHead {
            submitted: Vec::new(),
        },
    )
}

/// Store `marker`'s result and hand out `call`, as the wake does for a merge.
fn start(kit: &mut Testkit, call: Call, marker: MarkerStep) {
    let now_ms = kit.now_ms();
    let job = StartJob {
        now_ms,
        call,
        marker,
    };
    let outputs = kit.host.begin_call(job);
    kit.execute(outputs);
    kit.settle();
}

fn start_plain(kit: &mut Testkit, call: Call) {
    start(kit, call, Box::new(|_| Vec::new()));
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
    let sock = welcomed(&mut kit, "a1");
    kit.send(sock, &claim(1, "x.rs", "s"));
    kit.settle();
    let (claim_id, fence) = granted(&kit, sock);
    let submit = ClientMsg::Submit {
        req: RequestId(2),
        claim: claim_id,
        fence,
        fork_commit: CommitId("ab".repeat(20)),
        touched: vec![edit_file("x.rs")],
        decisions: DecisionRecord {
            evidence: vec!["tests passed".into()],
            ..DecisionRecord::default()
        },
    };
    kit.send(sock, &submit);
    kit.settle();
    assert_eq!(kit.wake(), Some(kit.now_ms() + 1), "the merge is due now");

    let now_ms = kit.now_ms();
    let prepare = Request::Prepare {
        claim: claim_id,
        agent: AgentId("a1".to_string()),
        fork_commit: CommitId("ab".repeat(20)),
        scopes: Vec::new(),
    };
    let marker: MarkerStep = Box::new(move |core| {
        core.begin_merge(now_ms).expect("the merge is dispatched");
        Vec::new()
    });
    start(&mut kit, call_with(1, prepare), marker);
    assert_eq!(kit.calls().len(), 1);

    kit.send(sock, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(
        kit.wake(),
        Some(kit.now_ms() + MERGE_WATCHDOG_MS),
        "a merge waiting here leaves only the watchdog"
    );

    kit.reply(CallId(1), CallAnswer::Tested);
    kit.settle();
    assert_eq!(
        kit.wake(),
        Some(kit.now_ms() + 1),
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
    start_plain(&mut kit, read_head(1));

    kit.fail_next_commit();
    start_plain(&mut kit, read_head(2));
    let mark = kit.history().len();
    kit.reply(CallId(1), CallAnswer::Unknown);
    assert_eq!(
        labels_since(&kit, mark),
        ["Load"],
        "the dropped core is reloaded first"
    );
    kit.settle();
    assert_eq!(
        labels_since(&kit, mark),
        ["Load", "Commit", "SetWake"],
        "then the answer is applied and stored"
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

/// The labels the host produces when a batch stores a step with these effects, with a1 connected
/// and no socket for "ghost". The step also starts a call, which goes out after the batch.
fn start_with(effects: Vec<Effect>) -> (Testkit, SocketId, Vec<&'static str>) {
    let mut kit = kit();
    let sock = welcomed(&mut kit, "a1");
    let mark = kit.history().len();
    start(&mut kit, read_head(1), Box::new(move |_| effects));
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

fn mixed_effects() -> Vec<Effect> {
    vec![
        Effect::Reply(welcome(1)),
        Effect::Notify {
            agent: AgentId("ghost".to_string()),
            msg: welcome(2),
        },
        Effect::Notify {
            agent: AgentId("a1".to_string()),
            msg: welcome(77),
        },
    ]
}

#[test]
fn host_settle_delivers_then_reschedules_even_when_delivery_failed() {
    let (kit, sock, labels) = start_with(mixed_effects());
    assert_eq!(labels, ["Commit", "Log", "Send", "SetWake", "Call"]);
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
    let (kit, sock, labels) = start_with(mixed_effects());
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
    start_plain(&mut kit, read_head(1));
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
    start_plain(&mut kit, read_head(1));

    let mark = kit.history().len();
    kit.send(sock, &ClientMsg::Heartbeat);
    kit.send(sock, &ClientMsg::Heartbeat);
    assert_eq!(labels_since(&kit, mark), ["Commit"], "one commit at a time");
    kit.reply(CallId(1), CallAnswer::Tested);
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit"],
        "an answer waits behind a commit as any input does"
    );
    kit.settle();
    assert_eq!(count(&kit, "Commit"), 5, "every input ran, in order");
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
    kit.input(Input::Wake { now_ms });
    assert_eq!(labels_since(&kit, mark), ["Log"]);
    let line = kit.logs().last().unwrap();
    assert!(line.contains("repo-1"), "{line}");
    assert!(
        line.contains("a wake is not served by this host yet"),
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
    welcomed(&mut kit, "a1");
    let mark = kit.history().len();
    kit.fail_next_commit();
    start_plain(&mut kit, read_head(1));
    assert_eq!(
        labels_since(&kit, mark),
        ["Commit", "Log"],
        "no Call and no SetWake"
    );
    assert!(kit.calls().is_empty());
    kit.reply(CallId(1), CallAnswer::Tested);
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

/// A stored core in which a1's claim on b.rs assumes a.rs, a2's edit of a.rs has merged and the
/// verification of the assumption is queued.
fn core_json_with_a_queued_verification() -> String {
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

    let now_ms = kit.now_ms();
    let trial = Request::Trial {
        agent: AgentId("a1".to_string()),
        commit: None,
        before: CommitId("old".into()),
        main: CommitId("new".into()),
    };
    let marker: MarkerStep = Box::new(move |core| {
        core.begin_verification(now_ms)
            .expect("the verification is dispatched");
        Vec::new()
    });
    start(&mut kit, call_with(1, trial), marker);
    assert_eq!(kit.calls().len(), 1);

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

fn merge_marker(now_ms: u64) -> MarkerStep {
    Box::new(move |core| {
        core.begin_merge(now_ms).expect("the merge is dispatched");
        Vec::new()
    })
}

#[test]
fn host_only_a_merge_call_holds_the_merge_wake_back() {
    let mut kit = kit();
    let (sock, _, _) = submitted(&mut kit);
    let now_ms = kit.now_ms();
    start(&mut kit, read_head(1), merge_marker(now_ms));

    kit.send(sock, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(
        kit.wake(),
        Some(kit.now_ms() + 1),
        "a head read is not the merge, so the stored marker is recovered at once"
    );
}

#[test]
fn host_a_lost_call_leaves_the_table_and_no_longer_holds_the_wake_back() {
    let mut kit = kit();
    let (sock, claim_id, fork_commit) = submitted(&mut kit);
    let now_ms = kit.now_ms();
    let prepare = Request::Prepare {
        claim: claim_id,
        agent: AgentId("a1".to_string()),
        fork_commit,
        scopes: Vec::new(),
    };
    start(&mut kit, call_with(1, prepare), merge_marker(now_ms));
    kit.send(sock, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(kit.wake(), Some(kit.now_ms() + MERGE_WATCHDOG_MS));

    let lost = Input::CallLost {
        now_ms: kit.now_ms(),
        call: CallId(1),
    };
    kit.input(lost);
    kit.settle();
    assert_eq!(kit.wake(), Some(kit.now_ms() + 1), "the wait is over");

    kit.reply(CallId(1), CallAnswer::Tested);
    assert!(kit.logs().last().unwrap().contains("no such call"));
}

// ---------- Sockets and sessions (R6 to R13, R31, R32) ----------

/// A stored core over the hard limit that no expiry can bring under it: one claim, a1's, expires
/// `LEASE_MS` after the testkit starts, and the claims that fill the state far outlive it.
fn core_json_over_the_hard_limit_with_one_lease_due() -> String {
    let mut core = shell::load_core(None, Some("test"), None, Vec::new(), Vec::new()).unwrap();
    core.handle(
        &AgentId("early".to_string()),
        claim(1, "early.rs", "s"),
        NOW_MS,
    );
    let later = NOW_MS + 1_000_000_000;
    let summary = "x".repeat(10_000);
    let mut made = 0;
    while serde_json::to_string(&core).unwrap().len() <= HARD_ENTRY_BYTES {
        made += 1;
        let agent = AgentId(format!("filler-{made}"));
        core.handle(&agent, claim(1, &format!("f{made}.rs"), &summary), later);
    }
    serde_json::to_string(&core).unwrap()
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

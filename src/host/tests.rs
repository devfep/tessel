//! Host tests of the call path: each is named for the rule of `docs/reference/runtime-rules.md`
//! it pins. They drive the host through the testkit, so they read transcripts and outputs, not
//! the host's fields.

use serde_json::{json, Value};

use super::call::{Call, CallAnswer, CallId, DispatchId, Request};
use super::testkit::Testkit;
use super::{AnswerStep, BatchId, HostConfig, Input, MarkerStep, Output, SocketId, StartJob};
use crate::coordinator::Effect;
use crate::merge::MERGE_WATCHDOG_MS;
use crate::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, DecisionRecord, ErrorCode, Fence, Intent, Mode,
    OnConflict, RequestId, Scope, ScopeClaim, ServerMsg,
};
use crate::shell::{self, HARD_ENTRY_BYTES, SOFT_ENTRY_BYTES, STATE_LIMIT_MESSAGE};

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

fn ignore_answer() -> AnswerStep {
    Box::new(|_, _, _| Vec::new())
}

/// Store `marker`'s result and hand out `call`, as the wake does for a merge.
fn start(kit: &mut Testkit, call: Call, on_answer: AnswerStep, marker: MarkerStep) {
    let now_ms = kit.now_ms();
    let job = StartJob {
        now_ms,
        call,
        on_answer,
        marker,
    };
    let outputs = kit.host.begin_call(job);
    kit.execute(outputs);
    kit.settle();
}

fn start_plain(kit: &mut Testkit, call: Call, on_answer: AnswerStep) {
    start(kit, call, on_answer, Box::new(|_| Vec::new()));
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
    assert_eq!(label(&fresh.history()[0]), "Load", "the first call loads");
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
    start(&mut kit, call_with(1, prepare), ignore_answer(), marker);
    assert_eq!(kit.calls().len(), 1);

    kit.send(sock, &ClientMsg::Heartbeat);
    kit.settle();
    assert_eq!(
        kit.wake(),
        Some(kit.now_ms() + MERGE_WATCHDOG_MS),
        "a merge waiting here leaves only the watchdog"
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
    let sock = welcomed(&mut kit, "a1");
    let on_answer: AnswerStep = Box::new(|core, _, now_ms| core.trunk_moved(now_ms));
    start_plain(&mut kit, read_head(1), on_answer);

    kit.fail_next_commit();
    kit.send(sock, &ClientMsg::Heartbeat);
    kit.settle();
    let mark = kit.history().len();
    kit.reply(CallId(1), CallAnswer::Unknown);
    assert_eq!(
        labels_since(&kit, mark),
        ["Load"],
        "the dropped core is reloaded first"
    );
    kit.settle();
    assert_eq!(
        state(&kit)["head_pokes"],
        json!(1),
        "then the answer is applied"
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
    assert_eq!(labels_since(&kit, 0), ["Load"]);
    kit.deliver_next();
    assert_eq!(labels_since(&kit, 0), ["Load", "Commit"]);
    assert!(kit.session(sock).is_none() && kit.received(sock).is_empty());
    kit.deliver_next();
    assert_eq!(
        labels_since(&kit, 0),
        ["Load", "Commit", "SetSession", "Send", "SetWake"],
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
    assert_eq!(kit.received(sock).len(), received, "nothing is sent");
    assert_eq!(kit.stored_state(), Some(before.as_str()));
    assert!(kit
        .logs()
        .last()
        .unwrap()
        .contains("persist state and events failed"));

    let mark = kit.history().len();
    kit.send(sock, &ClientMsg::Heartbeat);
    assert_eq!(
        labels_since(&kit, mark),
        ["Load"],
        "the core is reloaded from storage"
    );
}

/// The labels the host produces when the answer to a call has these effects, with a1 connected
/// and no socket for "ghost".
fn answer_with(effects: Vec<Effect>) -> (Testkit, SocketId, Vec<&'static str>) {
    let mut kit = kit();
    let sock = welcomed(&mut kit, "a1");
    let on_answer: AnswerStep = Box::new(move |_, _, _| effects);
    start_plain(&mut kit, read_head(1), on_answer);
    let mark = kit.history().len();
    kit.reply(CallId(1), CallAnswer::Tested);
    kit.settle();
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
    let (kit, sock, labels) = answer_with(mixed_effects());
    assert_eq!(labels, ["Commit", "Log", "Send", "SetWake"]);
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
    let (kit, sock, labels) = answer_with(mixed_effects());
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
    start_plain(&mut kit, read_head(1), ignore_answer());
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
    start_plain(&mut kit, read_head(1), ignore_answer());

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

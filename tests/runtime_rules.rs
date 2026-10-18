//! Characterisation of the rules in `src/runtime.rs` whose decision lives in `shell` or the core.
//!
//! `docs/reference/runtime-rules.md` names every rule; each test here is named for its rule and
//! drives only the public API, the way `runtime.rs` composes it. Rules that live in `runtime.rs`
//! alone are pinned by host tests when each area moves (Phase 1, M2).

use serde_json::{json, Value};
use tessel_coordinator::coordinator::{parse_head_response, Coordinator as Core, Effect};
use tessel_coordinator::merge::{MergeOutcome, TrialOutcome, TrialReport};
use tessel_coordinator::protocol::{
    AgentId, Assumption, ClaimId, ClientMsg, CommitId, DecisionRecord, ErrorCode, Event, EventKind,
    Fence, Intent, Mode, OnConflict, Outcome, RaceId, ReleaseReason, RequestId, RunId, Scope,
    ScopeClaim, ServerMsg, Summary, UnverifiedAssumption,
};
use tessel_coordinator::shell::{
    after_page, agents_to_withdraw, alarm_at_ms, binary_rejection, bind_on_welcome,
    clamp_watch_from, close_log_line, decide, error_log_line, keep_first, load_core, log_agent,
    merge_cut_off, parse_client_msg, plan_delivery, poke_reply, reviewers_or_none,
    sensitive_or_all, split_effects, state_limit_reply, watcher_indexes, Action, LoadError,
    Outbound, PokeReply, ReplayStep, Session, StoredSize, SummaryTally, Target, Work,
    HARD_ENTRY_BYTES, MAX_DATE_MS, MAX_FRAME_BYTES, MAX_SAFE_SEQ, REPLAY_PAGE, SOFT_ENTRY_BYTES,
    STATE_KEY,
};

const NOW: u64 = 1_000;

fn agent(name: &str) -> AgentId {
    AgentId(name.to_string())
}

fn bound(name: &str) -> Session {
    Session {
        verified: Some(agent(name)),
        agent: Some(agent(name)),
        ..Session::default()
    }
}

fn watching(from: u64) -> Session {
    Session {
        watch_from: Some(from),
        ..Session::default()
    }
}

fn hello(name: &str) -> ClientMsg {
    let text = json!({"type": "hello", "agent": name, "base": "abc"}).to_string();
    serde_json::from_str(&text).unwrap()
}

fn edit_file(path: &str) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::File { path: path.into() },
        mode: Mode::EditBody,
    }
}

fn claim_msg(req: u64, paths: &[&str], on_conflict: OnConflict) -> ClientMsg {
    ClientMsg::Claim {
        req: RequestId(req),
        intent: Intent {
            summary: "s".into(),
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes: paths.iter().map(|path| edit_file(path)).collect(),
        on_conflict,
    }
}

fn new_core() -> Core {
    load_core(None, Some("test"), None, Vec::new(), Vec::new()).unwrap()
}

fn as_json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}

fn error_of(msg: &ServerMsg) -> (Option<u64>, ErrorCode, String) {
    let ServerMsg::Error { req, code, message } = msg else {
        panic!("expected an error reply, got {msg:?}");
    };
    (req.map(|req| req.0), *code, message.clone())
}

fn event(seq: u64, kind: EventKind) -> Event {
    Event {
        seq,
        at_ms: 5,
        run: RunId("test".into()),
        kind,
    }
}

fn verified_denial(seq: u64, outcome: Outcome) -> Event {
    let kind = EventKind::DenialVerified {
        shadow_claim: ClaimId(2),
        blocking_claim: ClaimId(1),
        outcome,
    };
    event(seq, kind)
}

#[test]
fn r07_a_binary_frame_is_answered_malformed_without_a_req() {
    let (req, code, message) = error_of(&binary_rejection());
    assert_eq!(req, None);
    assert_eq!(code, ErrorCode::Malformed);
    assert_eq!(message, "binary frames are not supported");
}

#[test]
fn r08_a_frame_over_the_limit_or_not_a_message_gets_a_fixed_reply_that_never_echoes_it() {
    let padding = " ".repeat(MAX_FRAME_BYTES - r#"{"type":"heartbeat"}"#.len());
    let at_limit = format!(r#"{{"type":"heartbeat"}}{padding}"#);
    assert_eq!(at_limit.len(), MAX_FRAME_BYTES);
    assert!(
        parse_client_msg(&at_limit).is_ok(),
        "a frame at the limit is parsed"
    );

    let over = format!("{at_limit} ");
    let fault = parse_client_msg(&over).unwrap_err();
    let (_, code, message) = error_of(&fault.reply());
    assert_eq!(code, ErrorCode::Malformed);
    assert!(message.contains(&MAX_FRAME_BYTES.to_string()), "{message}");

    let secret = r#"{"type":"hello","agent":"SECRET-TEXT""#;
    let fault = parse_client_msg(secret).unwrap_err();
    let (req, code, message) = error_of(&fault.reply());
    assert_eq!((req, code), (None, ErrorCode::Malformed));
    assert_eq!(message, "not a valid message");
    assert!(!message.contains("SECRET"));
}

#[test]
fn r09_a_hello_for_another_agent_is_refused_closed_and_its_request_withdrawn() {
    let verified_as_b = bound("b");
    let Action::RejectAndClose(reply) = decide(&verified_as_b, &hello("c")) else {
        panic!("a hello naming another agent must close the socket");
    };
    assert_eq!(error_of(&reply).1, ErrorCode::NotOwner);

    let no_identity = Session::default();
    let Action::RejectAndClose(_) = decide(&no_identity, &hello("b")) else {
        panic!("a socket with no verified agent refuses every hello");
    };
    let Action::Call { agent: caller } = decide(&verified_as_b, &hello("b")) else {
        panic!("a hello for the verified agent is served");
    };
    assert_eq!(caller, agent("b"));

    let mut core = new_core();
    core.handle(&agent("a"), claim_msg(1, &["x.rs"], OnConflict::Fail), NOW);
    core.handle(&agent("b"), claim_msg(2, &["x.rs"], OnConflict::Wait), NOW);
    let queued = |who: &AgentId| core.has_queued_request(who);
    let closed = Session {
        closed: true,
        ..verified_as_b
    };
    assert_eq!(
        agents_to_withdraw(&[closed], &[bound("a")], queued),
        vec![agent("b")]
    );
}

#[test]
fn r13_a_closing_agent_is_withdrawn_unless_an_open_socket_or_no_request_shields_it() {
    let mut core = new_core();
    core.handle(&agent("a"), claim_msg(1, &["x.rs"], OnConflict::Fail), NOW);
    core.handle(&agent("b"), claim_msg(2, &["x.rs"], OnConflict::Wait), NOW);
    core.handle(&agent("c"), claim_msg(3, &["x.rs"], OnConflict::Wait), NOW);
    let queued = |who: &AgentId| core.has_queued_request(who);

    let both = [bound("b"), bound("c"), bound("b")];
    assert_eq!(
        agents_to_withdraw(&both, &[bound("e")], queued),
        vec![agent("b"), agent("c")],
        "each agent once, in the order of the closing sockets"
    );
    assert_eq!(
        agents_to_withdraw(&both, &[bound("b")], queued),
        vec![agent("c")],
        "another open socket of b shields b"
    );
    let dead_twin = Session {
        closed: true,
        ..bound("b")
    };
    assert_eq!(
        agents_to_withdraw(&[bound("b")], &[dead_twin], queued),
        vec![agent("b")],
        "a socket already marked closed shields nobody"
    );
    assert!(
        agents_to_withdraw(&[bound("a")], &[], queued).is_empty(),
        "a has no queued request"
    );
    assert!(
        agents_to_withdraw(&[watching(0)], &[], queued).is_empty(),
        "an unbound socket"
    );
}

#[test]
fn r13_the_first_withdrawal_error_is_the_one_returned() {
    let mut first: Option<&str> = None;
    keep_first(&mut first, Ok(()));
    assert_eq!(first, None, "a success records nothing");
    keep_first(&mut first, Err("first"));
    keep_first(&mut first, Ok(()));
    keep_first(&mut first, Err("second"));
    assert_eq!(first, Some("first"));
}

#[test]
fn r14_a_stored_core_keeps_its_run_and_takes_reviewers_and_sensitive_from_the_deployment() {
    let created = load_core(
        None,
        Some("run-1"),
        Some("true"),
        vec![agent("ana")],
        vec!["a/".into()],
    )
    .unwrap();
    let stored = serde_json::to_string(&created).unwrap();
    assert_eq!(as_json(&created)["config"]["run"], json!("run-1"));
    assert_eq!(as_json(&created)["config"]["shadow_enabled"], json!(true));

    let reloaded = load_core(
        Some(&stored),
        Some("run-2"),
        Some("false"),
        vec![agent("bo")],
        vec!["b/".into()],
    )
    .unwrap();
    let value = as_json(&reloaded);
    assert_eq!(
        value["config"]["run"],
        json!("run-1"),
        "RUN matters only when nothing is stored"
    );
    assert_eq!(value["config"]["shadow_enabled"], json!(true));
    assert_eq!(value["reviewers"], json!(["bo"]));
    assert_eq!(value["sensitive"], json!(["b/"]));

    let bad_config_ignored = load_core(Some(&stored), None, Some("junk"), Vec::new(), Vec::new());
    assert!(
        bad_config_ignored.is_ok(),
        "RUN and SHADOW_ENABLED are not read over stored state"
    );
}

#[test]
fn r14_bad_lists_fail_closed_and_corrupt_or_unconfigured_state_never_starts_empty() {
    let mut reports = Vec::new();
    let reviewers = reviewers_or_none(Some("ana, not valid"), |e| reports.push(e.to_string()));
    assert!(
        reviewers.is_empty(),
        "a bad reviewers list leaves nobody able to review"
    );
    let sensitive = sensitive_or_all(Some("a/, /abs"), |e| reports.push(e.to_string()));
    assert_eq!(
        sensitive,
        vec![String::new()],
        "a bad sensitive list holds everything"
    );
    assert_eq!(reports.len(), 2);
    assert!(reports
        .iter()
        .all(|report| !report.contains("not valid") && !report.contains("/abs")));

    let corrupt = load_core(
        Some(r#"{"claims":"intent-text""#),
        Some("r"),
        None,
        vec![],
        vec![],
    );
    let Err(LoadError::Corrupt { .. }) = corrupt else {
        panic!("corrupt stored state must fail, not start empty");
    };
    let nothing_configured = load_core(None, None, None, Vec::new(), Vec::new());
    assert_eq!(nothing_configured.unwrap_err(), LoadError::MissingRun);
    let junk_shadow = load_core(None, Some("r"), Some("yes"), Vec::new(), Vec::new());
    assert_eq!(junk_shadow.unwrap_err(), LoadError::InvalidShadowEnabled);
}

fn entries(state_bytes: usize, event_bytes: &[usize]) -> Vec<(String, String)> {
    let mut all = vec![(STATE_KEY.to_string(), "s".repeat(state_bytes))];
    for (index, bytes) in event_bytes.iter().enumerate() {
        all.push((format!("ev:{index}"), "e".repeat(*bytes)));
    }
    all
}

#[test]
fn r18_only_a_content_call_that_grows_the_state_past_the_soft_limit_is_refused() {
    let stored = "s".repeat(SOFT_ENTRY_BYTES + 10);
    let size = StoredSize::on_load(Some(&stored));
    let decision =
        |work, state, events: &[usize]| format!("{:?}", size.judge(work, &entries(state, events)));

    assert_eq!(
        decision(Work::Content, SOFT_ENTRY_BYTES + 10, &[]),
        "Store",
        "no growth"
    );
    assert_eq!(
        decision(Work::Content, SOFT_ENTRY_BYTES + 11, &[]),
        "Refuse",
        "one byte of growth"
    );
    assert_eq!(
        decision(Work::Plain, SOFT_ENTRY_BYTES + 11, &[]),
        "Store",
        "plain work goes on"
    );
    assert_eq!(
        decision(Work::Content, 10, &[SOFT_ENTRY_BYTES + 1]),
        "Refuse",
        "one big event"
    );
    assert_eq!(decision(Work::Content, 10, &[SOFT_ENTRY_BYTES]), "Store");
    assert_eq!(decision(Work::Plain, HARD_ENTRY_BYTES + 1, &[]), "OverHard");
    assert_eq!(
        decision(Work::Plain, 10, &[HARD_ENTRY_BYTES + 1]),
        "OverHard"
    );
    assert_eq!(decision(Work::Plain, HARD_ENTRY_BYTES, &[]), "Store");

    let after = size.on_write(&entries(5, &[]));
    assert_eq!(
        format!("{:?}", after.judge(Work::Content, &entries(6, &[]))),
        "Store"
    );
    let (req, code, message) = error_of(&state_limit_reply(Some(RequestId(7))));
    assert_eq!((req, code), (Some(7), ErrorCode::Malformed));
    assert_eq!(message, "repo state limit reached");
}

#[test]
fn r21_a_trunk_poke_is_dropped_without_stored_state_and_recorded_with_it() {
    assert_eq!(poke_reply(None), PokeReply::UnknownRepo);
    assert_eq!(poke_reply(Some("{}")), PokeReply::Recorded);
    assert_eq!(
        poke_reply(Some("")),
        PokeReply::Recorded,
        "any stored state counts"
    );
}

#[test]
fn r23_only_a_200_with_a_head_is_a_head() {
    let body = r#"{"head":"abc123"}"#;
    assert_eq!(
        parse_head_response(200, body),
        Some(CommitId("abc123".into()))
    );
    assert_eq!(parse_head_response(500, body), None);
    assert_eq!(parse_head_response(204, body), None);
    assert_eq!(parse_head_response(200, "{}"), None);
    assert_eq!(parse_head_response(200, r#"{"head":7}"#), None);
    assert_eq!(parse_head_response(200, "not json"), None);
}

#[test]
fn r24_a_stored_merge_marker_with_no_call_waiting_here_is_a_cut_off_that_is_recovered() {
    let mut core = new_core();
    let (claim, fence) = grant(&mut core, "a", "x.rs");
    let submit = ClientMsg::Submit {
        req: RequestId(9),
        claim,
        fence,
        fork_commit: CommitId("ab".repeat(20)),
        touched: vec![edit_file("x.rs")],
        decisions: DecisionRecord {
            evidence: vec!["tests passed".into()],
            ..DecisionRecord::default()
        },
    };
    core.handle(&agent("a"), submit, NOW);
    assert!(
        core.begin_merge(NOW).is_some(),
        "the submission is dispatched"
    );
    assert!(core.has_merge_in_flight());

    assert!(
        !merge_cut_off(true, core.has_merge_in_flight()),
        "this instance is waiting on it"
    );
    assert!(
        merge_cut_off(false, core.has_merge_in_flight()),
        "a restart forgot the call"
    );
    core.recover_merge(NOW + 1);
    assert!(!core.has_merge_in_flight(), "recovery clears the marker");
    assert!(
        !merge_cut_off(false, core.has_merge_in_flight()),
        "and nothing is left to recover"
    );
}

fn grant(core: &mut Core, who: &str, path: &str) -> (ClaimId, Fence) {
    let effects = core.handle(&agent(who), claim_msg(1, &[path], OnConflict::Fail), NOW);
    for effect in effects {
        if let Effect::Reply(ServerMsg::Granted { claim, fence, .. }) = effect {
            return (claim, fence);
        }
    }
    panic!("no grant for {who} on {path}");
}

#[test]
fn r26_a_failed_or_unknown_steward_answer_is_service_unavailable_and_a_4xx_is_a_refusal() {
    let landed = r#"{"outcome":"merged","base":"a","head":"b"}"#;
    assert_eq!(
        MergeOutcome::from_response(500, landed),
        MergeOutcome::ServiceUnavailable
    );
    assert_eq!(
        MergeOutcome::from_response(302, landed),
        MergeOutcome::ServiceUnavailable
    );
    assert_eq!(
        MergeOutcome::from_response(200, "not json"),
        MergeOutcome::ServiceUnavailable
    );
    assert_eq!(
        MergeOutcome::from_response(200, r#"{"outcome":"mystery"}"#),
        MergeOutcome::ServiceUnavailable
    );
    assert_eq!(
        MergeOutcome::from_response(404, landed),
        MergeOutcome::Refused
    );
    assert_eq!(MergeOutcome::from_response(499, ""), MergeOutcome::Refused);

    let stopped = |status, body| TrialReport::from_response(status, body);
    let unavailable = TrialReport::stopped(TrialOutcome::ServiceUnavailable);
    let clean_body = r#"{"before":{"outcome":"clean"},"after":{"outcome":"clean"}}"#;
    assert_eq!(stopped(503, clean_body), unavailable);
    assert_eq!(stopped(200, "garbage"), unavailable);
    assert_eq!(
        stopped(400, ""),
        TrialReport::stopped(TrialOutcome::Refused)
    );
    let clean = stopped(200, clean_body);
    assert_eq!(clean.before, Some(TrialOutcome::Clean {}));
    assert_eq!(clean.after, Some(TrialOutcome::Clean {}));
}

#[test]
fn r32_delivery_sends_replies_then_notifications_then_each_event_to_its_watchers() {
    let reply = |req| Outbound::Reply(state_limit_reply(Some(RequestId(req))));
    let notify = |who: &str, req| Outbound::Notify {
        agent: agent(who),
        msg: state_limit_reply(Some(RequestId(req))),
    };
    let outbound = [reply(1), notify("b", 2), notify("gone", 3), reply(4)];
    let events = [
        event(5, EventKind::AgentConnected { agent: agent("a") }),
        event(6, EventKind::AgentConnected { agent: agent("b") }),
    ];
    let sessions = [bound("b"), watching(0), Session::default(), watching(6)];

    let plan = plan_delivery(&outbound, &events, &sessions);
    let labelled: Vec<(Target, String)> = plan.iter().map(|(t, m)| (*t, label(m))).collect();
    let expected = [
        (Target::Sender, "req 1"),
        (Target::Socket(0), "req 2"),
        (Target::Sender, "req 4"),
        (Target::Socket(1), "event 5"),
        (Target::Socket(1), "event 6"),
        (Target::Socket(3), "event 6"),
    ];
    let expected: Vec<(Target, String)> =
        expected.iter().map(|(t, l)| (*t, l.to_string())).collect();
    assert_eq!(
        labelled, expected,
        "nothing for an agent with no open socket"
    );
}

fn label(msg: &ServerMsg) -> String {
    if let ServerMsg::Event { event } = msg {
        return format!("event {}", event.seq);
    }
    if let ServerMsg::Error { req: Some(req), .. } = msg {
        return format!("req {}", req.0);
    }
    panic!("unlabelled message {msg:?}");
}

#[test]
fn r29_the_socket_binds_to_the_caller_only_when_the_core_answered_welcome() {
    let mut core = new_core();
    let effects = core.handle(&agent("a"), hello("a"), NOW);
    let (_, outbound) = split_effects(effects);
    let unbound = Session {
        verified: Some(agent("a")),
        ..Session::default()
    };
    let rebound = bind_on_welcome(&unbound, &agent("a"), &outbound).unwrap();
    assert_eq!(rebound.agent, Some(agent("a")));
    assert_eq!(
        rebound.verified,
        Some(agent("a")),
        "the rest of the session is kept"
    );
    let refused = [Outbound::Reply(state_limit_reply(None))];
    assert!(bind_on_welcome(&unbound, &agent("a"), &refused).is_none());
}

#[test]
fn r35_the_wake_is_the_absolute_time_never_before_the_next_millisecond_or_past_the_date_range() {
    assert_eq!(alarm_at_ms(None, 5), None, "no wake deletes the alarm");
    assert_eq!(alarm_at_ms(Some(9_000), 5), Some(9_000.0));
    assert_eq!(alarm_at_ms(Some(5), 5), Some(6.0));
    assert_eq!(alarm_at_ms(Some(0), 5), Some(6.0));
    assert_eq!(alarm_at_ms(Some(u64::MAX), 5), Some(MAX_DATE_MS));
    assert_eq!(alarm_at_ms(Some(10), u64::MAX), Some(MAX_DATE_MS));
}

#[test]
fn r36_a_replay_reads_every_event_once_and_ends_on_a_short_or_empty_page() {
    for total in [
        0_usize,
        1,
        REPLAY_PAGE - 1,
        REPLAY_PAGE,
        REPLAY_PAGE + 1,
        2 * REPLAY_PAGE,
        250,
    ] {
        let log: Vec<u64> = (0..total as u64).collect();
        let mut seen = Vec::new();
        let mut start = 0;
        loop {
            let page: Vec<u64> = log
                .iter()
                .copied()
                .filter(|seq| *seq >= start)
                .take(REPLAY_PAGE)
                .collect();
            seen.extend(page.iter().copied());
            match after_page(page.len(), page.last().copied()) {
                ReplayStep::Next { start_seq } => start = start_seq,
                ReplayStep::Done => break,
            }
        }
        assert_eq!(seen, log, "a log of {total} events");
    }
    assert_eq!(
        after_page(REPLAY_PAGE, Some(u64::MAX)),
        ReplayStep::Done,
        "no seq after the last"
    );
}

#[test]
fn r36_a_watch_start_is_clamped_and_live_events_below_it_are_not_sent() {
    assert_eq!(clamp_watch_from(u64::MAX), MAX_SAFE_SEQ);
    assert_eq!(clamp_watch_from(12), 12);
    let sessions = [watching(7), Session::default(), watching(0)];
    assert_eq!(watcher_indexes(&sessions, 6), vec![2]);
    assert_eq!(watcher_indexes(&sessions, 7), vec![0, 2]);
}

#[test]
fn r37_the_summary_counts_a_log_in_pages_and_derives_precision_from_the_totals() {
    let empty = SummaryTally::default();
    assert_eq!(empty.next_seq(), 0);
    let body: Value = serde_json::from_str(&empty.report().unwrap()).unwrap();
    assert_eq!(body["head_seq"], Value::Null, "an empty log has no head");
    assert_eq!(body["summary"]["precision"], Value::Null);

    let mut tally = SummaryTally::default();
    tally.add_page(&[
        verified_denial(0, Outcome::TextualConflict),
        verified_denial(1, Outcome::TextualConflict),
    ]);
    assert_eq!(tally.next_seq(), 2);
    tally.add_page(&[]);
    assert_eq!(tally.next_seq(), 2, "an empty page counts nothing");
    tally.add_page(&[verified_denial(2, Outcome::Clean)]);
    assert_eq!(tally.next_seq(), 3);

    let body: Value = serde_json::from_str(&tally.report().unwrap()).unwrap();
    assert_eq!(body["head_seq"], json!(2));
    assert_eq!(body["summary"]["conflicts_prevented_verified"], json!(2));
    assert_eq!(body["summary"]["false_alarms"], json!(1));
    let precision = body["summary"]["precision"].as_f64().unwrap();
    assert!(
        (precision - 2.0 / 3.0).abs() < 1e-9,
        "not the mean of the pages: {precision}"
    );
}

#[test]
fn r40_accept_close_and_error_lines_carry_the_agent_and_cut_what_a_peer_wrote() {
    let who = agent("a1");
    assert_eq!(log_agent(Some(&who)), "a1");
    assert_eq!(log_agent(None), "-");
    let line = close_log_line(1008, false, "identity mismatch", Some(&who));
    assert_eq!(
        line,
        r#"socket closed: code=1008 clean=false reason="identity mismatch" agent=a1"#
    );
    let line = error_log_line("boom", None);
    assert_eq!(line, r#"socket error: "boom" agent=-"#);

    let hostile = format!("line one\nline two{}", "x".repeat(200));
    let line = close_log_line(1000, true, &hostile, Some(&agent("a\nb")));
    assert!(!line.contains('\n'), "{line}");
    assert!(line.contains(r"line one\nline two"));
    assert!(line.contains(r"agent=a\nb"));
    assert!(!line.contains(&"x".repeat(121)), "a long reason is cut");
}

fn assumption() -> Assumption {
    Assumption {
        scope: Scope::File {
            path: "a.rs".into(),
        },
        statement: "f returns Some".into(),
    }
}

/// `count` events of every kind `Summary` counts, with `count` entries in each list it measures.
fn every_counted_kind(count: u64) -> Vec<EventKind> {
    let n = count as usize;
    let many_agents: Vec<AgentId> = (0..n).map(|i| agent(&format!("n{i}"))).collect();
    let unverified = vec![
        UnverifiedAssumption {
            claim: ClaimId(4),
            assumption: assumption(),
        };
        n
    ];
    let mut kinds = Vec::new();
    for _ in 0..count {
        kinds.extend([
            EventKind::ClaimGranted {
                agent: agent("a"),
                claim: ClaimId(1),
                fence: Fence(1),
                scopes: Vec::new(),
                intent: Intent {
                    summary: "s".into(),
                    task_ref: None,
                    assumptions: Vec::new(),
                },
                race: None,
                at_risk: Vec::new(),
            },
            EventKind::ClaimShadowed {
                agent: agent("a"),
                claim: ClaimId(2),
                scopes: Vec::new(),
                conflicts: Vec::new(),
            },
            EventKind::DenialVerified {
                shadow_claim: ClaimId(2),
                blocking_claim: ClaimId(1),
                outcome: Outcome::TextualConflict,
            },
            EventKind::DenialVerified {
                shadow_claim: ClaimId(2),
                blocking_claim: ClaimId(1),
                outcome: Outcome::Clean,
            },
            EventKind::AssumptionChallenged {
                claim: ClaimId(4),
                assumption: assumption(),
                by: agent("a"),
                their_commit: CommitId("c".into()),
            },
            EventKind::AssumptionVerified {
                claim: ClaimId(4),
                assumption: assumption(),
                outcome: Outcome::TextualConflict,
            },
            EventKind::VerificationDropped {
                claim: ClaimId(4),
                assumption: assumption(),
                reason: ReleaseReason::Agent,
            },
            EventKind::Merged {
                claim: ClaimId(1),
                head: CommitId("abc".into()),
                unverified: unverified.clone(),
                untried_shadows: Vec::new(),
            },
            EventKind::ReviewRequested {
                claim: ClaimId(1),
                reasons: Vec::new(),
            },
            EventKind::BaseMoved {
                head: CommitId("abc".into()),
                by: agent("a"),
                notified: many_agents.clone(),
            },
            EventKind::RaceDecided {
                race: RaceId(1),
                winner: None,
                ranking: Vec::new(),
            },
            EventKind::ReplayMerged {
                agent: agent("a"),
                fork_commit: CommitId("c".into()),
                outcome: Outcome::TextualConflict,
            },
        ]);
    }
    kinds
}

#[test]
fn r37_every_counter_adds_up_across_pages_of_different_sizes() {
    let (first, second) = (every_counted_kind(2), every_counted_kind(3));
    let log: Vec<Event> = first
        .into_iter()
        .chain(second)
        .enumerate()
        .map(|(seq, kind)| event(seq as u64, kind))
        .collect();
    let page_len = 2 * 12;
    let mut tally = SummaryTally::default();
    for page in log.chunks(page_len) {
        tally.add_page(page);
    }
    let whole = Summary::from_events(&log);
    assert_eq!(tally.summary(), whole);

    let counters = as_json(&whole);
    let Value::Object(counters) = counters else {
        panic!("a summary is an object");
    };
    for (name, value) in counters {
        if name != "precision" {
            assert!(value.as_u64().unwrap() > 0, "{name} is not exercised");
        }
    }
}

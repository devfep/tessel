//! The shape of what a run writes: the JSON per run and the Markdown A/B table.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::time::Duration;

use tessel_coordinator::protocol::Summary;
use tessel_swarm::conn::HEARTBEAT_EVERY;
use tessel_swarm::off::{self, Counts, OffConfig, OffResult};
use tessel_swarm::on::{OnConfig, OnResult, Policy, Resolution, ShadowTrials, TaskResult};
use tessel_swarm::report::{ab_markdown, header_of, off_json, on_json, SCHEMA};
use tessel_swarm::tasks::{Kind, Task};

fn off_result() -> OffResult {
    let tasks = [
        Task {
            id: 1,
            func: "unitPrice".into(),
            kind: Kind::Body,
        },
        Task {
            id: 2,
            func: "unitPrice".into(),
            kind: Kind::Body,
        },
    ];
    let scratch = tempfile::tempdir().unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(off::run_off(
            &tasks,
            scratch.path(),
            OffConfig {
                agents: 2,
                work_ms: 0,
            },
        ))
        .unwrap()
}

fn on_result() -> OnResult {
    OnResult {
        wall_ms: 1200,
        events: Vec::new(),
        summary: Summary {
            merges: 2,
            claims_granted: 2,
            denials: 1,
            ..Summary::default()
        },
        rejected_in_log: 0,
        waits_in_log: 1,
        reviews_approved: 0,
        reviews_rejected: 0,
        reviews_held: 2,
        scripted_reviewer: false,
        shadow_trials: ShadowTrials::default(),
        results: vec![TaskResult {
            task: 1,
            agent: "a01".into(),
            result: Resolution::Merged,
            denials: 0,
            work_ms: 300,
            waited_ms: 0,
            note: None,
        }],
        work_ms_total: 600,
        wasted_ms: 0,
        waited_ms: 250,
    }
}

fn config() -> OnConfig {
    OnConfig {
        agents: 2,
        policy: Policy::Wait,
        work_ms: 0,
        task_timeout: Duration::from_secs(1),
        trial_wait: Duration::from_secs(1),
        heartbeat_every: HEARTBEAT_EVERY,
        max_denials: 1,
        scripted_reviewer: false,
    }
}

#[test]
fn json_results_carry_the_schema_the_run_and_where_each_number_came_from() {
    let header = header_of(&config(), 9, 2, 0.5);
    let off = off_json(&header, &off_result());
    assert_eq!(off["schema"], SCHEMA);
    assert_eq!(off["mode"], "off");
    assert_eq!(off["run"]["seed"], 9);
    assert!(off["scope"]
        .as_str()
        .unwrap()
        .contains("never sent to a coordinator"));
    assert_eq!(off["summary_from_local_replay_events"]["replay_merges"], 2);
    assert_eq!(
        off["summary_from_local_replay_events"]["replay_conflicts"],
        1
    );
    assert_eq!(off["counts"]["textual_conflicts"], 1);
    assert_eq!(off["merges"].as_array().map(Vec::len), Some(2));

    let on = on_json(&header, "local", Policy::Wait, &on_result());
    assert_eq!(on["mode"], "on");
    assert_eq!(on["policy"], "wait");
    assert_eq!(on["target"], "local");
    assert_eq!(on["summary_from_event_log"]["merges"], 2);
    assert_eq!(on["queued_waits_in_event_log"], 1);
    assert_eq!(on["tasks"][0]["result"], "merged");
    assert_eq!(on["measured_agent_ms"]["waiting"], 250);
}

#[test]
fn the_ab_table_has_two_labelled_columns_and_never_turns_a_missing_number_into_zero() {
    let header = header_of(&config(), 9, 2, 0.5);
    let table = ab_markdown(&header, "local", Policy::Wait, &off_result(), &on_result());
    let rows: Vec<&str> = table.lines().filter(|l| l.starts_with('|')).collect();
    assert!(
        rows[0].contains("off: no coordination, plain git, red merges rolled back, local replay"),
        "{}",
        rows[0]
    );
    assert!(
        rows[0].contains("on: Tessel (local coordinator, policy wait)"),
        "{}",
        rows[0]
    );
    assert!(
        rows.iter().all(|r| r.matches('|').count() == 4),
        "every row has three cells: {table}"
    );
    let cells = |label: &str| -> Vec<String> {
        let row = rows
            .iter()
            .find(|r| r.starts_with(&format!("| {label}")))
            .unwrap_or_else(|| unreachable!("{label}"));
        row.trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect()
    };
    assert_eq!(
        cells("Landed on the trunk"),
        ["Landed on the trunk", "1", "2"]
    );
    assert_eq!(cells("Rejected after the work was done")[1], "1");
    assert_eq!(
        cells("Conflicts prevented, verified by shadow runs")[1],
        "n/a"
    );
    assert!(cells("Conflicts prevented, verified by shadow runs")[2].starts_with("n/a"));
    assert_eq!(
        cells("Claims denied outright (a denial is not a prevented conflict)")[1],
        "n/a"
    );
    assert_eq!(
        cells("Claims denied outright (a denial is not a prevented conflict)")[2],
        "1"
    );
    assert!(table.contains("not sent to any coordinator"));
    assert!(table.contains("`Summary::from_events`"));
}

fn result(task: usize, result: Resolution) -> TaskResult {
    TaskResult {
        task,
        agent: "a01".into(),
        result,
        denials: 0,
        work_ms: 0,
        waited_ms: 0,
        note: None,
    }
}

fn cells(table: &str, label: &str) -> Vec<String> {
    let row = table
        .lines()
        .find(|l| l.starts_with(&format!("| {label} |")))
        .unwrap_or_else(|| unreachable!("no row {label:?} in {table}"));
    row.trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

#[test]
fn every_cell_of_the_table_is_pinned_to_the_number_it_shows() {
    let off = OffResult {
        wall_ms: 60_000,
        merges: Vec::new(),
        counts: Counts {
            clean: 3,
            textual_conflicts: 2,
            build_failed: 1,
            tests_failed: 4,
        },
        events: Vec::new(),
        summary: Summary::default(),
        work_ms: 120_000,
        wasted_ms: 30_000,
    };
    let mut on = on_result();
    on.wall_ms = 30_000;
    on.summary.merges = 6;
    on.wasted_ms = 6_000;
    on.work_ms_total = 90_000;
    on.results = vec![
        result(1, Resolution::Merged),
        result(2, Resolution::TimedOut),
        result(3, Resolution::NotRun),
        result(4, Resolution::Rejected),
        result(5, Resolution::Lapsed),
        result(6, Resolution::Disconnected),
        result(7, Resolution::Disconnected),
    ];
    let table = ab_markdown(
        &header_of(&config(), 9, 10, 0.5),
        "local",
        Policy::Wait,
        &off,
        &on,
    );
    let row = |label: &str, off: &str, on: &str| {
        assert_eq!(cells(&table, label), [label, off, on], "{label}");
    };
    row("Tasks", "10", "10");
    row("Landed on the trunk", "3", "6");
    row("Rejected after the work was done", "7", "0");
    row("of which textual conflict", "2", "n/a");
    row("of which broke the build", "1", "n/a");
    row("of which broke the tests", "4", "n/a");
    row(
        "Not finished (starved, timed out, failed, lapsed, disconnected, not run)",
        "0",
        "5",
    );
    row("of which the claim lapsed (lease expired)", "n/a", "1");
    row("of which the agent's connection closed", "n/a", "2");
    row("Landed per minute", "3.0", "12.0");
    row("Wall time (ms)", "60000", "30000");
    row("Agent-minutes of work later rejected", "0.500", "0.100");
    row("Agent-minutes of work in total", "2.000", "1.500");
    row("Held for review (not approved)", "n/a", "2");
    on.reviews_approved = 5;
    on.reviews_rejected = 1;
    let table = ab_markdown(
        &header_of(&config(), 9, 10, 0.5),
        "local",
        Policy::Wait,
        &off,
        &on,
    );
    let approvals = "Review approvals (the only reviewer is the script)";
    assert_eq!(cells(&table, approvals)[2], "5");
    assert_eq!(cells(&table, "Review rejections")[2], "1");
}

const SHADOW_ROW: &str = "Conflicts prevented, verified by shadow runs";

fn shadow_result() -> OnResult {
    let mut on = on_result();
    on.summary.denials = 5;
    on.summary.conflicts_prevented_verified = 2;
    on.summary.false_alarms = 1;
    on.shadow_trials = ShadowTrials {
        claims: 5,
        inconclusive: 1,
        never_verified: 1,
    };
    on
}

#[test]
fn the_shadow_row_reports_verified_outcomes_and_never_the_denials() {
    let table = ab_markdown(
        &header_of(&config(), 9, 2, 0.5),
        "local",
        Policy::Shadow,
        &off_result(),
        &shadow_result(),
    );
    assert_eq!(
        cells(&table, SHADOW_ROW)[2],
        "verified preventions 2, false alarms 1 \
         (shadow claims 5: inconclusive 1, never verified 1)"
    );
    assert_eq!(cells(&table, SHADOW_ROW)[1], "n/a");
}

#[test]
fn other_policies_cannot_produce_the_shadow_row_whatever_the_summary_holds() {
    for policy in [Policy::Wait, Policy::Skip] {
        let table = ab_markdown(
            &header_of(&config(), 9, 2, 0.5),
            "local",
            policy,
            &off_result(),
            &shadow_result(),
        );
        assert_eq!(
            cells(&table, SHADOW_ROW)[2],
            "n/a (no shadow run in this harness)"
        );
        assert!(
            !table.contains("shadow work") && !table.contains("not comparable"),
            "the wait and skip tables carry nothing of the shadow policy: {table}"
        );
    }
}

#[test]
fn the_shadow_policy_adds_its_own_rows_and_a_note_about_landed_counts() {
    let mut on = shadow_result();
    for (task, ms) in [(2, 60_000), (3, 30_000)] {
        let mut shadowed = result(task, Resolution::Shadowed);
        shadowed.work_ms = ms;
        on.results.push(shadowed);
    }
    let table = ab_markdown(
        &header_of(&config(), 9, 2, 0.5),
        "local",
        Policy::Shadow,
        &off_result(),
        &on,
    );
    let shadow_work = "Run as shadow work (submitted for verification, never to merge)";
    assert_eq!(cells(&table, shadow_work)[2], "2");
    assert_eq!(
        cells(&table, "Agent-minutes on shadow work (never merged)")[2],
        "1.500"
    );
    let unfinished = "Not finished (starved, timed out, failed, lapsed, disconnected, not run)";
    assert_eq!(
        cells(&table, unfinished)[2],
        "0",
        "shadow work is not unfinished work"
    );
    assert!(
        table.contains("not comparable with the `wait` or `skip` policies"),
        "{table}"
    );
}

#[test]
fn the_json_carries_the_shadow_counts_only_for_the_shadow_policy() {
    let header = header_of(&config(), 9, 2, 0.5);
    let shadow = on_json(&header, "local", Policy::Shadow, &shadow_result());
    assert_eq!(shadow["policy"], "shadow");
    assert_eq!(
        shadow["shadow_verification"]["conflicts_prevented_verified"],
        2
    );
    assert_eq!(shadow["shadow_verification"]["false_alarms"], 1);
    assert_eq!(shadow["shadow_verification"]["inconclusive"], 1);
    assert_eq!(shadow["shadow_verification"]["never_verified"], 1);
    let wait = on_json(&header, "local", Policy::Wait, &shadow_result());
    assert!(wait.get("shadow_verification").is_none());
}

//! The shape of what a run writes: the JSON per run and the Markdown A/B table.

#![expect(clippy::unwrap_used, reason = "test code")]

use std::time::Duration;

use tessel_coordinator::protocol::Summary;
use tessel_swarm::off::{self, OffConfig, OffResult};
use tessel_swarm::on::{OnConfig, OnResult, Policy, Resolution, TaskResult};
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
        max_denials: 1,
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
        rows[0].contains("off: no coordination, plain git, local replay"),
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

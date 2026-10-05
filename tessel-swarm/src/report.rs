//! Results on disk: one JSON file per run, and a Markdown table that puts the two runs of one
//! seed side by side. Columns say where each number comes from; a number a run cannot produce is
//! `n/a`, never zero.

use serde::Serialize;
use serde_json::{json, Value};

use crate::off::OffResult;
use crate::on::{OnConfig, OnResult, Policy, Resolution};

pub const SCHEMA: &str = "tessel-swarm/1";

/// What identifies a run: the same header on both sides of a comparison.
#[derive(Debug, Clone, Serialize)]
pub struct Header {
    pub seed: u64,
    pub tasks: usize,
    pub overlap: f64,
    pub agents: usize,
    pub work_ms: u64,
}

fn policy_name(policy: Policy) -> &'static str {
    match policy {
        Policy::Wait => "wait",
        Policy::Skip => "skip",
        Policy::Shadow => "shadow",
    }
}

/// `on` and `off` agree on the workload; only `on` has a policy.
pub fn header_of(config: &OnConfig, seed: u64, tasks: usize, overlap: f64) -> Header {
    Header {
        seed,
        tasks,
        overlap,
        agents: config.agents,
        work_ms: config.work_ms,
    }
}

pub fn off_json(header: &Header, off: &OffResult) -> Value {
    json!({
        "schema": SCHEMA,
        "mode": "off",
        "run": header,
        "scope": "local replay with plain git; never sent to a coordinator",
        "wall_ms": off.wall_ms,
        "summary_from_local_replay_events": off.summary,
        "merges": off.merges,
        "counts": off.counts,
        "measured_agent_ms": {
            "note": "per task: the configured work_ms plus the measured time to edit, test and \
                     commit, from when the agent could start",
            "work_total": off.work_ms,
            "on_work_later_rejected": off.wasted_ms,
        },
    })
}

/// The shadow row of the A/B table. Only the shadow policy makes shadow runs, so every other
/// policy says it cannot produce the number. The counts are `Summary::from_events` over the
/// coordinator's log: a denial is never counted here, only a `DenialVerified` conflict.
fn shadow_cell(policy: Policy, on: &OnResult) -> String {
    match policy {
        Policy::Wait | Policy::Skip => "n/a (this policy makes no shadow run)".into(),
        Policy::Shadow => format!(
            "verified preventions {}, false alarms {} (shadow claims {}: inconclusive {}, never \
             verified {})",
            on.summary.conflicts_prevented_verified,
            on.summary.false_alarms,
            on.shadow_trials.claims,
            on.shadow_trials.inconclusive,
            on.shadow_trials.never_verified,
        ),
    }
}

pub fn on_json(header: &Header, target: &str, policy: Policy, on: &OnResult) -> Value {
    let mut value = json!({
        "schema": SCHEMA,
        "mode": "on",
        "run": header,
        "policy": policy_name(policy),
        "target": target,
        "wall_ms": on.wall_ms,
        "summary_from_event_log": on.summary,
        "event_count": on.events.len(),
        "rejected_in_event_log": on.rejected_in_log,
        "queued_waits_in_event_log": on.waits_in_log,
        "held_for_review_not_approved": on.reviews_held,
        "review_approvals_in_event_log": on.reviews_approved,
        "review_rejections_in_event_log": on.reviews_rejected,
        "scripted_reviewer": on.scripted_reviewer,
        "tasks": on.results,
        "measured_agent_ms": {
            "note": "work: claim grant to commit pushed; waiting: claim sent to its answer, \
                     summed over attempts",
            "work_total": on.work_ms_total,
            "on_work_later_rejected": on.wasted_ms,
            "waiting": on.waited_ms,
        },
    });
    if policy == Policy::Shadow {
        value["shadow_verification"] = json!({
            "note": "from DenialVerified events in the coordinator's log; a denial is not a \
                     prevention",
            "conflicts_prevented_verified": on.summary.conflicts_prevented_verified,
            "false_alarms": on.summary.false_alarms,
            "precision": on.summary.precision,
            "shadow_claims": on.shadow_trials.claims,
            "inconclusive": on.shadow_trials.inconclusive,
            "never_verified": on.shadow_trials.never_verified,
        });
    }
    value
}

/// Tenths, from integers: 69.8 landed per minute.
fn per_minute(count: u64, wall_ms: u64) -> String {
    if wall_ms == 0 {
        return "n/a".into();
    }
    let tenths = count.saturating_mul(600_000) / wall_ms;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// Thousandths of a minute, from integers.
fn minutes(ms: u64) -> String {
    let thousandths = ms.saturating_mul(1000) / 60_000;
    format!("{}.{:03}", thousandths / 1000, thousandths % 1000)
}

fn row(label: &str, off: &str, on: &str) -> [String; 3] {
    [label.into(), off.into(), on.into()]
}

fn table_rows(header: &Header, policy: Policy, off: &OffResult, on: &OnResult) -> Vec<[String; 3]> {
    let unfinished = on
        .results
        .iter()
        .filter(|r| {
            r.result != Resolution::Merged
                && r.result != Resolution::Rejected
                && r.result != Resolution::Shadowed
        })
        .count();
    let shadowed = on
        .results
        .iter()
        .filter(|r| r.result == Resolution::Shadowed)
        .count();
    let counts = &off.counts;
    let off_rejected = counts.textual_conflicts + counts.build_failed + counts.tests_failed;
    let n = |value: u64| value.to_string();
    vec![
        row(
            "Tasks",
            &header.tasks.to_string(),
            &header.tasks.to_string(),
        ),
        row(
            "Landed on the trunk",
            &n(counts.clean),
            &n(on.summary.merges),
        ),
        row(
            "Rejected after the work was done",
            &n(off_rejected),
            &n(on.rejected_in_log),
        ),
        row(
            "of which textual conflict",
            &n(counts.textual_conflicts),
            "n/a",
        ),
        row("of which broke the build", &n(counts.build_failed), "n/a"),
        row("of which broke the tests", &n(counts.tests_failed), "n/a"),
        row(
            "Not finished (starved, timed out, failed, not run)",
            "0",
            &unfinished.to_string(),
        ),
        row(
            "Run as shadow work (submitted for verification, never to merge)",
            "n/a",
            &shadowed.to_string(),
        ),
        row(
            "Claims denied outright (a denial is not a prevented conflict)",
            "n/a",
            &n(on.summary.denials),
        ),
        row(
            "Claims queued behind a holder (wait policy)",
            "n/a",
            &n(on.waits_in_log),
        ),
        row(
            "Conflicts prevented, verified by shadow runs",
            "n/a",
            &shadow_cell(policy, on),
        ),
        row("Held for review (not approved)", "n/a", &n(on.reviews_held)),
        row(
            "Review approvals (the only reviewer is the script)",
            "n/a",
            &n(on.reviews_approved),
        ),
        row("Review rejections", "n/a", &n(on.reviews_rejected)),
        row("Wall time (ms)", &n(off.wall_ms), &n(on.wall_ms)),
        row(
            "Landed per minute",
            &per_minute(counts.clean, off.wall_ms),
            &per_minute(on.summary.merges, on.wall_ms),
        ),
        row(
            "Agent-minutes of work later rejected",
            &minutes(off.wasted_ms),
            &minutes(on.wasted_ms),
        ),
        row(
            "Agent-minutes of work in total",
            &minutes(off.work_ms),
            &minutes(on.work_ms_total),
        ),
    ]
}

fn notes(off: &OffResult, on: &OnResult) -> Vec<String> {
    vec![
        format!(
            "- `off` is a local replay. Its numbers are computed here from plain-git merges onto a \
             trunk in task order with the tests run after each merge, and are not sent to any \
             coordinator. A merge that breaks the build or the tests is rolled back, so each merge \
             is judged on a green trunk. `Summary::from_events` over the replay's own events gives \
             {} merges and {} conflicts.",
            off.summary.replay_merges, off.summary.replay_conflicts
        ),
        format!(
            "- Every `on` count of the coordinator's behaviour comes from `Summary::from_events` \
             over the coordinator's event log ({} events). \"Rejected\" and \"queued\" are counts \
             of `SubmitRejected` and `WaitQueued` events in that log, which `Summary` has no field \
             for.",
            on.events.len()
        ),
        "- `off` models `--agents` agents working at once: task i branches from the trunk after \
         tasks \
         1 to i minus agents were merged, as an agent that pulls before its next task would, and \
         does not see work still in flight. Red merges are rolled back, so this harness does not \
         measure how long main stayed green."
            .into(),
        "- Agent-minutes are the same quantity on both sides: the configured work time plus the \
         measured time to edit, run the tests and commit, per task. In `on` the clock starts when \
         the claim is granted."
            .into(),
        "- A cell reading `n/a` is a number this run cannot produce, not a zero. The log records \
         why a steward rejected a submission only as text, which the harness does not parse, so \
         `on` has no split of its rejections."
            .into(),
    ]
}

/// The side-by-side table. `target` is `local` or `live`: where the `on` run happened.
pub fn ab_markdown(
    header: &Header,
    target: &str,
    policy: Policy,
    off: &OffResult,
    on: &OnResult,
) -> String {
    let mut lines = vec![
        format!(
            "# A/B run: seed {}, {} tasks, overlap {}, {} agents",
            header.seed, header.tasks, header.overlap, header.agents
        ),
        String::new(),
        format!(
            "Same seed, same tasks, same starting repository, same {} ms of work per task.",
            header.work_ms
        ),
        String::new(),
        format!(
            "| Metric | off: no coordination, plain git, red merges rolled back, local replay | \
             on: Tessel ({target} coordinator, policy {}) |",
            policy_name(policy)
        ),
        "|---|---|---|".into(),
    ];
    for [metric, off_cell, on_cell] in table_rows(header, policy, off, on) {
        lines.push(format!("| {metric} | {off_cell} | {on_cell} |"));
    }
    lines.push(String::new());
    lines.extend(notes(off, on));
    lines.join("\n") + "\n"
}

//! The deterministic simulator. A seed fixes the clock, the client traffic and the faults; the
//! real `Host` runs against a simulated store; checkers read what clients received and what the
//! store holds. Nothing here reads a wall clock or the OS (the clippy ban in `clippy.toml` holds
//! it), so a seed reproduces on any machine, and every seed runs twice with the transcripts
//! compared.
//!
//! Checkers I1, I2, I4, I5, I8, I9 and I10 prove the invariants of `docs/proposals/phase-1.md` that
//! the call path and the wake carry. The merge path is asynchronous and faulty (`lander`, and
//! `driver::merge_path` for how its answers come back), and I3, I6, I7, I11 and I12 and
//! COORD-HEAD-FROM-AGENT's check (`merge_check`) read what it does. Their evidence and advisory
//! clauses come with the rows that add evidence records and advisory mode (M6).

pub mod check;
pub mod clock;
pub mod driver;
pub mod lander;
pub mod merge_check;
pub mod rng;
pub mod store;
pub mod traffic;
pub mod variant;

use crate::host::HostConfig;
pub use check::{Checker, Violation};
pub use driver::Sim;
pub use rng::Rng;
pub use variant::Variant;

/// How many seeds a run takes unless `TESSEL_SIM_SEEDS` says otherwise: the short set.
pub const DEFAULT_SEEDS: u64 = 20;

/// Steps of traffic per seed.
pub const STEPS: usize = 120;

/// The environment variable that overrides the number of seeds.
pub const SEEDS_VAR: &str = "TESSEL_SIM_SEEDS";

/// The environment variable that moves the first seed, so a failing seed reruns alone.
pub const FIRST_SEED_VAR: &str = "TESSEL_SIM_FIRST_SEED";

/// `text` as a whole number, or `default` when the variable is not set.
fn parse_number(
    var: &str,
    text: Result<String, std::env::VarError>,
    default: u64,
) -> Result<u64, String> {
    match text {
        Ok(text) => text
            .trim()
            .parse()
            .map_err(|_| format!("{var} must be a whole number, got {text:?}")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{var} is not valid text")),
    }
}

/// The seeds to run: `TESSEL_SIM_SEEDS` of them (or `DEFAULT_SEEDS`) from `TESSEL_SIM_FIRST_SEED`
/// (or 0).
pub fn seeds() -> Result<std::ops::Range<u64>, String> {
    seeds_from(std::env::var(SEEDS_VAR), std::env::var(FIRST_SEED_VAR))
}

fn seeds_from(
    count: Result<String, std::env::VarError>,
    first: Result<String, std::env::VarError>,
) -> Result<std::ops::Range<u64>, String> {
    let count = parse_number(SEEDS_VAR, count, DEFAULT_SEEDS)?;
    let first = parse_number(FIRST_SEED_VAR, first, 0)?;
    if count == 0 {
        return Err(format!("{SEEDS_VAR} must be at least 1: no seed would run"));
    }
    let Some(end) = first.checked_add(count) else {
        return Err(format!(
            "{FIRST_SEED_VAR}={first} and {SEEDS_VAR}={count} run past the largest seed"
        ));
    };
    Ok(first..end)
}

/// The test that sweeps the seeds (`tests/simulator.rs`), so a rerun runs only it.
pub const SWEEP_TEST: &str =
    "the_real_host_keeps_every_invariant_on_every_seed_and_each_seed_reproduces";

/// How to rerun `seed` alone.
pub fn rerun_hint(seed: u64) -> String {
    format!(
        "rerun with: {FIRST_SEED_VAR}={seed} {SEEDS_VAR}=1 cargo test -p tessel-coordinator \
         --test simulator {SWEEP_TEST}"
    )
}

/// What one run of a seed produced.
#[derive(Debug)]
pub struct Run {
    /// What the checks found at the first step that found anything.
    pub violations: Vec<Violation>,
    /// Steps completed.
    pub steps: usize,
    /// Crashes injected.
    pub crashes: u64,
    /// Watchers whose socket died after some of the replay had reached it.
    pub cut_off_replays: usize,
    /// Every input and output, in order.
    pub transcript: Vec<String>,
    /// The faults the merge path injected, by name.
    pub faults: lander::FaultCounts,
    /// `Merged` events in the log.
    pub merged: usize,
    /// Landings on main, the lander's own and others'.
    pub landings: usize,
    /// Answers the host had for a dispatch not in flight.
    pub stale_answers: usize,
    /// Trials the host dispatched.
    pub trials: usize,
}

fn host_config() -> HostConfig {
    HostConfig {
        repo: "sim".to_string(),
        run: Some("sim".to_string()),
        reviewers: Some(traffic::REVIEWER.to_string()),
        sensitive: Some("lib/".to_string()),
        ..HostConfig::default()
    }
}

/// Run `seed` once against `variant`, checking `enabled` after every step. It stops at the first
/// step that violates anything.
pub fn run_seed(seed: u64, variant: Variant, enabled: &[Checker]) -> Run {
    let mut sim = Sim::new(host_config(), variant);
    sim.inject_merge_faults(seed);
    let mut traffic = traffic::Traffic::new(seed);
    let mut violations = Vec::new();
    let mut steps = 0;
    for _ in 0..STEPS {
        steps += 1;
        if !traffic.step(&mut sim) {
            violations.push(Violation {
                checker: Checker::Harness,
                detail: "the host kept asking the adapter past any bound".to_string(),
            });
        }
        violations.extend(check::run(&sim, enabled));
        if !violations.is_empty() {
            break;
        }
    }
    Run {
        violations,
        steps,
        crashes: sim.crashes(),
        cut_off_replays: cut_off_replays(&sim),
        transcript: sim.log().to_vec(),
        faults: sim.lander().faults(),
        merged: merged_events(&sim),
        landings: sim.lander().history().len(),
        stale_answers: sim.stale_answers().len(),
        trials: sim
            .dispatches()
            .iter()
            .filter(|dispatch| matches!(dispatch.request, crate::host::call::Request::Trial { .. }))
            .count(),
    }
}

fn merged_events(sim: &Sim) -> usize {
    let (events, _) = sim.store().events();
    events
        .iter()
        .filter(|event| matches!(event.kind, crate::protocol::EventKind::Merged { .. }))
        .count()
}

/// Violations a seed is known to produce, with what must change before it stops: the seed, the
/// checker, and why. `check_seed` accepts these and no others, and refuses a seed that no longer
/// produces one, so a fix shows as a line to delete. The seeds are found by sweeping and move
/// with the traffic and the lander (see `traffic`).
///
/// Every entry is the core counting `Unknown` after `Land`, or recovering a `Land` marker with no
/// call waiting, as a failed attempt, and rejecting work the lander landed (I6). P1-CORE-RECONCILE
/// asks the lander where main is instead.
pub const EXPECTED_TO_FAIL: &[(u64, Checker, &str)] = &[
    (11, Checker::I6, UNKNOWN_AFTER_LAND),
    (108, Checker::I6, UNKNOWN_AFTER_LAND),
    (114, Checker::I6, UNKNOWN_AFTER_LAND),
    (187, Checker::I6, UNKNOWN_AFTER_LAND),
];

const UNKNOWN_AFTER_LAND: &str =
    "P1-CORE-RECONCILE: the core counts Unknown after Land as a failed attempt";

/// Whether `seed` is known to break `checker`.
fn expected(seed: u64, checker: Checker) -> bool {
    EXPECTED_TO_FAIL
        .iter()
        .any(|(s, c, _)| *s == seed && *c == checker)
}

/// Run `seed` against the real host with every checker, then again with none, and compare the two
/// transcripts. The error names what failed and how to rerun the seed alone and keep it. A
/// violation the seed is expected to produce (`EXPECTED_TO_FAIL`) is not a failure, and a seed that
/// no longer produces one is.
pub fn check_seed(seed: u64) -> Result<Run, String> {
    let first = run_seed(seed, Variant::Correct, &Checker::ALL);
    let second = run_seed(seed, Variant::Correct, &[]);
    let hint = rerun_hint(seed);
    let unexpected: Vec<String> = first
        .violations
        .iter()
        .filter(|v| !expected(seed, v.checker))
        .map(|v| format!("{:?}: {}", v.checker, v.detail))
        .collect();
    if !unexpected.is_empty() {
        return Err(format!(
            "seed {seed} broke {}\n  {hint}",
            unexpected.join("; ")
        ));
    }
    let stopped = !first.violations.is_empty();
    let upto = if stopped {
        first.transcript.len().min(second.transcript.len())
    } else {
        second.transcript.len()
    };
    if let Some(difference) = first_difference(&first.transcript, &second.transcript[..upto]) {
        return Err(format!(
            "seed {seed} is not deterministic: {difference}\n  {hint}"
        ));
    }
    if let Some((_, checker, why)) = EXPECTED_TO_FAIL
        .iter()
        .find(|(s, c, _)| *s == seed && !first.violations.iter().any(|v| v.checker == *c))
    {
        return Err(format!(
            "seed {seed} no longer breaks {checker:?} ({why}): remove it from EXPECTED_TO_FAIL"
        ));
    }
    if !stopped && first.steps != STEPS {
        return Err(format!(
            "seed {seed} stopped after {} steps\n  {hint}",
            first.steps
        ));
    }
    Ok(first)
}

/// How many failing seeds a report lists before it only counts the rest.
pub const REPORTED_FAILURES: usize = 20;

/// The report of a sweep's failures: at most `REPORTED_FAILURES` of them, each with a line for
/// keeping the seed unless it is already a regression case, then a count of the rest.
pub fn failure_report(failures: &[(u64, String)], regressions: &[u64]) -> String {
    let mut report = format!("{} seeds failed:", failures.len());
    for (seed, message) in failures.iter().take(REPORTED_FAILURES) {
        report.push('\n');
        report.push_str(message);
        if !regressions.contains(seed) {
            report.push_str("\n  ");
            report.push_str(&keep_hint(*seed));
        }
    }
    if failures.len() > REPORTED_FAILURES {
        let rest = failures.len() - REPORTED_FAILURES;
        report.push_str(&format!("\nand {rest} more failing seeds"));
    }
    report
}

/// How to keep `seed` as a regression case.
pub fn keep_hint(seed: u64) -> String {
    format!("keep it: add the line `{seed}  # what it found` to tests/simulator-regressions.txt")
}

/// The seeds of a regression list: one per line, `seed  # note`, blank lines and `#` comments
/// allowed. Each run replays them before the seeds of the sweep.
pub fn parse_regressions(text: &str) -> Result<Vec<u64>, String> {
    let mut seeds: Vec<u64> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let body = line.split('#').next().unwrap_or("").trim();
        if body.is_empty() {
            continue;
        }
        let number = index + 1;
        let seed: u64 = body
            .parse()
            .map_err(|_| format!("line {number}: {body:?} is not a seed (a whole number)"))?;
        if seeds.contains(&seed) {
            return Err(format!("line {number}: seed {seed} is listed twice"));
        }
        seeds.push(seed);
    }
    Ok(seeds)
}

/// The first line where two transcripts differ, if they do.
pub fn first_difference(a: &[String], b: &[String]) -> Option<String> {
    for (index, (left, right)) in a.iter().zip(b).enumerate() {
        if left != right {
            return Some(format!(
                "line {index}:\n  first run:  {left}\n  second run: {right}"
            ));
        }
    }
    (a.len() != b.len()).then(|| format!("one run has {} lines, the other {}", a.len(), b.len()))
}

fn cut_off_replays(sim: &Sim) -> usize {
    let received = |sock| {
        sim.transcript()
            .iter()
            .filter(|delivery| delivery.sock == sock)
            .any(|delivery| {
                if let crate::protocol::ServerMsg::Event { .. } = delivery.msg {
                    return true;
                }
                false
            })
    };
    sim.watches()
        .iter()
        .filter(|watch| watch.dropped && received(watch.sock))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simulated_host_names_its_repo_run_reviewer_and_sensitive_path() {
        let config = host_config();
        assert_eq!(config.repo, "sim");
        assert_eq!(config.run.as_deref(), Some("sim"));
        assert_eq!(config.reviewers.as_deref(), Some(traffic::REVIEWER));
        assert_eq!(config.sensitive.as_deref(), Some("lib/"));
    }

    fn set(text: &str) -> Result<String, std::env::VarError> {
        Ok(text.to_string())
    }

    fn unset() -> Result<String, std::env::VarError> {
        Err(std::env::VarError::NotPresent)
    }

    #[test]
    fn a_count_is_read_and_a_missing_one_takes_the_default() {
        assert_eq!(seeds_from(unset(), unset()), Ok(0..DEFAULT_SEEDS));
        assert_eq!(seeds_from(set(" 3 "), set("10")), Ok(10..13));
    }

    #[test]
    fn a_count_that_is_not_a_number_is_refused_with_its_name() {
        let Err(message) = seeds_from(set("many"), unset()) else {
            panic!("a bad count was accepted");
        };
        assert!(
            message.contains(SEEDS_VAR) && message.contains("many"),
            "{message}"
        );
    }

    #[test]
    fn no_seeds_and_seeds_past_the_largest_are_refused() {
        let Err(none) = seeds_from(set("0"), unset()) else {
            panic!("zero seeds were accepted");
        };
        assert!(none.contains("at least 1"), "{none}");
        let Err(past) = seeds_from(set("2"), set(&u64::MAX.to_string())) else {
            panic!("a range past u64::MAX was accepted");
        };
        assert!(past.contains("largest seed"), "{past}");
        assert_eq!(
            seeds_from(set("1"), set(&(u64::MAX - 1).to_string())),
            Ok(u64::MAX - 1..u64::MAX)
        );
    }

    #[test]
    fn a_regression_list_has_seeds_comments_and_blank_lines() {
        let text = "# header\n\n17  # found by I9\n  42\n# end\n";
        assert_eq!(parse_regressions(text), Ok(vec![17, 42]));
        assert_eq!(parse_regressions(""), Ok(Vec::new()));
    }

    #[test]
    fn a_bad_or_repeated_regression_line_is_refused_with_its_line_number() {
        let Err(bad) = parse_regressions("1\nseven\n") else {
            panic!("a bad line was accepted");
        };
        assert!(bad.starts_with("line 2:"), "{bad}");
        let Err(twice) = parse_regressions("5\n\n5  # again\n") else {
            panic!("a repeated seed was accepted");
        };
        assert!(twice.starts_with("line 3:"), "{twice}");
    }

    #[test]
    fn a_failing_seed_says_how_to_rerun_and_keep_it() {
        let keep = keep_hint(9);
        assert!(keep.contains("9  # what it found") && keep.contains("simulator-regressions.txt"));
        assert!(
            rerun_hint(9).ends_with(SWEEP_TEST),
            "the rerun runs only the sweep"
        );
    }

    #[test]
    fn a_report_lists_twenty_failures_counts_the_rest_and_skips_kept_seeds() {
        let failures: Vec<(u64, String)> = (0..25)
            .map(|seed| (seed, format!("seed {seed} broke")))
            .collect();
        let report = failure_report(&failures, &[3]);
        assert!(report.starts_with("25 seeds failed:"), "{report}");
        assert!(report.contains("seed 19 broke") && !report.contains("seed 20 broke"));
        assert!(report.ends_with("and 5 more failing seeds"), "{report}");
        assert_eq!(
            report.matches("keep it:").count(),
            19,
            "seed 3 is already kept"
        );
        let few = failure_report(&failures[..2], &[]);
        assert!(!few.contains("more failing seeds"));
    }

    #[test]
    fn the_rerun_hint_names_the_seed_and_one_seed_only() {
        let hint = rerun_hint(41);
        assert!(hint.contains("TESSEL_SIM_FIRST_SEED=41"), "{hint}");
        assert!(hint.contains("TESSEL_SIM_SEEDS=1"), "{hint}");
    }

    /// What the default seeds did to the merge path, summed.
    fn default_seeds_totals() -> (lander::FaultCounts, usize, usize, usize, usize) {
        let mut faults = lander::FaultCounts::new();
        let (mut merged, mut landings, mut stale, mut trials) = (0, 0, 0, 0);
        for seed in 0..DEFAULT_SEEDS {
            let run = run_seed(seed, Variant::Correct, &[]);
            for (fault, count) in run.faults {
                *faults.entry(fault).or_insert(0) += count;
            }
            merged += run.merged;
            landings += run.landings;
            stale += run.stale_answers;
            trials += run.trials;
        }
        (faults, merged, landings, stale, trials)
    }

    #[test]
    fn the_default_seeds_make_the_merge_path_delay_repeat_drop_and_refuse() {
        let (faults, merged, landings, stale, trials) = default_seeds_totals();
        let mut missing = Vec::new();
        for fault in [
            "delayed",
            "reply_delayed",
            "duplicated",
            "never_runs",
            "lost_reply",
            "pushed_then_lost_reply",
            "unknown",
            "rate_limited",
            "compare_and_swap_refused",
            "pushed_to_main",
            "cut_off_always",
            "candidate_lost",
            "flipped_on_rerun",
        ] {
            if faults.get(fault).copied().unwrap_or(0) == 0 {
                missing.push(fault);
            }
        }
        assert!(
            missing.is_empty(),
            "no default seed injected {missing:?}: {faults:?}"
        );
        assert!(merged > 0 && landings > 0, "nothing landed");
        assert!(stale > 0, "no answer came for a dispatch not in flight");
        assert!(trials > 0, "no verification was dispatched");
    }

    #[test]
    fn a_seed_with_merge_faults_reproduces_and_a_different_seed_does_not() {
        let a = run_seed(3, Variant::Correct, &[]);
        let again = run_seed(3, Variant::Correct, &[]);
        let other = run_seed(4, Variant::Correct, &[]);
        assert_eq!(first_difference(&a.transcript, &again.transcript), None);
        assert!(first_difference(&a.transcript, &other.transcript).is_some());
    }

    #[test]
    fn every_seed_that_is_expected_to_fail_still_fails_its_checker_and_no_other() {
        for (seed, checker, why) in EXPECTED_TO_FAIL {
            let run = run_seed(*seed, Variant::Correct, &Checker::ALL);
            assert!(
                !run.violations.is_empty() && run.violations.iter().all(|v| v.checker == *checker),
                "seed {seed} ({why}): {:?}",
                run.violations
            );
        }
    }

    #[test]
    fn two_transcripts_that_differ_say_where() {
        let a = vec!["x".to_string(), "y".to_string()];
        let b = vec!["x".to_string(), "z".to_string()];
        assert!(first_difference(&a, &b).is_some_and(|d| d.starts_with("line 1")));
        assert!(first_difference(&a, &a[..1]).is_some());
        assert!(first_difference(&a, &a).is_none());
    }
}

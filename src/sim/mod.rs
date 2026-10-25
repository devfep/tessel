//! The deterministic simulator. A seed fixes the clock, the client traffic and the faults; the
//! real `Host` runs against a simulated store; checkers read what clients received and what the
//! store holds. Nothing here reads a wall clock or the OS (the clippy ban in `clippy.toml` holds
//! it), so a seed reproduces on any machine, and every seed runs twice with the transcripts
//! compared.
//!
//! Checkers I1, I2, I4, I5, I8, I9 and I10 prove the invariants of `docs/proposals/phase-1.md` that
//! the call path and the wake carry; the merge path's (I3, I6, I7, I11, I12) come with its
//! simulation.

pub mod check;
pub mod clock;
pub mod driver;
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

/// How to rerun `seed` alone.
pub fn rerun_hint(seed: u64) -> String {
    format!(
        "rerun with: {FIRST_SEED_VAR}={seed} {SEEDS_VAR}=1 cargo test -p tessel-coordinator \
         --test simulator"
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
    }
}

/// Run `seed` against the real host with every checker, then again with none, and compare the two
/// transcripts. The error names what failed and how to rerun the seed alone and keep it.
pub fn check_seed(seed: u64) -> Result<Run, String> {
    let first = run_seed(seed, Variant::Correct, &Checker::ALL);
    let second = run_seed(seed, Variant::Correct, &[]);
    let hint = format!("{}\n  {}", rerun_hint(seed), keep_hint(seed));
    if !first.violations.is_empty() {
        let found: Vec<String> = first
            .violations
            .iter()
            .map(|v| format!("{:?}: {}", v.checker, v.detail))
            .collect();
        return Err(format!("seed {seed} broke {}\n  {hint}", found.join("; ")));
    }
    if let Some(difference) = first_difference(&first.transcript, &second.transcript) {
        return Err(format!(
            "seed {seed} is not deterministic: {difference}\n  {hint}"
        ));
    }
    if first.steps != STEPS {
        return Err(format!(
            "seed {seed} stopped after {} steps\n  {hint}",
            first.steps
        ));
    }
    Ok(first)
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
    }

    #[test]
    fn the_rerun_hint_names_the_seed_and_one_seed_only() {
        let hint = rerun_hint(41);
        assert!(hint.contains("TESSEL_SIM_FIRST_SEED=41"), "{hint}");
        assert!(hint.contains("TESSEL_SIM_SEEDS=1"), "{hint}");
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

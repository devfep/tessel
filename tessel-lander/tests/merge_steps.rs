//! Port of `tessel-steward/src/merge-steps.test.ts` (`runMerge`): 44 `it` and 3 `it.each` (9
//! rows), 53 tests, each row of an `it.each` its own test. A fake git runner and fake ports stand
//! in for the sandbox and the platform; the real git is in `merge_git.rs`. The title of each test
//! is the TS title in snake case, so its original is recognisable.
//!
//! The last section holds tests of the two-call split (`prepare`, `land`) and the error paths that
//! the TS version expressed as thrown errors.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::Path;

use tessel_coordinator::protocol::{Mode, Scope, ScopeClaim};
use tessel_lander::git::{Git, GitCommand, MergeSources, LOCAL_GIT_COMMANDS_MAX, LOCAL_TIMEOUT};
use tessel_lander::merge::{
    land, prepare, run_merge, BoundaryError, GateReview, MergeContext, MergeError, MergePolicy,
    MergePorts, MergeRequest, Prepared, PushUpdate,
};
use tessel_lander::outcome::{
    EvidenceResult, GateStep, LandedMerge, MergeOutcome, PackageStep, StepOutcome,
};
use tessel_lander::run::{GitOutput, GitRunner, RunError};
use tessel_lander::types::Sha;
use tessel_lander::workspace::CloneError;

fn sha(character: char) -> Sha {
    Sha::parse(&character.to_string().repeat(40)).expect("test sha")
}

fn base() -> Sha {
    sha('1')
}
fn merge_base() -> Sha {
    sha('2')
}
fn commit() -> Sha {
    sha('3')
}
fn head() -> Sha {
    sha('4')
}
fn racer() -> Sha {
    sha('5')
}

fn recorded() -> EvidenceResult {
    EvidenceResult::Written {
        repo: "demo--evidence".to_owned(),
        path: format!("merges/{}.md", head()),
        head: sha('e'),
    }
}

fn scope(scope: Scope, mode: Mode) -> ScopeClaim {
    ScopeClaim { scope, mode }
}

fn dir(path: &str, mode: Mode) -> ScopeClaim {
    scope(
        Scope::Dir {
            path: path.to_owned(),
        },
        mode,
    )
}

fn file(path: &str, mode: Mode) -> ScopeClaim {
    scope(
        Scope::File {
            path: path.to_owned(),
        },
        mode,
    )
}

fn whole_repo() -> Vec<ScopeClaim> {
    vec![dir("", Mode::EditSignature), dir("", Mode::Create)]
}

const VALID_GATE: &str = "instance = \"standard-4\"\n[[test]]\nargv = [\"pnpm\", \"test\"]\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GitStep {
    Clone,
    Fetch,
    Base,
    Exists,
    Reachable,
    MergeBase,
    Rebase,
    Conflicts,
    Changed,
    Head,
    Show,
    Push,
}

impl GitStep {
    fn label(self) -> &'static str {
        match self {
            GitStep::Clone => "clone",
            GitStep::Fetch => "fetch",
            GitStep::Base => "base",
            GitStep::Exists => "exists",
            GitStep::Reachable => "reachable",
            GitStep::MergeBase => "mergebase",
            GitStep::Rebase => "rebase",
            GitStep::Conflicts => "conflicts",
            GitStep::Changed => "changed",
            GitStep::Head => "head",
            GitStep::Show => "show",
            GitStep::Push => "push",
        }
    }
}

/// Which step a command is, by its argv text, in the order the steward's test matches them.
fn git_step(command: &GitCommand) -> GitStep {
    let words: Vec<String> = command
        .argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let text = words.join(" ");
    let matches = [
        (" clone ", GitStep::Clone),
        (" fetch ", GitStep::Fetch),
        ("origin/main^{commit}", GitStep::Base),
        ("--quiet", GitStep::Exists),
        ("--is-ancestor", GitStep::Reachable),
        (" merge-base ", GitStep::MergeBase),
        (" rebase ", GitStep::Rebase),
        ("--diff-filter=U", GitStep::Conflicts),
        ("--name-status", GitStep::Changed),
        ("HEAD^{commit}", GitStep::Head),
        (" show ", GitStep::Show),
        (" push ", GitStep::Push),
    ];
    matches
        .into_iter()
        .find(|(needle, _)| text.contains(needle))
        .map_or_else(
            || unreachable!("unexpected command {text}"),
            |(_, step)| step,
        )
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    /// The command could not be started.
    ExecFailed,
    /// The output was longer than the runner keeps: the analogue of a cut-off result.
    TooLarge,
}

#[derive(Debug, Clone)]
struct Reply {
    exit: i32,
    stdout: String,
    fault: Option<Fault>,
}

fn exit(exit: i32) -> Reply {
    out(exit, "")
}

fn out(exit: i32, stdout: &str) -> Reply {
    Reply {
        exit,
        stdout: stdout.to_owned(),
        fault: None,
    }
}

fn fault(fault: Fault) -> Reply {
    Reply {
        exit: 0,
        stdout: String::new(),
        fault: Some(fault),
    }
}

fn default_reply(step: GitStep) -> Reply {
    match step {
        GitStep::Base => out(0, &format!("{}\n", base())),
        GitStep::Exists => out(0, &format!("{}\n", commit())),
        GitStep::MergeBase => out(0, &format!("{}\n", merge_base())),
        GitStep::Head => out(0, &format!("{}\n", head())),
        GitStep::Changed => out(0, "M\0src/a.ts\0"),
        GitStep::Show => out(0, VALID_GATE),
        GitStep::Clone
        | GitStep::Fetch
        | GitStep::Reachable
        | GitStep::Rebase
        | GitStep::Conflicts
        | GitStep::Push => out(0, ""),
    }
}

#[derive(Clone)]
struct Plan {
    git: HashMap<GitStep, Reply>,
    install: i32,
    test: i32,
    test_wall_ms: Option<u64>,
    revoked_reads: bool,
    main_after_push: Option<Sha>,
    record: EvidenceResult,
    pinned_main: Sha,
}

impl Plan {
    fn new() -> Plan {
        Plan {
            git: HashMap::new(),
            install: 0,
            test: 0,
            test_wall_ms: None,
            revoked_reads: true,
            main_after_push: Some(head()),
            record: recorded(),
            pinned_main: base(),
        }
    }

    fn git(mut self, step: GitStep, reply: Reply) -> Plan {
        self.git.insert(step, reply);
        self
    }

    fn changed(self, stdout: &str) -> Plan {
        self.git(GitStep::Changed, out(0, stdout))
    }
}

/// The fake runner and the fake ports in one value, so they share one event log.
struct Harness {
    plan: Plan,
    events: RefCell<Vec<String>>,
    commands: RefCell<Vec<GitCommand>>,
    landings: RefCell<Vec<LandedMerge>>,
}

impl Harness {
    fn new(plan: Plan) -> Harness {
        Harness {
            plan,
            events: RefCell::new(Vec::new()),
            commands: RefCell::new(Vec::new()),
            landings: RefCell::new(Vec::new()),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events.borrow().clone()
    }

    fn has_event(&self, event: &str) -> bool {
        self.events.borrow().iter().any(|e| e == event)
    }

    fn ran_package_step(&self) -> bool {
        self.events
            .borrow()
            .iter()
            .any(|e| e.starts_with("package:"))
    }

    fn minted_write_access(&self) -> bool {
        self.events
            .borrow()
            .iter()
            .any(|e| e.starts_with("mint-write"))
    }

    fn index_of(&self, event: &str) -> usize {
        self.events
            .borrow()
            .iter()
            .position(|e| e == event)
            .unwrap_or_else(|| unreachable!("no event {event}"))
    }

    fn command(&self, step: GitStep) -> GitCommand {
        self.commands
            .borrow()
            .iter()
            .find(|c| git_step(c) == step)
            .cloned()
            .unwrap_or_else(|| unreachable!("no {} command ran", step.label()))
    }

    fn argv(&self, step: GitStep) -> Vec<String> {
        self.command(step)
            .argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }
}

impl GitRunner for Harness {
    fn run(&self, command: &GitCommand) -> Result<GitOutput, RunError> {
        let step = git_step(command);
        self.events
            .borrow_mut()
            .push(format!("git:{}", step.label()));
        self.commands.borrow_mut().push(command.clone());
        let reply = self
            .plan
            .git
            .get(&step)
            .cloned()
            .unwrap_or_else(|| default_reply(step));
        match reply.fault {
            Some(Fault::ExecFailed) => Err(RunError::Spawn {
                command: step.label().to_owned(),
                source: io::Error::other("exec failed"),
            }),
            Some(Fault::TooLarge) => Err(RunError::OutputTooLarge {
                command: step.label().to_owned(),
                stream: "stdout",
                limit: 16,
            }),
            None => Ok(GitOutput {
                exit_code: Some(reply.exit),
                stdout: reply.stdout,
                stderr: format!("{} err", step.label()),
            }),
        }
    }
}

impl MergePorts for Harness {
    fn revoke_read_access(&self) -> bool {
        self.events.borrow_mut().push("revoke-reads".to_owned());
        self.plan.revoked_reads
    }

    fn run_package_step(&self, step: PackageStep) -> Result<StepOutcome, BoundaryError> {
        let (label, exit_code) = match step {
            PackageStep::Install => ("install", self.plan.install),
            PackageStep::Test => ("test", self.plan.test),
        };
        self.events.borrow_mut().push(format!("package:{label}"));
        let mut outcome = StepOutcome::finished(step, exit_code, &format!("{label} out"), "");
        if step == PackageStep::Test {
            outcome.wall_ms = self.plan.test_wall_ms;
        }
        Ok(outcome)
    }

    fn with_push_access<T>(
        &self,
        update: &PushUpdate,
        use_access: impl FnOnce() -> T,
    ) -> Result<T, BoundaryError> {
        self.events
            .borrow_mut()
            .push(format!("mint-write {} {}", update.base, update.head));
        let result = use_access();
        self.events.borrow_mut().push("revoke-write".to_owned());
        Ok(result)
    }

    fn current_main(&self) -> Option<Sha> {
        self.events.borrow_mut().push("currentMain".to_owned());
        self.plan.main_after_push.clone()
    }

    fn record_merge(&self, landed: &LandedMerge) -> EvidenceResult {
        self.events.borrow_mut().push("record-merge".to_owned());
        self.landings.borrow_mut().push(landed.clone());
        self.plan.record.clone()
    }
}

const SOURCES_MAIN: &str = "https://git.example/demo.git";

fn sources() -> MergeSources {
    MergeSources {
        main_remote: SOURCES_MAIN.to_owned(),
        fork_remote: "https://git.example/demo--a1.git".to_owned(),
        fork_branch: "main".to_owned(),
    }
}

fn git() -> Git {
    Git::new("/lander/home".into(), "/lander/hooks".into())
}

fn context<'a>(
    harness: &'a Harness,
    git: &'a Git,
    sources: &'a MergeSources,
    commit_pin: &'a Sha,
) -> MergeContext<'a> {
    MergeContext {
        git,
        runner: harness,
        sources,
        clone_dir: Path::new("/workspace"),
        pinned_main: commit_pin,
    }
}

fn merge_with(
    harness: &Harness,
    scopes: &[ScopeClaim],
    policy: MergePolicy,
) -> Result<MergeOutcome, MergeError> {
    let (git, sources, pin, commit) =
        (git(), sources(), harness.plan.pinned_main.clone(), commit());
    let ctx = context(harness, &git, &sources, &pin);
    let request = MergeRequest {
        commit: &commit,
        scopes,
        policy,
    };
    run_merge(&ctx, harness, &request)
}

fn merge(harness: &Harness, scopes: &[ScopeClaim]) -> MergeOutcome {
    merge_with(harness, scopes, MergePolicy::Submission).expect("the merge finishes")
}

fn admin(harness: &Harness, scopes: &[ScopeClaim]) -> MergeOutcome {
    merge_with(harness, scopes, MergePolicy::Admin).expect("the merge finishes")
}

fn merged() -> MergeOutcome {
    MergeOutcome::Merged {
        base: base(),
        head: head(),
        evidence: Some(recorded()),
    }
}

fn step_outcome(step: PackageStep, exit_code: i32, timed_out: bool) -> StepOutcome {
    let label = match step {
        PackageStep::Install => "install",
        PackageStep::Test => "test",
    };
    StepOutcome {
        timed_out,
        ..StepOutcome::finished(step, exit_code, &format!("{label} out"), "")
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

// ---- runMerge ----

#[test]
fn fetches_revokes_the_reads_verifies_rebases_tests_then_mints_and_revokes_the_write_token() {
    let harness = Harness::new(Plan::new());
    assert_eq!(merge(&harness, &whole_repo()), merged());
    assert_eq!(
        harness.events(),
        strings(&[
            "git:clone",
            "git:fetch",
            "revoke-reads",
            "git:base",
            "git:exists",
            "git:reachable",
            "git:mergebase",
            "git:rebase",
            "git:head",
            "git:changed",
            "package:install",
            "package:test",
            &format!("mint-write {} {}", base(), head()),
            "git:push",
            "revoke-write",
            "currentMain",
            "record-merge",
        ])
    );
}

#[test]
fn pushes_the_rebased_head_with_a_lease_on_the_main_that_was_cloned() {
    let harness = Harness::new(Plan::new());
    merge(&harness, &whole_repo());
    let argv = harness.argv(GitStep::Push);
    assert!(argv.contains(&format!("--force-with-lease=refs/heads/main:{}", base())));
    assert!(argv.contains(&format!("{}:refs/heads/main", head())));
}

#[test]
fn rebases_the_submitted_commit_not_the_fork_tip_onto_the_cloned_main() {
    let harness = Harness::new(Plan::new());
    merge(&harness, &whole_repo());
    let argv = harness.argv(GitStep::Rebase);
    assert_eq!(
        argv[argv.len() - 4..],
        [
            "--onto".to_owned(),
            base().to_string(),
            merge_base().to_string(),
            commit().to_string()
        ]
    );
}

#[test]
fn writes_the_record_only_after_main_reads_merged_after_the_write_token_and_gate() {
    let harness = Harness::new(Plan::new());
    merge(&harness, &whole_repo());
    let record = harness.index_of("record-merge");
    assert_eq!(record, harness.events().len() - 1);
    assert!(record > harness.index_of("currentMain"));
    assert!(record > harness.index_of("revoke-write"));
    assert!(record > harness.index_of("package:test"));
}

#[test]
fn writes_no_record_for_an_outcome_that_is_not_merged() {
    let plans = [
        Plan {
            test: 1,
            ..Plan::new()
        },
        Plan {
            install: 1,
            ..Plan::new()
        },
        Plan {
            test: 124,
            ..Plan::new()
        },
        Plan {
            main_after_push: Some(base()),
            ..Plan::new()
        },
        Plan {
            main_after_push: Some(racer()),
            ..Plan::new()
        },
        Plan {
            main_after_push: None,
            ..Plan::new()
        },
        Plan::new().changed("M\0src/a.ts\0A\0docs/x.md\0"),
    ];
    for plan in plans {
        let harness = Harness::new(plan);
        let outcome = merge(&harness, &[dir("src", Mode::EditSignature)]);
        assert!(
            !matches!(outcome, MergeOutcome::Merged { .. }),
            "{outcome:?}"
        );
        assert!(!harness.has_event("record-merge"));
    }
}

#[test]
fn records_the_gates_steps_as_they_ran_with_the_time_the_lander_measured() {
    let harness = Harness::new(Plan {
        test_wall_ms: Some(12_340),
        ..Plan::new()
    });
    merge(&harness, &whole_repo());
    assert_eq!(
        *harness.landings.borrow(),
        [LandedMerge {
            base: base(),
            head: head(),
            gate: Some(vec![
                GateStep {
                    step: PackageStep::Install,
                    exit_code: 0,
                    wall_ms: None,
                    timed_out: false
                },
                GateStep {
                    step: PackageStep::Test,
                    exit_code: 0,
                    wall_ms: Some(12_340),
                    timed_out: false
                },
            ]),
        }]
    );
}

#[test]
fn keeps_the_outcome_merged_when_the_record_was_not_written_and_carries_why() {
    let failed = EvidenceResult::NotWritten {
        reason: "push exit 1".to_owned(),
    };
    let harness = Harness::new(Plan {
        record: failed.clone(),
        ..Plan::new()
    });
    assert_eq!(
        merge(&harness, &whole_repo()),
        MergeOutcome::Merged {
            base: base(),
            head: head(),
            evidence: Some(failed)
        }
    );
}

#[test]
fn never_mints_a_write_token_when_the_tests_fail() {
    let harness = Harness::new(Plan {
        test: 1,
        ..Plan::new()
    });
    let outcome = merge(&harness, &whole_repo());
    assert!(matches!(
        outcome,
        MergeOutcome::TestsFailed { ref base, ref head, .. } if *base == self::base() && *head == self::head()
    ));
    assert!(!harness.minted_write_access());
    assert!(!harness.has_event("git:push"));
}

#[test]
fn returns_the_capped_test_output_with_a_failed_test() {
    let harness = Harness::new(Plan {
        test: 1,
        ..Plan::new()
    });
    assert_eq!(
        merge(&harness, &whole_repo()),
        MergeOutcome::TestsFailed {
            base: base(),
            head: head(),
            result: step_outcome(PackageStep::Test, 1, false),
        }
    );
}

#[test]
fn reports_a_test_step_that_timed_out_as_a_timeout_that_is_still_the_test_step() {
    for test in [124, 137] {
        let harness = Harness::new(Plan {
            test,
            ..Plan::new()
        });
        assert_eq!(
            merge(&harness, &whole_repo()),
            MergeOutcome::Timeout {
                base: base(),
                head: head(),
                result: step_outcome(PackageStep::Test, test, true),
            }
        );
        assert!(!harness.minted_write_access());
    }
}

#[test]
fn reports_an_install_step_that_timed_out_as_a_timeout_with_reason_timeout_running_no_tests() {
    for install in [124, 137] {
        let harness = Harness::new(Plan {
            install,
            ..Plan::new()
        });
        assert_eq!(
            merge(&harness, &whole_repo()),
            MergeOutcome::Timeout {
                base: base(),
                head: head(),
                result: step_outcome(PackageStep::Install, install, true),
            }
        );
        assert!(!harness.has_event("package:test"));
        assert!(!harness.minted_write_access());
    }
}

#[test]
fn rejects_a_change_to_tessel_toml_as_gate_changed_before_the_coverage_check_and_any_repo_code() {
    let records = [
        "M\0tessel.toml\0",
        "A\0src/b.ts\0M\0tessel.toml\0",
        "D\0tessel.toml\0",
        "R100\0tessel.toml\0gate.toml\0",
        "R100\0old.toml\0tessel.toml\0",
    ];
    for stdout in records {
        let harness = Harness::new(Plan::new().changed(stdout));
        assert_eq!(
            merge(&harness, &whole_repo()),
            MergeOutcome::GateChanged {
                base: base(),
                head: head()
            },
            "{stdout:?}"
        );
        assert!(!harness.ran_package_step());
        assert!(!harness.minted_write_access());
    }
}

#[test]
fn rejects_a_change_to_tessel_toml_over_the_submission_route_even_when_the_heads_file_is_valid() {
    let harness = Harness::new(Plan::new().changed("M\0tessel.toml\0"));
    assert_eq!(
        merge_with(&harness, &whole_repo(), MergePolicy::Submission).expect("finishes"),
        MergeOutcome::GateChanged {
            base: base(),
            head: head()
        }
    );
}

// ---- an admin merge that changes tessel.toml ----

const GATE_AND_CODE: &str = "M\0tessel.toml\0M\0src/a.ts\0";

#[test]
fn admin_refuses_a_head_file_that_is_invalid_oversized_or_deleted_running_nothing_of_the_repos() {
    let oversized = format!("{VALID_GATE}# {}\n", "x".repeat(4096));
    let heads = [
        out(0, "instance = \"standard-4\"\nshell = \"sh\"\n"),
        out(0, "not toml ["),
        out(0, ""),
        out(0, &VALID_GATE.replace("pnpm", "sh")),
        out(0, &oversized),
        out(128, ""),
    ];
    for show in heads {
        let plan = Plan::new()
            .changed(GATE_AND_CODE)
            .git(GitStep::Show, show.clone());
        let harness = Harness::new(plan);
        assert_eq!(
            admin(&harness, &whole_repo()),
            MergeOutcome::GateInvalid {
                base: base(),
                head: head()
            },
            "{:?}",
            show.stdout.len()
        );
        assert!(!harness.ran_package_step());
        assert!(!harness.minted_write_access());
    }
}

#[test]
fn admin_reads_the_heads_file_from_the_rebased_commit() {
    let harness = Harness::new(Plan::new().changed(GATE_AND_CODE));
    admin(&harness, &whole_repo());
    assert!(harness
        .argv(GitStep::Show)
        .contains(&format!("{}:tessel.toml", head())));
}

#[test]
fn admin_still_runs_the_trunks_tests_when_the_diff_has_code_besides_the_gate() {
    let harness = Harness::new(Plan::new().changed(GATE_AND_CODE));
    assert!(matches!(
        admin(&harness, &whole_repo()),
        MergeOutcome::Merged { .. }
    ));
    assert!(harness.has_event("package:test"));
}

#[test]
fn admin_judges_a_diff_of_only_tessel_toml_by_the_heads_file_alone_no_install_no_tests_then_the_push(
) {
    let harness = Harness::new(Plan {
        install: 3,
        test: 1,
        ..Plan::new().changed("M\0tessel.toml\0")
    });
    assert!(matches!(
        admin(&harness, &whole_repo()),
        MergeOutcome::Merged { .. }
    ));
    assert!(!harness.ran_package_step());
    assert!(harness.minted_write_access());
}

#[test]
fn admin_records_a_merge_of_only_tessel_toml_with_no_gate_steps() {
    let harness = Harness::new(Plan::new().changed("M\0tessel.toml\0"));
    admin(&harness, &whole_repo());
    assert_eq!(
        *harness.landings.borrow(),
        [LandedMerge {
            base: base(),
            head: head(),
            gate: None
        }]
    );
}

#[test]
fn admin_still_enforces_the_claims_coverage_on_a_diff_of_only_tessel_toml() {
    let harness = Harness::new(Plan::new().changed("M\0tessel.toml\0"));
    assert_eq!(
        admin(&harness, &[dir("src", Mode::EditBody)]),
        MergeOutcome::Uncovered {
            base: base(),
            head: head(),
            files: strings(&["tessel.toml"]),
            total: 1
        }
    );
    assert!(!harness.minted_write_access());
}

#[test]
fn admin_still_enforces_the_claims_coverage() {
    let harness = Harness::new(Plan::new().changed("M\0tessel.toml\0M\0src/a.ts\0"));
    let outcome = admin(&harness, &[file("tessel.toml", Mode::EditBody)]);
    assert!(
        matches!(outcome, MergeOutcome::Uncovered { .. }),
        "{outcome:?}"
    );
}

#[test]
fn does_not_mistake_other_files_named_like_the_gate_for_it() {
    let harness =
        Harness::new(Plan::new().changed("M\0docs/tessel.toml\0M\0tessel.toml.bak\0M\0src/a.ts\0"));
    assert!(matches!(
        merge(&harness, &whole_repo()),
        MergeOutcome::Merged { .. }
    ));
}

#[test]
fn issues_at_most_local_git_commands_max_local_git_commands_on_its_longest_path() {
    let plans = [
        Plan::new(),
        Plan::new()
            .git(GitStep::Rebase, exit(1))
            .git(GitStep::Conflicts, out(0, "a\0")),
        Plan::new().changed(GATE_AND_CODE),
    ];
    for plan in plans {
        let harness = Harness::new(plan);
        merge_with(&harness, &whole_repo(), MergePolicy::Admin).expect("finishes");
        let local = harness
            .commands
            .borrow()
            .iter()
            .filter(|c| c.timeout == LOCAL_TIMEOUT)
            .count();
        assert!(local > 0);
        assert!(local <= LOCAL_GIT_COMMANDS_MAX, "{local}");
    }
}

#[test]
fn is_main_moved_before_verifying_or_rebasing_when_the_clone_finds_main_elsewhere() {
    let pinned = sha('9');
    let harness = Harness::new(Plan {
        pinned_main: pinned.clone(),
        ..Plan::new()
    });
    assert_eq!(
        merge(&harness, &whole_repo()),
        MergeOutcome::MainMoved {
            expected: pinned,
            actual: base()
        }
    );
    assert!(!harness.has_event("git:exists"));
    assert!(!harness.ran_package_step());
}

#[test]
fn reports_a_stopped_rebase_as_a_conflict_with_the_unmerged_paths_and_runs_no_tests() {
    let harness = Harness::new(
        Plan::new()
            .git(GitStep::Rebase, exit(1))
            .git(GitStep::Conflicts, out(0, "src/a.ts\0docs/b md\0")),
    );
    assert_eq!(
        merge(&harness, &whole_repo()),
        MergeOutcome::Conflict {
            base: base(),
            files: strings(&["src/a.ts", "docs/b md"])
        }
    );
    assert!(!harness.ran_package_step());
}

#[test]
fn does_not_call_a_failed_rebase_a_conflict_when_no_file_is_unmerged() {
    let harness = Harness::new(
        Plan::new()
            .git(GitStep::Rebase, exit(128))
            .git(GitStep::Conflicts, exit(0)),
    );
    let outcome = merge(&harness, &whole_repo());
    let MergeOutcome::GitFailed { result } = outcome else {
        unreachable!("{outcome:?}")
    };
    assert_eq!(result.exit_code, Some(128));
}

fn refuses_when(git_step: GitStep) {
    let harness = Harness::new(Plan::new().git(git_step, exit(1)));
    assert_eq!(
        merge(&harness, &whole_repo()),
        MergeOutcome::CommitNotInFork
    );
    assert!(!harness.has_event("git:rebase"));
    assert!(!harness.ran_package_step());
}

#[test]
fn refuses_when_the_commit_object_is_missing() {
    refuses_when(GitStep::Exists);
}

#[test]
fn refuses_when_the_commit_is_not_an_ancestor_of_the_forks_default_branch() {
    refuses_when(GitStep::Reachable);
}

#[test]
fn calls_an_unexpected_git_failure_while_verifying_infrastructure_not_a_missing_commit() {
    for step in [GitStep::Reachable, GitStep::Exists] {
        let harness = Harness::new(Plan::new().git(step, exit(128)));
        let outcome = merge(&harness, &whole_repo());
        assert!(
            matches!(outcome, MergeOutcome::GitFailed { .. }),
            "{outcome:?}"
        );
    }
}

#[test]
fn reports_a_failed_clone_or_fetch_as_a_clone_outcome_and_verifies_nothing() {
    for failing in [GitStep::Clone, GitStep::Fetch] {
        let harness = Harness::new(Plan::new().git(failing, exit(128)));
        let outcome = merge(&harness, &whole_repo());
        let MergeOutcome::Clone { result } = outcome else {
            unreachable!("{outcome:?}")
        };
        assert_eq!(result.exit_code, Some(128));
        assert!(harness.has_event("revoke-reads"));
        assert!(!harness.has_event("git:base"));
    }
}

#[test]
fn runs_nothing_after_a_failed_revocation_of_the_read_tokens() {
    let harness = Harness::new(Plan {
        revoked_reads: false,
        ..Plan::new()
    });
    let error = merge_with(&harness, &whole_repo(), MergePolicy::Submission)
        .expect_err("the revocation failed");
    assert!(matches!(error, MergeError::ReadAccessNotRevoked), "{error}");
    assert_eq!(
        harness.events(),
        strings(&["git:clone", "git:fetch", "revoke-reads"])
    );
}

#[test]
fn reports_a_repo_with_dependencies_as_an_install_outcome_and_never_pushes() {
    const DEPENDENCIES_DECLARED_EXIT_CODE: i32 = 3;
    let harness = Harness::new(Plan {
        install: DEPENDENCIES_DECLARED_EXIT_CODE,
        ..Plan::new()
    });
    assert_eq!(
        merge(&harness, &whole_repo()),
        MergeOutcome::Install {
            base: base(),
            head: head(),
            result: step_outcome(PackageStep::Install, DEPENDENCIES_DECLARED_EXIT_CODE, false),
        }
    );
    assert!(!harness.has_event("package:test"));
    assert!(!harness.minted_write_access());
}

#[test]
fn reports_a_commit_main_already_contains_as_already_merged_without_testing_or_pushing() {
    let harness = Harness::new(Plan::new().git(GitStep::Head, out(0, &format!("{}\n", base()))));
    assert_eq!(
        merge(&harness, &whole_repo()),
        MergeOutcome::AlreadyMerged { base: base() }
    );
    assert!(!harness.ran_package_step());
    assert!(!harness.minted_write_access());
}

#[test]
fn trusts_the_read_of_main_not_the_exit_code_exit_0_with_main_unchanged_is_push_failed() {
    let harness = Harness::new(Plan {
        main_after_push: Some(base()),
        ..Plan::new().git(GitStep::Push, exit(0))
    });
    let outcome = merge(&harness, &whole_repo());
    assert!(
        matches!(outcome, MergeOutcome::PushFailed { ref base, .. } if *base == self::base()),
        "{outcome:?}"
    );
}

#[test]
fn reports_merged_when_the_push_exits_non_zero_but_main_is_at_head() {
    let harness = Harness::new(Plan::new().git(GitStep::Push, exit(1)));
    assert_eq!(merge(&harness, &whole_repo()), merged());
}

#[test]
fn reports_main_moved_when_the_push_exits_0_but_main_is_at_neither_base_nor_head() {
    let harness = Harness::new(Plan {
        main_after_push: Some(racer()),
        ..Plan::new().git(GitStep::Push, exit(0))
    });
    assert_eq!(
        merge(&harness, &whole_repo()),
        MergeOutcome::MainMoved {
            expected: base(),
            actual: racer()
        }
    );
}

#[test]
fn does_not_report_merged_when_the_push_exits_0_and_main_cannot_be_read() {
    let harness = Harness::new(Plan {
        main_after_push: None,
        ..Plan::new().git(GitStep::Push, exit(0))
    });
    let outcome = merge(&harness, &whole_repo());
    assert!(
        matches!(outcome, MergeOutcome::PushFailed { .. }),
        "{outcome:?}"
    );
}

#[test]
fn reports_main_moved_with_both_shas_when_the_lease_rejects_the_push() {
    let harness = Harness::new(Plan {
        main_after_push: Some(racer()),
        ..Plan::new().git(GitStep::Push, exit(1))
    });
    assert_eq!(
        merge(&harness, &whole_repo()),
        MergeOutcome::MainMoved {
            expected: base(),
            actual: racer()
        }
    );
}

#[test]
fn reports_push_failed_not_main_moved_when_the_push_fails_and_main_is_unchanged() {
    let harness = Harness::new(Plan {
        main_after_push: Some(base()),
        ..Plan::new().git(GitStep::Push, exit(128))
    });
    let outcome = merge(&harness, &whole_repo());
    let MergeOutcome::PushFailed {
        base: failed_base,
        head: failed_head,
        result,
    } = outcome
    else {
        unreachable!("{outcome:?}")
    };
    assert_eq!((failed_base, failed_head), (base(), head()));
    assert_eq!(result.exit_code, Some(128));
}

#[test]
fn reports_push_failed_when_the_push_fails_and_main_cannot_be_read() {
    let harness = Harness::new(Plan {
        main_after_push: None,
        ..Plan::new().git(GitStep::Push, exit(128))
    });
    let outcome = merge(&harness, &whole_repo());
    assert!(
        matches!(outcome, MergeOutcome::PushFailed { .. }),
        "{outcome:?}"
    );
}

#[test]
fn revokes_the_write_token_when_the_push_throws() {
    let harness = Harness::new(Plan::new().git(GitStep::Push, fault(Fault::ExecFailed)));
    let error = merge_with(&harness, &whole_repo(), MergePolicy::Submission)
        .expect_err("the push could not run");
    assert!(error.to_string().contains("exec failed"), "{error}");
    assert_eq!(
        harness.events().last().map(String::as_str),
        Some("revoke-write")
    );
}

fn git_failed_when_no_sha(step: GitStep) {
    let harness = Harness::new(Plan::new().git(step, out(0, "not a sha\n")));
    let outcome = merge(&harness, &whole_repo());
    assert!(
        matches!(outcome, MergeOutcome::GitFailed { .. }),
        "{outcome:?}"
    );
}

#[test]
fn reports_git_failed_when_base_does_not_print_a_sha() {
    git_failed_when_no_sha(GitStep::Base);
}

#[test]
fn reports_git_failed_when_mergebase_does_not_print_a_sha() {
    git_failed_when_no_sha(GitStep::MergeBase);
}

#[test]
fn reports_git_failed_when_head_does_not_print_a_sha() {
    git_failed_when_no_sha(GitStep::Head);
}

// ---- runMerge coverage check (invariant 11) ----

fn file_only() -> Vec<ScopeClaim> {
    vec![file("src/a.ts", Mode::EditBody)]
}

#[test]
fn coverage_reads_the_diff_of_the_rebased_range_after_the_rebase_and_before_any_repo_code() {
    let harness = Harness::new(Plan::new());
    merge(&harness, &whole_repo());
    assert!(harness
        .argv(GitStep::Changed)
        .contains(&format!("{}..{}", base(), head())));
    assert!(harness.index_of("git:changed") > harness.index_of("git:head"));
    assert!(harness.index_of("git:changed") < harness.index_of("package:install"));
}

#[test]
fn coverage_reads_the_diff_without_external_diff_drivers_with_renames_and_nul_separators() {
    let harness = Harness::new(Plan::new());
    merge(&harness, &whole_repo());
    let argv = harness.argv(GitStep::Changed);
    for expected in ["--name-status", "-z", "-M", "--no-ext-diff"] {
        assert!(argv.contains(&expected.to_owned()), "{expected}");
    }
}

#[test]
fn coverage_rejects_a_change_outside_the_claim_as_uncovered_running_no_tests_and_minting_no_token()
{
    let harness = Harness::new(Plan::new().changed("M\0src/a.ts\0A\0src/other.ts\0"));
    assert_eq!(
        merge(&harness, &file_only()),
        MergeOutcome::Uncovered {
            base: base(),
            head: head(),
            files: strings(&["src/other.ts"]),
            total: 1
        }
    );
    assert!(!harness.ran_package_step());
    assert!(!harness.minted_write_access());
    assert!(!harness.has_event("git:push"));
}

#[test]
fn coverage_merges_when_the_claim_covers_every_changed_file() {
    let harness = Harness::new(Plan::new());
    assert!(matches!(
        merge(&harness, &file_only()),
        MergeOutcome::Merged { .. }
    ));
}

#[test]
fn coverage_names_at_most_50_files_and_counts_them_all() {
    let records = (0..120)
        .map(|i| format!("A\0new/f{i}.ts\0"))
        .collect::<Vec<_>>()
        .concat();
    let harness = Harness::new(Plan::new().changed(&records));
    let outcome = merge(&harness, &file_only());
    let MergeOutcome::Uncovered { files, total, .. } = outcome else {
        unreachable!("{outcome:?}")
    };
    assert_eq!(total, 120);
    assert_eq!(files.len(), 50);
    assert_eq!(files[0], "new/f0.ts");
}

#[test]
fn coverage_does_not_look_at_the_diff_when_the_rebase_left_nothing_to_add() {
    let harness = Harness::new(Plan::new().git(GitStep::Head, out(0, &format!("{}\n", base()))));
    assert_eq!(
        merge(&harness, &[]),
        MergeOutcome::AlreadyMerged { base: base() }
    );
    assert!(!harness.has_event("git:changed"));
}

fn git_failed_not_covered(changed: Reply) {
    let harness = Harness::new(Plan::new().git(GitStep::Changed, changed));
    let outcome = merge(&harness, &whole_repo());
    assert!(
        matches!(outcome, MergeOutcome::GitFailed { .. }),
        "{outcome:?}"
    );
    assert!(!harness.ran_package_step());
}

#[test]
fn coverage_reports_git_failed_not_covered_for_a_failed_diff() {
    git_failed_not_covered(out(128, ""));
}

#[test]
fn coverage_reports_git_failed_not_covered_for_a_cut_off_diff() {
    git_failed_not_covered(fault(Fault::TooLarge));
}

#[test]
fn coverage_reports_git_failed_not_covered_for_an_unknown_status() {
    git_failed_not_covered(out(0, "X\0src/a.ts\0"));
}

#[test]
fn coverage_reports_git_failed_not_covered_for_a_record_without_a_path() {
    git_failed_not_covered(out(0, "M\0"));
}

// ---- the two calls, and the errors the steward expressed as thrown ones ----

fn prepared(harness: &Harness) -> Prepared {
    let (git, sources, pin, commit) = (git(), sources(), base(), commit());
    let ctx = context(harness, &git, &sources, &pin);
    let scopes = whole_repo();
    let request = MergeRequest {
        commit: &commit,
        scopes: &scopes,
        policy: MergePolicy::Submission,
    };
    prepare(&ctx, harness, &request).expect("prepare finishes")
}

#[test]
fn prepare_returns_the_head_having_run_no_repo_code_and_minted_no_write_access() {
    let harness = Harness::new(Plan::new());
    let Prepared::Ready(ready) = prepared(&harness) else {
        unreachable!("expected a rebased change")
    };
    assert_eq!((ready.base, ready.head), (base(), head()));
    assert_eq!(ready.gate, GateReview::TrunkTests);
    assert_eq!(ready.workspace.path(), Path::new("/workspace"));
    assert!(!harness.ran_package_step());
    assert!(!harness.minted_write_access());
    assert!(!harness.has_event("git:push"));
    assert!(!harness.has_event("currentMain"));
}

#[test]
fn prepare_of_a_diff_of_only_the_gate_by_an_admin_asks_for_no_tests() {
    let harness = Harness::new(Plan::new().changed("M\0tessel.toml\0"));
    let (git, sources, pin, commit) = (git(), sources(), base(), commit());
    let ctx = context(&harness, &git, &sources, &pin);
    let scopes = whole_repo();
    let request = MergeRequest {
        commit: &commit,
        scopes: &scopes,
        policy: MergePolicy::Admin,
    };
    let Prepared::Ready(ready) = prepare(&ctx, &harness, &request).expect("finishes") else {
        unreachable!("expected a rebased change")
    };
    assert_eq!(ready.gate, GateReview::HeadFileOnly);
}

#[test]
fn prepare_ends_with_the_outcome_when_the_commit_conflicts() {
    let harness = Harness::new(
        Plan::new()
            .git(GitStep::Rebase, exit(1))
            .git(GitStep::Conflicts, out(0, "a.txt\0")),
    );
    let Prepared::Done(outcome) = prepared(&harness) else {
        unreachable!("expected an outcome")
    };
    assert_eq!(
        outcome,
        MergeOutcome::Conflict {
            base: base(),
            files: strings(&["a.txt"])
        }
    );
}

#[test]
fn land_pushes_from_base_to_head_and_reads_main_back_without_writing_a_record() {
    let harness = Harness::new(Plan::new());
    let (git, sources, pin, commit) = (git(), sources(), base(), commit());
    let ctx = context(&harness, &git, &sources, &pin);
    let scopes = whole_repo();
    let request = MergeRequest {
        commit: &commit,
        scopes: &scopes,
        policy: MergePolicy::Submission,
    };
    let Prepared::Ready(ready) = prepare(&ctx, &harness, &request).expect("finishes") else {
        unreachable!("expected a rebased change")
    };
    let before = harness.events().len();

    let outcome = land(
        &ctx,
        &harness,
        &ready.workspace,
        &PushUpdate {
            base: ready.base,
            head: ready.head,
        },
    )
    .expect("land finishes");

    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            base: base(),
            head: head(),
            evidence: None
        }
    );
    assert_eq!(
        harness.events()[before..],
        strings(&[
            &format!("mint-write {} {}", base(), head()),
            "git:push",
            "revoke-write",
            "currentMain",
        ])
    );
}

#[test]
fn a_clone_that_cannot_run_still_revokes_the_read_tokens_and_runs_nothing_after() {
    let harness = Harness::new(Plan::new().git(GitStep::Clone, fault(Fault::ExecFailed)));
    let error = merge_with(&harness, &whole_repo(), MergePolicy::Submission)
        .expect_err("the clone could not run");
    assert!(
        matches!(error, MergeError::Clone(CloneError::Run(_))),
        "{error}"
    );
    assert_eq!(harness.events(), strings(&["git:clone", "revoke-reads"]));
}

#[test]
fn an_unsafe_fork_branch_is_an_error_after_the_read_tokens_are_revoked() {
    let harness = Harness::new(Plan::new());
    let (git, pin, commit) = (git(), base(), commit());
    let hostile = MergeSources {
        fork_branch: "main --upload-pack=x".to_owned(),
        ..sources()
    };
    let ctx = context(&harness, &git, &hostile, &pin);
    let scopes = whole_repo();
    let request = MergeRequest {
        commit: &commit,
        scopes: &scopes,
        policy: MergePolicy::Submission,
    };

    let error = run_merge(&ctx, &harness, &request).expect_err("unsafe branch");

    assert!(matches!(error, MergeError::Command(_)), "{error}");
    assert_eq!(harness.events(), strings(&["git:clone", "revoke-reads"]));
}

#[test]
fn exit_codes_next_to_the_timeout_ones_are_failures_not_timeouts() {
    for (install, test) in [(125, 0), (0, 123), (0, 138)] {
        let harness = Harness::new(Plan {
            install,
            test,
            ..Plan::new()
        });
        let outcome = merge(&harness, &whole_repo());
        let (MergeOutcome::Install { result, .. } | MergeOutcome::TestsFailed { result, .. }) =
            outcome
        else {
            unreachable!("{outcome:?}")
        };
        assert!(!result.timed_out, "{result:?}");
    }
}

#[test]
fn a_package_step_that_cannot_run_is_an_error_and_mints_no_write_access() {
    struct Broken(Harness);

    impl GitRunner for Broken {
        fn run(&self, command: &GitCommand) -> Result<GitOutput, RunError> {
            self.0.run(command)
        }
    }

    impl MergePorts for Broken {
        fn revoke_read_access(&self) -> bool {
            self.0.revoke_read_access()
        }
        fn run_package_step(&self, _step: PackageStep) -> Result<StepOutcome, BoundaryError> {
            Err(BoundaryError {
                operation: "run_package_step",
                message: "the sandbox is gone".to_owned(),
            })
        }
        fn with_push_access<T>(
            &self,
            update: &PushUpdate,
            use_access: impl FnOnce() -> T,
        ) -> Result<T, BoundaryError> {
            self.0.with_push_access(update, use_access)
        }
        fn current_main(&self) -> Option<Sha> {
            self.0.current_main()
        }
        fn record_merge(&self, landed: &LandedMerge) -> EvidenceResult {
            self.0.record_merge(landed)
        }
    }

    let broken = Broken(Harness::new(Plan::new()));
    let (git, sources, pin, commit) = (git(), sources(), base(), commit());
    let ctx = MergeContext {
        git: &git,
        runner: &broken,
        sources: &sources,
        clone_dir: Path::new("/workspace"),
        pinned_main: &pin,
    };
    let scopes = whole_repo();
    let request = MergeRequest {
        commit: &commit,
        scopes: &scopes,
        policy: MergePolicy::Submission,
    };

    let error = run_merge(&ctx, &broken, &request).expect_err("the step could not run");

    assert!(error.to_string().contains("the sandbox is gone"), "{error}");
    assert!(!broken.0.minted_write_access());
}

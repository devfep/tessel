//! Everything the runner refuses before it runs anything.

use std::os::unix::fs::symlink;

use proptest::prelude::*;
use tessel_lander::gate::CARGO_TEST_FLAGS;
use tessel_runner::{
    ConfigReason, RequestRefusal, RunnerConfig, RunnerError, DEFAULT_TIMEOUT_SECS, MAX_TIMEOUT_SECS,
};

use crate::common::{argv, config, own_config_text, request, write_config, TEST_ARGV};

fn config_reason(error: &RunnerError) -> ConfigReason {
    match *error {
        RunnerError::Config { reason, .. } => reason,
        RunnerError::Request(_) | RunnerError::Unsupervised { .. } => {
            panic!("expected a configuration refusal, got {error}")
        }
    }
}

fn request_refusal(error: RunnerError) -> RequestRefusal {
    match error {
        RunnerError::Request(refusal) => refusal,
        RunnerError::Config { .. } | RunnerError::Unsupervised { .. } => {
            panic!("expected a request refusal, got {error}")
        }
    }
}

#[test]
fn the_runner_refuses_to_start_without_the_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let error = RunnerConfig::load(&dir.path().join("tessel-runner.toml")).unwrap_err();
    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("tessel-runner.toml"), "{error}");
    assert_eq!(config_reason(&error), ConfigReason::Missing);
}

#[test]
fn the_runner_refuses_a_config_naming_another_user() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "[runner]\nuser = \"somebody-else\"\n", 0o600);
    let error = RunnerConfig::load(&path).unwrap_err();
    assert!(error.to_string().contains("[runner] user"), "{error}");
    assert_eq!(config_reason(&error), ConfigReason::WrongUser);
}

#[test]
fn a_config_naming_the_current_user_loads_with_the_default_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &own_config_text(), 0o600);
    let loaded = RunnerConfig::load(&path).unwrap();
    assert_eq!(loaded.user(), config().user());
    assert_eq!(loaded.timeout_secs(), DEFAULT_TIMEOUT_SECS);
}

#[test]
fn the_runner_refuses_a_config_that_is_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let real = write_config(dir.path(), &own_config_text(), 0o600);
    let link = dir.path().join("link.toml");
    symlink(&real, &link).unwrap();
    let error = RunnerConfig::load(&link).unwrap_err();
    assert_eq!(config_reason(&error), ConfigReason::NotRegular);
}

#[test]
fn the_runner_refuses_a_config_with_an_unknown_key() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!("{}sandbox = true\n", own_config_text());
    let path = write_config(dir.path(), &text, 0o600);
    let error = RunnerConfig::load(&path).unwrap_err();
    assert_eq!(config_reason(&error), ConfigReason::Invalid);
}

#[test]
fn the_runner_refuses_a_zero_timeout_in_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!("{}timeout_secs = 0\n", own_config_text());
    let path = write_config(dir.path(), &text, 0o600);
    let error = RunnerConfig::load(&path).unwrap_err();
    assert_eq!(config_reason(&error), ConfigReason::ZeroTimeout);
}

#[test]
fn the_runner_refuses_a_relative_tree() {
    let refused = request(std::path::Path::new("relative/tree"), "", &TEST_ARGV)
        .plan(&config())
        .unwrap_err();
    assert_eq!(refused.exit_code(), 2);
    assert_eq!(request_refusal(refused), RequestRefusal::TreeNotAbsolute);
}

#[test]
fn the_runner_refuses_a_tree_that_is_a_file_or_missing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, "x").unwrap();
    for tree in [file, dir.path().join("missing")] {
        let refused = request(&tree, "", &TEST_ARGV).plan(&config()).unwrap_err();
        assert_eq!(request_refusal(refused), RequestRefusal::TreeNotDirectory);
    }
}

#[test]
fn the_runner_refuses_a_tree_that_is_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    symlink(&real, &link).unwrap();
    let refused = request(&link, "", &TEST_ARGV).plan(&config()).unwrap_err();
    assert_eq!(request_refusal(refused), RequestRefusal::TreeNotDirectory);
}

#[test]
fn the_runner_refuses_a_dir_that_climbs_out_of_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    for escape in ["..", "a/../..", "/tmp", "./x"] {
        let refused = request(dir.path(), escape, &TEST_ARGV)
            .plan(&config())
            .unwrap_err();
        assert!(
            matches!(request_refusal(refused), RequestRefusal::NotAllowed(_)),
            "{escape}"
        );
    }
}

#[test]
fn the_runner_refuses_a_dir_that_is_a_link_out_of_the_tree() {
    let outside = tempfile::tempdir().unwrap();
    let tree = tempfile::tempdir().unwrap();
    symlink(outside.path(), tree.path().join("out")).unwrap();
    let refused = request(tree.path(), "out", &TEST_ARGV)
        .plan(&config())
        .unwrap_err();
    assert_eq!(request_refusal(refused), RequestRefusal::DirEscapes);
}

#[test]
fn the_runner_refuses_a_dir_that_is_missing() {
    let tree = tempfile::tempdir().unwrap();
    let refused = request(tree.path(), "nope", &TEST_ARGV)
        .plan(&config())
        .unwrap_err();
    assert_eq!(request_refusal(refused), RequestRefusal::DirNotDirectory);
}

#[test]
fn the_runner_refuses_an_argv_outside_the_grammar() {
    let tree = tempfile::tempdir().unwrap();
    let outside: [&[&str]; 6] = [
        &["sh", "-c", "true"],
        &["cargo", "+nightly", "test"],
        &["cargo", "test", "--manifest-path", "other/Cargo.toml"],
        &["cargo", "test", "--", "--nocapture"],
        &["cargo", "test", "--workspace;", "id"],
        &[],
    ];
    for words in outside {
        let refused = request(tree.path(), "", words).plan(&config()).unwrap_err();
        assert_eq!(refused.exit_code(), 2);
        assert!(
            matches!(request_refusal(refused), RequestRefusal::NotAllowed(_)),
            "{words:?}"
        );
    }
}

#[test]
fn the_runner_refuses_an_empty_test_list_and_a_zero_timeout() {
    let tree = tempfile::tempdir().unwrap();
    let mut empty = request(tree.path(), "", &TEST_ARGV);
    empty.tests.clear();
    assert!(matches!(
        request_refusal(empty.plan(&config()).unwrap_err()),
        RequestRefusal::NotAllowed(_)
    ));
    let mut zero = request(tree.path(), "", &TEST_ARGV);
    zero.timeout_secs = 0;
    assert_eq!(
        request_refusal(zero.plan(&config()).unwrap_err()),
        RequestRefusal::ZeroTimeout
    );
}

#[test]
fn the_runner_accepts_the_gates_own_commands() {
    let tree = tempfile::tempdir().unwrap();
    std::fs::create_dir(tree.path().join("web")).unwrap();
    let accepted: [(&str, &[&str]); 4] = [
        ("", &TEST_ARGV),
        ("", &["cargo", "fmt", "--all", "--check"]),
        ("web", &["pnpm", "test"]),
        (
            "",
            &[
                "cargo",
                "test",
                "--workspace",
                "--locked",
                "--offline",
                "--release",
            ],
        ),
    ];
    for (dir, words) in accepted {
        let plan = request(tree.path(), dir, words).plan(&config()).unwrap();
        assert_eq!(plan.commands[0].argv, argv(words));
        assert!(plan.commands[0].dir.starts_with(&plan.root));
    }
}

fn in_grammar(words: &[String]) -> bool {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["pnpm" | "npm", "test"] | ["cargo", "fmt", "--all", "--check"] => true,
        ["cargo", "test", flags @ ..] => flags.iter().all(|flag| CARGO_TEST_FLAGS.contains(flag)),
        _ => false,
    }
}

fn word() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("cargo".to_owned()),
        Just("test".to_owned()),
        Just("npm".to_owned()),
        Just("pnpm".to_owned()),
        Just("sh".to_owned()),
        Just("fmt".to_owned()),
        Just("--all".to_owned()),
        Just("--check".to_owned()),
        Just("--workspace".to_owned()),
        Just("--locked".to_owned()),
        Just("--manifest-path".to_owned()),
        Just("--".to_owned()),
        Just("+nightly".to_owned()),
        ".*",
    ]
}

proptest! {
    #[test]
    fn any_argv_outside_the_grammar_is_refused(words in prop::collection::vec(word(), 0..6)) {
        prop_assume!(!in_grammar(&words));
        let tree = tempfile::tempdir().unwrap();
        let mut asked = request(tree.path(), "", &TEST_ARGV);
        asked.tests[0].argv = words;
        let refused = asked.plan(&config());
        prop_assert!(
            matches!(refused, Err(RunnerError::Request(RequestRefusal::NotAllowed(_)))),
            "{:?}", refused
        );
    }
}

#[test]
fn the_runner_refuses_a_config_timeout_above_a_day() {
    let dir = tempfile::tempdir().unwrap();
    for secs in [MAX_TIMEOUT_SECS + 1, i64::MAX as u64] {
        let text = format!("{}timeout_secs = {secs}\n", own_config_text());
        let path = write_config(dir.path(), &text, 0o600);
        let error = RunnerConfig::load(&path).unwrap_err();
        assert!(error.to_string().contains("timeout_secs"), "{error}");
        assert_eq!(config_reason(&error), ConfigReason::TimeoutTooLong);
    }
    let text = format!("{}timeout_secs = {MAX_TIMEOUT_SECS}\n", own_config_text());
    let path = write_config(dir.path(), &text, 0o600);
    assert_eq!(
        RunnerConfig::load(&path).unwrap().timeout_secs(),
        MAX_TIMEOUT_SECS
    );
}

#[test]
fn the_runner_refuses_a_tree_another_user_owns() {
    let refused = request(std::path::Path::new("/usr"), "", &TEST_ARGV)
        .plan(&config())
        .unwrap_err();
    assert_eq!(request_refusal(refused), RequestRefusal::TreeNotOwned);
}

#[test]
fn a_config_file_may_be_readable_by_others_when_nobody_but_its_owner_can_write_it() {
    for mode in [0o600, 0o644, 0o640, 0o444] {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), &own_config_text(), mode);
        assert!(RunnerConfig::load(&path).is_ok(), "mode {mode:o}");
    }
}

#[test]
fn a_config_file_that_group_or_other_can_write_is_refused() {
    for mode in [0o664, 0o666, 0o620, 0o602, 0o4644, 0o2644, 0o1644] {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), &own_config_text(), mode);
        let error = RunnerConfig::load(&path).unwrap_err();
        assert_eq!(
            config_reason(&error),
            ConfigReason::WrongMode,
            "mode {mode:o}"
        );
    }
}

fn sandbox_text(dir: &std::path::Path) -> String {
    format!(
        "{}path = \"/opt/toolchain/bin:/usr/bin:/bin\"\ncargo_home = \"/var/cargo\"\n\
         tmpdir = \"{tmp}\"\nwork_dir = \"{work}\"\nhandoff_dir = \"{handoff}\"\n\
         max_processes = 2000\nsweep_dirs = []\n",
        own_config_text(),
        tmp = dir.join("tmp").display(),
        work = dir.join("work").display(),
        handoff = dir.join("handoff").display(),
    )
}

#[test]
fn a_sandbox_config_takes_the_environment_from_the_file_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &sandbox_text(dir.path()), 0o644);
    let loaded = RunnerConfig::load(&path).unwrap();
    let sandbox = loaded
        .sandbox()
        .expect("work_dir and handoff_dir make a sandbox");
    assert_eq!(sandbox.max_processes(), 2000);
    assert_eq!(sandbox.work_dir(), dir.path().join("work"));
    assert_eq!(sandbox.handoff_dir(), dir.path().join("handoff"));
    let env = loaded.env();
    let get = |name: &str| {
        env.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("PATH"), Some("/opt/toolchain/bin:/usr/bin:/bin"));
    assert_eq!(get("CARGO_HOME"), Some("/var/cargo"));
    assert_eq!(get("HOME"), Some("/var/empty"));
    assert_eq!(
        get("TMPDIR"),
        Some(dir.path().join("tmp").to_str().unwrap())
    );
    assert_eq!(env.len(), 4, "{env:?}");
}

#[test]
fn a_sandbox_config_needs_both_directories_a_path_and_absolute_paths() {
    let dir = tempfile::tempdir().unwrap();
    let full = sandbox_text(dir.path());
    let broken = [
        full.replace("work_dir", "unused_work"),
        full.replace("handoff_dir", "unused_handoff"),
        full.replace("path = ", "unused_path = "),
        full.replace("cargo_home = \"/var/cargo\"", "cargo_home = \"relative\""),
        full.replace("max_processes = 2000", "max_processes = 0"),
    ];
    for text in broken {
        let path = write_config(dir.path(), &text, 0o644);
        let error = RunnerConfig::load(&path).unwrap_err();
        assert_eq!(config_reason(&error), ConfigReason::Invalid, "{text}");
    }
}

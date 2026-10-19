//! Port of `tessel-steward/src/merge-commands.test.ts`, one Rust test per `it`, plus the
//! fixed-environment guarantees the steward left to its container.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::path::{Path, PathBuf};

mod support;

use tessel_lander::git::{
    ensure_supported_git, parse_nul_separated, CommandError, Git, GitCommand, MergeSources,
    COMMITTER_DATE, COMMITTER_EMAIL, COMMITTER_NAME, MAX_PUSH_BODY_BYTES,
};
use tessel_lander::types::{is_safe_branch_name, Sha};
use tessel_lander::workspace::Workspace;

use support::workspace_at;

fn sha(character: char) -> Sha {
    Sha::parse(&character.to_string().repeat(40)).expect("test sha")
}

fn git() -> Git {
    Git::new(
        PathBuf::from("/lander/home"),
        PathBuf::from("/lander/hooks"),
    )
}

fn sources() -> MergeSources {
    MergeSources {
        main_remote: "https://git.example/tessel/demo.git".to_owned(),
        fork_remote: "https://git.example/tessel/demo--a1.git".to_owned(),
        fork_branch: "main".to_owned(),
    }
}

fn workspace() -> Workspace {
    workspace_at("/workspace")
}

fn argv(command: &GitCommand) -> Vec<String> {
    command
        .argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

fn env_value(command: &GitCommand, key: &str) -> Option<String> {
    command
        .env
        .get(key)
        .map(|v| v.to_string_lossy().into_owned())
}

fn position(argv: &[String], needle: &str) -> usize {
    argv.iter()
        .position(|a| a == needle)
        .expect("argv element present")
}

fn config_of(command: &GitCommand) -> Vec<(String, String)> {
    let count: usize = env_value(command, "GIT_CONFIG_COUNT")
        .expect("count")
        .parse()
        .expect("number");
    (0..count)
        .map(|i| {
            (
                env_value(command, &format!("GIT_CONFIG_KEY_{i}")).expect("key"),
                env_value(command, &format!("GIT_CONFIG_VALUE_{i}")).expect("value"),
            )
        })
        .collect()
}

#[test]
fn clones_main_with_dashdash_before_the_remote_so_it_cannot_be_read_as_an_option() {
    let sources = sources();
    let argv = argv(&git().clone_command(&sources, Path::new("/workspace")));
    assert_eq!(argv.last().map(String::as_str), Some("/workspace"));
    assert!(position(&argv, "--") < position(&argv, &sources.main_remote));
    assert!(argv.contains(&"--branch=main".to_owned()));
}

#[test]
fn fetches_only_the_forks_default_branch_into_refs_remotes_fork() {
    let sources = sources();
    let argv = argv(
        &git()
            .fetch_fork_command(&workspace(), &sources)
            .expect("safe branch"),
    );
    assert_eq!(
        argv[argv.len() - 2..],
        [
            sources.fork_remote.clone(),
            "+refs/heads/main:refs/remotes/fork/main".to_owned()
        ]
    );
}

#[test]
fn refuses_a_fork_branch_name_that_is_not_safe_in_a_refspec() {
    let hostile = MergeSources {
        fork_branch: "main --upload-pack=x".to_owned(),
        ..sources()
    };
    let fetch = git()
        .fetch_fork_command(&workspace(), &hostile)
        .expect_err("hostile branch");
    assert!(fetch.to_string().contains("unsafe fork branch"), "{fetch}");
    let reachable = git()
        .reachable_command(&workspace(), &hostile, &sha('a'))
        .expect_err("hostile branch");
    assert!(
        reachable.to_string().contains("unsafe fork branch"),
        "{reachable}"
    );
}

#[test]
fn passes_a_commit_as_one_argv_element_never_inside_shell_text() {
    let command = git()
        .reachable_command(&workspace(), &sources(), &sha('a'))
        .expect("safe branch");
    assert_eq!(
        argv(&command),
        [
            "git",
            "-C",
            "/workspace",
            "merge-base",
            "--is-ancestor",
            &"a".repeat(40),
            "refs/remotes/fork/main",
        ]
    );
}

#[test]
fn rebases_with_a_fixed_committer_identity_and_date_and_no_hooks_or_signing() {
    let command = git().rebase_command(&workspace(), &sha('b'), &sha('c'), &sha('d'));
    assert_eq!(
        env_value(&command, "GIT_COMMITTER_NAME").as_deref(),
        Some(COMMITTER_NAME)
    );
    assert_eq!(
        env_value(&command, "GIT_COMMITTER_EMAIL").as_deref(),
        Some(COMMITTER_EMAIL)
    );
    assert_eq!(
        env_value(&command, "GIT_COMMITTER_DATE").as_deref(),
        Some(COMMITTER_DATE)
    );
    assert_eq!(
        env_value(&command, "GIT_CONFIG_GLOBAL").as_deref(),
        Some("/dev/null")
    );
    let config = config_of(&command);
    assert!(config.contains(&("core.hooksPath".to_owned(), "/lander/hooks".to_owned())));
    let argv = argv(&command);
    assert!(argv.contains(&"commit.gpgsign=false".to_owned()));
    assert_eq!(
        argv[argv.len() - 4..],
        [
            "--onto".to_owned(),
            "b".repeat(40),
            "c".repeat(40),
            "d".repeat(40)
        ]
    );
}

#[test]
fn sizes_gits_post_buffer_to_the_gateways_body_limit_so_git_sends_no_probe_request() {
    let argv = argv(&git().push_command(&workspace(), &sources(), &sha('b'), &sha('e')));
    assert_eq!(MAX_PUSH_BODY_BYTES, 16_777_216);
    let flag = position(&argv, "http.postBuffer=16777216");
    assert_eq!(argv[flag - 1], "-c");
    assert!(flag < position(&argv, "push"));
}

#[test]
fn pushes_with_a_lease_on_the_sha_main_had_when_it_was_cloned_and_never_forces() {
    let sources = sources();
    let argv = argv(&git().push_command(&workspace(), &sources, &sha('b'), &sha('e')));
    assert!(argv.contains(&format!(
        "--force-with-lease=refs/heads/main:{}",
        "b".repeat(40)
    )));
    assert!(!argv.contains(&"--force".to_owned()));
    assert!(!argv.contains(&"-f".to_owned()));
    assert_eq!(
        argv[argv.len() - 3..],
        [
            "--".to_owned(),
            sources.main_remote.clone(),
            format!("{}:refs/heads/main", "e".repeat(40))
        ]
    );
}

#[test]
fn runs_every_local_command_against_the_workspace_not_the_current_directory() {
    let git = git();
    let w = workspace_at("/w");
    let base = argv(&git.base_command(&w));
    assert_eq!(base[..3], ["git", "-C", "/w"]);
    let conflicts = argv(&git.conflicts_command(&w));
    assert_eq!(conflicts[..3], ["git", "-C", "/w"]);
}

#[test]
fn parse_nul_separated_splits_paths_on_nul_and_keeps_names_with_spaces_and_newlines_whole() {
    assert_eq!(
        parse_nul_separated("a b.txt\0dir/x\ny\0"),
        ["a b.txt", "dir/x\ny"]
    );
}

#[test]
fn parse_nul_separated_returns_no_paths_for_empty_output() {
    assert_eq!(parse_nul_separated(""), Vec::<String>::new());
}

#[test]
fn diff_command_diffs_the_merge_base_against_the_commit_with_no_repo_named_program() {
    let command = git().diff_command(&workspace(), &sha('a'), &sha('b'));
    assert_eq!(
        argv(&command),
        [
            "git",
            "-C",
            "/workspace",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "-M",
            &format!("{}..{}", "a".repeat(40), "b".repeat(40)),
            "--",
        ]
    );
}

#[test]
fn every_command_runs_with_no_user_config_empty_hooks_fsck_and_no_prompt() {
    let command = git().head_command(&workspace_at("/w"));
    assert_eq!(
        env_value(&command, "GIT_CONFIG_NOSYSTEM").as_deref(),
        Some("1")
    );
    assert_eq!(
        env_value(&command, "GIT_CONFIG_GLOBAL").as_deref(),
        Some("/dev/null")
    );
    assert_eq!(env_value(&command, "HOME").as_deref(), Some("/lander/home"));
    assert_eq!(
        env_value(&command, "GIT_TERMINAL_PROMPT").as_deref(),
        Some("0")
    );
    assert_eq!(env_value(&command, "GIT_EDITOR").as_deref(), Some("true"));
    assert_eq!(
        env_value(&command, "GIT_ATTR_NOSYSTEM").as_deref(),
        Some("1")
    );
    let config = config_of(&command);
    for expected in [
        ("core.hooksPath", "/lander/hooks"),
        ("transfer.fsckObjects", "true"),
        ("core.fsmonitor", "false"),
        ("core.attributesFile", "/dev/null"),
        ("protocol.allow", "never"),
        ("protocol.https.allow", "always"),
        ("protocol.ssh.allow", "always"),
        ("protocol.file.allow", "user"),
    ] {
        assert!(
            config.contains(&(expected.0.to_owned(), expected.1.to_owned())),
            "{expected:?}"
        );
    }
}

#[test]
fn only_the_rebase_sets_a_committer() {
    let head = git().head_command(&workspace_at("/w"));
    assert!(head
        .env
        .keys()
        .all(|k| !k.starts_with("GIT_COMMITTER") && !k.starts_with("GIT_AUTHOR")));
}

#[test]
fn builds_the_remaining_local_commands_against_the_workspace_with_one_element_per_value() {
    let git = git();
    let w = &workspace_at("/w");
    let (a, b) = (sha('a'), sha('b'));
    let (a40, b40) = ("a".repeat(40), "b".repeat(40));
    assert_eq!(
        argv(&git.on_main_command(w, &a)),
        [
            "git",
            "-C",
            "/w",
            "merge-base",
            "--is-ancestor",
            &a40,
            "refs/remotes/origin/main"
        ]
    );
    assert_eq!(
        argv(&git.commit_exists_command(w, &a)),
        [
            "git",
            "-C",
            "/w",
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{a40}^{{commit}}")
        ]
    );
    assert_eq!(
        argv(&git.merge_base_command(w, &a, &b)),
        ["git", "-C", "/w", "merge-base", &a40, &b40]
    );
    assert_eq!(
        argv(&git.base_command(w)),
        [
            "git",
            "-C",
            "/w",
            "rev-parse",
            "--verify",
            "refs/remotes/origin/main^{commit}"
        ]
    );
    assert_eq!(
        argv(&git.head_command(w)),
        ["git", "-C", "/w", "rev-parse", "--verify", "HEAD^{commit}"]
    );
    assert_eq!(
        argv(&git.conflicts_command(w)),
        [
            "git",
            "-C",
            "/w",
            "diff",
            "--name-only",
            "--diff-filter=U",
            "-z"
        ]
    );
    assert_eq!(
        argv(&git.changed_files_command(w, &a, &b)),
        [
            "git",
            "-C",
            "/w",
            "diff",
            "--name-status",
            "-z",
            "-M",
            "--no-ext-diff",
            &format!("{a40}..{b40}"),
            "--",
        ]
    );
    assert_eq!(
        argv(&git.show_file_command(w, &a, "src/--odd name")),
        ["git", "-C", "/w", "show", &format!("{a40}:src/--odd name")]
    );
}

#[test]
fn a_path_with_shell_metacharacters_stays_one_argv_element() {
    let command = git().show_file_command(&workspace_at("/w"), &sha('a'), "x; touch y $(z)\n");
    let last = command.argv.last().expect("argv");
    assert_eq!(
        last.to_string_lossy(),
        format!("{}:x; touch y $(z)\n", "a".repeat(40))
    );
}

#[test]
fn sha_parse_accepts_only_forty_lowercase_hex_characters() {
    assert!(Sha::parse("0123456789abcdef0123456789abcdef01234567").is_ok());
    for bad in [
        "",
        &"a".repeat(39),
        &"a".repeat(41),
        &"A".repeat(40),
        &"g".repeat(40),
        &format!("{} ", "a".repeat(39)),
        &format!("-{}", "a".repeat(39)),
    ] {
        assert!(Sha::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn branch_names_are_safe_only_when_they_cannot_be_an_option_or_escape_a_ref() {
    for ok in [
        "main",
        "feature/x-1.2_3",
        "a",
        &"a".repeat(128),
        "release/1.0",
    ] {
        assert!(is_safe_branch_name(ok), "{ok:?}");
    }
    for bad in [
        "",
        "-x",
        ".x",
        "/x",
        "a/",
        "a..b",
        "a b",
        "a;b",
        "a\nb",
        "a:b",
        "a~b",
        &"a".repeat(129),
    ] {
        assert!(!is_safe_branch_name(bad), "{bad:?}");
    }
}

#[test]
fn git_2_31_or_newer_is_accepted() {
    for version in [
        "git version 2.31.0",
        "git version 2.56.0\n",
        "git version 2.39.5 (Apple Git-154)",
        "git version 2.100.0",
        "git version 3.0.1",
        "git version 10.2",
    ] {
        assert!(ensure_supported_git(version).is_ok(), "{version:?}");
    }
}

#[test]
fn older_or_unreadable_git_is_refused_naming_what_was_found() {
    for version in [
        "git version 2.30.9",
        "git version 2.9.0",
        "git version 1.99.0",
        "git version 2",
        "git version x.y",
        "version 2.40.0",
        "",
    ] {
        let error = ensure_supported_git(version).expect_err(version);
        assert!(
            matches!(error, CommandError::UnsupportedGit { .. }),
            "{version:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains(&format!("{:?}", version.trim())),
            "{message}"
        );
        assert!(message.contains("2.31"), "{message}");
    }
}

#[test]
fn create_in_makes_empty_directories_and_checks_the_installed_git() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let git = Git::create_in(root.path()).expect("fresh root");
    assert!(root.path().join("home").is_dir());
    assert!(root.path().join("hooks").is_dir());
    git.check_version().expect("installed git is supported");
    Git::create_in(root.path()).expect("existing empty directories are fine");
}

#[test]
fn create_in_refuses_a_hooks_or_home_directory_that_holds_anything() {
    for dir in ["hooks", "home"] {
        let root = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(root.path().join(dir)).expect("dir");
        std::fs::write(root.path().join(dir).join("pre-rebase"), "#!/bin/sh\n").expect("file");
        let error = Git::create_in(root.path()).expect_err("not empty");
        assert!(matches!(error, CommandError::NotEmpty { .. }), "{error}");
        assert!(error.to_string().contains("pre-rebase"), "{error}");
    }
}

//! Port of `tessel-steward/src/tessel-config.test.ts` (`parseGateConfig`) and of the
//! `readGatePlan` tests of `gate-plan.test.ts`. `gate_invalid` rests on this parser, so it is
//! tested here the way the steward tested it. Not ported: the "does not read the bytes of a file
//! over the size bound" test (the reader returns `TooLarge` instead of a blob) and `startOptions`
//! (container images).

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::cell::RefCell;

use tessel_lander::gate::{
    parse_gate_config, read_gate_plan, GateConfig, GateInstance, GatePlan, TestCommand, TrunkFile,
    TrunkFiles, CARGO_TEST_FLAGS, MAX_CONFIG_BYTES,
};
use tessel_lander::merge::BoundaryError;
use tessel_lander::types::Sha;

const GOOD: &str = r#"
instance = "standard-4"

[install]
cargo = true
pnpm = ["tessel-steward"]

[[test]]
argv = ["cargo", "test", "--workspace", "--locked", "--offline"]

[[test]]
dir = "tessel-steward"
argv = ["pnpm", "test"]
"#;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn rejected(text: &str) -> String {
    match parse_gate_config(text) {
        Ok(config) => unreachable!("expected a rejection, got {config:?}"),
        Err(error) => error.reason,
    }
}

fn gate_with_argv(argv: &[&str]) -> String {
    let list: Vec<String> = argv.iter().map(|a| format!("{a:?}")).collect();
    format!(
        "instance = \"standard-4\"\n[[test]]\nargv = [{}]\n",
        list.join(", ")
    )
}

#[test]
fn reads_the_gate_defaulting_the_directory_of_a_command_to_the_repo_root() {
    assert_eq!(
        parse_gate_config(GOOD),
        Ok(GateConfig {
            instance: Some(GateInstance::Standard4),
            install_cargo: true,
            install_pnpm: strings(&["tessel-steward"]),
            tests: vec![
                TestCommand {
                    dir: ".".to_owned(),
                    argv: strings(&["cargo", "test", "--workspace", "--locked", "--offline"])
                },
                TestCommand {
                    dir: "tessel-steward".to_owned(),
                    argv: strings(&["pnpm", "test"])
                },
            ],
        })
    );
}

#[test]
fn accepts_the_tessel_toml_committed_at_the_root_of_this_repo() {
    let text = include_str!("../../tessel.toml");
    let config = parse_gate_config(text).expect("the repo's own gate is valid");
    assert_eq!(config.instance, Some(GateInstance::Standard4));
}

#[test]
fn needs_no_install_table() {
    let config =
        parse_gate_config("instance = \"standard-1\"\n[[test]]\nargv = [\"npm\", \"test\"]\n")
            .expect("valid");
    assert_eq!(config.instance, Some(GateInstance::Standard1));
    assert!(!config.install_cargo);
    assert_eq!(config.install_pnpm, Vec::<String>::new());
}

#[test]
fn rejects_unknown_keys_at_every_level() {
    assert!(rejected(&format!("{GOOD}timeout = 9999\n")).contains("only the keys"));
    assert!(
        rejected(&GOOD.replace("cargo = true", "cargo = true\nnpm = [\"x\"]")).contains("install")
    );
    assert!(
        rejected(&GOOD.replace("argv = [\"pnpm\"", "env = \"A=b\"\nargv = [\"pnpm\""))
            .contains("[[test]]")
    );
}

#[test]
fn rejects_an_unknown_instance_and_lite() {
    for instance in ["lite", "standard-9", "huge", ""] {
        let text = format!("instance = \"{instance}\"\n[[test]]\nargv = [\"npm\", \"test\"]\n");
        assert!(rejected(&text).contains("instance"), "{instance}");
    }
}

#[test]
fn accepts_a_gate_without_an_instance() {
    let config =
        parse_gate_config("[[test]]\nargv = [\"npm\", \"test\"]\n").expect("instance is optional");
    assert_eq!(config.instance, None);
    assert_eq!(config.tests.len(), 1);
}

#[test]
fn accepts_each_instance_it_names() {
    for (name, instance) in [
        ("standard-1", GateInstance::Standard1),
        ("standard-2", GateInstance::Standard2),
        ("standard-3", GateInstance::Standard3),
        ("standard-4", GateInstance::Standard4),
    ] {
        let text = format!("instance = \"{name}\"\n[[test]]\nargv = [\"npm\", \"test\"]\n");
        assert_eq!(
            parse_gate_config(&text).expect("valid").instance,
            Some(instance)
        );
    }
}

#[test]
fn rejects_a_file_over_the_size_bound_before_parsing_it() {
    let padded = format!("{GOOD}\n# {}\n", "x".repeat(MAX_CONFIG_BYTES));
    assert!(rejected(&padded).contains("larger than"));
    let at_the_bound = format!("{GOOD}#{}\n", "x".repeat(MAX_CONFIG_BYTES - GOOD.len() - 2));
    assert_eq!(at_the_bound.len(), MAX_CONFIG_BYTES);
    assert!(parse_gate_config(&at_the_bound).is_ok());
}

#[test]
fn rejects_text_that_is_not_toml() {
    assert!(rejected("instance = [").contains("not valid TOML"));
}

#[test]
fn rejects_a_command_given_as_a_shell_string_instead_of_an_argv_array() {
    assert!(
        rejected("instance = \"standard-4\"\n[[test]]\nargv = \"cargo test\"\n").contains("array")
    );
    assert!(rejected("instance = \"standard-4\"\n[[test]]\nargv = []\n").contains("array"));
}

#[test]
fn rejects_shell_metacharacters_and_whitespace_in_any_argument() {
    for bad in [
        "test && rm",
        "test;rm",
        "$HOME",
        "`id`",
        "a|b",
        "a>b",
        "(x)",
        "*.rs",
        "a b",
        "a\nb",
        "'x'",
        "\"x\"",
        "a\\b",
        "~",
        "!x",
    ] {
        assert!(
            rejected(&gate_with_argv(&["cargo", bad])).contains("shell characters"),
            "{bad:?}"
        );
    }
}

#[test]
fn rejects_an_argument_over_the_length_bound() {
    let long = "a".repeat(201);
    assert!(rejected(&gate_with_argv(&["cargo", &long])).contains("shell characters"));
}

#[test]
fn rejects_a_shell_and_any_program_outside_the_allowlist() {
    for program in [
        "sh", "bash", "/bin/sh", "rm", "env", "curl", "./run.sh", "node",
    ] {
        let reason = rejected(&gate_with_argv(&[program, "-c"]));
        assert!(
            reason.contains("must be pnpm test") || reason.contains("shell characters"),
            "{program}: {reason}"
        );
    }
}

#[test]
fn rejects_every_argv_outside_the_allowlist() {
    let refused: [&[&str]; 22] = [
        &["cargo", "test", "--config=build.rustc-wrapper=evil"],
        &["cargo", "test", "--config", "x"],
        &["cargo", "test", "--config"],
        &["cargo", "test", "--workspace", "--config"],
        &["cargo", "test", "-Zunstable-options"],
        &["cargo", "test", "-Z", "build-std"],
        &["cargo", "+nightly", "test"],
        &["cargo", "test", "--manifest-path=other/Cargo.toml"],
        &["cargo", "test", "--target=x86_64-unknown-linux-gnu"],
        &["cargo", "test", "--"],
        &["cargo", "test", "--", "--nocapture"],
        &["cargo", "build"],
        &["cargo", "run"],
        &["cargo"],
        &["cargo", "test", "--workspace", "--features=x"],
        &["RUSTC_WRAPPER=evil", "cargo", "test"],
        &["pnpm", "test", "--filter=x"],
        &["pnpm", "run", "test"],
        &["pnpm", "exec", "vitest"],
        &["pnpm"],
        &["npm", "test", "--ignore-scripts"],
        &["npm", "run", "test"],
    ];
    for argv in refused {
        let reason = rejected(&gate_with_argv(argv));
        assert!(
            reason.contains("must be pnpm test") || reason.contains("shell characters"),
            "{argv:?}: {reason}"
        );
    }
}

#[test]
fn accepts_cargo_test_with_each_listed_flag_and_pnpm_test_and_npm_test() {
    let mut every_flag = vec!["cargo", "test"];
    every_flag.extend(CARGO_TEST_FLAGS);
    for argv in [
        vec!["cargo", "test"],
        vec!["pnpm", "test"],
        vec!["npm", "test"],
        every_flag,
    ] {
        assert!(
            parse_gate_config(&gate_with_argv(&argv)).is_ok(),
            "{argv:?}"
        );
    }
}

#[test]
fn accepts_cargo_fmt_all_check_and_only_that_argv_of_cargo_fmt() {
    assert!(
        parse_gate_config(&gate_with_argv(&["cargo", "fmt", "--all", "--check"])).is_ok(),
        "cargo fmt --all --check is the one cargo fmt argv"
    );
    let refused: [&[&str]; 12] = [
        &["cargo", "fmt"],
        &["cargo", "fmt", "--all", "--check", "--manifest-path=x"],
        &["cargo", "fmt", "-p", "x", "--all", "--check"],
        &["cargo", "fmt", "--all", "--check", "--", "--emit", "files"],
        &["cargo", "fmt", "--all"],
        &["cargo", "fmt", "--check"],
        &["cargo", "fmt", "--check", "--all"],
        &["cargo", "fmt", "--all", "--check", "--"],
        &["cargo", "fmt", "--all", "--check", "--verbose"],
        &["cargo", "fmt", "--all", "--check", "--config=x=y"],
        &["cargo", "fmt", "--all", "--check", "--all"],
        &["cargo", "+nightly", "fmt", "--all", "--check"],
    ];
    for argv in refused {
        let reason = rejected(&gate_with_argv(argv));
        assert!(reason.contains("must be pnpm test"), "{argv:?}: {reason}");
    }
}

#[test]
fn rejects_directories_that_leave_the_repo_or_are_not_plain_paths() {
    for dir in [
        "..", "../x", "/etc", "a/../b", ".git", "a//b", "a/", "a b", "~", "",
    ] {
        let test = format!(
            "instance = \"standard-4\"\n[[test]]\ndir = {dir:?}\nargv = [\"npm\", \"test\"]\n"
        );
        assert!(rejected(&test).contains("directory"), "{dir:?}");
        let install = format!(
            "instance = \"standard-4\"\n[install]\npnpm = [{dir:?}]\n[[test]]\nargv = [\"npm\", \"test\"]\n"
        );
        assert!(rejected(&install).contains("directory"), "{dir:?}");
    }
}

#[test]
fn accepts_plain_directories() {
    for dir in [".", "a", "a/b", "_x/y-z.w", "tessel-steward"] {
        let text = format!(
            "instance = \"standard-4\"\n[[test]]\ndir = {dir:?}\nargv = [\"npm\", \"test\"]\n"
        );
        assert!(parse_gate_config(&text).is_ok(), "{dir:?}");
    }
}

#[test]
fn bounds_the_number_of_commands_directories_and_arguments() {
    let tests = "[[test]]\nargv = [\"npm\", \"test\"]\n".repeat(7);
    assert!(rejected(&format!("instance = \"standard-4\"\n{tests}")).contains("test must be"));
    let six = "[[test]]\nargv = [\"npm\", \"test\"]\n".repeat(6);
    assert!(parse_gate_config(&format!("instance = \"standard-4\"\n{six}")).is_ok());
    let dirs = "[\"a\",\"b\",\"c\",\"d\",\"e\"]";
    let install = format!(
        "instance = \"standard-4\"\n[install]\npnpm = {dirs}\n[[test]]\nargv = [\"npm\", \"test\"]\n"
    );
    assert!(rejected(&install).contains("at most"));
    let four = "instance = \"standard-4\"\n[install]\npnpm = [\"a\",\"b\",\"c\",\"d\"]\n[[test]]\nargv = [\"npm\", \"test\"]\n";
    assert!(parse_gate_config(four).is_ok());
    let many = vec!["\"a\""; 25].join(",");
    let text = format!("instance = \"standard-4\"\n[[test]]\nargv = [\"npm\", {many}]\n");
    assert!(rejected(&text).contains("1 to 24"));
}

#[test]
fn requires_at_least_one_test() {
    assert!(rejected("instance = \"standard-4\"\n").contains("test must be"));
}

#[test]
fn rejects_wrong_types() {
    assert!(rejected("instance = 4\n[[test]]\nargv = [\"npm\", \"test\"]\n").contains("instance"));
    assert!(rejected(
        "instance = \"standard-4\"\n[install]\ncargo = \"yes\"\n[[test]]\nargv = [\"npm\", \"test\"]\n"
    )
    .contains("install.cargo"));
    assert!(rejected("instance = \"standard-4\"\ntest = \"npm test\"\n").contains("test must be"));
    assert!(rejected(
        "instance = \"standard-4\"\ninstall = 3\n[[test]]\nargv = [\"npm\", \"test\"]\n"
    )
    .contains("install must be a table"));
}

// ---- readGatePlan ----

struct Trunk {
    file: Result<TrunkFile, &'static str>,
    reads: RefCell<Vec<(String, String)>>,
}

impl Trunk {
    fn with(file: TrunkFile) -> Trunk {
        Trunk {
            file: Ok(file),
            reads: RefCell::new(Vec::new()),
        }
    }
}

impl TrunkFiles for Trunk {
    fn read_file(&self, commit: &Sha, path: &str) -> Result<TrunkFile, BoundaryError> {
        self.reads
            .borrow_mut()
            .push((commit.to_string(), path.to_owned()));
        self.file.clone().map_err(|message| BoundaryError {
            operation: "read_file",
            message: message.to_owned(),
        })
    }
}

fn commit() -> Sha {
    Sha::parse(&"a".repeat(40)).expect("sha")
}

const GOOD_PLAN: &str =
    "instance = \"standard-4\"\n[[test]]\nargv = [\"cargo\", \"test\", \"--locked\"]\n";

#[test]
fn reads_tessel_toml_at_the_commit_it_is_given() {
    let trunk = Trunk::with(TrunkFile::Found(GOOD_PLAN.as_bytes().to_vec()));
    let plan = read_gate_plan(&trunk, &commit()).expect("reads");
    assert_eq!(
        *trunk.reads.borrow(),
        [(commit().to_string(), "tessel.toml".to_owned())]
    );
    let GatePlan::Configured(config) = plan else {
        unreachable!("{plan:?}")
    };
    assert_eq!(config.instance, Some(GateInstance::Standard4));
}

#[test]
fn is_legacy_when_main_has_no_tessel_toml() {
    let trunk = Trunk::with(TrunkFile::Missing);
    assert_eq!(
        read_gate_plan(&trunk, &commit()).expect("reads"),
        GatePlan::Legacy
    );
}

#[test]
fn is_invalid_never_legacy_for_a_file_that_is_invalid_oversized_not_utf_8_or_too_big_to_read() {
    let cases = [
        TrunkFile::Found(b"instance = \"lite\"\n".to_vec()),
        TrunkFile::Found(vec![b'x'; MAX_CONFIG_BYTES + 1]),
        TrunkFile::Found(vec![0xff, 0xfe, 0x00]),
        TrunkFile::TooLarge,
    ];
    for file in cases {
        let trunk = Trunk::with(file.clone());
        assert_eq!(
            read_gate_plan(&trunk, &commit()).expect("reads"),
            GatePlan::Invalid,
            "{file:?}"
        );
    }
}

#[test]
fn fails_on_an_unexpected_reader_failure_instead_of_guessing_a_gate() {
    let trunk = Trunk {
        file: Err("down"),
        reads: RefCell::new(Vec::new()),
    };
    let error = read_gate_plan(&trunk, &commit()).expect_err("the reader failed");
    assert!(error.to_string().contains("down"), "{error}");
}

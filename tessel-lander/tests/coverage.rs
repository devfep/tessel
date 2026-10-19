//! Port of `tessel-steward/src/merge-coverage.test.ts`. Each `it` is one test; each `it.each`
//! row is its own test, named after the row's label.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use serde_json::{json, Value};
use tessel_coordinator::protocol::{Mode, Scope, ScopeClaim, SymbolId};
use tessel_lander::coverage::{
    parse_name_status, parse_scopes, uncovered_paths, NameStatusError, Requirement, ScopesError,
};

const MODES: [Mode; 4] = [
    Mode::Depend,
    Mode::EditBody,
    Mode::EditSignature,
    Mode::Create,
];

/// A fixed copy of `Mode::permits` in src/protocol.rs: rows are held, columns are needed.
const PERMITS_TABLE: [[bool; 4]; 4] = [
    [true, false, false, false],
    [true, true, false, false],
    [true, true, true, false],
    [true, false, false, true],
];

fn dir(path: &str, mode: Mode) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::Dir {
            path: path.to_owned(),
        },
        mode,
    }
}

fn file(path: &str, mode: Mode) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::File {
            path: path.to_owned(),
        },
        mode,
    }
}

fn symbol(path: &str, mode: Mode) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::Symbol(SymbolId {
            path: path.to_owned(),
            qualified_name: "m::f".to_owned(),
        }),
        mode,
    }
}

fn requirement(path: &str, mode: Mode) -> Requirement {
    Requirement {
        path: path.to_owned(),
        mode,
    }
}

fn uncovered(status: &str, claim: &[ScopeClaim]) -> Vec<String> {
    let required = parse_name_status(status).expect("parsed");
    uncovered_paths(&required, claim)
}

// ---- modePermits ----

#[test]
fn mode_permits_matches_the_protocol_table_for_all_16_pairs() {
    for (row, held) in MODES.into_iter().enumerate() {
        for (column, needed) in MODES.into_iter().enumerate() {
            assert_eq!(
                held.permits(needed),
                PERMITS_TABLE[row][column],
                "{held:?} permits {needed:?}"
            );
        }
    }
}

// ---- parseNameStatus: it.each "maps %s" ----

fn maps(stdout: &str, expected: &[Requirement]) {
    assert_eq!(parse_name_status(stdout).expect("parsed"), expected);
}

#[test]
fn parse_name_status_maps_added() {
    maps("A\0a.ts\0", &[requirement("a.ts", Mode::Create)]);
}

#[test]
fn parse_name_status_maps_modified() {
    maps("M\0a.ts\0", &[requirement("a.ts", Mode::EditBody)]);
}

#[test]
fn parse_name_status_maps_deleted() {
    maps("D\0a.ts\0", &[requirement("a.ts", Mode::EditSignature)]);
}

#[test]
fn parse_name_status_maps_type_changed() {
    maps("T\0a.ts\0", &[requirement("a.ts", Mode::EditSignature)]);
}

#[test]
fn parse_name_status_maps_renamed() {
    maps(
        "R087\0old.ts\0new.ts\0",
        &[
            requirement("old.ts", Mode::EditSignature),
            requirement("new.ts", Mode::Create),
        ],
    );
}

#[test]
fn parse_name_status_maps_copied() {
    maps(
        "C075\0src.ts\0copy.ts\0",
        &[requirement("copy.ts", Mode::Create)],
    );
}

#[test]
fn parse_name_status_maps_several_records_and_a_name_with_a_space_and_a_newline() {
    maps(
        "A\0a b.ts\0M\0c\nd.ts\0",
        &[
            requirement("a b.ts", Mode::Create),
            requirement("c\nd.ts", Mode::EditBody),
        ],
    );
}

#[test]
fn parse_name_status_maps_nothing() {
    maps("", &[]);
}

// ---- parseNameStatus: it.each "returns undefined for %s" ----

#[test]
fn parse_name_status_returns_an_error_for_an_unmerged_record() {
    let error = parse_name_status("U\0a.ts\0").expect_err("unmerged");
    assert!(
        matches!(error, NameStatusError::UnknownStatus { .. }),
        "{error}"
    );
}

#[test]
fn parse_name_status_returns_an_error_for_an_unknown_status() {
    let error = parse_name_status("Z\0a.ts\0").expect_err("unknown");
    assert!(
        matches!(error, NameStatusError::UnknownStatus { .. }),
        "{error}"
    );
}

#[test]
fn parse_name_status_returns_an_error_for_a_status_without_a_path() {
    let error = parse_name_status("M\0").expect_err("no path");
    assert!(
        matches!(error, NameStatusError::MissingPath { .. }),
        "{error}"
    );
}

#[test]
fn parse_name_status_returns_an_error_for_a_rename_with_one_path() {
    let error = parse_name_status("R100\0old.ts\0").expect_err("one path");
    assert!(
        matches!(error, NameStatusError::MissingPath { .. }),
        "{error}"
    );
}

#[test]
fn parse_name_status_returns_an_error_for_an_empty_path() {
    let error = parse_name_status("M\0\0").expect_err("empty path");
    assert!(
        matches!(error, NameStatusError::MissingPath { .. }),
        "{error}"
    );
}

// ---- uncoveredPaths ----

#[test]
fn covers_a_file_by_an_exact_file_scope_in_a_permitting_mode() {
    assert_eq!(
        uncovered("M\0src/a.ts\0", &[file("src/a.ts", Mode::EditBody)]),
        Vec::<String>::new()
    );
    assert_eq!(
        uncovered("M\0src/b.ts\0", &[file("src/a.ts", Mode::EditBody)]),
        ["src/b.ts"]
    );
}

#[test]
fn covers_everything_beneath_an_ancestor_dir_but_not_a_sibling_or_a_longer_name() {
    let claim = [dir("src/auth", Mode::EditSignature)];
    assert_eq!(
        uncovered("M\0src/auth/a.ts\0M\0src/auth/deep/b.ts\0", &claim),
        Vec::<String>::new()
    );
    assert_eq!(
        uncovered("M\0src/authz/a.ts\0M\0src/other/b.ts\0M\0src\0", &claim),
        ["src/authz/a.ts", "src/other/b.ts", "src"]
    );
}

#[test]
fn covers_every_path_with_the_root_dir() {
    assert_eq!(
        uncovered("M\0a.ts\0M\0x/y/z.ts\0", &[dir("", Mode::EditBody)]),
        Vec::<String>::new()
    );
}

#[test]
fn covers_a_file_by_a_symbol_scope_in_it_and_no_other_file() {
    let claim = [symbol("src/a.ts", Mode::EditBody)];
    assert_eq!(uncovered("M\0src/a.ts\0", &claim), Vec::<String>::new());
    assert_eq!(uncovered("M\0src/b.ts\0", &claim), ["src/b.ts"]);
}

#[test]
fn rejects_a_change_whose_mode_the_claim_does_not_permit() {
    assert_eq!(
        uncovered("D\0a.ts\0", &[file("a.ts", Mode::EditBody)]),
        ["a.ts"]
    );
    assert_eq!(
        uncovered("A\0a.ts\0", &[file("a.ts", Mode::EditBody)]),
        ["a.ts"]
    );
    assert_eq!(
        uncovered("M\0a.ts\0", &[file("a.ts", Mode::Depend)]),
        ["a.ts"]
    );
}

#[test]
fn covers_a_modified_file_by_edit_body_or_by_create_but_not_by_depend() {
    assert_eq!(
        uncovered("M\0a.ts\0", &[file("a.ts", Mode::EditBody)]),
        Vec::<String>::new()
    );
    assert_eq!(
        uncovered("M\0a.ts\0", &[file("a.ts", Mode::Create)]),
        Vec::<String>::new()
    );
    assert_eq!(
        uncovered("M\0a.ts\0", &[file("a.ts", Mode::Depend)]),
        ["a.ts"]
    );
}

#[test]
fn does_not_let_create_cover_a_delete_a_type_change_or_the_old_path_of_a_rename() {
    let claim = [file("x.ts", Mode::Create)];
    assert_eq!(uncovered("D\0x.ts\0", &claim), ["x.ts"]);
    assert_eq!(uncovered("T\0x.ts\0", &claim), ["x.ts"]);
    let with_new = [file("x.ts", Mode::Create), file("y.ts", Mode::Create)];
    assert_eq!(uncovered("R100\0x.ts\0y.ts\0", &with_new), ["x.ts"]);
}

#[test]
fn lets_one_scope_in_a_permitting_mode_cover_what_another_scope_in_a_weak_mode_does_not() {
    let claim = [dir("src", Mode::Depend), file("src/a.ts", Mode::EditBody)];
    assert_eq!(
        uncovered("M\0src/a.ts\0M\0src/b.ts\0", &claim),
        ["src/b.ts"]
    );
}

#[test]
fn needs_edit_signature_on_the_old_path_and_create_on_the_new_path_for_a_rename() {
    let rename = "R090\0old.ts\0new.ts\0";
    assert_eq!(
        uncovered(rename, &[file("old.ts", Mode::EditSignature)]),
        ["new.ts"]
    );
    assert_eq!(
        uncovered(rename, &[file("new.ts", Mode::Create)]),
        ["old.ts"]
    );
    let both = [
        file("old.ts", Mode::EditSignature),
        file("new.ts", Mode::Create),
    ];
    assert_eq!(uncovered(rename, &both), Vec::<String>::new());
}

#[test]
fn needs_create_on_the_new_path_only_for_a_copy() {
    assert_eq!(
        uncovered("C100\0src.ts\0copy.ts\0", &[file("copy.ts", Mode::Create)]),
        Vec::<String>::new()
    );
}

#[test]
fn lists_a_path_once_and_covers_nothing_with_an_empty_claim() {
    assert_eq!(uncovered("M\0a.ts\0D\0a.ts\0", &[]), ["a.ts"]);
}

// ---- parseScopes ----

#[test]
fn reads_the_scopes_the_coordinator_sends_symbol_scopes_flattened_by_serde() {
    let sent = json!([
        { "scope": { "kind": "dir", "path": "" }, "mode": "depend" },
        { "scope": { "kind": "file", "path": "a.ts" }, "mode": "edit_body" },
        { "scope": { "kind": "symbol", "path": "a.ts", "qualified_name": "m::f" }, "mode": "create" },
    ]);
    let scopes = parse_scopes(&sent).expect("valid");
    assert_eq!(
        scopes,
        [
            dir("", Mode::Depend),
            file("a.ts", Mode::EditBody),
            symbol("a.ts", Mode::Create)
        ]
    );
    assert_eq!(serde_json::to_value(&scopes).expect("serialises"), sent);
}

#[test]
fn accepts_an_empty_list_which_covers_nothing() {
    assert_eq!(
        parse_scopes(&json!([])).expect("valid"),
        Vec::<ScopeClaim>::new()
    );
}

fn rejects(value: &Value) {
    let error = parse_scopes(value).expect_err("rejected");
    assert!(
        matches!(
            error,
            ScopesError::Malformed { .. } | ScopesError::EmptyPath { .. }
        ),
        "{error}"
    );
}

#[test]
fn parse_scopes_rejects_undefined() {
    rejects(&Value::Null);
}

#[test]
fn parse_scopes_rejects_an_object() {
    rejects(&json!({}));
}

#[test]
fn parse_scopes_rejects_a_string() {
    rejects(&json!("dir"));
}

#[test]
fn parse_scopes_rejects_a_non_object_item() {
    rejects(&json!(["dir"]));
}

#[test]
fn parse_scopes_rejects_an_unknown_kind() {
    rejects(&json!([{ "scope": { "kind": "glob", "path": "*" }, "mode": "edit_body" }]));
}

#[test]
fn parse_scopes_rejects_an_unknown_mode() {
    rejects(&json!([{ "scope": { "kind": "dir", "path": "" }, "mode": "write" }]));
}

#[test]
fn parse_scopes_rejects_a_missing_mode() {
    rejects(&json!([{ "scope": { "kind": "dir", "path": "" } }]));
}

#[test]
fn parse_scopes_rejects_a_file_scope_with_an_empty_path() {
    let error =
        parse_scopes(&json!([{ "scope": { "kind": "file", "path": "" }, "mode": "create" }]))
            .expect_err("rejected");
    assert!(
        matches!(error, ScopesError::EmptyPath { kind: "file" }),
        "{error}"
    );
}

#[test]
fn parse_scopes_rejects_a_symbol_scope_with_an_empty_path() {
    let sent = json!([{
        "scope": { "kind": "symbol", "path": "", "qualified_name": "m::f" }, "mode": "create"
    }]);
    let error = parse_scopes(&sent).expect_err("rejected");
    assert!(
        matches!(error, ScopesError::EmptyPath { kind: "symbol" }),
        "{error}"
    );
}

#[test]
fn parse_scopes_rejects_a_symbol_scope_without_a_name() {
    rejects(&json!([{ "scope": { "kind": "symbol", "path": "a" }, "mode": "create" }]));
}

#[test]
fn parse_scopes_rejects_a_non_string_path() {
    rejects(&json!([{ "scope": { "kind": "dir", "path": 1 }, "mode": "create" }]));
}

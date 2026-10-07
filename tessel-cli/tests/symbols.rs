//! Symbol-level claims end to end: the pre-edit hook decides what to claim from the file and the
//! edit, the daemon escalates many symbols of one file to the file, and `tessel submit` reports
//! the symbols a commit changed. The real binary and daemon run against the fake coordinator.

#![expect(
    clippy::panic_in_result_fn,
    reason = "assertions are how these tests fail; they return Result so `?` carries setup errors"
)]
#![expect(
    dead_code,
    reason = "each test crate compiles all of support, which the other test crates also use"
)]

mod support;

use anyhow::Result;
use serde_json::{json, Value};
use support::{git, Agent, Fake};
use tessel_coordinator::protocol::{ClientMsg, Mode, Scope, ScopeClaim, SymbolId};

const TOK1: &str = "tok-a1-S3CRETvalue";
const TOK2: &str = "tok-a2-S3CRETvalue";

const SOURCE: &str = "\
use std::io;

pub fn one() {
    first();
}

pub fn two() {
    second();
}

pub fn three() {
    third();
}

pub fn four() {
    fourth();
}

pub fn five() {
    fifth();
}

pub fn six() {
    sixth();
}
";

async fn world() -> Result<(Fake, Agent, Agent)> {
    let fake = Fake::start(30_000, &[("a1", TOK1), ("a2", TOK2)]).await?;
    let a1 = Agent::new(&fake, "a1", TOK1)?;
    let a2 = Agent::new(&fake, "a2", TOK2)?;
    Ok((fake, a1, a2))
}

/// An agent that has started, with `src/m.rs` holding `SOURCE` in the work tree.
async fn started() -> Result<(Fake, Agent, Agent)> {
    let (fake, a1, a2) = world().await?;
    for agent in [&a1, &a2] {
        std::fs::write(agent.root().join("src/m.rs"), SOURCE)?;
    }
    a1.start("symbol claims")?;
    Ok((fake, a1, a2))
}

/// Like `started`, with `src/m.rs` committed first, so a later commit changes it rather than adding it.
async fn started_committed() -> Result<(Fake, Agent, Agent)> {
    let (fake, a1, a2) = world().await?;
    for agent in [&a1, &a2] {
        std::fs::write(agent.root().join("src/m.rs"), SOURCE)?;
        git(&agent.root(), &["add", "src/m.rs"])?;
        git(&agent.root(), &["commit", "-q", "-m", "add m"])?;
    }
    a1.start("symbol claims")?;
    Ok((fake, a1, a2))
}

/// Every scope the agent holds as `"<kind> <name> <mode>"`, in claim order.
fn held(agent: &Agent) -> Result<Vec<String>> {
    let status = agent.status()?;
    let mut out = Vec::new();
    let claims = status["state"]["claims"].as_array().cloned();
    for claim in claims.unwrap_or_default() {
        for scope in claim["scopes"].as_array().cloned().unwrap_or_default() {
            let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
            let target = &scope["scope"];
            let name = match target["kind"].as_str() {
                Some("symbol") => format!(
                    "{}::{}",
                    text(&target["path"]),
                    text(&target["qualified_name"])
                ),
                _ => text(&target["path"]),
            };
            out.push(format!(
                "{} {name} {}",
                text(&target["kind"]),
                text(&scope["mode"])
            ));
        }
    }
    Ok(out)
}

fn edit(path: &str, old: &str, new: &str, all: bool) -> Value {
    json!({ "file_path": path, "old_string": old, "new_string": new, "replace_all": all })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_edit_claims_that_symbol_for_body_edits() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    let done = a1.hook_input(
        "Edit",
        &edit("src/m.rs", "first();", "first(); first();", false),
    )?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(held(&a1)?, ["symbol src/m.rs::m::one edit_body"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_of_a_signature_claims_that_symbol_for_signature_edits() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    let done = a1.hook_input(
        "Edit",
        &edit("src/m.rs", "pub fn two()", "pub fn two(n: u8)", false),
    )?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(held(&a1)?, ["symbol src/m.rs::m::two edit_signature"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_across_two_symbols_claims_the_file() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    let old = "first();\n}\n\npub fn two() {\n    second();";
    let new = "first(); 1;\n}\n\npub fn two() {\n    second(); 2;";
    let done = a1.hook_input("Edit", &edit("src/m.rs", old, new, false))?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(held(&a1)?, ["file src/m.rs edit_body"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_old_string_that_occurs_twice_claims_the_file_not_the_first_match() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    let text = SOURCE.replace("third();", "first();");
    std::fs::write(a1.root().join("src/m.rs"), text)?;
    let done = a1.hook_input("Edit", &edit("src/m.rs", "first();", "other();", false))?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(held(&a1)?, ["file src/m.rs edit_body"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_all_claims_every_symbol_it_reaches_as_the_file() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    let text = SOURCE.replace("third();", "first();");
    std::fs::write(a1.root().join("src/m.rs"), text)?;
    let done = a1.hook_input("Edit", &edit("src/m.rs", "first();", "other();", true))?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(held(&a1)?, ["file src/m.rs edit_body"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_multi_edit_inside_one_symbol_claims_that_symbol() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    let input = json!({
        "file_path": "src/m.rs",
        "edits": [
            { "old_string": "first();", "new_string": "first(); a();" },
            { "old_string": "a();", "new_string": "a(); b();" },
        ],
    });
    let done = a1.hook_input("MultiEdit", &input)?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(held(&a1)?, ["symbol src/m.rs::m::one edit_body"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_that_adds_a_function_also_claims_create() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    let done = a1.hook_input(
        "Edit",
        &edit(
            "src/m.rs",
            "use std::io;",
            "use std::io;\n\nfn added() {}",
            false,
        ),
    )?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(
        held(&a1)?,
        ["file src/m.rs edit_body", "file src/m.rs create"]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_language_without_a_grammar_is_claimed_as_a_file() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    std::fs::write(a1.root().join("notes.md"), "# title\n")?;
    let done = a1.hook_input("Edit", &edit("notes.md", "title", "heading", false))?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(held(&a1)?, ["file notes.md edit_body"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_with_a_syntax_error_is_claimed_like_a_rewrite() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    std::fs::write(
        a1.root().join("src/m.rs"),
        "pub fn one( {\n    first();\n}\n",
    )?;
    let done = a1.hook_input("Edit", &edit("src/m.rs", "first();", "second();", false))?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(
        held(&a1)?,
        ["file src/m.rs edit_signature", "file src/m.rs create"]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_claims_an_existing_file_for_rewrites_and_a_new_one_for_creating() -> Result<()> {
    let (_fake, a1, _a2) = started().await?;
    let existing = json!({ "file_path": "src/m.rs", "content": "pub fn other() {}\n" });
    assert_eq!(a1.hook_input("Write", &existing)?.code, 0);
    let fresh = json!({ "file_path": "src/fresh.rs", "content": "pub fn f() {}\n" });
    assert_eq!(a1.hook_input("Write", &fresh)?.code, 0);
    assert_eq!(
        held(&a1)?,
        [
            "file src/m.rs edit_signature",
            "file src/m.rs create",
            "file src/fresh.rs create",
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fifth_symbol_in_one_file_escalates_to_the_file() -> Result<()> {
    let (_fake, a1, a2) = started().await?;
    for (old, new) in [
        ("first();", "first(); 1;"),
        ("second();", "second(); 1;"),
        ("third();", "third(); 1;"),
        ("fourth();", "fourth(); 1;"),
    ] {
        let done = a1.hook_input("Edit", &edit("src/m.rs", old, new, false))?;
        assert_eq!(done.code, 0, "{}", done.all());
    }
    let four = held(&a1)?;
    assert_eq!(four.len(), 4, "{four:?}");
    assert!(
        four.iter().all(|scope| scope.starts_with("symbol ")),
        "{four:?}"
    );

    let done = a1.hook_input("Edit", &edit("src/m.rs", "fifth();", "fifth(); 1;", false))?;
    assert_eq!(done.code, 0, "{}", done.all());
    let five = held(&a1)?;
    assert_eq!(
        five.last().map(String::as_str),
        Some("file src/m.rs edit_body"),
        "{five:?}"
    );

    // The file claim holds against others, including for symbols nobody edited yet.
    a2.start("sixth")?;
    let denied = a2.hook_input("Edit", &edit("src/m.rs", "sixth();", "sixth(); 1;", false))?;
    assert_eq!(denied.code, 2, "{}", denied.all());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_denied_symbol_claim_blocks_the_edit_and_does_not_fall_back_to_the_file() -> Result<()> {
    let (_fake, a1, a2) = started().await?;
    assert_eq!(a1.tessel(&["claim", "src/m.rs::m::one"])?.code, 0);
    a2.start("also one")?;
    let denied = a2.hook_input("Edit", &edit("src/m.rs", "first();", "first(); 2;", false))?;
    assert_eq!(denied.code, 2, "{}", denied.all());
    assert!(denied.stderr.contains("agent a1"), "{}", denied.stderr);
    assert!(
        denied.stderr.contains("src/m.rs::m::one"),
        "{}",
        denied.stderr
    );
    assert!(
        denied
            .stderr
            .contains("tessel claim src/m.rs::m::one --wait"),
        "{}",
        denied.stderr
    );
    assert_eq!(
        a2.held_claims()?,
        0,
        "nothing may be claimed behind the agent's back"
    );

    // A different function of the same file is not in the way.
    let free = a2.hook_input(
        "Edit",
        &edit("src/m.rs", "second();", "second(); 2;", false),
    )?;
    assert_eq!(free.code, 0, "{}", free.all());
    assert_eq!(held(&a2)?, ["symbol src/m.rs::m::two edit_body"]);
    Ok(())
}

fn symbol(name: &str, mode: Mode) -> ScopeClaim {
    ScopeClaim {
        scope: Scope::Symbol(SymbolId {
            path: "src/m.rs".into(),
            qualified_name: name.into(),
        }),
        mode,
    }
}

fn sent_touched(fake: &Fake) -> Vec<ScopeClaim> {
    fake.received("a1")
        .into_iter()
        .find_map(|msg| {
            let ClientMsg::Submit { touched, .. } = msg else {
                return None;
            };
            Some(touched)
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_sends_the_symbols_the_hook_claimed() -> Result<()> {
    let (fake, a1, _a2) = started_committed().await?;
    let done = a1.hook_input("Edit", &edit("src/m.rs", "first();", "first(); 1;", false))?;
    assert_eq!(done.code, 0, "{}", done.all());
    std::fs::write(
        a1.root().join("src/m.rs"),
        SOURCE.replace("first();", "first(); 1;"),
    )?;
    git(&a1.root(), &["commit", "-q", "-am", "change one"])?;

    let done = a1.tessel(&["submit", "--evidence", "cargo test passed"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(sent_touched(&fake), [symbol("m::one", Mode::EditBody)]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_symbol_claim_does_not_cover_a_commit_that_changes_another_symbol() -> Result<()> {
    let (fake, a1, _a2) = started_committed().await?;
    assert_eq!(a1.tessel(&["claim", "src/m.rs::m::one"])?.code, 0);
    std::fs::write(
        a1.root().join("src/m.rs"),
        SOURCE.replace("second();", "second(); 1;"),
    )?;
    git(&a1.root(), &["commit", "-q", "-am", "change two"])?;

    let done = a1.tessel(&["submit", "--evidence", "cargo test passed"])?;
    assert_eq!(done.code, 5, "{}", done.all());
    assert!(
        done.stdout.contains("src/m.rs::m::two (edit-body)"),
        "{}",
        done.stdout
    );
    assert!(
        sent_touched(&fake).is_empty(),
        "nothing is sent when the CLI finds it"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_addition_made_through_a_syntax_error_is_covered_and_submits() -> Result<()> {
    let (fake, a1, _a2) = started_committed().await?;
    let path = a1.root().join("src/m.rs");
    let step1 = edit("src/m.rs", "pub fn two()", "fn extra(\npub fn two()", false);
    assert_eq!(a1.hook_input("Edit", &step1)?.code, 0);
    std::fs::write(
        &path,
        SOURCE.replace("pub fn two()", "fn extra(\npub fn two()"),
    )?;
    let step2 = edit("src/m.rs", "fn extra(\n", "fn extra() {}\n", false);
    let done = a1.hook_input("Edit", &step2)?;
    assert_eq!(done.code, 0, "{}", done.all());
    std::fs::write(
        &path,
        SOURCE.replace("pub fn two()", "fn extra() {}\npub fn two()"),
    )?;
    git(&a1.root(), &["commit", "-q", "-am", "add extra"])?;

    let done = a1.tessel(&["submit", "--evidence", "cargo test passed"])?;
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(sent_touched(&fake).contains(&symbol("m::extra", Mode::Create)));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_of_more_than_256_scopes_is_refused_locally_with_exit_1() -> Result<()> {
    let (fake, a1, _a2) = world().await?;
    let root = a1.root();
    for i in 0..300 {
        std::fs::write(root.join(format!("src/g{i}.rs")), "pub fn f() {}\n")?;
    }
    git(&root, &["add", "src"])?;
    git(&root, &["commit", "-q", "-m", "many files"])?;
    a1.start("many")?;
    assert_eq!(a1.tessel(&["claim", "src/"])?.code, 0);
    for i in 0..300 {
        std::fs::write(root.join(format!("src/g{i}.rs")), "pub fn f() { 1; }\n")?;
    }
    git(&root, &["commit", "-q", "-am", "change all"])?;
    let done = a1.tessel(&["submit", "--evidence", "cargo test passed"])?;
    assert_eq!(done.code, 1, "{}", done.all());
    assert!(done.stderr.contains("at most 256"), "{}", done.stderr);
    assert!(sent_touched(&fake).is_empty());
    Ok(())
}

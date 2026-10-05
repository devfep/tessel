//! What to claim for a change, at the finest scope that stays honest. Pure: it reads text and
//! returns scopes, so the hook (before an edit) and `tessel submit` (after a commit) share one
//! rule and cannot disagree about what a change touched.

use std::collections::BTreeMap;
use std::ops::Range;

use tessel_coordinator::protocol::{uncovered, Mode, Scope, ScopeClaim, SymbolId};

use crate::symbols::{extract, language_of, Symbol};

/// An agent holding more symbol scopes than this in one file holds the file instead (invariant 6:
/// many symbol claims in one file escalate to a single file claim).
pub const MAX_SYMBOL_SCOPES_PER_FILE: usize = 4;

/// One replacement of an `Edit` or one step of a `MultiEdit`.
#[derive(Debug, Clone, Copy)]
pub struct Replace<'a> {
    pub old: &'a str,
    pub new: &'a str,
    pub all: bool,
}

pub fn file_scope(path: &str) -> Scope {
    Scope::File {
        path: path.to_string(),
    }
}

fn symbol_scope(path: &str, name: &str) -> Scope {
    Scope::Symbol(SymbolId {
        path: path.to_string(),
        qualified_name: name.to_string(),
    })
}

/// The file claims that permit every mode in `modes`: `edit_signature` also permits `edit_body`,
/// but nothing permits `create` except `create`, so a file may need two.
pub fn file_claims(path: &str, modes: &[Mode]) -> Vec<ScopeClaim> {
    let has = |wanted: Mode| modes.contains(&wanted);
    let mut out = Vec::new();
    if has(Mode::EditSignature) {
        out.push(ScopeClaim {
            scope: file_scope(path),
            mode: Mode::EditSignature,
        });
    } else if has(Mode::EditBody) || modes.is_empty() {
        out.push(ScopeClaim {
            scope: file_scope(path),
            mode: Mode::EditBody,
        });
    }
    if has(Mode::Create) {
        out.push(ScopeClaim {
            scope: file_scope(path),
            mode: Mode::Create,
        });
    }
    out
}

// ---------- what a change touched ----------

/// A symbol's text, split at its signature. Definitions that share a name (a `cfg`-gated pair,
/// overloads) are one scope, so their texts are joined.
#[derive(Default, PartialEq, Eq)]
struct Parts {
    signature: String,
    body: String,
}

struct Shape {
    symbols: BTreeMap<String, Parts>,
    /// Everything outside every symbol: imports, container headers, the gaps between symbols.
    residual: String,
}

fn shape(path: &str, source: &str) -> Option<Shape> {
    let found = extract(path, source)?;
    let mut symbols: BTreeMap<String, Parts> = BTreeMap::new();
    let mut residual = String::new();
    let mut at = 0;
    for symbol in &found {
        residual.push_str(&source[at..symbol.range.start]);
        residual.push('\0');
        at = symbol.range.end;
        let parts = symbols.entry(symbol.name.clone()).or_default();
        parts.signature.push_str(&source[symbol.signature.clone()]);
        parts.signature.push('\0');
        parts.body.push_str(&source[symbol.body()]);
        parts.body.push('\0');
    }
    residual.push_str(&source[at..]);
    Some(Shape { symbols, residual })
}

/// The scopes `after` changes relative to `before`, in `path`:
/// a symbol whose signature changed or that was removed is `edit_signature`, one whose body alone
/// changed is `edit_body`, a new one is `create`, and a change outside every symbol is `edit_body`
/// on the file. `None` when either text cannot be parsed (an unsupported language or a syntax
/// error), so the caller claims and reports the whole file. An empty list means no change the
/// extractor can see.
pub fn changed_scopes(path: &str, before: &str, after: &str) -> Option<Vec<ScopeClaim>> {
    let (old, new) = (shape(path, before)?, shape(path, after)?);
    let mut out = Vec::new();
    for (name, was) in &old.symbols {
        let mode = match new.symbols.get(name) {
            None => Mode::EditSignature,
            Some(now) if now.signature != was.signature => Mode::EditSignature,
            Some(now) if now.body != was.body => Mode::EditBody,
            Some(_) => continue,
        };
        out.push(ScopeClaim {
            scope: symbol_scope(path, name),
            mode,
        });
    }
    for name in new.symbols.keys().filter(|n| !old.symbols.contains_key(*n)) {
        out.push(ScopeClaim {
            scope: symbol_scope(path, name),
            mode: Mode::Create,
        });
    }
    if old.residual != new.residual {
        out.push(ScopeClaim {
            scope: file_scope(path),
            mode: Mode::EditBody,
        });
    }
    Some(out)
}

// ---------- the pre-edit claim ----------

/// Applies `edits` in order, each to the result of the one before. `None` when any `old` is
/// empty, absent, or ambiguous: it occurs more than once and `all` is not set.
fn apply(current: &str, edits: &[Replace<'_>]) -> Option<String> {
    let mut text = current.to_string();
    for edit in edits {
        let hits = if edit.old.is_empty() {
            0
        } else {
            text.matches(edit.old).count()
        };
        if hits == 0 || (hits > 1 && !edit.all) {
            return None;
        }
        text = text.replace(edit.old, edit.new);
    }
    Some(text)
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// The whole file, `edit_signature` if any `old` text of the edits sits on a signature of the
/// current text, else `edit_body`.
fn file_fallback(path: &str, current: &str, edits: &[Replace<'_>]) -> Vec<ScopeClaim> {
    let signatures: Vec<Range<usize>> = extract(path, current)
        .unwrap_or_default()
        .into_iter()
        .map(|symbol: Symbol| symbol.signature)
        .collect();
    let touches_signature = edits.iter().filter(|e| !e.old.is_empty()).any(|edit| {
        current.match_indices(edit.old).any(|(at, hit)| {
            signatures
                .iter()
                .any(|s| overlaps(&(at..at + hit.len()), s))
        })
    });
    let mode = if touches_signature {
        Mode::EditSignature
    } else {
        Mode::EditBody
    };
    file_claims(path, &[mode])
}

/// What to claim before `edits` are applied to the file `path` whose text is `current`.
///
/// The edits are applied in memory and the result is compared with `current` symbol by symbol, so
/// the claim matches what `tessel submit` will later report. It is one symbol when the change is
/// exactly one symbol's body (`edit_body`) or signature (`edit_signature`). Everything else is
/// the file: a change across several symbols, outside every symbol, one that adds symbols (the
/// claim then includes `create`), an `old` text that is missing or ambiguous, or a language
/// without a grammar. A syntax error before or after the edit, or an empty file, in a language with
/// a grammar claims what a rewrite does (`edit_signature` and `create`).
pub fn plan_edit(path: &str, current: &str, edits: &[Replace<'_>]) -> Vec<ScopeClaim> {
    let grammar = language_of(path).is_some();
    // Filling an empty file, or an edit that passes through a syntax error, can add any symbol in
    // a later step, so it holds what a rewrite holds.
    if grammar && current.is_empty() {
        return plan_rewrite(path);
    }
    let Some(updated) = apply(current, edits) else {
        if grammar && extract(path, current).is_none() {
            return plan_rewrite(path);
        }
        return file_fallback(path, current, edits);
    };
    let Some(changed) = changed_scopes(path, current, &updated) else {
        if grammar {
            return plan_rewrite(path);
        }
        return file_fallback(path, current, edits);
    };
    match changed.as_slice() {
        [] => file_fallback(path, current, edits),
        [only] if matches!(only.scope, Scope::Symbol(_)) && only.mode != Mode::Create => {
            vec![only.clone()]
        }
        _ => {
            let modes: Vec<Mode> = changed.iter().map(|c| c.mode).collect();
            file_claims(path, &modes)
        }
    }
}

/// A full rewrite can change anything, including adding symbols, so it holds the file for
/// signature edits and for creating.
pub fn plan_rewrite(path: &str) -> Vec<ScopeClaim> {
    file_claims(path, &[Mode::EditSignature, Mode::Create])
}

pub fn plan_create(path: &str) -> Vec<ScopeClaim> {
    file_claims(path, &[Mode::Create])
}

/// The most scope entries one `Claim`, `Amend` or `Submit` may carry. A copy of
/// `MAX_SCOPES_PER_MESSAGE` in `src/coordinator.rs`, which refuses a longer list.
pub const MAX_SCOPES_PER_MESSAGE: usize = 256;

fn symbol_path(claim: &ScopeClaim) -> Option<&str> {
    match &claim.scope {
        Scope::Symbol(id) => Some(&id.path),
        Scope::Dir { .. } | Scope::File { .. } => None,
    }
}

/// Brings `touched` down to `limit` entries by replacing the symbol scopes of a whole file with
/// file scopes in the modes those symbols need (`file_claims`). Files whose collapsed scopes the
/// `held` claims already cover go first, then the others, the most symbols first, so the least
/// information is lost. Nothing changes while `touched` fits.
pub fn collapse(touched: Vec<ScopeClaim>, held: &[ScopeClaim], limit: usize) -> Vec<ScopeClaim> {
    let mut out = touched;
    while out.len() > limit {
        let mut counts: Vec<(&str, usize, bool)> = Vec::new();
        for path in out.iter().filter_map(symbol_path) {
            if counts.iter().all(|(seen, _, _)| *seen != path) {
                let modes: Vec<Mode> = out
                    .iter()
                    .filter(|c| symbol_path(c) == Some(path))
                    .map(|c| c.mode)
                    .collect();
                let covered = uncovered(held, &file_claims(path, &modes)).is_empty();
                counts.push((path, modes.len(), covered));
            }
        }
        let Some((path, _, _)) = counts
            .iter()
            .max_by_key(|(_, symbols, covered)| (*covered, *symbols))
            .filter(|(_, symbols, _)| *symbols > 1)
            .copied()
        else {
            return out;
        };
        let path = path.to_string();
        let modes: Vec<Mode> = out
            .iter()
            .filter(|c| symbol_path(c) == Some(path.as_str()))
            .map(|c| c.mode)
            .collect();
        let at = out
            .iter()
            .position(|c| symbol_path(c) == Some(path.as_str()))
            .unwrap_or(0);
        out.retain(|c| symbol_path(c) != Some(path.as_str()));
        for (offset, claim) in file_claims(&path, &modes).into_iter().enumerate() {
            if !out.contains(&claim) {
                out.insert(at + offset, claim);
            }
        }
    }
    out
}

// ---------- escalation ----------

/// Replaces `wanted` symbol scopes by file claims in every file where `held` and `wanted`
/// together would hold more than `MAX_SYMBOL_SCOPES_PER_FILE` distinct symbols. The file claims
/// permit the modes of all of those symbols. Scopes in other files, and file or directory scopes,
/// pass through unchanged.
pub fn escalate(held: &[ScopeClaim], wanted: &[ScopeClaim]) -> Vec<ScopeClaim> {
    let mut paths: Vec<&str> = Vec::new();
    for claim in wanted {
        if let Scope::Symbol(id) = &claim.scope {
            if !paths.contains(&id.path.as_str()) {
                paths.push(&id.path);
            }
        }
    }
    let mut out = wanted.to_vec();
    for path in paths {
        let in_file = |claim: &&ScopeClaim| match &claim.scope {
            Scope::Symbol(id) => id.path == path,
            Scope::Dir { .. } | Scope::File { .. } => false,
        };
        let mut symbols: Vec<&Scope> = Vec::new();
        let mut modes: Vec<Mode> = Vec::new();
        for claim in held.iter().chain(wanted).filter(in_file) {
            if !symbols.contains(&&claim.scope) {
                symbols.push(&claim.scope);
            }
            modes.push(claim.mode);
        }
        if symbols.len() <= MAX_SYMBOL_SCOPES_PER_FILE {
            continue;
        }
        out.retain(|claim| !in_file(&claim));
        for claim in file_claims(path, &modes) {
            if !out.contains(&claim) {
                out.push(claim);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATH: &str = "src/auth.rs";
    const SOURCE: &str = "\
use std::fmt;

pub fn login(user: &str) -> bool {
    check(user)
}

pub fn logout(user: &str) {
    forget(user);
}

fn check(user: &str) -> bool {
    !user.is_empty()
}
";

    fn sym(name: &str, mode: Mode) -> ScopeClaim {
        ScopeClaim {
            scope: symbol_scope(PATH, name),
            mode,
        }
    }

    fn file(mode: Mode) -> ScopeClaim {
        ScopeClaim {
            scope: file_scope(PATH),
            mode,
        }
    }

    fn file_in(path: &str, mode: Mode) -> ScopeClaim {
        ScopeClaim {
            scope: file_scope(path),
            mode,
        }
    }

    fn one(old: &'static str, new: &'static str) -> [Replace<'static>; 1] {
        [Replace {
            old,
            new,
            all: false,
        }]
    }

    #[test]
    fn a_body_edit_claims_that_symbol_for_body_edits() {
        let edits = one("check(user)", "check(user) && true");
        assert_eq!(
            plan_edit(PATH, SOURCE, &edits),
            [sym("auth::login", Mode::EditBody)]
        );
    }

    #[test]
    fn an_edit_that_touches_a_signature_claims_that_symbol_for_signature_edits() {
        let edits = one(
            "login(user: &str) -> bool",
            "login(user: &str, n: u8) -> bool",
        );
        assert_eq!(
            plan_edit(PATH, SOURCE, &edits),
            [sym("auth::login", Mode::EditSignature)]
        );
    }

    #[test]
    fn an_edit_across_two_symbols_claims_the_file() {
        let edits = one(
            "check(user)\n}\n\npub fn logout(user: &str) {",
            "check(user) && true\n}\n\npub fn logout(user: &str, n: u8) {",
        );
        let claims = plan_edit(PATH, SOURCE, &edits);
        assert_eq!(claims, [file(Mode::EditSignature)]);
    }

    #[test]
    fn two_body_edits_in_different_symbols_claim_the_file_for_body_edits() {
        let edits = [
            Replace {
                old: "check(user)",
                new: "check(user) && true",
                all: false,
            },
            Replace {
                old: "forget(user);",
                new: "forget(user); forget(user);",
                all: false,
            },
        ];
        assert_eq!(plan_edit(PATH, SOURCE, &edits), [file(Mode::EditBody)]);
    }

    #[test]
    fn an_edit_outside_every_symbol_claims_the_file() {
        let edits = one("use std::fmt;", "use std::fmt;\nuse std::io;");
        assert_eq!(plan_edit(PATH, SOURCE, &edits), [file(Mode::EditBody)]);
    }

    #[test]
    fn an_ambiguous_old_string_claims_the_file_not_the_first_match() {
        let edits = one("user", "who");
        assert_eq!(plan_edit(PATH, SOURCE, &edits), [file(Mode::EditSignature)]);
        let in_bodies = one("(user)", "(user, 1)");
        assert_eq!(plan_edit(PATH, SOURCE, &in_bodies), [file(Mode::EditBody)]);
    }

    #[test]
    fn replace_all_applies_to_every_match_and_still_resolves_to_a_symbol_when_they_share_one() {
        let source = "fn a() {\n    x();\n    x();\n}\n\nfn b() {}\n";
        let all = [Replace {
            old: "x()",
            new: "y()",
            all: true,
        }];
        assert_eq!(
            plan_edit("src/a.rs", source, &all),
            [ScopeClaim {
                scope: symbol_scope("src/a.rs", "a::a"),
                mode: Mode::EditBody
            }]
        );
        let not_all = one("x()", "y()");
        assert_eq!(
            plan_edit("src/a.rs", source, &not_all),
            [file_in("src/a.rs", Mode::EditBody)]
        );
    }

    #[test]
    fn an_edit_that_adds_a_symbol_also_claims_create() {
        let edits = one(
            "fn check(user: &str) -> bool {",
            "fn extra() {}\n\nfn check(user: &str) -> bool {",
        );
        assert_eq!(
            plan_edit(PATH, SOURCE, &edits),
            [file(Mode::EditBody), file(Mode::Create)]
        );
    }

    #[test]
    fn a_missing_old_string_claims_the_file() {
        let edits = one("no such text", "x");
        assert_eq!(plan_edit(PATH, SOURCE, &edits), [file(Mode::EditBody)]);
    }

    #[test]
    fn a_file_that_does_not_parse_is_claimed_whole() {
        let broken = "fn a() {\n    x();\n";
        let edits = [Replace {
            old: "x();",
            new: "y();",
            all: false,
        }];
        assert_eq!(
            plan_edit("src/a.rs", broken, &edits),
            [
                file_in("src/a.rs", Mode::EditSignature),
                file_in("src/a.rs", Mode::Create)
            ]
        );
    }

    #[test]
    fn an_edit_that_breaks_the_syntax_claims_what_a_rewrite_claims() {
        let edits = one("check(user)\n}", "check(user)");
        assert_eq!(
            plan_edit(PATH, SOURCE, &edits),
            [file(Mode::EditSignature), file(Mode::Create)]
        );
        let md = [Replace {
            old: "a",
            new: "b",
            all: false,
        }];
        assert_eq!(
            plan_edit("a.md", "a", &md),
            [file_in("a.md", Mode::EditBody)]
        );
    }

    #[test]
    fn filling_an_empty_file_claims_what_a_rewrite_claims() {
        let fill = [Replace {
            old: "",
            new: "fn a() {}\n",
            all: false,
        }];
        assert_eq!(plan_edit(PATH, "", &fill), plan_rewrite(PATH));
        assert_eq!(
            plan_edit("a.md", "", &fill),
            [file_in("a.md", Mode::EditBody)]
        );
    }

    #[test]
    fn the_second_step_of_a_two_step_addition_is_covered_by_the_first() {
        let step1 = one("pub fn logout(", "fn extra(\npub fn logout(");
        let first = plan_edit(PATH, SOURCE, &step1);
        assert_eq!(first, [file(Mode::EditSignature), file(Mode::Create)]);
        let broken = SOURCE.replace("pub fn logout(", "fn extra(\npub fn logout(");
        let step2 = one("fn extra(\n", "fn extra() {}\n");
        let second = plan_edit(PATH, &broken, &step2);
        assert!(uncovered(&first, &second).is_empty(), "{second:?}");
    }

    #[test]
    fn moving_a_symbol_across_other_text_is_a_change_outside_every_symbol() {
        let before = "use a::A;\nfn f() {}use b::B;\n";
        let after = "fn f() {}use a::A;\nuse b::B;\n";
        assert_eq!(
            changed_scopes("src/lib.rs", before, after).unwrap(),
            [file_in("src/lib.rs", Mode::EditBody)]
        );
    }

    fn many(path: &str, n: usize, mode: Mode) -> Vec<ScopeClaim> {
        (0..n)
            .map(|i| ScopeClaim {
                scope: symbol_scope(path, &format!("f{i}")),
                mode,
            })
            .collect()
    }

    #[test]
    fn collapse_leaves_a_list_that_fits_alone() {
        let touched = many("src/a.rs", 5, Mode::EditBody);
        assert_eq!(collapse(touched.clone(), &[], 5), touched);
    }

    #[test]
    fn more_than_the_limit_of_symbols_collapse_to_the_file_the_claim_holds() {
        let mut touched = many("src/a.rs", MAX_SCOPES_PER_MESSAGE + 44, Mode::EditBody);
        touched.extend(many("src/b.rs", 3, Mode::EditSignature));
        let held = [file_in("src/a.rs", Mode::EditBody)];
        let got = collapse(touched, &held, MAX_SCOPES_PER_MESSAGE);
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(got.contains(&file_in("src/a.rs", Mode::EditBody)));
        assert!(uncovered(&held, &got)
            .iter()
            .all(|c| c.scope != file_scope("src/a.rs")));
    }

    #[test]
    fn collapse_keeps_the_strongest_modes_and_goes_on_until_the_list_fits() {
        let mut touched = many("src/a.rs", 200, Mode::EditBody);
        touched.push(sym_in("src/a.rs", "g", Mode::Create));
        touched.extend(many("src/b.rs", 100, Mode::EditSignature));
        touched.extend(many("src/c.rs", 10, Mode::EditBody));
        let got = collapse(touched, &[], MAX_SCOPES_PER_MESSAGE);
        assert!(got.len() <= MAX_SCOPES_PER_MESSAGE, "{}", got.len());
        assert!(
            got.contains(&file_in("src/a.rs", Mode::EditBody)),
            "{got:?}"
        );
        assert!(got.contains(&file_in("src/a.rs", Mode::Create)), "{got:?}");
        assert_eq!(
            got.iter()
                .filter(|c| c.scope == file_scope("src/b.rs"))
                .count(),
            0
        );
    }

    fn sym_in(path: &str, name: &str, mode: Mode) -> ScopeClaim {
        ScopeClaim {
            scope: symbol_scope(path, name),
            mode,
        }
    }

    #[test]
    fn a_language_without_a_grammar_claims_the_file() {
        let edits = [Replace {
            old: "title",
            new: "heading",
            all: false,
        }];
        let claims = plan_edit("docs/a.md", "# title\n", &edits);
        assert_eq!(
            claims,
            [ScopeClaim {
                scope: file_scope("docs/a.md"),
                mode: Mode::EditBody
            }]
        );
    }

    #[test]
    fn a_rewrite_claims_the_file_for_signatures_and_for_creating() {
        assert_eq!(
            plan_rewrite(PATH),
            [file(Mode::EditSignature), file(Mode::Create)]
        );
        assert_eq!(plan_create(PATH), [file(Mode::Create)]);
    }

    #[test]
    fn changed_scopes_names_each_kind_of_change() {
        let after = SOURCE
            .replace("forget(user);", "forget(user); log();")
            .replace(
                "fn check(user: &str) -> bool",
                "fn check(user: &str, n: u8) -> bool",
            )
            .replace("pub fn login", "pub fn signin")
            .replace("use std::fmt;", "use std::io;");
        let got = changed_scopes(PATH, SOURCE, &after).unwrap();
        assert_eq!(
            got,
            [
                sym("auth::check", Mode::EditSignature),
                sym("auth::login", Mode::EditSignature),
                sym("auth::logout", Mode::EditBody),
                sym("auth::signin", Mode::Create),
                file(Mode::EditBody),
            ]
        );
    }

    #[test]
    fn a_removed_symbol_is_edit_signature() {
        let after = "use std::fmt;\n\npub fn login(user: &str) -> bool {\n    check(user)\n}\n";
        let got = changed_scopes(PATH, SOURCE, after).unwrap();
        assert!(
            got.contains(&sym("auth::logout", Mode::EditSignature)),
            "{got:?}"
        );
        assert!(
            got.contains(&sym("auth::check", Mode::EditSignature)),
            "{got:?}"
        );
    }

    #[test]
    fn changed_scopes_is_none_when_either_side_does_not_parse() {
        assert!(changed_scopes(PATH, SOURCE, "fn broken( {").is_none());
        assert!(changed_scopes(PATH, "fn broken( {", SOURCE).is_none());
        assert!(changed_scopes("a.md", "x", "y").is_none());
        assert_eq!(changed_scopes(PATH, SOURCE, SOURCE), Some(vec![]));
    }

    #[test]
    fn same_named_definitions_are_one_scope() {
        let before = "#[cfg(unix)]\nfn f() { 1 }\n#[cfg(not(unix))]\nfn f() { 2 }\n";
        let after = "#[cfg(unix)]\nfn f() { 1 }\n#[cfg(not(unix))]\nfn f() { 3 }\n";
        assert_eq!(
            changed_scopes("src/lib.rs", before, after).unwrap(),
            [ScopeClaim {
                scope: symbol_scope("src/lib.rs", "f"),
                mode: Mode::EditBody
            }]
        );
    }

    fn syms(n: usize, mode: Mode) -> Vec<ScopeClaim> {
        (0..n).map(|i| sym(&format!("f{i}"), mode)).collect()
    }

    #[test]
    fn the_symbol_that_exceeds_the_limit_escalates_to_the_file() {
        let held = syms(MAX_SYMBOL_SCOPES_PER_FILE, Mode::EditBody);
        let within = escalate(
            &held[..MAX_SYMBOL_SCOPES_PER_FILE - 1],
            &[sym("new", Mode::EditBody)],
        );
        assert_eq!(within, [sym("new", Mode::EditBody)]);
        let over = escalate(&held, &[sym("new", Mode::EditBody)]);
        assert_eq!(over, [file(Mode::EditBody)]);
    }

    #[test]
    fn an_escalated_file_claim_permits_every_mode_held() {
        let mut held = syms(2, Mode::EditBody);
        held.push(sym("sig", Mode::EditSignature));
        held.push(sym("made", Mode::Create));
        let got = escalate(&held, &[sym("new", Mode::EditBody)]);
        assert_eq!(got, [file(Mode::EditSignature), file(Mode::Create)]);
        for mode in [Mode::EditBody, Mode::EditSignature, Mode::Create] {
            let touched = [ScopeClaim {
                scope: symbol_scope(PATH, "x"),
                mode,
            }];
            assert!(
                tessel_coordinator::protocol::uncovered(&got, &touched).is_empty(),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn escalation_leaves_other_files_and_non_symbol_scopes_alone() {
        let held = syms(MAX_SYMBOL_SCOPES_PER_FILE, Mode::EditBody);
        let other = ScopeClaim {
            scope: symbol_scope("src/other.rs", "g"),
            mode: Mode::EditBody,
        };
        let dir = ScopeClaim {
            scope: Scope::Dir {
                path: "docs".into(),
            },
            mode: Mode::EditBody,
        };
        let wanted = vec![other.clone(), sym("new", Mode::EditBody), dir.clone()];
        assert_eq!(escalate(&held, &wanted), [other, dir, file(Mode::EditBody)]);
    }

    #[test]
    fn a_symbol_already_held_is_not_counted_twice() {
        let held = syms(MAX_SYMBOL_SCOPES_PER_FILE, Mode::EditBody);
        let again = vec![sym("f0", Mode::EditSignature)];
        assert_eq!(escalate(&held, &again), again);
    }
}

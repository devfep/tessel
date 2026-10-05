//! The parts of `tessel submit` that do not need the daemon: choosing the claim, resolving the
//! commit, computing the touched scopes from git and building the decision record.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};
use tessel_coordinator::protocol::{DecisionRecord, Mode, RejectedApproach, Scope, ScopeClaim};

use crate::plan::changed_scopes;
use crate::render::escape;
use crate::scope;
use crate::state::{HeldClaim, State};

/// The claim to submit: `wanted`, or the only claim that is not already submitted.
pub fn pick_claim(state: &State, wanted: Option<u64>) -> anyhow::Result<&HeldClaim> {
    if let Some(id) = wanted {
        let Some(held) = state.claims.iter().find(|held| held.claim.0 == id) else {
            bail!("no held claim {id}; `tessel status` lists the claims this worktree holds");
        };
        if held.submitted {
            bail!("claim {id} is already submitted; watch `tessel inbox` for the merge");
        }
        return Ok(held);
    }
    let open: Vec<&HeldClaim> = state.claims.iter().filter(|held| !held.submitted).collect();
    match open.as_slice() {
        [only] => Ok(only),
        [] if state.claims.is_empty() => {
            bail!("no claim is held; claim what you changed first with `tessel claim`")
        }
        [] => bail!("every held claim is already submitted; watch `tessel inbox` for the merge"),
        several => {
            let ids: Vec<String> = several
                .iter()
                .map(|held| held.claim.0.to_string())
                .collect();
            bail!(
                "this worktree holds several claims ({}); name one with --claim <id>",
                ids.join(", ")
            )
        }
    }
}

/// `"<approach>::<reason>"` split at the first `::`.
fn parse_rejected(arg: &str) -> anyhow::Result<RejectedApproach> {
    let Some((approach, reason)) = arg.split_once("::") else {
        bail!("--rejected {arg:?} must read \"<approach>::<reason>\"");
    };
    let (approach, reason) = (approach.trim(), reason.trim());
    if approach.is_empty() || reason.is_empty() {
        bail!("--rejected {arg:?} needs both an approach and a reason around the `::`");
    }
    Ok(RejectedApproach {
        approach: approach.to_string(),
        reason: reason.to_string(),
    })
}

/// The decision record to send. Evidence is required: a submission without any is held for human
/// review (invariant 12), so it is refused here.
pub fn decisions(evidence: &[String], rejected: &[String]) -> anyhow::Result<DecisionRecord> {
    let evidence: Vec<String> = evidence
        .iter()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .collect();
    if evidence.is_empty() {
        bail!(
            "at least one --evidence \"<text>\" is required, for example --evidence \"cargo test \
             auth:: passed (42 tests)\". A submission without evidence is held for human review \
             before it can merge"
        );
    }
    let mut record = DecisionRecord {
        evidence,
        ..DecisionRecord::default()
    };
    for arg in rejected {
        record.rejected.push(parse_rejected(arg)?);
    }
    Ok(record)
}

/// The full 40-hex id of `rev` (default `HEAD`) in the worktree at `root`.
pub fn resolve_commit(root: &Path, rev: Option<&str>) -> anyhow::Result<String> {
    let explicit = rev.is_some();
    let rev = rev.unwrap_or("HEAD");
    let what = if explicit {
        format!("--commit {}", escape(rev))
    } else {
        format!("{} (the current commit)", escape(rev))
    };
    if rev.starts_with('-') {
        bail!("{what} is not a commit");
    }
    let out = git(
        root,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
    .with_context(|| format!("{what} is not a commit in this repository"))?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!(
            "git resolved {} to {sha:?}, which is not a 40-hex commit id",
            escape(rev)
        );
    }
    Ok(sha)
}

/// The commit to diff `work` from, in order: the coordinator's head when it is `work` or in its
/// history and the start commit is not newer; else the fork point of that head and `work` when
/// the commit the daemon started at is at or before it; else the start commit when that is in
/// `work`'s history. The coordinator's head is preferred so a lane that merged the trunk into
/// its branch is not charged with other agents' changes, and its fork point serves a lane whose
/// trunk has since moved on. The head goes stale when work lands outside the coordinator, and
/// every worktree shares one object store, so a stale head can exist locally, on another line of
/// history or behind the start commit; either is older than the start commit, so the start
/// commit is used instead. Neither moves when the connection drops (the `Hello` base does, so it
/// is never used here). Fails when none applies, because a diff from the wrong commit hides or
/// invents changed files.
pub fn diff_base(root: &Path, state: &State, work: &str) -> anyhow::Result<String> {
    if let Some(head) = state.coordinator_head.as_deref() {
        let start_is_newer = is_ancestor(root, head, &state.start_base)
            && head != state.start_base
            && is_ancestor(root, &state.start_base, work);
        if is_ancestor(root, head, work) && !start_is_newer {
            return Ok(head.to_string());
        }
        if is_commit(root, head) && is_commit(root, work) {
            if let Ok(fork_point) = merge_base(root, head, work) {
                if is_ancestor(root, &state.start_base, &fork_point) {
                    return Ok(fork_point);
                }
            }
        }
    }
    if is_ancestor(root, &state.start_base, work) {
        return Ok(state.start_base.clone());
    }
    bail!(
        "cannot tell what this work is based on: neither the coordinator's head ({}) nor the \
         commit this work started from ({}) is {} or in its history. Merge or rebase onto one of \
         them, or fetch the coordinator's head into this repository. A restart does not repair \
         this while you hold claims, because it keeps the start commit that was pinned before",
        state
            .coordinator_head
            .as_deref()
            .map_or_else(|| "unknown".into(), escape),
        escape(&state.start_base),
        escape(work)
    )
}

/// Whether `ancestor` is `descendant` or lies in its history.
pub fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> bool {
    is_commit(root, ancestor)
        && is_commit(root, descendant)
        && git(root, &["merge-base", "--is-ancestor", ancestor, descendant]).is_ok()
}

fn is_commit(root: &Path, rev: &str) -> bool {
    !rev.is_empty()
        && !rev.starts_with('-')
        && git(root, &["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_ok()
}

/// What `commit` changed since `base`. A file is one scope per change: an added file is `create`,
/// a deleted or type-changed one `edit-signature` (which triggers review), and a rename is
/// `edit-signature` on the old path plus `create` on the new. A modified file in a language with
/// a grammar is one scope per changed symbol (see `plan::changed_scopes`); a modified file in any
/// other language, or one that does not parse on either side, is `edit-body` on the file. The
/// diff runs from the merge base of `base` and `commit`, so work that landed on `base`'s side
/// since the fork is not counted.
pub fn touched(root: &Path, base: &str, commit: &str) -> anyhow::Result<Vec<ScopeClaim>> {
    let range = format!("{base}...{commit}");
    let out = git(
        root,
        &["diff", "--name-status", "-z", "-M", "--no-ext-diff", &range],
    )
    .with_context(|| {
        format!("cannot compute what {commit} changed since its common ancestor with {base}")
    })?;
    let files = parse_name_status(&out)?;
    if !files
        .iter()
        .any(|claim| claim.mode == Mode::EditBody && matches!(claim.scope, Scope::File { .. }))
    {
        return Ok(files);
    }
    let fork_point = merge_base(root, base, commit)?;
    let mut out = Vec::new();
    for claim in files {
        let Scope::File { path } = &claim.scope else {
            out.push(claim);
            continue;
        };
        if claim.mode != Mode::EditBody {
            out.push(claim);
            continue;
        }
        let symbols = changed_symbols(root, &fork_point, commit, path);
        match symbols {
            Some(scopes) => out.extend(scopes),
            None => out.push(claim),
        }
    }
    Ok(out)
}

fn merge_base(root: &Path, base: &str, commit: &str) -> anyhow::Result<String> {
    let out = git(root, &["merge-base", base, commit])
        .with_context(|| format!("cannot find the common ancestor of {base} and {commit}"))?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// The symbol-level scopes of one modified file, or `None` when the file has to be claimed whole:
/// a language without a grammar, a syntax error on either side, text that is not UTF-8, or a
/// change the extractor cannot see.
fn changed_symbols(root: &Path, from: &str, to: &str, path: &str) -> Option<Vec<ScopeClaim>> {
    let before = blob(root, from, path)?;
    let after = blob(root, to, path)?;
    let scopes = changed_scopes(path, &before, &after)?;
    (!scopes.is_empty()).then_some(scopes)
}

fn blob(root: &Path, rev: &str, path: &str) -> Option<String> {
    let out = git(root, &["show", &format!("{rev}:{path}")]).ok()?;
    String::from_utf8(out).ok()
}

fn parse_name_status(raw: &[u8]) -> anyhow::Result<Vec<ScopeClaim>> {
    let mut fields = raw
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut out = Vec::new();
    while let Some(status) = fields.next() {
        let status = String::from_utf8_lossy(status).into_owned();
        let mut path = || -> anyhow::Result<String> {
            let bytes = fields
                .next()
                .with_context(|| format!("git diff status {status} has no path"))?;
            let text = std::str::from_utf8(bytes).map_err(|_| {
                anyhow::anyhow!(
                    "a changed path is not valid UTF-8, so no scope can name it: {}",
                    escape(&String::from_utf8_lossy(bytes))
                )
            })?;
            Ok(text.to_string())
        };
        match status.chars().next() {
            Some('A') => out.push(file_claim(&path()?, Mode::Create)?),
            Some('M') => out.push(file_claim(&path()?, Mode::EditBody)?),
            // A file that became a symlink or back is not a body edit.
            Some('D' | 'T') => out.push(file_claim(&path()?, Mode::EditSignature)?),
            Some('R') => {
                let (old, new) = (path()?, path()?);
                out.push(file_claim(&old, Mode::EditSignature)?);
                out.push(file_claim(&new, Mode::Create)?);
            }
            Some('C') => {
                let (_source, copy) = (path()?, path()?);
                out.push(file_claim(&copy, Mode::Create)?);
            }
            _ => bail!("git diff reported an unsupported change {status:?}"),
        }
    }
    Ok(out)
}

fn file_claim(path: &str, mode: Mode) -> anyhow::Result<ScopeClaim> {
    let scope: Scope = scope::file(path).with_context(|| format!("cannot claim {path:?}"))?;
    Ok(ScopeClaim { scope, mode })
}

fn git(root: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .context("cannot run git; is git installed?")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_coordinator::protocol::SymbolId;

    fn run(root: &Path, args: &[&str]) {
        git(root, args).unwrap();
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/keep.rs"), "pub fn keep() {}\n").unwrap();
        std::fs::write(root.join("src/edit.rs"), "pub fn edit() {}\n").unwrap();
        std::fs::write(root.join("src/gone.rs"), "pub fn gone() {}\n").unwrap();
        std::fs::write(
            root.join("src/old.rs"),
            "pub fn moved() { /* body */ }\n".repeat(20),
        )
        .unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.test"],
            vec!["config", "user.name", "Test"],
            vec!["add", "src"],
            vec!["commit", "-q", "-m", "base"],
        ] {
            run(root, &args);
        }
        dir
    }

    fn commit_all(root: &Path, message: &str) {
        run(root, &["add", "-A"]);
        run(root, &["commit", "-q", "-m", message]);
    }

    fn claim(path: &str, mode: Mode) -> ScopeClaim {
        ScopeClaim {
            scope: Scope::File { path: path.into() },
            mode,
        }
    }

    fn symbol(path: &str, name: &str, mode: Mode) -> ScopeClaim {
        ScopeClaim {
            scope: Scope::Symbol(SymbolId {
                path: path.into(),
                qualified_name: name.into(),
            }),
            mode,
        }
    }

    fn head(root: &Path) -> String {
        resolve_commit(root, None).unwrap()
    }

    fn commit_file(root: &Path, path: &str, text: &str) -> String {
        std::fs::write(root.join(path), text).unwrap();
        commit_all(root, "edit");
        head(root)
    }

    #[test]
    fn an_added_file_is_create_and_a_modified_one_is_its_changed_symbol() {
        let dir = repo();
        let root = dir.path();
        let base = head(root);
        std::fs::write(root.join("src/new.rs"), "pub fn new() {}\n").unwrap();
        std::fs::write(root.join("src/edit.rs"), "pub fn edit() { 1; }\n").unwrap();
        commit_all(root, "change");
        let got = touched(root, &base, &head(root)).unwrap();
        assert_eq!(
            got,
            vec![
                symbol("src/edit.rs", "edit::edit", Mode::EditBody),
                claim("src/new.rs", Mode::Create)
            ]
        );
    }

    #[test]
    fn a_deleted_file_is_edit_signature_on_the_old_path() {
        let dir = repo();
        let root = dir.path();
        let base = head(root);
        std::fs::remove_file(root.join("src/gone.rs")).unwrap();
        commit_all(root, "delete");
        let got = touched(root, &base, &head(root)).unwrap();
        assert_eq!(got, vec![claim("src/gone.rs", Mode::EditSignature)]);
    }

    #[test]
    fn a_rename_is_edit_signature_on_the_old_path_and_create_on_the_new() {
        let dir = repo();
        let root = dir.path();
        let base = head(root);
        run(root, &["mv", "src/old.rs", "src/renamed.rs"]);
        commit_all(root, "rename");
        let got = touched(root, &base, &head(root)).unwrap();
        assert_eq!(
            got,
            vec![
                claim("src/old.rs", Mode::EditSignature),
                claim("src/renamed.rs", Mode::Create)
            ]
        );
    }

    #[test]
    fn work_that_landed_on_the_base_branch_since_is_not_counted() {
        let dir = repo();
        let root = dir.path();
        let base = head(root);
        run(root, &["checkout", "-q", "-b", "side"]);
        std::fs::write(root.join("src/edit.rs"), "pub fn edit() { 2; }\n").unwrap();
        commit_all(root, "side work");
        let side = head(root);
        run(root, &["checkout", "-q", "-"]);
        std::fs::write(root.join("src/keep.rs"), "pub fn keep() { 3; }\n").unwrap();
        commit_all(root, "main moves");
        let main_tip = head(root);
        // From the fork point and from the newer tip of the other side, only the side branch's own
        // change counts: a two-dot diff from `main_tip` would also list main's keep.rs.
        for from in [&base, &main_tip] {
            let got = touched(root, from, &side).unwrap();
            assert_eq!(
                got,
                vec![symbol("src/edit.rs", "edit::edit", Mode::EditBody)],
                "from {from}"
            );
        }
    }

    const TWO_FNS: &str =
        "use std::io;\n\npub fn one() {\n    a();\n}\n\npub fn two() {\n    b();\n}\n";

    /// Commits `TWO_FNS` as the base, then `text`, and returns what the second commit touched.
    fn touched_by(path: &str, base_text: &str, text: &str) -> Vec<ScopeClaim> {
        let dir = repo();
        let root = dir.path();
        let base = commit_file(root, path, base_text);
        let tip = commit_file(root, path, text);
        touched(root, &base, &tip).unwrap()
    }

    #[test]
    fn a_body_only_change_is_edit_body_on_that_symbol() {
        let text = TWO_FNS.replace("a();", "a(); a();");
        assert_eq!(
            touched_by("src/m.rs", TWO_FNS, &text),
            [symbol("src/m.rs", "m::one", Mode::EditBody)]
        );
    }

    #[test]
    fn a_signature_change_is_edit_signature_on_that_symbol() {
        let text = TWO_FNS.replace("pub fn two()", "pub fn two(n: u8)");
        assert_eq!(
            touched_by("src/m.rs", TWO_FNS, &text),
            [symbol("src/m.rs", "m::two", Mode::EditSignature)]
        );
    }

    #[test]
    fn an_added_symbol_is_create_and_a_removed_one_is_edit_signature() {
        let text = TWO_FNS.replace("pub fn two() {\n    b();\n}\n", "pub fn three() {}\n");
        assert_eq!(
            touched_by("src/m.rs", TWO_FNS, &text),
            [
                symbol("src/m.rs", "m::two", Mode::EditSignature),
                symbol("src/m.rs", "m::three", Mode::Create),
            ]
        );
    }

    #[test]
    fn a_change_outside_every_symbol_is_edit_body_on_the_file() {
        let text = TWO_FNS.replace("use std::io;", "use std::io;\nuse std::fmt;");
        assert_eq!(
            touched_by("src/m.rs", TWO_FNS, &text),
            [claim("src/m.rs", Mode::EditBody)]
        );
    }

    #[test]
    fn a_file_that_does_not_parse_on_either_side_is_edit_body_on_the_file() {
        let broken = TWO_FNS.replace("pub fn one() {", "pub fn one( {");
        let fixed = TWO_FNS.replace("a();", "a(); a();");
        let file = [claim("src/m.rs", Mode::EditBody)];
        assert_eq!(touched_by("src/m.rs", TWO_FNS, &broken), file);
        assert_eq!(touched_by("src/m.rs", &broken, &fixed), file);
    }

    #[test]
    fn a_language_without_a_grammar_is_edit_body_on_the_file() {
        assert_eq!(
            touched_by("notes.md", "# one\n", "# two\n"),
            [claim("notes.md", Mode::EditBody)]
        );
    }

    #[test]
    fn symbols_are_compared_from_the_fork_point_not_from_the_base() {
        let dir = repo();
        let root = dir.path();
        let fork = commit_file(root, "src/m.rs", TWO_FNS);
        run(root, &["checkout", "-q", "-b", "side"]);
        let side = commit_file(root, "src/m.rs", &TWO_FNS.replace("b();", "b(); b();"));
        run(root, &["checkout", "-q", "-"]);
        let main_tip = commit_file(root, "src/m.rs", &TWO_FNS.replace("a();", "a(); a();"));
        for from in [&fork, &main_tip] {
            assert_eq!(
                touched(root, from, &side).unwrap(),
                [symbol("src/m.rs", "m::two", Mode::EditBody)],
                "from {from}"
            );
        }
    }

    #[test]
    fn a_file_name_with_a_double_colon_or_a_space_stays_one_file_scope() {
        let dir = repo();
        let root = dir.path();
        let base = head(root);
        std::fs::write(root.join("src/a::b c.rs"), "x\n").unwrap();
        commit_all(root, "odd name");
        let got = touched(root, &base, &head(root)).unwrap();
        assert_eq!(got, vec![claim("src/a::b c.rs", Mode::Create)]);
    }

    #[test]
    fn a_file_that_becomes_a_symlink_is_edit_signature() {
        let dir = repo();
        let root = dir.path();
        let base = head(root);
        std::fs::remove_file(root.join("src/edit.rs")).unwrap();
        std::os::unix::fs::symlink("keep.rs", root.join("src/edit.rs")).unwrap();
        commit_all(root, "link");
        let got = touched(root, &base, &head(root)).unwrap();
        assert_eq!(got, vec![claim("src/edit.rs", Mode::EditSignature)]);
    }

    fn state_with(start_base: &str, coordinator_head: Option<&str>) -> State {
        State {
            pid: 1,
            agent: "a1".into(),
            repo: "demo".into(),
            summary: String::new(),
            task_ref: None,
            start_base: start_base.into(),
            coordinator_head: coordinator_head.map(str::to_string),
            base: "moves-with-every-connection".into(),
            socket: String::new(),
            connection: crate::state::Connection::Online,
            lease_ms: None,
            last_error: None,
            claims: Vec::new(),
            queued: None,
            updated_at_ms: 0,
        }
    }

    /// Commits `path` on a side branch cut from the current commit, then returns to where it was.
    fn commit_on_side_branch(root: &Path, path: &str) -> String {
        let here = head(root);
        run(root, &["checkout", "-q", "-b", "side", &here]);
        let side = commit_file(root, path, "pub fn side() {}\n");
        run(root, &["checkout", "-q", &here]);
        run(root, &["branch", "-q", "-D", "side"]);
        side
    }

    #[test]
    fn the_diff_base_is_the_coordinator_head_when_it_is_in_the_work_s_history() {
        let dir = repo();
        let root = dir.path();
        let start = head(root);
        let tip = commit_file(root, "src/keep.rs", "pub fn keep() { 9; }\n");
        let work = commit_file(root, "src/edit.rs", "pub fn edit() { 9; }\n");
        let both = state_with(&start, Some(&tip));
        assert_eq!(diff_base(root, &both, &work).unwrap(), tip);
        let equal = state_with(&start, Some(&work));
        assert_eq!(diff_base(root, &equal, &work).unwrap(), work);
    }

    #[test]
    fn the_diff_base_is_the_start_commit_when_the_coordinator_head_is_unusable() {
        let dir = repo();
        let root = dir.path();
        let start = head(root);
        let unrelated = commit_on_side_branch(root, "src/side.rs");
        let work = commit_file(root, "src/edit.rs", "pub fn edit() { 9; }\n");
        let absent = "1".repeat(40);
        for coordinator_head in [Some(unrelated.as_str()), Some(absent.as_str()), None] {
            let state = state_with(&start, coordinator_head);
            assert_eq!(
                diff_base(root, &state, &work).unwrap(),
                start,
                "{coordinator_head:?}"
            );
        }
    }

    #[test]
    fn an_unrelated_coordinator_head_does_not_charge_the_lane_with_other_work() {
        let dir = repo();
        let root = dir.path();
        let older = commit_on_side_branch(root, "src/side.rs");
        let start = commit_file(root, "src/keep.rs", "pub fn keep() { 9; }\n");
        let work = commit_file(root, "src/edit.rs", "pub fn edit() { 9; }\n");
        let state = state_with(&start, Some(&older));
        let base = diff_base(root, &state, &work).unwrap();
        let got = touched(root, &base, &work).unwrap();
        assert_eq!(
            got,
            vec![symbol("src/edit.rs", "edit::edit", Mode::EditBody)]
        );
    }

    #[test]
    fn a_coordinator_head_ahead_of_the_work_diffs_from_its_fork_point() {
        let dir = repo();
        let root = dir.path();
        let start = head(root);
        let merged_trunk = commit_file(root, "src/keep.rs", "pub fn keep() { 9; }\n");
        let trunk_moved_on = commit_on_side_branch(root, "src/side.rs");
        let work = commit_file(root, "src/edit.rs", "pub fn edit() { 9; }\n");
        let state = state_with(&start, Some(&trunk_moved_on));
        let base = diff_base(root, &state, &work).unwrap();
        assert_eq!(base, merged_trunk);
        let got = touched(root, &base, &work).unwrap();
        assert_eq!(
            got,
            vec![symbol("src/edit.rs", "edit::edit", Mode::EditBody)]
        );
    }

    #[test]
    fn a_coordinator_head_older_than_the_start_commit_is_not_the_diff_base() {
        let dir = repo();
        let root = dir.path();
        let stale = head(root);
        let start = commit_file(root, "src/keep.rs", "pub fn keep() { 9; }\n");
        let work = commit_file(root, "src/edit.rs", "pub fn edit() { 9; }\n");
        let state = state_with(&start, Some(&stale));
        let base = diff_base(root, &state, &work).unwrap();
        assert_eq!(base, start);
        let got = touched(root, &base, &work).unwrap();
        assert_eq!(
            got,
            vec![symbol("src/edit.rs", "edit::edit", Mode::EditBody)]
        );
    }

    #[test]
    fn the_error_names_both_commits_when_they_differ() {
        let dir = repo();
        let root = dir.path();
        let unrelated = commit_on_side_branch(root, "src/side.rs");
        let work = commit_file(root, "src/edit.rs", "pub fn edit() { 9; }\n");
        let absent = "1".repeat(40);
        let state = state_with(&absent, Some(&unrelated));
        let err = diff_base(root, &state, &work).unwrap_err().to_string();
        assert!(err.contains(&absent) && err.contains(&unrelated), "{err}");
    }

    #[test]
    fn with_no_base_in_the_work_s_history_the_diff_fails_closed_naming_both_commits() {
        let dir = repo();
        let root = dir.path();
        let unrelated = commit_on_side_branch(root, "src/side.rs");
        let work = commit_file(root, "src/edit.rs", "pub fn edit() { 9; }\n");
        let absent = "1".repeat(40);
        let cases = [
            state_with("", None),
            state_with(&absent, Some(&absent)),
            state_with(&unrelated, Some(&unrelated)),
        ];
        for state in cases {
            let err = diff_base(root, &state, &work).unwrap_err().to_string();
            assert!(
                err.contains("cannot tell what this work is based on"),
                "{err}"
            );
            assert!(err.contains(&escape(&state.start_base)), "{err}");
            let named = state.coordinator_head.as_deref().unwrap_or("unknown");
            assert!(err.contains(&escape(named)), "{err}");
        }
    }

    #[test]
    fn no_change_touches_nothing() {
        let dir = repo();
        let base = head(dir.path());
        assert!(touched(dir.path(), &base, &base).is_ok_and(|touched| touched.is_empty()));
    }

    #[test]
    fn an_unknown_base_is_an_error_that_names_the_commit() {
        let dir = repo();
        let err = touched(dir.path(), &"0".repeat(40), &head(dir.path()));
        assert!(err.is_err_and(|e| format!("{e:#}").contains("cannot compute")));
    }

    #[test]
    fn a_commit_resolves_to_forty_hex_and_a_bad_one_is_refused() {
        let dir = repo();
        let sha = head(dir.path());
        assert_eq!(sha.len(), 40);
        assert_eq!(resolve_commit(dir.path(), Some("HEAD")).unwrap(), sha);
        assert_eq!(resolve_commit(dir.path(), Some(&sha[..10])).unwrap(), sha);
        assert!(resolve_commit(dir.path(), Some("nonsense")).is_err());
        assert!(resolve_commit(dir.path(), Some("--output=x")).is_err());
    }

    #[test]
    fn the_error_names_the_flag_only_when_the_flag_was_given() {
        let empty = tempfile::tempdir().unwrap();
        run(empty.path(), &["init", "-q"]);
        let implicit = format!("{:#}", resolve_commit(empty.path(), None).unwrap_err());
        assert!(implicit.contains("HEAD (the current commit)"), "{implicit}");
        assert!(!implicit.contains("--commit"), "{implicit}");
        let explicit = format!(
            "{:#}",
            resolve_commit(empty.path(), Some("nonsense")).unwrap_err()
        );
        assert!(explicit.contains("--commit nonsense"), "{explicit}");
    }

    #[test]
    fn evidence_is_required_and_blank_evidence_does_not_count() {
        let err = decisions(&[], &[]).err().map(|e| e.to_string()).unwrap();
        assert!(
            err.contains("--evidence") && err.contains("review"),
            "{err}"
        );
        assert!(decisions(&["  ".into()], &[]).is_err());
        let record = decisions(&["cargo test passed".into()], &[]).unwrap();
        assert_eq!(record.evidence, vec!["cargo test passed".to_string()]);
    }

    #[test]
    fn a_rejected_approach_splits_at_the_first_double_colon() {
        let evidence = vec!["tests passed".to_string()];
        let record = decisions(&evidence, &["use a mutex::deadlocks with a::b".into()]).unwrap();
        assert_eq!(record.rejected.len(), 1);
        assert_eq!(record.rejected[0].approach, "use a mutex");
        assert_eq!(record.rejected[0].reason, "deadlocks with a::b");
        assert!(decisions(&evidence, &["no separator".into()]).is_err());
        assert!(decisions(&evidence, &["::reason only".into()]).is_err());
        assert!(decisions(&evidence, &["approach only::".into()]).is_err());
    }
}

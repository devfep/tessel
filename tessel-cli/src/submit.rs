//! The parts of `tessel submit` that do not need the daemon: choosing the claim, resolving the
//! commit, computing the touched scopes from git and building the decision record.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};
use tessel_coordinator::protocol::{DecisionRecord, Mode, RejectedApproach, Scope, ScopeClaim};

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
/// review (invariant 12), and review approval is not built yet, so it would never merge.
pub fn decisions(evidence: &[String], rejected: &[String]) -> anyhow::Result<DecisionRecord> {
    let evidence: Vec<String> = evidence
        .iter()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .collect();
    if evidence.is_empty() {
        bail!(
            "at least one --evidence \"<text>\" is required, for example --evidence \"cargo test \
             auth:: passed (42 tests)\". A submission without evidence is held for human review, \
             and review approval is not built yet, so it would never merge"
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
    let rev = rev.unwrap_or("HEAD");
    if rev.starts_with('-') {
        bail!("--commit {} is not a commit", escape(rev));
    }
    let out = git(
        root,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
    .with_context(|| {
        format!(
            "--commit {} is not a commit in this repository",
            escape(rev)
        )
    })?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!(
            "git resolved {} to {sha:?}, which is not a 40-hex commit id",
            escape(rev)
        );
    }
    Ok(sha)
}

/// What `commit` changed since `base`, one file-level scope per change:
/// an added file is `create`, a modified one `edit-body`, a deleted one `edit-signature` (which
/// triggers review), and a rename is `edit-signature` on the old path plus `create` on the new.
/// The diff runs from the merge base, so work that landed on `base`'s branch since is not counted.
pub fn touched(root: &Path, base: &str, commit: &str) -> anyhow::Result<Vec<ScopeClaim>> {
    let range = format!("{base}...{commit}");
    let out = git(
        root,
        &["diff", "--name-status", "-z", "-M", "--no-ext-diff", &range],
    )
    .with_context(|| format!("cannot compute what {commit} changed since {base}"))?;
    parse_name_status(&out)
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
            Some('M' | 'T') => out.push(file_claim(&path()?, Mode::EditBody)?),
            Some('D') => out.push(file_claim(&path()?, Mode::EditSignature)?),
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

    fn head(root: &Path) -> String {
        resolve_commit(root, None).unwrap()
    }

    #[test]
    fn an_added_file_is_create_and_a_modified_one_is_edit_body() {
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
                claim("src/edit.rs", Mode::EditBody),
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
        let got = touched(root, &base, &side).unwrap();
        assert_eq!(got, vec![claim("src/edit.rs", Mode::EditBody)]);
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

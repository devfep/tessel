//! Where the staged landing keeps the head a `Prepare` made: a ref on the repository it lands on
//! (`docs/proposals/candidate-ref.md`).
//!
//! `refs/tessel/candidate/<head sha>`: the name carries the head, so `Test` and `Land`, which are
//! asked about `{base, head}` and nothing else, find it, and a retry pushes the same name. A ref
//! outside `refs/heads` triggers no push workflow, matches no branch ruleset and is not fetched by a
//! default clone. If the host refuses a push to it, [`CANDIDATE_NAMESPACE`] becomes
//! `refs/heads/tessel/candidate/` and nothing else changes; the probe of
//! `scripts/dogfood/probe-candidate-ref.sh` decides.
//!
//! The ref's name and tip are untrusted: any account with push access to the repository can write
//! under any non-main ref. A listing is parsed strictly, and a tip other than the head the name
//! carries is read as a missing ref.

use crate::git::Git;
use crate::merge::{
    land, BoundaryError, CandidateWrite, MergeContext, MergeError, MergePorts, PushUpdate,
};
use crate::outcome::MergeOutcome;
use crate::run::GitRunner;
use crate::types::Sha;
use crate::workspace::{clone_main, CloneOutcome, Workspace};

/// The namespace of the candidate refs, with its trailing slash.
pub const CANDIDATE_NAMESPACE: &str = "refs/tessel/candidate/";
/// The most refs one sweep deletes, so a damaged remote cannot push a call past its deadline.
pub const MAX_SWEPT: usize = 50;
/// The most listing lines one sweep reads.
pub const MAX_LISTED: usize = 1000;

/// The ref the head `head` is kept on.
pub fn candidate_ref(head: &Sha) -> String {
    format!("{CANDIDATE_NAMESPACE}{head}")
}

/// One ref of a listing: the head its name carries and the commit it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub named: Sha,
    pub tip: Sha,
}

impl Listed {
    /// The ref's full name.
    pub fn reference(&self) -> String {
        candidate_ref(&self.named)
    }
}

/// Reads `git ls-remote` output, keeping only lines that are exactly `<40 hex> TAB
/// refs/tessel/candidate/<40 hex>`, at most [`MAX_LISTED`] of them. Anything else is skipped: another
/// writer controls those names.
pub fn parse_listing(stdout: &str) -> Vec<Listed> {
    let mut listed = Vec::new();
    for line in stdout.lines().take(MAX_LISTED) {
        let Some((tip, name)) = line.split_once('\t') else {
            continue;
        };
        let Some(named) = name.strip_prefix(CANDIDATE_NAMESPACE) else {
            continue;
        };
        if let (Ok(tip), Ok(named)) = (Sha::parse(tip), Sha::parse(named)) {
            listed.push(Listed { named, tip });
        }
    }
    listed
}

fn port(operation: &'static str, message: impl Into<String>) -> MergeError {
    MergeError::Port(BoundaryError {
        operation,
        message: message.into(),
    })
}

/// Where the candidate ref of `head` points on `remote`, or `None` when there is no such ref. A
/// ref whose tip is not `head` is returned as it is: the caller decides it is missing.
pub fn read_candidate(
    git: &Git,
    runner: &dyn GitRunner,
    remote: &str,
    head: &Sha,
) -> Result<Option<Sha>, MergeError> {
    let reference = candidate_ref(head);
    let output = runner.run(&git.ls_remote_command(remote, &reference))?;
    if !output.success() {
        return Err(port(
            "read the candidate ref",
            format!("git ls-remote exited {:?}", output.exit_code),
        ));
    }
    Ok(parse_listing(&output.stdout)
        .into_iter()
        .find(|listed| listed.named == *head)
        .map(|listed| listed.tip))
}

/// Pushes `head`, which `workspace` holds, to its candidate ref on `remote`. The ref is the
/// lander's own, so an existing one of the same name is overwritten.
pub fn push_candidate(
    git: &Git,
    runner: &dyn GitRunner,
    ports: &impl MergePorts,
    workspace: &Workspace,
    remote: &str,
    head: &Sha,
) -> Result<(), MergeError> {
    let push = git.push_ref_command(workspace, remote, head, &candidate_ref(head));
    let output =
        ports.with_candidate_access(&CandidateWrite::Push { head }, || runner.run(&push))??;
    if output.success() {
        Ok(())
    } else {
        Err(port(
            "push the candidate ref",
            format!("git push exited {:?}", output.exit_code),
        ))
    }
}

/// Deletes `reference` from `remote` if it still points at `sha`. True when it did.
fn delete_candidate(
    git: &Git,
    runner: &dyn GitRunner,
    ports: &impl MergePorts,
    workspace: &Workspace,
    remote: &str,
    (reference, sha): (&str, &Sha),
) -> bool {
    let command =
        git.delete_refs_command(workspace, remote, &[(reference.to_owned(), sha.clone())]);
    matches!(
        ports.with_candidate_access(&CandidateWrite::Delete { reference, sha }, || {
            runner.run(&command)
        }),
        Ok(Ok(output)) if output.success()
    )
}

/// Deletes the candidate refs of `remote` other than `keep`, at most [`MAX_SWEPT`], each with a
/// lease on the tip that was listed, so a ref that changed meanwhile is left. Best effort: a
/// deletion that fails is not an error, only a ref that stays. Returns how many were removed.
///
/// # Errors
/// The listing could not be read.
pub fn sweep_candidates(
    git: &Git,
    runner: &dyn GitRunner,
    ports: &impl MergePorts,
    workspace: &Workspace,
    remote: &str,
    keep: &Sha,
) -> Result<usize, MergeError> {
    let pattern = format!("{CANDIDATE_NAMESPACE}*");
    let output = runner.run(&git.ls_remote_command(remote, &pattern))?;
    if !output.success() {
        return Err(port(
            "list the candidate refs",
            format!("git ls-remote exited {:?}", output.exit_code),
        ));
    }
    let mut removed = 0;
    let stale = parse_listing(&output.stdout)
        .into_iter()
        .filter(|listed| listed.named != *keep)
        .take(MAX_SWEPT);
    for listed in stale {
        let reference = listed.reference();
        if delete_candidate(
            git,
            runner,
            ports,
            workspace,
            remote,
            (&reference, &listed.tip),
        ) {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Whether `head` is an ancestor of main, from a fresh clone: the answer to "did it land". A head
/// the clone does not hold cannot be on main.
fn main_holds(ctx: &MergeContext<'_>, head: &Sha) -> Result<bool, MergeError> {
    let workspace = match clone_main(ctx.git, ctx.runner, ctx.sources, ctx.clone_dir)? {
        CloneOutcome::Cloned(workspace) => workspace,
        CloneOutcome::Failed(output) => {
            return Err(port(
                "clone main",
                format!("git clone exited {:?}", output.exit_code),
            ))
        }
    };
    holds_in(ctx, &workspace, head)
}

fn holds_in(ctx: &MergeContext<'_>, workspace: &Workspace, head: &Sha) -> Result<bool, MergeError> {
    let known = ctx
        .runner
        .run(&ctx.git.commit_exists_command(workspace, head))?;
    match known.exit_code {
        Some(0) => {}
        Some(1) => return Ok(false),
        Some(_) | None => return Err(port("look for the head", "git rev-parse failed")),
    }
    let answer = ctx.runner.run(&ctx.git.on_main_command(workspace, head))?;
    match answer.exit_code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(_) | None => Err(port(
            "check ancestry",
            "git merge-base --is-ancestor failed",
        )),
    }
}

fn merged(update: &PushUpdate) -> MergeOutcome {
    MergeOutcome::Merged {
        base: update.base.clone(),
        head: update.head.clone(),
        evidence: None,
    }
}

/// The `Land` of the staged landing. The candidate ref is read first. Present, with the head for
/// a tip: main is cloned, the ref fetched and its tip checked, and, unless main already holds the
/// head, the head is pushed to main with a lease on `update.base` and read back by ancestry; the
/// ref is then deleted. Absent, or with another tip: main is read, and the head is `Merged` if
/// main holds it, else [`MergeOutcome::CandidateMissing`]. The order matters: reading main first
/// could miss a late call that pushed and deleted the ref in between.
///
/// `ctx.clone_dir` must be missing or empty; `ctx.pinned_main` and `ctx.test_dir` are not used.
///
/// # Errors
/// A git command could not run, a port failed, or a read was not an answer.
pub fn land_candidate(
    ctx: &MergeContext<'_>,
    ports: &impl MergePorts,
    update: &PushUpdate,
) -> Result<MergeOutcome, MergeError> {
    let remote = &ctx.sources.main_remote;
    let tip = read_candidate(ctx.git, ctx.runner, remote, &update.head)?;
    if tip.as_ref() != Some(&update.head) {
        return if main_holds(ctx, &update.head)? {
            Ok(merged(update))
        } else {
            Ok(MergeOutcome::CandidateMissing)
        };
    }
    let workspace = match clone_main(ctx.git, ctx.runner, ctx.sources, ctx.clone_dir)? {
        CloneOutcome::Cloned(workspace) => workspace,
        CloneOutcome::Failed(result) => return Ok(MergeOutcome::Clone { result }),
    };
    let reference = candidate_ref(&update.head);
    let fetched = ctx
        .runner
        .run(&ctx.git.fetch_ref_command(&workspace, remote, &reference))?;
    if !fetched.success() {
        return Ok(MergeOutcome::GitFailed { result: fetched });
    }
    let local = ctx
        .runner
        .run(&ctx.git.ref_tip_command(&workspace, &reference))?;
    if Sha::parse(local.stdout.trim()).ok().as_ref() != Some(&update.head) {
        return Ok(MergeOutcome::CandidateMissing);
    }
    let outcome = if holds_in(ctx, &workspace, &update.head)? {
        merged(update)
    } else {
        land(ctx, ports, &workspace, update)?
    };
    if matches!(outcome, MergeOutcome::Merged { .. }) {
        // The landing is the truth; a ref that stays is swept by the next `Prepare`.
        delete_candidate(
            ctx.git,
            ctx.runner,
            ports,
            &workspace,
            remote,
            (&reference, &update.head),
        );
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(character: char) -> String {
        character.to_string().repeat(40)
    }

    #[test]
    fn a_listing_keeps_only_lines_of_the_exact_shape() {
        let (a, b) = (sha('a'), sha('b'));
        let text = format!(
            "{a}\t{CANDIDATE_NAMESPACE}{b}\n\
             {a}\t{CANDIDATE_NAMESPACE}{b}/extra\n\
             {a}\t{CANDIDATE_NAMESPACE}not-a-sha\n\
             {a}\t{CANDIDATE_NAMESPACE}{}\n\
             {a}\trefs/heads/main\n\
             {}\t{CANDIDATE_NAMESPACE}{b}\n\
             {a} {CANDIDATE_NAMESPACE}{b}\n\
             {a}\t{CANDIDATE_NAMESPACE}{}\n",
            b.to_uppercase(),
            "g".repeat(40),
            "c".repeat(39),
        );
        let listed = parse_listing(&text);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].tip.as_str(), a);
        assert_eq!(listed[0].named.as_str(), b);
        assert_eq!(listed[0].reference(), format!("{CANDIDATE_NAMESPACE}{b}"));
    }

    #[test]
    fn a_listing_is_read_up_to_its_cap() {
        let (a, b) = (sha('a'), sha('b'));
        let line = format!("{a}\t{CANDIDATE_NAMESPACE}{b}\n");
        assert_eq!(
            parse_listing(&line.repeat(MAX_LISTED + 500)).len(),
            MAX_LISTED
        );
    }
}

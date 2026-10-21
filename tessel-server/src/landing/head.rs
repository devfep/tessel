//! `ReadHead` and `Reconcile`: where main is, and which commits are on it.

use std::slice;

use tessel_coordinator::host::call::CallAnswer;
use tessel_coordinator::protocol::CommitId;
use tessel_lander::git::MAIN_BRANCH;
use tessel_lander::types::Sha;

use super::answer::commit_id;
use super::git_read::{main_and_ancestors, read_branch, ReadError};
use super::{Inner, Work};

impl Inner {
    /// Main's head and which of `submitted` are on main. With nothing submitted this is one
    /// `ls-remote`; otherwise main is cloned to ask about ancestry.
    pub(super) fn read_head(&self, work: &Work, submitted: &[CommitId]) -> CallAnswer {
        match self.main_and_landed(work, submitted) {
            Ok((main, landed)) => CallAnswer::Head {
                main: Some(commit_id(&main)),
                landed: landed.iter().map(commit_id).collect(),
            },
            Err(error) => self.unanswered("read the head", &error),
        }
    }

    /// Whether `head` is on main, and where main is.
    pub(super) fn reconcile(&self, work: &Work, head: &CommitId) -> CallAnswer {
        match self.main_and_landed(work, slice::from_ref(head)) {
            Ok((main, landed)) => CallAnswer::Reconciled {
                main: commit_id(&main),
                landed: !landed.is_empty(),
            },
            Err(error) => self.unanswered("reconcile", &error),
        }
    }

    fn main_and_landed(
        &self,
        work: &Work,
        submitted: &[CommitId],
    ) -> Result<(Sha, Vec<Sha>), ReadError> {
        let commits: Vec<Sha> = submitted
            .iter()
            .filter_map(|commit| Sha::parse(&commit.0).ok())
            .collect();
        if commits.is_empty() {
            let main = read_branch(&self.git, &self.remote, MAIN_BRANCH)?
                .ok_or_else(|| ReadError("the remote has no main branch".to_string()))?;
            return Ok((main, Vec::new()));
        }
        main_and_ancestors(&self.git, &self.remote, &work.dir.child("head"), &commits)
    }

    fn unanswered(&self, what: &str, error: &ReadError) -> CallAnswer {
        tracing::warn!(repo = %self.repo, what, error = %error, "the remote could not be read");
        CallAnswer::Unknown
    }
}

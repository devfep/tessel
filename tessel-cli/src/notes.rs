//! Carrying a submitted commit's git note over to the commit the lander made from it.
//!
//! The lander rebases a submission onto main, so the landed commits are new objects with new
//! shas and the notes `tessel submit` wrote stay on the originals. The coordinator's `Merged`
//! names `base` (main before the landing) and `head` (main after), so `base..head` is exactly the
//! rebased commits. A landed commit is the same one as a submitted commit when their author line
//! (name, email, author date) and raw message are equal: the lander's rebase keeps all of them.
//! Patch ids are no key: a clean rebase past a nearby edit changes the context a patch id hashes.
//!
//! Everything printed carries shas and counts, never note text (CLAUDE.md rule 3).

use std::path::Path;

use crate::state::NoteJob;

/// The most commits looked at in either range. A landing longer than this is not a submission an
/// agent made; nothing is copied.
pub const MAX_RANGE: usize = 256;

/// What one finished job did, in counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Notes copied onto a landed commit.
    pub copied: usize,
    /// Landed commits that already had a note, which is kept.
    pub already_noted: usize,
    /// Noted submitted commits with no landed counterpart (dropped as already on main, a merge
    /// commit, or squashed before the submission).
    pub not_landed: usize,
    /// Noted submitted commits skipped because more than one commit shares their key.
    pub ambiguous: usize,
}

/// Why a job cannot finish yet. It stays queued and is tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// `base` or `head` is not in this repository yet (not fetched). Matching needs the objects.
    ObjectsMissing,
    /// `refs/notes/commits` is locked, by another worktree or process.
    NotesLocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Done(Report),
    Waiting(Wait),
    /// Nothing was copied and trying again would not help. The text carries no note content.
    Refused(String),
}

/// Runs `job` in the repository at `root`.
pub fn copy_notes(root: &Path, job: &NoteJob) -> Step {
    let _ = (root, job);
    Step::Done(Report::default())
}

/// One log line for a step: shas and counts only.
pub fn describe(job: &NoteJob, step: &Step) -> String {
    let _ = (job, step);
    String::new()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use tessel_coordinator::protocol::ClaimId;

    use super::*;

    const SECRET: &str = "SECRET-NOTE-BODY";

    struct Repo {
        dir: tempfile::TempDir,
    }

    fn run(dir: &Path, args: &[&str], date: Option<&str>) -> std::process::Output {
        let mut git = Command::new("git");
        git.arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Ann Author")
            .env("GIT_AUTHOR_EMAIL", "ann@example.com")
            .env("GIT_COMMITTER_NAME", "Lander")
            .env("GIT_COMMITTER_EMAIL", "lander@example.com");
        if let Some(date) = date {
            git.env("GIT_AUTHOR_DATE", date);
        }
        git.output().unwrap()
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = run(dir, args, None);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn date(second: u32) -> String {
        format!("2024-01-01T00:00:{second:02} +0000")
    }

    fn lines(edits: &[(usize, &str)]) -> String {
        let mut text = String::new();
        for n in 1..=20 {
            match edits.iter().find(|(line, _)| *line == n) {
                Some((_, to)) => text.push_str(to),
                None => text.push_str(&format!("line {n}")),
            }
            text.push('\n');
        }
        text
    }

    impl Repo {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            git(dir.path(), &["init", "-q", "-b", "main"]);
            std::fs::write(dir.path().join("a.rs"), lines(&[])).unwrap();
            let repo = Self { dir };
            repo.commit_all("start", 0);
            repo
        }

        fn root(&self) -> PathBuf {
            self.dir.path().to_path_buf()
        }

        fn commit_all(&self, message: &str, second: u32) -> String {
            git(&self.root(), &["add", "-A"]);
            let out = run(
                &self.root(),
                &["commit", "-q", "-m", message],
                Some(&date(second)),
            );
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            git(&self.root(), &["rev-parse", "HEAD"])
        }

        /// Edits `a.rs` and commits it as the next commit on the current branch.
        fn edit(&self, edits: &[(usize, &str)], message: &str, second: u32) -> String {
            std::fs::write(self.root().join("a.rs"), lines(edits)).unwrap();
            self.commit_all(message, second)
        }

        fn note(&self, commit: &str, text: &str) {
            git(&self.root(), &["notes", "add", "-m", text, commit]);
        }

        fn note_of(&self, commit: &str) -> Option<String> {
            let out = run(&self.root(), &["notes", "show", commit], None);
            out.status
                .success()
                .then(|| String::from_utf8(out.stdout).unwrap().trim().to_string())
        }

        /// Replays `commits` onto `onto` the way the lander does (author line and message kept,
        /// new committer), returning the new shas, and leaves the result on `landed`.
        fn rebase(&self, onto: &str, commits: &[&str]) -> Vec<String> {
            let tree = tempfile::tempdir().unwrap();
            let work = tree.path().join("work");
            let work = work.to_str().unwrap();
            git(
                &self.root(),
                &["worktree", "add", "-q", "--detach", work, onto],
            );
            let mut made = Vec::new();
            for commit in commits {
                git(Path::new(work), &["cherry-pick", commit]);
                made.push(git(Path::new(work), &["rev-parse", "HEAD"]));
            }
            git(
                &self.root(),
                &["branch", "-f", "landed", &made[made.len() - 1]],
            );
            git(&self.root(), &["worktree", "remove", "--force", work]);
            made
        }

        fn job(&self, source: &str, base: &str, head: &str) -> NoteJob {
            NoteJob {
                claim: ClaimId(1),
                source: source.to_string(),
                floor: None,
                base: base.to_string(),
                head: head.to_string(),
            }
        }
    }

    /// Three commits on a branch from the start, editing lines 2, 8 and 14 of `a.rs`.
    struct Submission {
        repo: Repo,
        start: String,
        commits: Vec<String>,
    }

    fn submission() -> Submission {
        let repo = Repo::new();
        let start = git(&repo.root(), &["rev-parse", "HEAD"]);
        git(&repo.root(), &["checkout", "-q", "-b", "work"]);
        let one = repo.edit(&[(2, "one")], "change one", 1);
        let two = repo.edit(&[(2, "one"), (8, "two")], "change two", 2);
        let three = repo.edit(&[(2, "one"), (8, "two"), (14, "three")], "change three", 3);
        git(&repo.root(), &["checkout", "-q", "main"]);
        Submission {
            repo,
            start,
            commits: vec![one, two, three],
        }
    }

    impl Submission {
        /// Another landing on main: line 4 changes, two lines from the first commit's edit, so
        /// the rebase is clean but the context of that commit's patch is not the same.
        fn main_moves(&self) -> String {
            self.repo.edit(&[(4, "elsewhere")], "someone else", 30)
        }

        fn patch_id(&self, commit: &str) -> String {
            let patch = git(&self.repo.root(), &["show", "--format=", commit]);
            let mut child = Command::new("git")
                .arg("-C")
                .arg(self.repo.root())
                .args(["patch-id", "--stable"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), patch.as_bytes()).unwrap();
            let out = child.wait_with_output().unwrap();
            String::from_utf8(out.stdout).unwrap()
        }
    }

    fn done(copied: usize, already: usize, not_landed: usize, ambiguous: usize) -> Step {
        Step::Done(Report {
            copied,
            already_noted: already,
            not_landed,
            ambiguous,
        })
    }

    #[test]
    fn a_clean_rebase_that_changes_context_still_carries_the_note() {
        let s = submission();
        s.repo.note(&s.commits[0], &format!("intent: one {SECRET}"));
        let base = s.main_moves();
        let landed = s
            .repo
            .rebase(&base, &[&s.commits[0], &s.commits[1], &s.commits[2]]);
        assert_ne!(
            s.patch_id(&s.commits[0]),
            s.patch_id(&landed[0]),
            "the scenario must defeat a patch-id key"
        );
        let job = s.repo.job(&s.commits[2], &base, &landed[2]);
        assert_eq!(copy_notes(&s.repo.root(), &job), done(1, 0, 0, 0));
        assert_eq!(
            s.repo.note_of(&landed[0]),
            Some(format!("intent: one {SECRET}"))
        );
        assert_eq!(s.repo.note_of(&landed[1]), None);
    }

    #[test]
    fn a_fast_forward_landing_changes_nothing() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: one");
        let job = s.repo.job(&s.commits[2], &s.commits[2], &s.commits[2]);
        assert_eq!(copy_notes(&s.repo.root(), &job), done(0, 0, 0, 0));
        let job = s.repo.job(&s.commits[2], &s.start, &s.commits[2]);
        assert_eq!(copy_notes(&s.repo.root(), &job), done(1, 0, 0, 0));
    }

    #[test]
    fn of_three_commits_two_noted_the_dropped_one_is_reported_not_landed() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: one");
        s.repo.note(&s.commits[1], "intent: two");
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0], &s.commits[2]]);
        let job = s.repo.job(&s.commits[2], &base, &landed[1]);
        assert_eq!(copy_notes(&s.repo.root(), &job), done(1, 0, 1, 0));
        assert_eq!(s.repo.note_of(&landed[0]).as_deref(), Some("intent: one"));
        assert_eq!(s.repo.note_of(&landed[1]), None);
    }

    #[test]
    fn two_landed_commits_with_one_key_get_no_note() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: one");
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0]]);
        git(&s.repo.root(), &["checkout", "-q", "landed"]);
        let twin = s
            .repo
            .edit(&[(4, "elsewhere"), (16, "twin")], "change one", 1);
        git(&s.repo.root(), &["checkout", "-q", "main"]);
        let twin_key = git(
            &s.repo.root(),
            &["log", "-1", "--format=%an %ae %ad %B", &twin],
        );
        let first_key = git(
            &s.repo.root(),
            &["log", "-1", "--format=%an %ae %ad %B", &landed[0]],
        );
        assert_eq!(twin_key, first_key, "the twin has the same key");
        let job = s.repo.job(&s.commits[0], &base, &twin);
        assert_eq!(copy_notes(&s.repo.root(), &job), done(0, 0, 0, 1));
        assert_eq!(s.repo.note_of(&landed[0]), None);
        assert_eq!(s.repo.note_of(&twin), None);
    }

    #[test]
    fn a_landed_commit_that_has_a_note_keeps_it() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: submitted");
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0]]);
        s.repo.note(&landed[0], "written by hand");
        let job = s.repo.job(&s.commits[0], &base, &landed[0]);
        assert_eq!(copy_notes(&s.repo.root(), &job), done(0, 1, 0, 0));
        assert_eq!(
            s.repo.note_of(&landed[0]).as_deref(),
            Some("written by hand")
        );
    }

    #[test]
    fn a_base_this_repository_does_not_have_waits_and_one_that_is_no_ancestor_is_refused() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: one");
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0]]);
        let absent = "0123456789abcdef0123456789abcdef01234567";
        let job = s.repo.job(&s.commits[0], absent, &landed[0]);
        assert_eq!(
            copy_notes(&s.repo.root(), &job),
            Step::Waiting(Wait::ObjectsMissing)
        );
        let job = s.repo.job(&s.commits[0], &landed[0], &base);
        assert!(matches!(copy_notes(&s.repo.root(), &job), Step::Refused(_)));
        assert_eq!(s.repo.note_of(&landed[0]), None);
    }

    #[test]
    fn a_head_that_is_fetched_later_finishes_the_job() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: one");
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0]]);
        let elsewhere = tempfile::tempdir().unwrap();
        let remote = s.repo.root();
        let remote = remote.to_str().unwrap();
        git(elsewhere.path(), &["init", "-q", "-b", "main"]);
        git(
            elsewhere.path(),
            &[
                "fetch",
                "-q",
                remote,
                "refs/heads/work:refs/remotes/o/work",
                "refs/notes/commits:refs/notes/commits",
            ],
        );
        let job = s.repo.job(&s.commits[0], &base, &landed[0]);
        assert_eq!(
            copy_notes(elsewhere.path(), &job),
            Step::Waiting(Wait::ObjectsMissing)
        );
        git(
            elsewhere.path(),
            &[
                "fetch",
                "-q",
                remote,
                "refs/heads/landed:refs/remotes/o/landed",
            ],
        );
        assert_eq!(copy_notes(elsewhere.path(), &job), done(1, 0, 0, 0));
        let shown = run(elsewhere.path(), &["notes", "show", &landed[0]], None);
        assert!(shown.status.success());
    }

    #[test]
    fn a_merge_commit_in_the_landing_copies_nothing() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: one");
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0]]);
        let tree = git(
            &s.repo.root(),
            &["rev-parse", &format!("{}^{{tree}}", landed[0])],
        );
        let merge = git(
            &s.repo.root(),
            &[
                "commit-tree",
                &tree,
                "-p",
                &landed[0],
                "-p",
                &s.start,
                "-m",
                "merge",
            ],
        );
        let job = s.repo.job(&s.commits[0], &base, &merge);
        assert!(matches!(copy_notes(&s.repo.root(), &job), Step::Refused(_)));
        assert_eq!(s.repo.note_of(&landed[0]), None);
    }

    #[test]
    fn a_landing_over_the_cap_copies_nothing() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: one");
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0]]);
        let tree = git(
            &s.repo.root(),
            &["rev-parse", &format!("{}^{{tree}}", landed[0])],
        );
        let mut tip = base.clone();
        for n in 0..=MAX_RANGE {
            tip = git(
                &s.repo.root(),
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    &tip,
                    "-m",
                    &format!("filler {n}"),
                ],
            );
        }
        let job = s.repo.job(&s.commits[0], &base, &tip);
        assert!(matches!(copy_notes(&s.repo.root(), &job), Step::Refused(_)));
        assert_eq!(s.repo.note_of(&landed[0]), None);
    }

    #[test]
    fn a_locked_notes_ref_leaves_the_job_waiting_until_it_is_free() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: one");
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0]]);
        let lock = s.repo.root().join(".git/refs/notes/commits.lock");
        std::fs::write(&lock, "").unwrap();
        let job = s.repo.job(&s.commits[0], &base, &landed[0]);
        assert_eq!(
            copy_notes(&s.repo.root(), &job),
            Step::Waiting(Wait::NotesLocked)
        );
        std::fs::remove_file(&lock).unwrap();
        assert_eq!(copy_notes(&s.repo.root(), &job), done(1, 0, 0, 0));
    }

    #[test]
    fn a_value_that_is_no_sha_is_refused_before_git_sees_it() {
        let s = submission();
        let good = &s.commits[2];
        for bad in ["--output=/tmp/x", "-n", "HEAD", "main", "abc", ""] {
            for job in [
                s.repo.job(good, bad, good),
                s.repo.job(good, good, bad),
                s.repo.job(bad, good, good),
            ] {
                assert!(
                    matches!(copy_notes(&s.repo.root(), &job), Step::Refused(_)),
                    "{job:?}"
                );
            }
        }
    }

    #[test]
    fn what_a_job_reports_never_holds_note_text() {
        let s = submission();
        s.repo.note(&s.commits[0], &format!("intent: {SECRET}"));
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0]]);
        let job = s.repo.job(&s.commits[0], &base, &landed[0]);
        let step = copy_notes(&s.repo.root(), &job);
        assert_eq!(step, done(1, 0, 0, 0));
        assert!(!describe(&job, &step).contains(SECRET));
        let refused = copy_notes(&s.repo.root(), &s.repo.job("-x", &base, &landed[0]));
        assert!(!describe(&job, &refused).contains(SECRET));
        assert!(describe(&job, &step).contains(&landed[0][..7]));
    }
}

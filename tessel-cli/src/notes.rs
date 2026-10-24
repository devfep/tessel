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

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

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

/// How often a locked notes ref is tried before the job is left waiting, and the pause between.
const LOCK_TRIES: u32 = 4;
const LOCK_PAUSE: Duration = Duration::from_millis(150);

/// Runs `job` in the repository at `root`. Blocking: the daemon calls it on a blocking thread.
pub fn copy_notes(root: &Path, job: &NoteJob) -> Step {
    copy_notes_with(root, job, LOCK_TRIES, LOCK_PAUSE)
}

fn copy_notes_with(root: &Path, job: &NoteJob, tries: u32, pause: Duration) -> Step {
    for (name, value) in [
        ("source", &job.source),
        ("base", &job.base),
        ("head", &job.head),
    ] {
        if !is_sha(value) {
            return Step::Refused(format!("the {name} is not a commit sha"));
        }
    }
    if job.base == job.head {
        return Step::Done(Report::default());
    }
    let git = Git { root };
    if ![&job.source, &job.base, &job.head]
        .into_iter()
        .all(|sha| git.has_commit(sha))
    {
        return Step::Waiting(Wait::ObjectsMissing);
    }
    match plan(&git, job) {
        Ok(plan) => plan.run(&git, tries, pause),
        Err(refusal) => Step::Refused(refusal),
    }
}

/// One log line for a step: shas and counts only.
pub fn describe(job: &NoteJob, step: &Step) -> String {
    let span = format!(
        "claim {}: {}..{}",
        job.claim.0,
        short(&job.base),
        short(&job.head)
    );
    match step {
        Step::Done(report) => format!(
            "notes after rebase, {span}: copied {}, already noted {}, not landed {}, ambiguous {}",
            report.copied, report.already_noted, report.not_landed, report.ambiguous
        ),
        Step::Waiting(Wait::ObjectsMissing) => format!(
            "notes after rebase, {span}: the commits are not in this repository yet; waiting"
        ),
        Step::Waiting(Wait::NotesLocked) => {
            format!("notes after rebase, {span}: refs/notes/commits is locked; waiting")
        }
        Step::Refused(why) => format!("notes after rebase, {span}: nothing copied: {why}"),
    }
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// A full hex object name, SHA-1 or SHA-256. Anything else, such as `-x` or `HEAD`, never reaches
/// git: `CommitId` is an unchecked string that came over the wire.
fn is_sha(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

struct Git<'a> {
    root: &'a Path,
}

impl Git<'_> {
    fn output(&self, args: &[&str]) -> Option<Output> {
        Command::new("git")
            .arg("-C")
            .arg(self.root)
            .args(args)
            .output()
            .ok()
    }

    /// Stdout of a git command that succeeded.
    fn ok(&self, args: &[&str]) -> Option<String> {
        let out = self.output(args).filter(|out| out.status.success())?;
        String::from_utf8(out.stdout).ok()
    }

    fn has_commit(&self, sha: &str) -> bool {
        let rev = format!("{sha}^{{commit}}");
        self.ok(&["rev-parse", "--verify", "--quiet", "--end-of-options", &rev])
            .is_some()
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        self.output(&[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            ancestor,
            descendant,
        ])
        .is_some_and(|out| out.status.success())
    }

    /// The commits of `range` (a `^`-style exclusion list), oldest first, with their parent
    /// counts. `None` if git failed.
    fn walk(&self, include: &str, exclude: &[&str]) -> Option<Vec<(String, usize)>> {
        let mut args = vec![
            "rev-list",
            "--parents",
            "--reverse",
            "--end-of-options",
            include,
        ];
        let excluded: Vec<String> = exclude.iter().map(|sha| format!("^{sha}")).collect();
        args.extend(excluded.iter().map(String::as_str));
        let text = self.ok(&args)?;
        let mut commits = Vec::new();
        for line in text.lines() {
            let mut shas = line.split(' ');
            let commit = shas.next()?.to_string();
            commits.push((commit, shas.count()));
        }
        Some(commits)
    }

    /// The author line and the message of a commit, which is what a rebase keeps.
    fn key(&self, sha: &str) -> Option<String> {
        let raw = self.ok(&["cat-file", "commit", sha])?;
        let author = raw.lines().find(|line| line.starts_with("author "))?;
        let message = raw.split_once("\n\n").map_or("", |(_, message)| message);
        Some(format!("{author}\n\n{message}"))
    }

    /// The commits that have a note, from `git notes list` (`<note> <commit>` per line).
    fn noted(&self) -> Option<HashSet<String>> {
        let text = self.ok(&["notes", "list"])?;
        Some(
            text.lines()
                .filter_map(|line| line.split_once(' '))
                .map(|(_, commit)| commit.to_string())
                .collect(),
        )
    }
}

/// What to copy, decided before anything is written.
struct Plan {
    copies: Vec<(String, String)>,
    report: Report,
}

fn plan(git: &Git<'_>, job: &NoteJob) -> Result<Plan, String> {
    if !git.is_ancestor(&job.base, &job.head) {
        return Err("the base is not an ancestor of the head".to_string());
    }
    let landed = git
        .walk(&job.head, &[&job.base])
        .ok_or("cannot list the landed commits")?;
    let submitted = submitted_range(git, job)?;
    for (range, name) in [(&landed, "landing"), (&submitted, "submission")] {
        if range.len() > MAX_RANGE {
            return Err(format!("the {name} has more than {MAX_RANGE} commits"));
        }
        if range.iter().any(|(_, parents)| *parents > 1) {
            return Err(format!("the {name} contains a merge commit"));
        }
    }
    let noted = git.noted().ok_or("cannot list the notes")?;
    let mut report = Report::default();
    let mut copies = Vec::new();
    let targets = keyed(git, &landed)?;
    let sources = keyed(git, &submitted)?;
    for (source, key) in sources.iter().filter(|(source, _)| noted.contains(source)) {
        let twins = sources.iter().filter(|(_, other)| other == key).count();
        let hits: Vec<&String> = targets
            .iter()
            .filter(|(_, target)| target == key)
            .map(|(commit, _)| commit)
            .collect();
        match (twins, hits.as_slice()) {
            (_, []) => report.not_landed += 1,
            (_, [target]) if **target == *source => {}
            (1, [target]) if noted.contains(*target) => report.already_noted += 1,
            (1, [target]) => copies.push((source.clone(), (*target).clone())),
            _ => report.ambiguous += 1,
        }
    }
    Ok(Plan { copies, report })
}

/// The submitted commits that can have landed: those after the merge base of the submission and
/// the landing's base, and after the diff base before this submission (which landed earlier).
fn submitted_range(git: &Git<'_>, job: &NoteJob) -> Result<Vec<(String, usize)>, String> {
    let fork_point = git
        .ok(&["merge-base", "--end-of-options", &job.source, &job.base])
        .map(|out| out.trim().to_string())
        .filter(|sha| is_sha(sha))
        .ok_or("the submission shares no history with the landing's base")?;
    let mut exclude = vec![fork_point.as_str()];
    let floor = job.floor.as_deref().filter(|floor| {
        is_sha(floor) && git.has_commit(floor) && git.is_ancestor(floor, &job.source)
    });
    exclude.extend(floor);
    git.walk(&job.source, &exclude)
        .ok_or_else(|| "cannot list the submitted commits".to_string())
}

fn keyed(git: &Git<'_>, commits: &[(String, usize)]) -> Result<Vec<(String, String)>, String> {
    let mut keyed = Vec::new();
    for (commit, _) in commits {
        let key = git
            .key(commit)
            .ok_or_else(|| format!("cannot read commit {}", short(commit)))?;
        keyed.push((commit.clone(), key));
    }
    Ok(keyed)
}

impl Plan {
    fn run(self, git: &Git<'_>, tries: u32, pause: Duration) -> Step {
        let Plan { copies, mut report } = self;
        for (source, target) in copies {
            match copy_one(git, &source, &target, tries, pause) {
                Copy::Copied => report.copied += 1,
                Copy::AlreadyNoted => report.already_noted += 1,
                Copy::Locked => return Step::Waiting(Wait::NotesLocked),
                Copy::Failed(why) => return Step::Refused(why),
            }
        }
        Step::Done(report)
    }
}

enum Copy {
    Copied,
    AlreadyNoted,
    Locked,
    Failed(String),
}

/// `git notes copy`, never forced: a target that gained a note meanwhile keeps it.
fn copy_one(git: &Git<'_>, source: &str, target: &str, tries: u32, pause: Duration) -> Copy {
    let args = ["notes", "copy", "--end-of-options", source, target];
    for attempt in 0..tries {
        if attempt > 0 {
            std::thread::sleep(pause);
        }
        let Some(out) = git.output(&args) else {
            return Copy::Failed("cannot run git".to_string());
        };
        if out.status.success() {
            return Copy::Copied;
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("existing notes") {
            return Copy::AlreadyNoted;
        }
        if !stderr.contains("lock") {
            let code = out.status.code().unwrap_or(-1);
            return Copy::Failed(format!(
                "git notes copy {} to {} failed with exit code {code}",
                short(source),
                short(target)
            ));
        }
    }
    Copy::Locked
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
            if let Some((_, to)) = edits.iter().find(|(line, _)| *line == n) {
                text.push_str(to);
            } else {
                text.push_str("line ");
                text.push_str(&n.to_string());
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
    }

    fn job_of(source: &str, base: &str, head: &str) -> NoteJob {
        NoteJob {
            claim: ClaimId(1),
            source: source.to_string(),
            floor: None,
            base: base.to_string(),
            head: head.to_string(),
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
        let job = job_of(&s.commits[2], &base, &landed[2]);
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
        let job = job_of(&s.commits[2], &s.commits[2], &s.commits[2]);
        assert_eq!(copy_notes(&s.repo.root(), &job), done(0, 0, 0, 0));
        let job = job_of(&s.commits[2], &s.start, &s.commits[2]);
        assert_eq!(
            copy_notes(&s.repo.root(), &job),
            done(0, 0, 0, 0),
            "main took the submitted commits as they were"
        );
    }

    #[test]
    fn of_three_commits_two_noted_the_dropped_one_is_reported_not_landed() {
        let s = submission();
        s.repo.note(&s.commits[0], "intent: one");
        s.repo.note(&s.commits[1], "intent: two");
        let base = s.main_moves();
        let landed = s.repo.rebase(&base, &[&s.commits[0], &s.commits[2]]);
        let job = job_of(&s.commits[2], &base, &landed[1]);
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
        let job = job_of(&s.commits[0], &base, &twin);
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
        let job = job_of(&s.commits[0], &base, &landed[0]);
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
        let job = job_of(&s.commits[0], absent, &landed[0]);
        assert_eq!(
            copy_notes(&s.repo.root(), &job),
            Step::Waiting(Wait::ObjectsMissing)
        );
        let job = job_of(&s.commits[0], &landed[0], &base);
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
        let job = job_of(&s.commits[0], &base, &landed[0]);
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
        let job = job_of(&s.commits[0], &base, &merge);
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
        let job = job_of(&s.commits[0], &base, &tip);
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
        let job = job_of(&s.commits[0], &base, &landed[0]);
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
                job_of(good, bad, good),
                job_of(good, good, bad),
                job_of(bad, good, good),
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
        let job = job_of(&s.commits[0], &base, &landed[0]);
        let step = copy_notes(&s.repo.root(), &job);
        assert_eq!(step, done(1, 0, 0, 0));
        assert!(!describe(&job, &step).contains(SECRET));
        let refused = copy_notes(&s.repo.root(), &job_of("-x", &base, &landed[0]));
        assert!(!describe(&job, &refused).contains(SECRET));
        assert!(describe(&job, &step).contains(&landed[0][..7]));
    }
}

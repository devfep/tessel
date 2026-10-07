//! What the daemon keeps on disk so that agents and humans can read it: `state.json` (claims,
//! fences, expiries, connection) and `inbox.jsonl` (append-only notices).

use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tessel_coordinator::protocol::{ClaimId, Fence, RaceId, RequestId, ScopeClaim, ServerMsg};

use crate::worktree::Worktree;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldClaim {
    pub claim: ClaimId,
    pub fence: Fence,
    /// Estimated from the last heartbeat (or the grant) on this machine's clock; the coordinator
    /// does not report a renewed expiry.
    pub expires_at_ms: u64,
    pub race: Option<RaceId>,
    pub scopes: Vec<ScopeClaim>,
    /// The coordinator accepted a submission for this claim and holds it until `Merged` or
    /// `SubmitRejected` (invariant 5). A submitted claim does not expire and cannot be released.
    #[serde(default)]
    pub submitted: bool,
    /// The fork commit of the submission, to move the diff base to when it merges.
    #[serde(default)]
    pub submitted_commit: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    /// First connection attempt, before the coordinator has welcomed us.
    Connecting,
    Online,
    /// Lost the socket; retrying with backoff.
    Reconnecting,
    Stopped,
}

/// A `--wait` claim sitting in the coordinator's queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedWait {
    pub req: RequestId,
    pub scopes: Vec<ScopeClaim>,
    pub position: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub pid: u32,
    pub agent: String,
    pub repo: String,
    pub summary: String,
    pub task_ref: Option<String>,
    /// HEAD at the first start in the worktree, moved only when one of this agent's submissions
    /// merges. A reconnect, restart, `stop` or lapsed lease never changes it, so `submit` has a diff
    /// base that comes from the commit graph, not from the connection.
    #[serde(default)]
    pub start_base: String,
    /// The coordinator's head commit, from `Welcome`, `Merged` and `BaseMoved`.
    #[serde(default)]
    pub coordinator_head: Option<String>,
    /// HEAD sent in the last `Hello`; it moves with every reconnect and is not a diff base.
    pub base: String,
    /// Where the daemon listens; outside `.tessel/` because worktree paths can be long.
    #[serde(default)]
    pub socket: String,
    pub connection: Connection,
    pub lease_ms: Option<u64>,
    pub last_error: Option<String>,
    pub claims: Vec<HeldClaim>,
    pub queued: Option<QueuedWait>,
    pub updated_at_ms: u64,
}

impl State {
    pub fn read(worktree: &Worktree) -> Result<Option<Self>, StateError> {
        let path = worktree.state_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| StateError::Corrupt {
                    path: path.display().to_string(),
                    message: e.to_string(),
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StateError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn write(&self, worktree: &Worktree) -> Result<(), StateError> {
        let text = serde_json::to_vec_pretty(self).map_err(|e| StateError::Corrupt {
            path: worktree.state_path().display().to_string(),
            message: e.to_string(),
        })?;
        write_atomic(&worktree.state_path(), &text)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("cannot access {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not valid: {message}")]
    Corrupt { path: String, message: String },
}

/// Writes `bytes` to a sibling temp file and renames it over `path`, so a reader never sees a
/// half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    let io_err = |source| StateError::Io {
        path: path.display().to_string(),
        source,
    };
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(io_err)?;
    std::fs::rename(&tmp, path).map_err(io_err)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Denied,
    AtRisk,
    BaseMoved,
    AssumptionChallenged,
    LeaseExpired,
    WaitQueued,
    GrantedAfterWait,
    /// The socket dropped while a `--wait` was queued; the coordinator withdraws it.
    WaitWithdrawn,
    /// The daemon compared its claims with the coordinator's log after a reconnect and
    /// changed something.
    Reconciled,
    /// The steward merged a submitted claim; the daemon dropped it.
    Merged,
    /// The merge of a submitted claim was refused; the claim is active again.
    SubmitRejected,
    /// The coordinator found touched scopes outside the claim.
    Uncovered,
    /// A submission is held for a human to approve.
    ReviewRequired,
    Error,
    /// A server message this CLI does not act on.
    Unexpected,
}

/// One inbox line. `note` is the CLI's own text; anything an agent wrote is inside `server` and
/// is only ever shown as quoted data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notice {
    pub at_ms: u64,
    pub kind: NoticeKind,
    pub note: String,
    pub server: Option<ServerMsg>,
}

pub fn append_notice(
    worktree: &Worktree,
    notice: &Notice,
    redact: &dyn Fn(&str) -> String,
) -> Result<(), StateError> {
    let line = serde_json::to_string(notice).map_err(|e| StateError::Corrupt {
        path: worktree.inbox_path().display().to_string(),
        message: e.to_string(),
    })?;
    let line = redact(&line);
    let path = worktree.inbox_path();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()))
        .map_err(|source| StateError::Io {
            path: path.display().to_string(),
            source,
        })
}

/// Notices not yet read (or all of them), in order. Marks everything read.
pub fn take_inbox(worktree: &Worktree, all: bool) -> Result<Vec<Notice>, StateError> {
    let _lock = CursorLock::acquire(worktree)?;
    let lines = read_lines(&worktree.inbox_path())?;
    let cursor = read_cursor(worktree).min(lines.len());
    let start = if all { 0 } else { cursor };
    let mut notices = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(start) {
        notices.push(parse_line(index, line));
    }
    write_atomic(
        &worktree.inbox_cursor_path(),
        lines.len().to_string().as_bytes(),
    )?;
    Ok(notices)
}

/// What one bounded read of the inbox produced.
#[derive(Debug, PartialEq, Eq)]
pub struct Taken {
    /// The shown notices, each as `render` made it.
    pub text: String,
    pub shown: usize,
    /// Unread notices left for the next read.
    pub more: usize,
}

/// At most `max_notices` unread notices, rendered by `render`, in at most about `max_chars`
/// characters (the first one is cut to fit; later ones that would not fit wait). The shared cursor
/// moves past what is returned and no further, under a lock, so hooks running in parallel neither
/// skip nor repeat a notice.
pub fn take_inbox_bounded(
    worktree: &Worktree,
    render: &dyn Fn(&Notice) -> String,
    max_notices: usize,
    max_chars: usize,
) -> Result<Taken, StateError> {
    let _lock = CursorLock::acquire(worktree)?;
    let lines = read_lines(&worktree.inbox_path())?;
    let cursor = read_cursor(worktree).min(lines.len());
    let mut text = String::new();
    let mut used = 0;
    let mut shown = 0;
    for (index, line) in lines.iter().enumerate().skip(cursor) {
        if shown == max_notices {
            break;
        }
        let piece = render(&parse_line(index, line));
        let length = piece.chars().count();
        if shown > 0 && used + length > max_chars {
            break;
        }
        if length > max_chars {
            text.extend(piece.chars().take(max_chars));
            text.push_str("\n  [cut; `tessel inbox --all` has the whole notice]\n");
        } else {
            text.push_str(&piece);
        }
        used += length;
        shown += 1;
    }
    if shown > 0 {
        write_atomic(
            &worktree.inbox_cursor_path(),
            (cursor + shown).to_string().as_bytes(),
        )?;
    }
    Ok(Taken {
        text,
        shown,
        more: lines.len() - cursor - shown,
    })
}

/// How many lines the inbox holds now.
pub fn inbox_len(worktree: &Worktree) -> Result<usize, StateError> {
    Ok(read_lines(&worktree.inbox_path())?.len())
}

/// The notices from line `start` on, with their line numbers. Does not move the cursor.
pub fn notices_from(worktree: &Worktree, start: usize) -> Result<Vec<(usize, Notice)>, StateError> {
    let lines = read_lines(&worktree.inbox_path())?;
    Ok(lines
        .iter()
        .enumerate()
        .skip(start)
        .map(|(index, line)| (index, parse_line(index, line)))
        .collect())
}

fn parse_line(index: usize, line: &str) -> Notice {
    serde_json::from_str(line).unwrap_or_else(|e| Notice {
        at_ms: 0,
        kind: NoticeKind::Error,
        note: format!("inbox line {} is unreadable: {e}", index + 1),
        server: None,
    })
}

/// An advisory `flock` on `.tessel/inbox.lock`, held while the cursor is read and moved.
struct CursorLock {
    /// Closing the file releases the lock.
    _file: std::fs::File,
}

impl CursorLock {
    fn acquire(worktree: &Worktree) -> Result<Self, StateError> {
        let path = worktree.dir().join("inbox.lock");
        let io_err = |source| StateError::Io {
            path: path.display().to_string(),
            source,
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_err)?;
        loop {
            // SAFETY: `flock` only reads the descriptor, which `file` keeps open.
            let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if status == 0 {
                return Ok(Self { _file: file });
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(io_err(error));
            }
        }
    }
}

pub fn unread_count(worktree: &Worktree) -> Result<usize, StateError> {
    let total = read_lines(&worktree.inbox_path())?.len();
    Ok(total.saturating_sub(read_cursor(worktree)))
}

fn read_lines(path: &Path) -> Result<Vec<String>, StateError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text.lines().map(str::to_string).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(StateError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn read_cursor(worktree: &Worktree) -> usize {
    std::fs::read_to_string(worktree.inbox_cursor_path())
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt::Write as _;
    use std::sync::Mutex;

    use super::*;

    fn temp_worktree() -> (tempfile::TempDir, Worktree) {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        let worktree = Worktree::discover(dir.path()).unwrap();
        std::fs::create_dir_all(worktree.dir()).unwrap();
        (dir, worktree)
    }

    #[test]
    fn threads_taking_one_notice_at_a_time_see_each_notice_exactly_once() {
        let (_dir, worktree) = temp_worktree();
        let mut text = String::new();
        for n in 0..300 {
            let _ = writeln!(
                text,
                r#"{{"at_ms":1,"kind":"reconciled","note":"n{n}","server":null}}"#
            );
        }
        std::fs::write(worktree.inbox_path(), text).unwrap();
        let seen = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| loop {
                    let taken = take_inbox_bounded(&worktree, &|n: &Notice| n.note.clone(), 1, 100)
                        .unwrap();
                    if taken.shown == 0 {
                        break;
                    }
                    seen.lock().unwrap().push(taken.text);
                });
            }
        });
        let seen = seen.into_inner().unwrap();
        let distinct: BTreeSet<&String> = seen.iter().collect();
        assert_eq!(seen.len(), 300);
        assert_eq!(distinct.len(), 300);
    }

    #[test]
    fn the_cursor_moves_only_past_what_was_shown() {
        let (_dir, worktree) = temp_worktree();
        let line = r#"{"at_ms":1,"kind":"reconciled","note":"abcdefghij","server":null}"#;
        std::fs::write(worktree.inbox_path(), format!("{line}\n{line}\n{line}\n")).unwrap();
        let render = |n: &Notice| n.note.clone();
        let first = take_inbox_bounded(&worktree, &render, 10, 15).unwrap();
        assert_eq!((first.shown, first.more), (1, 2));
        assert_eq!(unread_count(&worktree).unwrap(), 2);
        let rest = take_inbox_bounded(&worktree, &render, 10, 25).unwrap();
        assert_eq!((rest.shown, rest.more), (2, 0));
        let none = take_inbox_bounded(&worktree, &render, 10, 25).unwrap();
        assert_eq!((none.shown, none.more, none.text.as_str()), (0, 0, ""));
    }
}

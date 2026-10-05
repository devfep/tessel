//! What the daemon keeps on disk so that agents and humans can read it: `state.json` (claims,
//! fences, expiries, connection) and `inbox.jsonl` (append-only notices).

use std::io::Write;
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
    pub base: String,
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
    let lines = read_lines(&worktree.inbox_path())?;
    let cursor = read_cursor(worktree).min(lines.len());
    let start = if all { 0 } else { cursor };
    let mut notices = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(start) {
        notices.push(serde_json::from_str(line).unwrap_or_else(|e| Notice {
            at_ms: 0,
            kind: NoticeKind::Error,
            note: format!("inbox line {} is unreadable: {e}", index + 1),
            server: None,
        }));
    }
    write_atomic(
        &worktree.inbox_cursor_path(),
        lines.len().to_string().as_bytes(),
    )?;
    Ok(notices)
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

//! The JSON-lines protocol between CLI commands and the per-worktree daemon, over the Unix
//! socket named by `Worktree::sock`. One request and one reply per connection.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tessel_coordinator::protocol::{
    ClaimId, Conflict, DecisionRecord, ErrorCode, HeldAssumption, ReviewReason, ScopeClaim,
};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::state::{HeldClaim, State};

/// A request line longer than this is refused; claims and assumptions are short.
pub const MAX_LINE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Claim {
        scopes: Vec<ScopeClaim>,
        wait: bool,
        assumptions: Vec<String>,
        /// Make a separate claim even when the one open claim could be amended.
        #[serde(default)]
        new: bool,
    },
    /// Make sure a held claim covers this repo-relative file for editing (or creating) it.
    Ensure {
        path: String,
        create: bool,
    },
    Release {
        claim: Option<ClaimId>,
    },
    /// Send a `Submit` for a held claim. The command has already computed `touched` and checked
    /// coverage; the daemon only owns the fence and the connection.
    Submit {
        claim: ClaimId,
        fork_commit: String,
        touched: Vec<ScopeClaim>,
        decisions: DecisionRecord,
    },
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Status {
        state: Box<State>,
    },
    Claim {
        outcome: ClaimOutcome,
    },
    Released {
        claims: Vec<ClaimId>,
        /// Submitted claims a release of all claims left alone: the coordinator holds them
        /// until they merge or are rejected.
        #[serde(default)]
        kept_submitted: Vec<ClaimId>,
    },
    Submit {
        outcome: SubmitOutcome,
    },
    /// The daemon is stopping. `unreleased` lists claims it could not release because it was
    /// offline; they stay held until their lease ends. `submitted` lists claims it left with
    /// the coordinator because they were submitted.
    Stopping {
        unreleased: Vec<ClaimId>,
        #[serde(default)]
        submitted: Vec<ClaimId>,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ClaimOutcome {
    Granted {
        claim: HeldClaim,
        at_risk: Vec<HeldAssumption>,
        /// The scopes were added to the claim already held (it has a new fence).
        #[serde(default)]
        amended: bool,
    },
    /// `Ensure` found a held claim that already permits the work.
    Covered,
    Denied {
        conflicts: Vec<Conflict>,
    },
    Queued {
        position: u32,
    },
    /// The coordinator refused the request, or it could not be sent.
    Refused {
        code: Option<ErrorCode>,
        message: String,
    },
}

/// How the coordinator answered a `Submit`. The merge itself comes later, as an inbox notice.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SubmitOutcome {
    /// In the merge queue at this 1-based position (0 for a shadow claim, which never merges).
    Accepted { queue_position: u32 },
    /// Invariant 11: the coordinator found these touched scopes outside the claim.
    Uncovered { scopes: Vec<ScopeClaim> },
    /// Invariant 12: held for a human to approve.
    ReviewRequired { reasons: Vec<ReviewReason> },
    /// The coordinator or the daemon refused the request, or it could not be sent.
    Refused {
        code: Option<ErrorCode>,
        message: String,
    },
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("no tessel daemon is running for this worktree")]
    NotRunning,
    #[error("the tessel daemon did not answer within {0:?}")]
    Timeout(Duration),
    #[error("daemon connection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("daemon sent an unreadable reply: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Sends `request` to the daemon listening on `sock` and returns its reply.
pub async fn call(sock: &Path, request: &Request, limit: Duration) -> Result<Reply, ClientError> {
    match tokio::time::timeout(limit, exchange(sock, request)).await {
        Ok(result) => result,
        Err(_) => Err(ClientError::Timeout(limit)),
    }
}

async fn exchange(sock: &Path, request: &Request) -> Result<Reply, ClientError> {
    let stream = match UnixStream::connect(sock).await {
        Ok(stream) => stream,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(ClientError::NotRunning)
        }
        Err(e) => return Err(e.into()),
    };
    let (read, mut write) = stream.into_split();
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    write.write_all(&line).await?;
    let mut reader = BufReader::new(read);
    let mut reply = String::new();
    let bytes = reader.read_line(&mut reply).await?;
    if bytes == 0 {
        return Err(ClientError::Io(std::io::ErrorKind::UnexpectedEof.into()));
    }
    Ok(serde_json::from_str(&reply)?)
}

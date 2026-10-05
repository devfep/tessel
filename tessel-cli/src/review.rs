//! `tessel review`: a reviewer's decision on a submission the coordinator holds (invariant 12).
//!
//! The coordinator answers a `Review` only when it refuses it, with an `Error` for that request.
//! On success it says nothing to the reviewer, so a decision is confirmed by finding its
//! `ReviewDecided` event in the log, past the point the log had reached before the request was
//! sent. This runs on its own short-lived connection: a reviewer needs no daemon and holds no
//! claims.

use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use futures_util::{SinkExt, StreamExt};
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, CommitId, ErrorCode, Event, EventKind, RequestId, ServerMsg,
    PROTOCOL_VERSION,
};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::config::Config;
use crate::daemon::{open_socket, read_log, ConnectFailure, Socket};

/// The longest note a reviewer may attach. A longer one is refused here rather than cut.
pub const MAX_NOTE_BYTES: usize = 1024;

/// The `Hello` base of a reviewer with no commit to offer. The coordinator ignores an all-zeros
/// base when it sets the repository head, so this never becomes the head, even if the reviewer
/// connects first.
pub const NO_BASE: &str = "0000000000000000000000000000000000000000";

const REVIEW_REQ: RequestId = RequestId(1);
/// How long to wait for an `Error` before taking the silence as acceptance.
const REFUSAL_WAIT: Duration = Duration::from_secs(2);
const WELCOME_WAIT: Duration = Duration::from_secs(5);
/// How often, and how far apart, the log is read looking for the decision.
const CONFIRM_READS: u32 = 5;
const CONFIRM_PAUSE: Duration = Duration::from_millis(300);

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// The log holds the decision.
    Confirmed,
    /// The coordinator answered with an `Error`: not a reviewer, not awaiting review, ...
    Refused { code: ErrorCode, message: String },
    /// No refusal came and the decision is not in the log (yet).
    Unconfirmed,
}

pub fn check_note(note: Option<&str>) -> anyhow::Result<()> {
    match note {
        Some(note) if note.len() > MAX_NOTE_BYTES => bail!(
            "--note is {} bytes; the limit is {MAX_NOTE_BYTES}. Shorten it; nothing was sent",
            note.len()
        ),
        Some(_) | None => Ok(()),
    }
}

/// Sends the decision and reports how it ended. `base` is a commit of this repository or
/// `NO_BASE`: the `Hello` needs one.
pub async fn decide(
    config: &Config,
    base: &str,
    claim: ClaimId,
    approve: bool,
    note: Option<String>,
) -> anyhow::Result<Decision> {
    let before = read_log(config, base.to_string())
        .await
        .map_err(|e| anyhow!("cannot read the coordinator's event log: {e}"))?;
    if !before.complete {
        bail!(
            "cannot read the whole event log, so a decision could not be confirmed; nothing was \
             sent. Try again"
        );
    }
    let from_seq = before.events.last().map_or(0, |event| event.seq + 1);

    let mut socket = connect(config, base).await?;
    let review = ClientMsg::Review {
        req: REVIEW_REQ,
        claim,
        approve,
        note,
    };
    send(&mut socket, &review).await?;
    let refusal = wait_for_refusal(&mut socket).await;
    let _ = socket.close(None).await;
    if let Some((code, message)) = refusal {
        return Ok(Decision::Refused { code, message });
    }

    for read in 0..CONFIRM_READS {
        if read > 0 {
            tokio::time::sleep(CONFIRM_PAUSE).await;
        }
        let Ok(log) = read_log(config, base.to_string()).await else {
            continue;
        };
        if decided(&log.events, from_seq, claim, approve) {
            return Ok(Decision::Confirmed);
        }
    }
    Ok(Decision::Unconfirmed)
}

/// Whether `events` hold this decision on this claim, at or after `from_seq`.
fn decided(events: &[Event], from_seq: u64, claim: ClaimId, approve: bool) -> bool {
    events
        .iter()
        .filter(|event| event.seq >= from_seq)
        .any(|event| {
            if let EventKind::ReviewDecided {
                claim: decided,
                approve: verdict,
                ..
            } = &event.kind
            {
                *decided == claim && *verdict == approve
            } else {
                false
            }
        })
}

async fn send(socket: &mut Socket, msg: &ClientMsg) -> anyhow::Result<()> {
    let text = serde_json::to_string(msg).context("cannot encode a message")?;
    socket
        .send(Message::text(text))
        .await
        .context("cannot send to the coordinator")
}

/// Opens the socket and says `Hello`; returns once the coordinator has welcomed this agent.
async fn connect(config: &Config, base: &str) -> anyhow::Result<Socket> {
    let mut socket = open_socket(config).await.map_err(|failure| match failure {
        ConnectFailure::Fatal(message) | ConnectFailure::Transient(message) => anyhow!(message),
    })?;
    let hello = ClientMsg::Hello {
        agent: AgentId(config.agent.clone()),
        base: CommitId(base.to_string()),
        protocol: PROTOCOL_VERSION,
    };
    send(&mut socket, &hello).await?;
    let deadline = Instant::now() + WELCOME_WAIT;
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            bail!("the coordinator did not welcome this agent within {WELCOME_WAIT:?}");
        };
        let next = tokio::time::timeout(left, socket.next()).await;
        match next {
            Err(_) | Ok(None) => {
                bail!("the coordinator closed the connection before it welcomed us")
            }
            Ok(Some(Err(e))) => {
                bail!("the connection failed before the coordinator welcomed us: {e}")
            }
            Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ServerMsg>(&text) {
                Ok(ServerMsg::Welcome { .. }) => return Ok(socket),
                Ok(ServerMsg::Error { code, message, .. }) => {
                    bail!("the coordinator refused the connection ({code:?}): {message}")
                }
                Ok(_) | Err(_) => {}
            },
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// The `Error` the coordinator sends for the review, if one arrives within `REFUSAL_WAIT`.
async fn wait_for_refusal(socket: &mut Socket) -> Option<(ErrorCode, String)> {
    let deadline = Instant::now() + REFUSAL_WAIT;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        let next = tokio::time::timeout(left, socket.next()).await.ok()?;
        // A closed socket cannot refuse anything; the log read decides.
        let frame = next.as_ref()?;
        let Ok(Message::Text(text)) = frame else {
            continue;
        };
        if let Some(refusal) = refusal_in(text) {
            return Some(refusal);
        }
    }
}

/// The refusal in one frame: an `Error` for the review, or one that names no request, since the
/// review is the only request this connection makes.
fn refusal_in(frame: &str) -> Option<(ErrorCode, String)> {
    let Ok(ServerMsg::Error { req, code, message }) = serde_json::from_str(frame) else {
        return None;
    };
    (req.is_none() || req == Some(REVIEW_REQ)).then_some((code, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_coordinator::protocol::RunId;

    fn event(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: 0,
            run: RunId("test".into()),
            kind,
        }
    }

    fn decision(claim: u64, approve: bool) -> EventKind {
        EventKind::ReviewDecided {
            claim: ClaimId(claim),
            approve,
            note: None,
        }
    }

    #[test]
    fn a_note_up_to_the_limit_passes_and_one_byte_more_is_refused() {
        assert!(check_note(None).is_ok());
        assert!(check_note(Some(&"a".repeat(MAX_NOTE_BYTES))).is_ok());
        let err = check_note(Some(&"a".repeat(MAX_NOTE_BYTES + 1))).unwrap_err();
        assert!(err.to_string().contains("1025 bytes"), "{err}");
    }

    #[test]
    fn an_error_for_the_review_or_for_no_request_is_a_refusal_and_nothing_else_is() {
        let error = |req: &str| {
            format!(r#"{{"type":"error","req":{req},"code":"malformed","message":"no"}}"#)
        };
        assert_eq!(
            refusal_in(&error("1")),
            Some((ErrorCode::Malformed, "no".into()))
        );
        assert_eq!(
            refusal_in(&error("null")),
            Some((ErrorCode::Malformed, "no".into()))
        );
        assert_eq!(refusal_in(&error("2")), None, "another request's error");
        assert_eq!(refusal_in(r#"{"type":"heartbeat_ack"}"#), None);
        assert_eq!(refusal_in("not json"), None);
    }

    #[test]
    fn the_limit_counts_bytes_not_characters() {
        let note = "é".repeat(MAX_NOTE_BYTES / 2 + 1);
        assert!(check_note(Some(&note)).is_err());
    }

    #[test]
    fn only_a_matching_decision_after_the_starting_point_confirms() {
        let log = [event(4, decision(7, true)), event(5, decision(8, true))];
        assert!(decided(&log, 4, ClaimId(7), true));
        assert!(
            !decided(&log, 5, ClaimId(7), true),
            "decided before we asked"
        );
        assert!(
            !decided(&log, 0, ClaimId(7), false),
            "decided the other way"
        );
        assert!(!decided(&log, 0, ClaimId(9), true), "another claim");
    }
}

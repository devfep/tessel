//! `tessel race`: open a race, enter one, pick its winner (protocol invariant 7). The coordinator
//! decides races end to end; its rules are at the top of `src/coordinator/racing.rs`.
//!
//! Opening and picking are a reviewer's acts, with the same authority as a review, so they run on
//! their own short-lived connection as `tessel review` does: a reviewer needs no daemon and holds
//! no claim. Joining makes a claim, so it goes through the daemon, which records the grant like
//! any other.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail};
use futures_util::StreamExt;
use tessel_coordinator::protocol::{
    AgentId, ClaimId, ClientMsg, Conflict, Criterion, ErrorCode, Event, EventKind, Intent, Mode,
    RaceEntry, RaceId, RequestId, ScopeClaim, ServerMsg,
};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::commands::Report;
use crate::config::Config;
use crate::daemon;
use crate::render::{
    conflicts_text, race_joined_text, race_opened_text, race_refused_text, race_result_text,
};
use crate::review;
use crate::rpc::{self, ClaimOutcome, ClientError, Reply, Request};
use crate::scope;
use crate::submit;
use crate::worktree::{Worktree, WorktreeError};
use crate::RaceAction;

/// The one request a race connection makes.
const RACE_REQ: RequestId = RequestId(1);
/// How long to wait for the coordinator's answer to an open or a pick.
pub const ANSWER_WAIT: Duration = Duration::from_secs(10);
const MINUTE_MS: u64 = 60_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Exit code of `race open` when the race's scopes are held, as `tessel claim`'s for a denial.
const EXIT_DENIED: u8 = 3;
/// Exit code of `race` when the coordinator refused the request (not a reviewer, the race is
/// closed or full, ...).
const EXIT_RACE_REFUSED: u8 = 10;
/// Exit code of `race open` and `race pick` when no answer came in time; it may still land.
const EXIT_RACE_UNANSWERED: u8 = 11;

/// What `tessel race open` was given on its command line.
pub struct OpenArgs {
    pub intent: String,
    pub scopes: Vec<String>,
    pub entrants: u32,
    pub deadline_minutes: u64,
    pub by: String,
}

/// An `OpenRace` ready to send.
pub struct Opening {
    pub intent: Intent,
    pub scopes: Vec<ScopeClaim>,
    pub max_entrants: u32,
    pub deadline_ms: u64,
    pub criteria: Vec<Criterion>,
}

/// What the coordinator said to a race request.
#[derive(Debug)]
pub enum Answer {
    Opened {
        race: RaceId,
        deadline_ms: u64,
    },
    /// The race's scopes are held, so it was not opened.
    Denied {
        conflicts: Vec<Conflict>,
    },
    Decided {
        race: RaceId,
        winner: Option<ClaimId>,
        ranking: Vec<ClaimId>,
        entries: Vec<RaceEntry>,
    },
    Refused {
        code: ErrorCode,
        message: String,
    },
    /// Nothing that answers the request came within `ANSWER_WAIT`; it may still have landed.
    Unanswered,
}

/// `--by`: a comma list of criteria, kept in the order given. `lowest_risk` and `smallest_diff`
/// are refused here, before anything is sent, for the reason the coordinator refuses them.
pub fn parse_criteria(list: &str) -> anyhow::Result<Vec<Criterion>> {
    let mut criteria = Vec::new();
    for word in list.split(',').map(str::trim) {
        let criterion = match word {
            "tests_pass" => Criterion::TestsPass,
            "first_submitted" => Criterion::FirstSubmitted,
            "human_pick" => Criterion::HumanPick,
            "lowest_risk" => Criterion::LowestRisk,
            "smallest_diff" => Criterion::SmallestDiff,
            other => bail!(
                "--by {other:?} is not a criterion; use tests_pass, first_submitted or \
                 human_pick. Nothing was sent"
            ),
        };
        match criterion {
            Criterion::LowestRisk | Criterion::SmallestDiff => bail!(
                "--by {word} is refused: risk_bp and diff_lines are never measured, so ranking \
                 by them would present the earliest joiner as the lowest risk \
                 (src/coordinator/racing.rs). Use tests_pass, first_submitted or human_pick; \
                 nothing was sent"
            ),
            Criterion::TestsPass | Criterion::FirstSubmitted | Criterion::HumanPick => {
                criteria.push(criterion);
            }
        }
    }
    Ok(criteria)
}

/// The deadline `minutes` from `now_ms`, in milliseconds since the epoch.
pub fn deadline_ms(now_ms: u64, minutes: u64) -> u64 {
    now_ms.saturating_add(minutes.saturating_mul(MINUTE_MS))
}

/// Sends `OpenRace` as the reviewer and waits for `RaceOpened`, `Denied` or a refusal.
pub async fn open(config: &Config, base: &str, opening: Opening) -> anyhow::Result<Answer> {
    let msg = ClientMsg::OpenRace {
        req: RACE_REQ,
        intent: opening.intent,
        scopes: opening.scopes,
        max_entrants: opening.max_entrants,
        deadline_ms: opening.deadline_ms,
        criteria: opening.criteria,
    };
    exchange(config, base, &msg, |answer| match answer {
        Answer::Opened { .. } | Answer::Denied { .. } | Answer::Refused { .. } => true,
        Answer::Decided { .. } | Answer::Unanswered => false,
    })
    .await
}

/// Sends `PickWinner` as the reviewer. The coordinator answers a pick only when it refuses it;
/// otherwise the picker hears the `RaceResult` that names `claim` the winner.
pub async fn pick(
    config: &Config,
    base: &str,
    race: RaceId,
    claim: ClaimId,
) -> anyhow::Result<Answer> {
    let msg = ClientMsg::PickWinner {
        req: RACE_REQ,
        race,
        claim,
    };
    exchange(config, base, &msg, |answer| match answer {
        Answer::Decided {
            race: decided,
            winner,
            ..
        } => *decided == race && *winner == Some(claim),
        Answer::Refused { .. } => true,
        Answer::Opened { .. } | Answer::Denied { .. } | Answer::Unanswered => false,
    })
    .await
}

/// Connects, sends `msg` and returns the first answer `wanted` accepts.
async fn exchange(
    config: &Config,
    base: &str,
    msg: &ClientMsg,
    wanted: impl Fn(&Answer) -> bool,
) -> anyhow::Result<Answer> {
    let mut socket = review::connect(config, base).await?;
    review::send(&mut socket, msg).await?;
    let deadline = Instant::now() + ANSWER_WAIT;
    let answer = loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break Answer::Unanswered;
        };
        let frame = match tokio::time::timeout(left, socket.next()).await {
            Err(_) | Ok(None | Some(Err(_))) => break Answer::Unanswered,
            Ok(Some(Ok(frame))) => frame,
        };
        let Message::Text(text) = frame else {
            continue;
        };
        let Some(answer) = serde_json::from_str(&text).ok().and_then(answer_in) else {
            continue;
        };
        if wanted(&answer) {
            break answer;
        }
    };
    let _ = socket.close(None).await;
    Ok(answer)
}

/// What one message on a race connection says. An `Error` that names no request counts, since
/// the race request is the only one the connection makes.
fn answer_in(msg: ServerMsg) -> Option<Answer> {
    match msg {
        ServerMsg::RaceOpened {
            req,
            race,
            deadline_ms,
        } => (req == RACE_REQ).then_some(Answer::Opened { race, deadline_ms }),
        ServerMsg::Denied { req, conflicts } => {
            (req == RACE_REQ).then_some(Answer::Denied { conflicts })
        }
        ServerMsg::RaceResult {
            race,
            winner,
            ranking,
            entries,
        } => Some(Answer::Decided {
            race,
            winner,
            ranking,
            entries,
        }),
        ServerMsg::Error { req, code, message } => req
            .is_none_or(|req| req == RACE_REQ)
            .then_some(Answer::Refused { code, message }),
        ServerMsg::Welcome { .. }
        | ServerMsg::Granted { .. }
        | ServerMsg::Shadowed { .. }
        | ServerMsg::Queued { .. }
        | ServerMsg::Accepted { .. }
        | ServerMsg::Merged { .. }
        | ServerMsg::SubmitRejected { .. }
        | ServerMsg::Uncovered { .. }
        | ServerMsg::ReviewRequired { .. }
        | ServerMsg::BaseMoved { .. }
        | ServerMsg::MergeDoubt { .. }
        | ServerMsg::AssumptionChallenged { .. }
        | ServerMsg::LeaseExpired { .. }
        | ServerMsg::Event { .. } => None,
    }
}

/// The scopes `race` holds, from its `RaceOpened` event. An entry holds exactly these, and the
/// grant does not repeat them.
pub fn race_scopes(events: &[Event], race: RaceId) -> Option<Vec<ScopeClaim>> {
    events.iter().find_map(|event| {
        let EventKind::RaceOpened {
            race: opened,
            scopes,
            ..
        } = &event.kind
        else {
            return None;
        };
        (*opened == race).then(|| scopes.clone())
    })
}

/// The inbox note for a `RaceResult` sent to `me`: the race and where my entry placed.
pub fn result_note(
    race: RaceId,
    winner: Option<ClaimId>,
    ranking: &[ClaimId],
    entries: &[RaceEntry],
    me: &AgentId,
) -> String {
    let place = match entries.iter().find(|entry| entry.agent == *me) {
        None => "you have no submitted entry in it".to_string(),
        Some(mine) => match ranking.iter().position(|claim| *claim == mine.claim) {
            Some(index) => {
                let won = if winner == Some(mine.claim) {
                    " and won"
                } else {
                    ""
                };
                format!(
                    "your entry, claim {}, placed {} of {}{won}",
                    mine.claim.0,
                    index + 1,
                    ranking.len()
                )
            }
            None => format!(
                "your entry, claim {}, was not ranked: its tests did not pass or did not run",
                mine.claim.0
            ),
        },
    };
    format!("race {}: {place}", race.0)
}

// ---------- the commands ----------

pub async fn command(cwd: &Path, action: RaceAction) -> anyhow::Result<Report> {
    match action {
        RaceAction::Open {
            intent,
            scopes,
            entrants,
            deadline,
            by,
        } => {
            let args = OpenArgs {
                intent,
                scopes,
                entrants,
                deadline_minutes: deadline,
                by,
            };
            open_command(cwd, args).await
        }
        RaceAction::Join { race } => join_command(cwd, RaceId(race)).await,
        RaceAction::Pick { race, claim } => pick_command(cwd, RaceId(race), ClaimId(claim)).await,
    }
}

fn printed(stdout: String, code: u8) -> Report {
    Report {
        stdout,
        stderr: String::new(),
        code,
    }
}

fn refused(what: &str, code: Option<ErrorCode>, message: &str) -> Report {
    Report {
        stdout: String::new(),
        stderr: race_refused_text(what, code, message),
        code: EXIT_RACE_REFUSED,
    }
}

fn unanswered(what: &str) -> Report {
    Report {
        stdout: String::new(),
        stderr: format!(
            "tessel: the coordinator did not answer within {} s; {what}\n",
            ANSWER_WAIT.as_secs()
        ),
        code: EXIT_RACE_UNANSWERED,
    }
}

fn load_config(dir: &Path) -> anyhow::Result<Config> {
    Ok(Config::load(dir, |name| std::env::var(name).ok())?)
}

/// The base a reviewer's `Hello` sends: this checkout's HEAD, or `NO_BASE` outside a commit.
fn hello_base(worktree: Option<&Worktree>) -> String {
    worktree
        .and_then(|worktree| submit::resolve_commit(&worktree.root, None).ok())
        .unwrap_or_else(|| review::NO_BASE.to_string())
}

/// A reviewer opens a race on its own connection; no daemon is needed. The criteria and scopes
/// are checked before anything is sent.
async fn open_command(cwd: &Path, args: OpenArgs) -> anyhow::Result<Report> {
    let criteria = parse_criteria(&args.by)?;
    let worktree = Worktree::discover(cwd)?;
    let config = load_config(&worktree.root)?;
    let mut scopes = Vec::new();
    for arg in &args.scopes {
        let scope = scope::parse(arg)?;
        scope::check_exists(&worktree.root, arg, &scope, Mode::EditBody)?;
        scopes.push(ScopeClaim {
            scope,
            mode: Mode::EditBody,
        });
    }
    let now = daemon::now_ms();
    let opening = Opening {
        intent: Intent {
            summary: args.intent,
            task_ref: None,
            assumptions: Vec::new(),
        },
        scopes,
        max_entrants: args.entrants,
        deadline_ms: deadline_ms(now, args.deadline_minutes),
        criteria,
    };
    Ok(
        match open(&config, &hello_base(Some(&worktree)), opening).await? {
            Answer::Opened { race, deadline_ms } => {
                printed(race_opened_text(race, deadline_ms, now), 0)
            }
            Answer::Denied { conflicts } => printed(
                format!(
                    "race not opened: its scopes are held\n{}",
                    conflicts_text(&conflicts)
                ),
                EXIT_DENIED,
            ),
            Answer::Refused { code, message } => refused("open", Some(code), &message),
            Answer::Decided { .. } | Answer::Unanswered => {
                unanswered("the race may still have opened")
            }
        },
    )
}

/// Enters a race through the daemon, which records the grant like any claim. The race's scopes
/// come from its `RaceOpened` event, since the grant does not carry them.
async fn join_command(cwd: &Path, race: RaceId) -> anyhow::Result<Report> {
    let worktree = Worktree::discover(cwd)?;
    let config = load_config(&worktree.root)?;
    let Reply::Status { state } = call_daemon(&worktree, &Request::Status).await? else {
        bail!("the daemon answered with something unexpected");
    };
    let log = daemon::read_log(&config, state.base.clone())
        .await
        .map_err(|e| anyhow!("cannot read the coordinator's event log: {e}"))?;
    let Some(scopes) = race_scopes(&log.events, race) else {
        let cut = if log.complete {
            ""
        } else {
            " (the read was cut short; try again)"
        };
        bail!(
            "race {} is not in the coordinator's event log{cut}; nothing was sent",
            race.0
        );
    };
    join_through_daemon(&worktree, race, scopes).await
}

/// Sends the join to the daemon and prints the entry it records.
async fn join_through_daemon(
    worktree: &Worktree,
    race: RaceId,
    scopes: Vec<ScopeClaim>,
) -> anyhow::Result<Report> {
    let request = Request::JoinRace { race, scopes };
    let Reply::Claim { outcome } = call_daemon(worktree, &request).await? else {
        bail!("the daemon answered with something unexpected");
    };
    match outcome {
        ClaimOutcome::Granted { claim, at_risk, .. } => {
            Ok(printed(race_joined_text(race, &claim, &at_risk), 0))
        }
        ClaimOutcome::Refused { code, message } => Ok(refused("join", code, &message)),
        ClaimOutcome::Covered | ClaimOutcome::Denied { .. } | ClaimOutcome::Queued { .. } => {
            bail!("the daemon answered with something unexpected")
        }
    }
}

/// A reviewer picks the winner of a `human_pick` race on its own connection.
async fn pick_command(cwd: &Path, race: RaceId, claim: ClaimId) -> anyhow::Result<Report> {
    // A reviewer needs no checkout, as for `tessel review`.
    let worktree = match Worktree::discover(cwd) {
        Ok(worktree) => Some(worktree),
        Err(WorktreeError::NotARepo(_)) => None,
        Err(e) => return Err(e.into()),
    };
    let config_dir = worktree
        .as_ref()
        .map_or(cwd, |worktree| worktree.root.as_path());
    let config = load_config(config_dir)?;
    let base = hello_base(worktree.as_ref());
    Ok(match pick(&config, &base, race, claim).await? {
        Answer::Decided {
            race,
            winner,
            ranking,
            entries,
        } => printed(
            format!(
                "picked claim {} as the winner of race {}; it goes to the merge queue and the \
                 other entries are rejected\n{}",
                claim.0,
                race.0,
                race_result_text(race, winner, &ranking, &entries)
            ),
            0,
        ),
        Answer::Refused { code, message } => refused("pick", Some(code), &message),
        Answer::Opened { .. } | Answer::Denied { .. } | Answer::Unanswered => {
            unanswered("the pick may still have landed; `tessel inbox` shows the result")
        }
    })
}

/// Calls the daemon, turning "not running" into advice, as the other commands do.
async fn call_daemon(worktree: &Worktree, request: &Request) -> anyhow::Result<Reply> {
    match rpc::call(&worktree.sock(), request, CALL_TIMEOUT).await {
        Ok(reply) => Ok(reply),
        Err(ClientError::NotRunning) => {
            bail!("no daemon is running for this worktree; run `tessel start \"<intent>\"` first")
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_coordinator::protocol::CommitId;

    #[test]
    fn criteria_keep_the_order_given() {
        let criteria = parse_criteria("human_pick, tests_pass,first_submitted").unwrap_or_default();
        assert_eq!(
            criteria,
            [
                Criterion::HumanPick,
                Criterion::TestsPass,
                Criterion::FirstSubmitted
            ]
        );
    }

    #[test]
    fn unmeasured_unknown_and_empty_criteria_are_refused() {
        for list in ["lowest_risk", "tests_pass,smallest_diff"] {
            let error = parse_criteria(list).map_or_else(|e| e.to_string(), |_| String::new());
            assert!(error.contains("never measured"), "{list}: {error}");
        }
        for list in ["", "tests_pass,", "fastest"] {
            let error = parse_criteria(list).map_or_else(|e| e.to_string(), |_| String::new());
            assert!(error.contains("is not a criterion"), "{list}: {error}");
        }
    }

    #[test]
    fn the_deadline_is_minutes_after_now_in_ms() {
        assert_eq!(deadline_ms(1_000, 30), 1_000 + 30 * 60 * 1000);
        assert_eq!(deadline_ms(u64::MAX - 1, 1), u64::MAX);
    }

    fn entry(claim: u64, agent: &str) -> RaceEntry {
        RaceEntry {
            claim: ClaimId(claim),
            agent: AgentId(agent.into()),
            fork_commit: CommitId("f".repeat(40)),
            submitted_at_ms: claim,
            tests_passed: Some(claim != 9),
            risk_bp: None,
            diff_lines: None,
        }
    }

    #[test]
    fn the_note_names_the_place_of_my_entry_or_says_i_had_none() {
        let entries = [entry(5, "a1"), entry(7, "a2"), entry(9, "a3")];
        let ranking = [ClaimId(7), ClaimId(5)];
        let note = |winner, me: &str| {
            result_note(RaceId(2), winner, &ranking, &entries, &AgentId(me.into()))
        };
        assert_eq!(
            note(Some(ClaimId(7)), "a2"),
            "race 2: your entry, claim 7, placed 1 of 2 and won"
        );
        assert_eq!(
            note(Some(ClaimId(7)), "a1"),
            "race 2: your entry, claim 5, placed 2 of 2"
        );
        assert_eq!(
            note(None, "a1"),
            "race 2: your entry, claim 5, placed 2 of 2"
        );
        assert!(note(None, "a3").contains("claim 9, was not ranked"));
        assert_eq!(
            note(None, "r1"),
            "race 2: you have no submitted entry in it"
        );
    }
}

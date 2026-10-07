//! The Claude Code `PreToolUse` hook: claim a file before the agent edits it, and block the
//! edit when the claim is denied. `install` writes the hook entry into the worktree's
//! `.claude/settings.local.json`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tessel_coordinator::protocol::{Conflict, Mode, Scope, ScopeClaim};
use thiserror::Error;

use crate::plan::{file_claims, plan_create, plan_edit, plan_rewrite, Replace};
use crate::render::{denial_text, escape, mode_text, one_line, quote_untrusted};
use crate::rpc::{self, ClaimOutcome, ClientError, Reply, Request};
use crate::scope::{locate, Located};
use crate::state::write_atomic;
use crate::worktree::{Worktree, WorktreeError};

const HOOK_TIMEOUT: Duration = Duration::from_secs(15);

/// One entry `hook install` writes into `settings.local.json`.
struct HookSpec {
    event: &'static str,
    /// The tools the hook runs for; `None` runs it for every occurrence of the event.
    matcher: Option<&'static str>,
    subcommand: &'static str,
    /// Claude Code's own timeout in seconds; `None` leaves its default.
    timeout: Option<u64>,
}

const PRE_EDIT: HookSpec = HookSpec {
    event: "PreToolUse",
    matcher: Some("Edit|MultiEdit|Write|NotebookEdit"),
    subcommand: "hook pre-edit",
    timeout: None,
};

/// Longer than the 120 s the stop hook waits for the steward.
const STOP_TIMEOUT_SECS: u64 = 150;

const HOOKS: [HookSpec; 5] = [
    PRE_EDIT,
    HookSpec {
        event: "PostToolUse",
        matcher: None,
        subcommand: "hook inbox",
        timeout: Some(10),
    },
    HookSpec {
        event: "UserPromptSubmit",
        matcher: None,
        subcommand: "hook inbox",
        timeout: Some(10),
    },
    HookSpec {
        event: "SessionStart",
        matcher: None,
        subcommand: "hook inbox",
        timeout: Some(10),
    },
    HookSpec {
        event: "Stop",
        matcher: None,
        subcommand: "hook stop",
        timeout: Some(STOP_TIMEOUT_SECS),
    },
];

/// Exit code that lets the tool run.
const EXIT_ALLOW: u8 = 0;
/// Exit code that blocks the tool and feeds stderr back to the agent. Any other non-zero code
/// would let the edit through, so every failure maps here.
pub const EXIT_BLOCK: u8 = 2;

/// What the hook tells Claude Code: `exit` 0 lets the tool run, 2 blocks it and feeds `message`
/// (stderr) back to the agent.
#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    pub exit: u8,
    pub message: String,
}

/// Every way one `PreToolUse` event can end. `verdict` is the only place that turns an outcome
/// into an exit code, and it matches exhaustively: the first five variants allow the edit, every
/// other variant blocks it.
#[derive(Debug)]
enum Outcome {
    Covered,
    Granted,
    OutsideWorktree,
    IgnoredPath,
    NotAnEditTool,
    UnreadableInput(String),
    MissingPath {
        tool: String,
        key: &'static str,
    },
    PathNotUtf8(String),
    NoRoot,
    CwdUnusable(String),
    Worktree(WorktreeError),
    NoDaemon(PathBuf),
    DaemonUnreachable(ClientError),
    Denied {
        rel: String,
        hint: String,
        conflicts: Vec<Conflict>,
    },
    Queued {
        rel: String,
    },
    Refused {
        rel: String,
        message: String,
    },
    Failed {
        rel: String,
        message: String,
    },
    UnexpectedReply,
}

impl Outcome {
    fn verdict(self) -> Verdict {
        let block = |message: String| Verdict {
            exit: EXIT_BLOCK,
            message,
        };
        match self {
            Self::Covered
            | Self::Granted
            | Self::OutsideWorktree
            | Self::IgnoredPath
            | Self::NotAnEditTool => Verdict {
                exit: EXIT_ALLOW,
                message: String::new(),
            },
            Self::UnreadableInput(why) => block(format!(
                "tessel hook: cannot read the hook input ({why}); blocking the edit rather than \
                 letting it through unclaimed\n"
            )),
            Self::MissingPath { tool, key } => block(format!(
                "tessel hook: {tool} came without `{key}`; blocking the edit\n"
            )),
            Self::PathNotUtf8(raw) => block(format!(
                "tessel hook: {} names a path that is not valid UTF-8, which cannot be claimed; \
                 blocking the edit\n",
                escape(&raw)
            )),
            Self::NoRoot => block(
                "tessel hook: this hook does not know which worktree it guards (no `--root` and \
                 no $CLAUDE_PROJECT_DIR); blocking the edit. Run `tessel hook install` again in \
                 the worktree.\n"
                    .to_string(),
            ),
            Self::CwdUnusable(cwd) => block(format!(
                "tessel hook: the working directory {} is not usable, so the edited path cannot \
                 be resolved; blocking the edit\n",
                escape(&cwd)
            )),
            Self::Worktree(e) => block(format!(
                "tessel hook: cannot set up the guarded worktree ({}); blocking the edit rather \
                 than letting it through unclaimed. If the worktree moved, run `tessel hook \
                 install` again in it.\n",
                one_line(&e.to_string())
            )),
            Self::NoDaemon(sock) => block(format!(
                "tessel: no daemon is running for this worktree (nothing listens on {}), so edits \
                 cannot be claimed. Run `tessel start \"<what you are about to do>\"` and \
                 retry.\n",
                escape(&sock.display().to_string())
            )),
            Self::DaemonUnreachable(e) => block(format!("tessel: cannot reach the daemon: {e}\n")),
            Self::Denied {
                rel,
                hint,
                conflicts,
            } => block(format!(
                "tessel: cannot edit {}; another agent holds it.\n{}",
                escape(&rel),
                denial_text(&conflicts, &escape(&hint))
            )),
            Self::Queued { rel } => block(format!(
                "tessel: {} is queued behind another agent; wait for the grant in `tessel inbox`\n",
                escape(&rel)
            )),
            Self::Refused { rel, message } => block(format!(
                "tessel: could not claim {}.\n{}",
                escape(&rel),
                quote_untrusted("the coordinator", &message)
            )),
            Self::Failed { rel, message } => block(format!(
                "tessel: could not claim {}: {}\n",
                escape(&rel),
                one_line(&message)
            )),
            Self::UnexpectedReply => {
                block("tessel: the daemon answered with something unexpected\n".to_string())
            }
        }
    }
}

#[derive(Deserialize)]
struct HookInput {
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    cwd: Option<String>,
}

/// Decides one `PreToolUse` event. `stdin` is the hook JSON as read from standard input;
/// a relative path is read from the event's `cwd` and never from this process's own directory,
/// which can be anywhere; an event with no `cwd` and a relative path is blocked. `root` is the
/// guarded worktree: whether an edit is inside it is decided from the edited file's resolved
/// path, never from the cwd.
pub async fn pre_edit(stdin: std::io::Result<Vec<u8>>, root: Option<&Path>) -> Verdict {
    decide(stdin, root).await.verdict()
}

async fn decide(stdin: std::io::Result<Vec<u8>>, root: Option<&Path>) -> Outcome {
    let bytes = match stdin {
        Ok(bytes) => bytes,
        Err(e) => return Outcome::UnreadableInput(e.to_string()),
    };
    let input: HookInput = match serde_json::from_slice(&bytes) {
        Ok(input) => input,
        Err(e) => return Outcome::UnreadableInput(e.to_string()),
    };
    let key = match input.tool_name.as_str() {
        "Edit" | "MultiEdit" | "Write" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return Outcome::NotAnEditTool,
    };
    let Some(raw) = input.tool_input.get(key).and_then(Value::as_str) else {
        return Outcome::MissingPath {
            tool: input.tool_name,
            key,
        };
    };
    let cwd = input.cwd.map(std::path::PathBuf::from);
    let Some(root) = root else {
        return Outcome::NoRoot;
    };
    let worktree = match Worktree::discover(root) {
        Ok(worktree) => worktree,
        Err(e) => return Outcome::Worktree(e),
    };
    let cwd = match cwd {
        Some(cwd) if cwd.is_absolute() && cwd.is_dir() => cwd,
        Some(cwd) if Path::new(raw).is_relative() => {
            return Outcome::CwdUnusable(cwd.display().to_string());
        }
        None if Path::new(raw).is_relative() => {
            return Outcome::CwdUnusable("(the hook input has no cwd)".to_string());
        }
        Some(_) | None => worktree.root.clone(),
    };
    let rel = match locate(&worktree.root, &cwd, raw) {
        Located::Inside(rel) => rel,
        Located::Outside => return Outcome::OutsideWorktree,
        Located::NotUtf8 => return Outcome::PathNotUtf8(raw.to_string()),
    };
    let internal = |dir: &str| rel == dir || rel.starts_with(&format!("{dir}/"));
    if internal(".tessel") || internal(".git") {
        return Outcome::IgnoredPath;
    }
    let wanted = wanted_scopes(&input.tool_name, &input.tool_input, &worktree.root, &rel);
    claim_scopes(&worktree, wanted).await
}

/// The scopes to hold before this tool call runs: the finest ones that stay honest, decided from
/// the file as it is now and the edit as the agent sent it (see `plan::plan_edit`).
fn wanted_scopes(tool: &str, input: &Value, root: &Path, rel: &str) -> Vec<ScopeClaim> {
    let abs = root.join(rel);
    if !abs.exists() {
        return plan_create(rel);
    }
    match tool {
        "Write" => plan_rewrite(rel),
        "Edit" | "MultiEdit" => {
            let current = std::fs::read_to_string(&abs).ok();
            let edits = parse_edits(tool, input);
            match (current, edits) {
                (Some(current), Some(edits)) => plan_edit(rel, &current, &edits),
                (None, _) | (_, None) => file_claims(rel, &[Mode::EditBody]),
            }
        }
        _ => file_claims(rel, &[Mode::EditBody]),
    }
}

/// The replacements of an `Edit` (one) or a `MultiEdit` (a list), or `None` when the tool input
/// does not have the expected shape.
fn parse_edits<'a>(tool: &str, input: &'a Value) -> Option<Vec<Replace<'a>>> {
    let one = |edit: &'a Value| {
        Some(Replace {
            old: edit.get("old_string")?.as_str()?,
            new: edit.get("new_string")?.as_str()?,
            all: edit
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    };
    if tool == "Edit" {
        return Some(vec![one(input)?]);
    }
    input.get("edits")?.as_array()?.iter().map(one).collect()
}

/// A scope as the agent would type it to `tessel claim`.
pub fn claim_arg(scope: &Scope) -> String {
    match scope {
        Scope::Dir { path } => format!("{path}/"),
        Scope::File { path } => path.clone(),
        Scope::Symbol(id) => format!("{}::{}", id.path, id.qualified_name),
    }
}

/// The `tessel claim` command that waits for these scopes, one per mode.
fn wait_hint(wanted: &[ScopeClaim]) -> String {
    let mut modes: Vec<Mode> = Vec::new();
    for claim in wanted {
        if !modes.contains(&claim.mode) {
            modes.push(claim.mode);
        }
    }
    let commands: Vec<String> = modes
        .iter()
        .map(|mode| {
            let scopes: Vec<String> = wanted
                .iter()
                .filter(|claim| claim.mode == *mode)
                .map(|claim| shell_quote(&claim_arg(&claim.scope)))
                .collect();
            let flag = match mode {
                Mode::EditBody => String::new(),
                Mode::Depend | Mode::EditSignature | Mode::Create => {
                    format!(" --mode {}", mode_text(*mode))
                }
            };
            format!("tessel claim {}{flag} --wait", scopes.join(" "))
        })
        .collect();
    commands.join("; ")
}

async fn claim_scopes(worktree: &Worktree, wanted: Vec<ScopeClaim>) -> Outcome {
    let rel = wanted
        .iter()
        .map(|claim| claim_arg(&claim.scope))
        .collect::<Vec<_>>()
        .join(", ");
    let hint = wait_hint(&wanted);
    let request = Request::Ensure { wanted };
    let reply = match rpc::call(&worktree.sock(), &request, HOOK_TIMEOUT).await {
        Ok(reply) => reply,
        Err(ClientError::NotRunning) => return Outcome::NoDaemon(worktree.sock()),
        Err(e) => return Outcome::DaemonUnreachable(e),
    };
    match reply {
        Reply::Claim {
            outcome: ClaimOutcome::Granted { .. },
        } => Outcome::Granted,
        Reply::Claim {
            outcome: ClaimOutcome::Covered,
        } => Outcome::Covered,
        Reply::Claim {
            outcome: ClaimOutcome::Denied { conflicts },
        } => Outcome::Denied {
            rel,
            hint,
            conflicts,
        },
        Reply::Claim {
            outcome: ClaimOutcome::Queued { .. },
        } => Outcome::Queued { rel },
        Reply::Claim {
            outcome: ClaimOutcome::Refused { message, .. },
        } => Outcome::Refused { rel, message },
        Reply::Failed { message } => Outcome::Failed { rel, message },
        Reply::Status { .. }
        | Reply::Released { .. }
        | Reply::Submit { .. }
        | Reply::Stopping { .. } => Outcome::UnexpectedReply,
    }
}

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("{path} is not a JSON object with the expected shape: {reason}; fix or remove it")]
    Shape { path: String, reason: String },
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {message}")]
    Write { path: String, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Added,
    Updated,
    AlreadyPresent,
}

impl Installed {
    /// The outcome of two entries together: updated if either was (something already there
    /// changed), else added if either was, else already present.
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Updated, _) | (_, Self::Updated) => Self::Updated,
            (Self::Added, _) | (_, Self::Added) => Self::Added,
            (Self::AlreadyPresent, Self::AlreadyPresent) => Self::AlreadyPresent,
        }
    }
}

/// Merges the hook entries into `<worktree>/.claude/settings.local.json`, keeping everything else.
/// Running it twice changes nothing the second time.
pub fn install(worktree: &Worktree, exe: &Path) -> Result<Installed, InstallError> {
    let path = worktree.root.join(".claude").join("settings.local.json");
    let shown = path.display().to_string();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Value>(&text).map_err(|e| InstallError::Shape {
            path: shown.clone(),
            reason: e.to_string(),
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(source) => {
            return Err(InstallError::Read {
                path: shown,
                source,
            })
        }
    };
    let (Some(exe), Some(root)) = (exe.to_str(), worktree.root.to_str()) else {
        return Err(InstallError::Shape {
            path: shown,
            reason: "the tessel binary or the worktree path is not valid UTF-8".into(),
        });
    };
    let shape = |reason: &str| InstallError::Shape {
        path: shown.clone(),
        reason: reason.into(),
    };
    let mut outcome = Installed::AlreadyPresent;
    for spec in &HOOKS {
        // Never write the pre-edit hook a `timeout` below `HOOK_TIMEOUT`: Claude Code does not
        // block the edit when a hook times out, so a short timeout would let an edit through
        // while the daemon is still answering.
        let command = format!(
            "{} {} --root {}",
            shell_quote(exe),
            spec.subcommand,
            shell_quote(root)
        );
        outcome = outcome.combine(merge_entry(&mut doc, spec, &command).map_err(shape)?);
    }
    if outcome == Installed::AlreadyPresent {
        return Ok(outcome);
    }
    let write_err = |message: String| InstallError::Write {
        path: shown.clone(),
        message,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| write_err(e.to_string()))?;
    }
    let mut text = serde_json::to_string_pretty(&doc).map_err(|e| write_err(e.to_string()))?;
    text.push('\n');
    write_atomic(&path, text.as_bytes()).map_err(|e| write_err(e.to_string()))?;
    Ok(outcome)
}

fn merge_entry(doc: &mut Value, spec: &HookSpec, command: &str) -> Result<Installed, &'static str> {
    let root = doc.as_object_mut().ok_or("top level is not an object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("`hooks` is not an object")?;
    let entries = hooks
        .entry(spec.event)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("`hooks.<event>` is not an array")?;
    let mut outcome = None;
    for entry in entries.iter_mut() {
        let Some(commands) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut ours = false;
        for hook in commands {
            let is_ours = hook
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|text| is_our_command(text, spec.subcommand));
            if !is_ours {
                continue;
            }
            ours = true;
            outcome = outcome.or(Some(Installed::AlreadyPresent));
            if hook["command"] != json!(command) {
                hook["command"] = json!(command);
                outcome = Some(Installed::Updated);
            }
            if let Some(timeout) = spec.timeout {
                if hook["timeout"] != json!(timeout) {
                    hook["timeout"] = json!(timeout);
                    outcome = Some(Installed::Updated);
                }
            }
        }
        if ours && set_matcher(entry, spec.matcher) {
            outcome = Some(Installed::Updated);
        }
    }
    if let Some(outcome) = outcome {
        return Ok(outcome);
    }
    let mut hook = json!({ "type": "command", "command": command });
    if let Some(timeout) = spec.timeout {
        hook["timeout"] = json!(timeout);
    }
    let mut entry = json!({ "hooks": [hook] });
    if let Some(matcher) = spec.matcher {
        entry["matcher"] = json!(matcher);
    }
    entries.push(entry);
    Ok(Installed::Added)
}

/// Makes `entry`'s matcher `matcher`; true when that changed it.
fn set_matcher(entry: &mut Value, matcher: Option<&str>) -> bool {
    let Some(object) = entry.as_object_mut() else {
        return false;
    };
    match matcher {
        Some(matcher) if object.get("matcher") != Some(&json!(matcher)) => {
            object.insert("matcher".into(), json!(matcher));
            true
        }
        None if object.contains_key("matcher") => {
            object.remove("matcher");
            true
        }
        Some(_) | None => false,
    }
}

/// Our hook for `subcommand`, in the old form (`<exe> hook pre-edit`) or the current one
/// (`... --root <dir>`).
fn is_our_command(command: &str, subcommand: &str) -> bool {
    command
        .find(subcommand)
        .is_some_and(|at| command[..at].contains("tessel"))
}

pub fn shell_quote(text: &str) -> String {
    let safe = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':'));
    if safe {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "/bin/tessel hook pre-edit";

    #[test]
    fn merge_keeps_other_hooks_and_is_idempotent() {
        let mut doc = json!({
            "permissions": { "allow": ["Bash(ls)"] },
            "hooks": { "PreToolUse": [
                { "matcher": "Bash", "hooks": [{ "type": "command", "command": "other" }] }
            ] }
        });
        assert_eq!(merge_entry(&mut doc, &PRE_EDIT, CMD), Ok(Installed::Added));
        let once = doc.clone();
        assert_eq!(
            merge_entry(&mut doc, &PRE_EDIT, CMD),
            Ok(Installed::AlreadyPresent)
        );
        assert_eq!(doc, once);
        assert_eq!(doc["permissions"]["allow"][0], "Bash(ls)");
        assert_eq!(doc["hooks"]["PreToolUse"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn merge_upgrades_an_old_entry_without_root_in_place() {
        let mut doc = json!({});
        merge_entry(&mut doc, &PRE_EDIT, CMD).unwrap();
        let new = "/bin/tessel hook pre-edit --root /work/a";
        assert_eq!(
            merge_entry(&mut doc, &PRE_EDIT, new),
            Ok(Installed::Updated)
        );
        assert_eq!(doc["hooks"]["PreToolUse"].as_array().map(Vec::len), Some(1));
        assert_eq!(doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"], new);
        assert_eq!(
            merge_entry(&mut doc, &PRE_EDIT, new),
            Ok(Installed::AlreadyPresent)
        );
        let moved = "/bin/tessel hook pre-edit --root /work/b";
        assert_eq!(
            merge_entry(&mut doc, &PRE_EDIT, moved),
            Ok(Installed::Updated)
        );
    }

    #[test]
    fn merge_updates_a_moved_binary_in_place() {
        let mut doc = json!({});
        merge_entry(&mut doc, &PRE_EDIT, "/old/tessel hook pre-edit").unwrap();
        assert_eq!(
            merge_entry(&mut doc, &PRE_EDIT, CMD),
            Ok(Installed::Updated)
        );
        assert_eq!(doc["hooks"]["PreToolUse"].as_array().map(Vec::len), Some(1));
        assert_eq!(doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"], CMD);
    }

    #[test]
    fn merge_refuses_a_malformed_settings_file() {
        let mut doc = json!({ "hooks": [] });
        assert!(merge_entry(&mut doc, &PRE_EDIT, "x tessel hook pre-edit").is_err());
        let mut doc = json!([]);
        assert!(merge_entry(&mut doc, &PRE_EDIT, "x tessel hook pre-edit").is_err());
        let mut doc = json!({ "hooks": { "Stop": {} } });
        assert!(merge_entry(&mut doc, &HOOKS[4], "x tessel hook stop").is_err());
    }

    #[test]
    fn each_subcommand_finds_only_its_own_entry() {
        let inbox = &HOOKS[1];
        let stop = &HOOKS[4];
        assert!(is_our_command(
            "/bin/tessel hook inbox --root /w",
            inbox.subcommand
        ));
        assert!(!is_our_command(
            "/bin/tessel hook inbox --root /w",
            stop.subcommand
        ));
        assert!(!is_our_command("/bin/other hook inbox", inbox.subcommand));
        let mut doc = json!({ "hooks": { "Stop": [
            { "hooks": [{ "type": "command", "command": "their-stop-hook" }] }
        ] } });
        assert_eq!(
            merge_entry(&mut doc, stop, "/bin/tessel hook stop --root /w"),
            Ok(Installed::Added)
        );
        assert_eq!(doc["hooks"]["Stop"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            doc["hooks"]["Stop"][1]["hooks"][0]["timeout"],
            STOP_TIMEOUT_SECS
        );
        assert!(doc["hooks"]["Stop"][1].get("matcher").is_none());
    }

    #[test]
    fn merge_resets_a_changed_timeout_and_a_stray_matcher() {
        let stop = &HOOKS[4];
        let command = "/bin/tessel hook stop --root /w";
        let mut doc = json!({ "hooks": { "Stop": [
            { "matcher": "x", "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }
        ] } });
        assert_eq!(merge_entry(&mut doc, stop, command), Ok(Installed::Updated));
        assert_eq!(
            doc["hooks"]["Stop"][0]["hooks"][0]["timeout"],
            STOP_TIMEOUT_SECS
        );
        assert!(doc["hooks"]["Stop"][0].get("matcher").is_none());
        assert_eq!(
            merge_entry(&mut doc, stop, command),
            Ok(Installed::AlreadyPresent)
        );
    }

    #[test]
    fn combined_outcomes_report_a_change_to_an_existing_entry_first() {
        use Installed::{Added, AlreadyPresent, Updated};
        assert_eq!(AlreadyPresent.combine(AlreadyPresent), AlreadyPresent);
        assert_eq!(AlreadyPresent.combine(Added), Added);
        assert_eq!(Added.combine(Updated), Updated);
        assert_eq!(Updated.combine(Added), Updated);
    }

    #[test]
    fn quoting_protects_spaces_and_quotes() {
        assert_eq!(shell_quote("/a/b-c_d.e"), "/a/b-c_d.e");
        assert_eq!(shell_quote("/a b/it's"), "'/a b/it'\\''s'");
    }
}

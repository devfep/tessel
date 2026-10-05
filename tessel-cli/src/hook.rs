//! The Claude Code `PreToolUse` hook: claim a file before the agent edits it, and block the
//! edit when the claim is denied. `install` writes the hook entry into the worktree's
//! `.claude/settings.local.json`.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;

use crate::render::{denial_text, one_line, quote_untrusted};
use crate::rpc::{self, ClaimOutcome, ClientError, Reply, Request};
use crate::scope::relative_to_root;
use crate::state::write_atomic;
use crate::worktree::Worktree;

const HOOK_TIMEOUT: Duration = Duration::from_secs(15);
const HOOK_MATCHER: &str = "Edit|MultiEdit|Write|NotebookEdit";
const HOOK_SUBCOMMAND: &str = "hook pre-edit";

/// What the hook tells Claude Code: exit 0 lets the tool run, exit 2 blocks it and feeds
/// `message` (stderr) back to the agent.
#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    pub allow: bool,
    pub message: String,
}

impl Verdict {
    fn allow() -> Self {
        Self {
            allow: true,
            message: String::new(),
        }
    }

    fn block(message: String) -> Self {
        Self {
            allow: false,
            message,
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

/// Decides one `PreToolUse` event. `stdin` is the hook JSON; `process_cwd` is used when the
/// event carries no `cwd`.
pub async fn pre_edit(stdin: &str, process_cwd: &Path) -> Verdict {
    let input: HookInput = match serde_json::from_str(stdin) {
        Ok(input) => input,
        Err(e) => {
            return Verdict::block(format!(
                "tessel hook: cannot read the hook input ({e}); blocking the edit rather than \
                 letting it through unclaimed\n"
            ))
        }
    };
    let key = match input.tool_name.as_str() {
        "Edit" | "MultiEdit" | "Write" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return Verdict::allow(),
    };
    let Some(raw) = input.tool_input.get(key).and_then(Value::as_str) else {
        return Verdict::block(format!(
            "tessel hook: {} came without `{key}`; blocking the edit\n",
            input.tool_name
        ));
    };
    let cwd = input
        .cwd
        .map_or_else(|| process_cwd.to_path_buf(), Into::into);
    let Ok(worktree) = Worktree::discover(&cwd) else {
        return Verdict::allow();
    };
    let Some(rel) = relative_to_root(&worktree.root, &cwd, raw) else {
        return Verdict::allow();
    };
    if rel == ".tessel" || rel.starts_with(".tessel/") {
        return Verdict::allow();
    }
    let create = !worktree.root.join(&rel).exists();
    claim_file(&worktree, &rel, create).await
}

async fn claim_file(worktree: &Worktree, rel: &str, create: bool) -> Verdict {
    let request = Request::Ensure {
        path: rel.to_string(),
        create,
    };
    let reply = match rpc::call(&worktree.sock(), &request, HOOK_TIMEOUT).await {
        Ok(reply) => reply,
        Err(ClientError::NotRunning) => {
            return Verdict::block(
                "tessel: no daemon is running for this worktree, so edits cannot be claimed. \
                 Run `tessel start \"<what you are about to do>\"` and retry.\n"
                    .to_string(),
            )
        }
        Err(e) => return Verdict::block(format!("tessel: cannot reach the daemon: {e}\n")),
    };
    match reply {
        Reply::Claim {
            outcome: ClaimOutcome::Granted { .. } | ClaimOutcome::Covered,
        } => Verdict::allow(),
        Reply::Claim {
            outcome: ClaimOutcome::Denied { conflicts },
        } => {
            let hint = format!("tessel claim {rel} --wait");
            Verdict::block(format!(
                "tessel: cannot edit {rel}; another agent holds it.\n{}",
                denial_text(&conflicts, &hint)
            ))
        }
        Reply::Claim {
            outcome: ClaimOutcome::Queued { .. },
        } => Verdict::block(format!(
            "tessel: {rel} is queued behind another agent; wait for the grant in `tessel inbox`\n"
        )),
        Reply::Claim {
            outcome: ClaimOutcome::Refused { message, .. },
        } => Verdict::block(format!(
            "tessel: could not claim {rel}.\n{}",
            quote_untrusted("the coordinator", &message)
        )),
        Reply::Failed { message } => Verdict::block(format!(
            "tessel: could not claim {rel}: {}\n",
            one_line(&message)
        )),
        Reply::Status { .. } | Reply::Released { .. } | Reply::Stopping => {
            Verdict::block("tessel: the daemon answered with something unexpected\n".to_string())
        }
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

#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    Added,
    Updated,
    AlreadyPresent,
}

/// Merges the hook entry into `<worktree>/.claude/settings.local.json`, keeping everything else.
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
    let command = format!(
        "{} {HOOK_SUBCOMMAND}",
        shell_quote(&exe.display().to_string())
    );
    let shape = |reason: &str| InstallError::Shape {
        path: shown.clone(),
        reason: reason.into(),
    };
    let outcome = merge_entry(&mut doc, &command).map_err(shape)?;
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

fn merge_entry(doc: &mut Value, command: &str) -> Result<Installed, &'static str> {
    let root = doc.as_object_mut().ok_or("top level is not an object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("`hooks` is not an object")?;
    let pre = hooks
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("`hooks.PreToolUse` is not an array")?;
    let mut outcome = None;
    for entry in pre.iter_mut() {
        let Some(commands) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        for hook in commands {
            let ours = hook
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| c.contains("tessel") && c.trim_end().ends_with(HOOK_SUBCOMMAND));
            if !ours {
                continue;
            }
            if hook["command"] == json!(command) {
                outcome = outcome.or(Some(Installed::AlreadyPresent));
            } else {
                hook["command"] = json!(command);
                outcome = Some(Installed::Updated);
            }
        }
        if outcome.is_some() && entry["matcher"] != json!(HOOK_MATCHER) {
            entry["matcher"] = json!(HOOK_MATCHER);
            outcome = Some(Installed::Updated);
        }
    }
    if let Some(outcome) = outcome {
        return Ok(outcome);
    }
    pre.push(json!({
        "matcher": HOOK_MATCHER,
        "hooks": [{ "type": "command", "command": command }],
    }));
    Ok(Installed::Added)
}

fn shell_quote(text: &str) -> String {
    let safe = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'));
    if safe {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_other_hooks_and_is_idempotent() {
        let mut doc = json!({
            "permissions": { "allow": ["Bash(ls)"] },
            "hooks": { "PreToolUse": [
                { "matcher": "Bash", "hooks": [{ "type": "command", "command": "other" }] }
            ] }
        });
        assert_eq!(
            merge_entry(&mut doc, "/bin/tessel hook pre-edit"),
            Ok(Installed::Added)
        );
        let once = doc.clone();
        assert_eq!(
            merge_entry(&mut doc, "/bin/tessel hook pre-edit"),
            Ok(Installed::AlreadyPresent)
        );
        assert_eq!(doc, once);
        assert_eq!(doc["permissions"]["allow"][0], "Bash(ls)");
        assert_eq!(doc["hooks"]["PreToolUse"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn merge_updates_a_moved_binary_in_place() {
        let mut doc = json!({});
        merge_entry(&mut doc, "/old/tessel hook pre-edit").unwrap();
        assert_eq!(
            merge_entry(&mut doc, "/new/tessel hook pre-edit"),
            Ok(Installed::Updated)
        );
        assert_eq!(doc["hooks"]["PreToolUse"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "/new/tessel hook pre-edit"
        );
    }

    #[test]
    fn merge_refuses_a_malformed_settings_file() {
        let mut doc = json!({ "hooks": [] });
        assert!(merge_entry(&mut doc, "x tessel hook pre-edit").is_err());
        let mut doc = json!([]);
        assert!(merge_entry(&mut doc, "x tessel hook pre-edit").is_err());
    }

    #[test]
    fn quoting_protects_spaces_and_quotes() {
        assert_eq!(shell_quote("/a/b-c_d.e"), "/a/b-c_d.e");
        assert_eq!(shell_quote("/a b/it's"), "'/a b/it'\\''s'");
    }
}

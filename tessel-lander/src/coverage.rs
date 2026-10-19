//! Invariant 11 (`src/protocol.rs`): every scope a submission touched must be covered by the
//! claim. The coordinator checks the `touched` list the agent sends; this module checks the
//! change the lander actually sees, the submitted commit rebased onto main.
//!
//! The check is file level. A `symbol` scope covers only its own file, so which symbol inside the
//! file was edited still rests on the agent's `touched`. File names come from the repo and are
//! untrusted data: they are compared as strings and never put into a command.
//!
//! Which modes permit which is [`Mode::permits`], called here and not copied, so the lander and
//! the coordinator cannot disagree.

use std::collections::HashSet;

use serde_json::Value;
use tessel_coordinator::protocol::{Mode, Scope, ScopeClaim};

/// Most paths a refusal names; the rest are counted, not listed.
pub const MAX_REPORTED_FILES: usize = 50;

/// A change to one path that needs a claim in a mode that permits `mode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub path: String,
    pub mode: Mode,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NameStatusError {
    #[error("unknown status {status:?} in `git diff --name-status` output {input:?}")]
    UnknownStatus { status: String, input: String },
    #[error(
        "missing or empty path after status {status:?} in `git diff --name-status` output {input:?}"
    )]
    MissingPath { status: String, input: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ScopesError {
    #[error("scopes must be an array of the claim's {{scope, mode}}: {source}")]
    Malformed { source: serde_json::Error },
    #[error("a {kind} scope must name a path, got an empty one")]
    EmptyPath { kind: &'static str },
}

/// Reads `git diff --name-status -z -M` output into what the claim must cover: added is `Create`,
/// modified is `EditBody`, deleted and type-changed are `EditSignature`, a rename is
/// `EditSignature` on the old path plus `Create` on the new one, a copy is `Create` on the new
/// path. Any status or shape it does not know is an error, so a surprise is never treated as
/// covered.
pub fn parse_name_status(stdout: &str) -> Result<Vec<Requirement>, NameStatusError> {
    let mut tokens: Vec<&str> = stdout.split('\0').collect();
    if tokens.last() == Some(&"") {
        tokens.pop();
    }
    let mut out = Vec::new();
    let mut tokens = tokens.into_iter();
    while let Some(token) = tokens.next() {
        let status = token.chars().next();
        let arity = if matches!(status, Some('R' | 'C')) {
            2
        } else {
            1
        };
        let mut paths = Vec::with_capacity(arity);
        for _ in 0..arity {
            match tokens.next() {
                Some(path) if !path.is_empty() => paths.push(path),
                Some(_) | None => {
                    return Err(NameStatusError::MissingPath {
                        status: token.to_owned(),
                        input: stdout.to_owned(),
                    })
                }
            }
        }
        let requirement = |path: &str, mode| Requirement {
            path: path.to_owned(),
            mode,
        };
        match (status, paths.as_slice()) {
            (Some('A'), [path]) => out.push(requirement(path, Mode::Create)),
            (Some('M'), [path]) => out.push(requirement(path, Mode::EditBody)),
            (Some('D' | 'T'), [path]) => out.push(requirement(path, Mode::EditSignature)),
            (Some('R'), [old, new]) => {
                out.push(requirement(old, Mode::EditSignature));
                out.push(requirement(new, Mode::Create));
            }
            (Some('C'), [_, new]) => out.push(requirement(new, Mode::Create)),
            _ => {
                return Err(NameStatusError::UnknownStatus {
                    status: token.to_owned(),
                    input: stdout.to_owned(),
                })
            }
        }
    }
    Ok(out)
}

/// A modified file is covered by a scope whose mode permits `EditBody` or `Create`: git records
/// an agent that adds a function to an existing file as `M`, and that agent holds `Create`.
/// Which symbol was edited, and in which mode, is the coordinator's symbol-level check on the
/// agent's `touched`; this file-level check cannot see it.
fn modes_for(mode: Mode) -> Vec<Mode> {
    match mode {
        Mode::EditBody => vec![Mode::EditBody, Mode::Create],
        Mode::Depend | Mode::EditSignature | Mode::Create => vec![mode],
    }
}

fn covers(scope: &Scope, path: &str) -> bool {
    match scope {
        Scope::Dir { path: dir } => {
            dir.is_empty()
                || path
                    .strip_prefix(dir.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        }
        Scope::File { path: file } => file == path,
        Scope::Symbol(symbol) => symbol.path == path,
    }
}

/// The paths, once each and in order, that no claimed scope covers in a permitting mode.
pub fn uncovered_paths(required: &[Requirement], claim: &[ScopeClaim]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut uncovered = Vec::new();
    for Requirement { path, mode } in required {
        let needed = modes_for(*mode);
        let covered = claim
            .iter()
            .any(|held| covers(&held.scope, path) && needed.iter().any(|n| held.mode.permits(*n)));
        if !covered && seen.insert(path.as_str()) {
            uncovered.push(path.clone());
        }
    }
    uncovered
}

/// Validates the `scopes` of a merge request: an array of `{ scope, mode }`.
pub fn parse_scopes(value: &Value) -> Result<Vec<ScopeClaim>, ScopesError> {
    let claims: Vec<ScopeClaim> = serde_json::from_value(value.clone())
        .map_err(|source| ScopesError::Malformed { source })?;
    for claim in &claims {
        match &claim.scope {
            Scope::File { path } if path.is_empty() => {
                return Err(ScopesError::EmptyPath { kind: "file" })
            }
            Scope::Symbol(symbol) if symbol.path.is_empty() => {
                return Err(ScopesError::EmptyPath { kind: "symbol" })
            }
            Scope::Dir { .. } | Scope::File { .. } | Scope::Symbol(_) => {}
        }
    }
    Ok(claims)
}

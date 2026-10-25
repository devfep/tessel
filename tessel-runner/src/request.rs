//! The request to run, and the checks that make it safe to run.
//!
//! The test commands are the parsed `[[test]]` entries of the trunk's `tessel.toml`. The runner
//! does not trust the sender to have parsed them: they go through the lander's own strict parser
//! again, so the runner accepts exactly the commands the gate accepts and nothing else.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tessel_lander::gate::{parse_gate_config, TestCommand};

use crate::config::{effective_uid, RunnerConfig};
use crate::error::{RequestRefusal, RunnerError};

/// The longest request, in bytes, that the runner reads.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// One test command of the trunk's `tessel.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTest {
    /// Relative to the tree; empty means the tree itself.
    #[serde(default)]
    pub dir: String,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRequest {
    /// An absolute path to a directory the runner's user owns.
    pub tree: String,
    pub tests: Vec<RequestTest>,
    /// The longest the whole run may take; the configuration's limit applies if it is lower.
    pub timeout_secs: u64,
}

/// A request that passed every check: what to run, where, and for how long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The tree with every link resolved.
    pub root: PathBuf,
    pub commands: Vec<PlannedCommand>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCommand {
    /// Inside [`Plan::root`], links resolved.
    pub dir: PathBuf,
    pub argv: Vec<String>,
}

impl RunRequest {
    /// Parses one JSON object. Unknown keys are refused.
    pub fn from_json(text: &str) -> Result<RunRequest, RunnerError> {
        if text.len() > MAX_REQUEST_BYTES {
            return Err(RequestRefusal::Malformed.into());
        }
        serde_json::from_str(text).map_err(|_| RequestRefusal::Malformed.into())
    }

    /// Checks the request against the configuration. Nothing runs and nothing is written.
    pub fn plan(&self, config: &RunnerConfig) -> Result<Plan, RunnerError> {
        if self.timeout_secs == 0 {
            return Err(RequestRefusal::ZeroTimeout.into());
        }
        let tests = allowed_tests(&self.tests)?;
        let root = checked_tree(&self.tree, config)?;
        let mut commands = Vec::with_capacity(tests.len());
        for test in tests {
            commands.push(PlannedCommand {
                dir: resolve_dir(&root, &test.dir)?,
                argv: test.argv,
            });
        }
        Ok(Plan {
            root,
            commands,
            timeout: Duration::from_secs(self.timeout_secs.min(config.timeout_secs())),
        })
    }
}

/// Runs the commands through the lander's parser by writing them as the `tessel.toml` they came
/// from. A command the gate would refuse, in its words, its flags or its directory, is refused.
fn allowed_tests(tests: &[RequestTest]) -> Result<Vec<TestCommand>, RequestRefusal> {
    let mut text = String::from("instance = \"standard-1\"\n");
    for test in tests {
        text.push_str("[[test]]\n");
        if !test.dir.is_empty() {
            text.push_str("dir = ");
            text.push_str(&toml_string(&test.dir));
            text.push('\n');
        }
        let words: Vec<String> = test.argv.iter().map(|arg| toml_string(arg)).collect();
        text.push_str("argv = [");
        text.push_str(&words.join(", "));
        text.push_str("]\n");
    }
    parse_gate_config(&text)
        .map(|gate| gate.tests)
        .map_err(|invalid| RequestRefusal::NotAllowed(invalid.reason))
}

/// A JSON string is a valid TOML basic string, and a character that is not is a parse error, which
/// the caller reports as a refusal.
fn toml_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// A tree to run in. Without a sandbox it is a directory the runner's user owns. In a sandbox it
/// is a directory strictly inside the handoff directory, which the lander made and the runner only
/// reads, or inside the work directory, which the runner owns.
fn checked_tree(tree: &str, config: &RunnerConfig) -> Result<PathBuf, RequestRefusal> {
    let path = Path::new(tree);
    if !path.is_absolute() {
        return Err(RequestRefusal::TreeNotAbsolute);
    }
    let meta = fs::symlink_metadata(path).map_err(|_| RequestRefusal::TreeNotDirectory)?;
    if !meta.file_type().is_dir() {
        return Err(RequestRefusal::TreeNotDirectory);
    }
    let real = fs::canonicalize(path).map_err(|_| RequestRefusal::TreeNotDirectory)?;
    let Some(sandbox) = config.sandbox() else {
        if meta.uid() != effective_uid() {
            return Err(RequestRefusal::TreeNotOwned);
        }
        return Ok(real);
    };
    let inside =
        |dir: &Path| fs::canonicalize(dir).is_ok_and(|dir| real != dir && real.starts_with(dir));
    if inside(sandbox.handoff_dir()) {
        Ok(real)
    } else if inside(sandbox.work_dir()) {
        if meta.uid() != effective_uid() {
            return Err(RequestRefusal::TreeNotOwned);
        }
        Ok(real)
    } else {
        Err(RequestRefusal::TreeNotInSandbox)
    }
}

fn resolve_dir(root: &Path, dir: &str) -> Result<PathBuf, RequestRefusal> {
    let real = fs::canonicalize(root.join(dir)).map_err(|_| RequestRefusal::DirNotDirectory)?;
    if !real.starts_with(root) {
        return Err(RequestRefusal::DirEscapes);
    }
    if !real.is_dir() {
        return Err(RequestRefusal::DirNotDirectory);
    }
    Ok(real)
}

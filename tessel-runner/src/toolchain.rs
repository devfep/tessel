//! The compiler the runner will use against the channel the tree asks for.
//!
//! The runner's `PATH` names one installed toolchain, so the channel in a tree's
//! `rust-toolchain.toml` does nothing there. A tree that wants another compiler must not be tested
//! on this one and reported as if it had been: the run is refused, before anything runs, and the
//! server reads the refusal as an attempt that did not finish.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use toml::Table;

use crate::config::{RunnerConfig, Sandbox};
use crate::error::RequestRefusal;

const MAX_TOOLCHAIN_FILE_BYTES: u64 = 64 * 1024;

/// A channel that names a version: `1.99` or `1.99.1`. `stable`, `nightly-…` and the like are not
/// compared.
fn version_channel(channel: &str) -> bool {
    let parts: Vec<&str> = channel.split('.').collect();
    (parts.len() == 2 || parts.len() == 3)
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn channel_of(root: &Path) -> Option<String> {
    let path = root.join("rust-toolchain.toml");
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_TOOLCHAIN_FILE_BYTES {
        return None;
    }
    let text = fs::read_to_string(&path).ok()?;
    let table: Table = text.parse().ok()?;
    let channel = table.get("toolchain")?.get("channel")?.as_str()?;
    Some(channel.to_owned())
}

/// Whether the first line of `rustc -V` is the compiler of `channel`.
fn compiler_matches(version_line: &str, channel: &str) -> bool {
    let wanted = format!("rustc {channel}");
    let Some(rest) = version_line.strip_prefix(&wanted) else {
        return false;
    };
    if channel.matches('.').count() == 2 {
        rest.starts_with(' ')
    } else {
        rest.starts_with('.')
    }
}

/// Refuses a tree whose version channel is not the compiler on the runner's `PATH`.
pub(crate) fn check(
    config: &RunnerConfig,
    sandbox: &Sandbox,
    root: &Path,
) -> Result<(), RequestRefusal> {
    let Some(channel) = channel_of(root).filter(|channel| version_channel(channel)) else {
        return Ok(());
    };
    // From the work directory, which holds no toolchain file, so a `rustup` proxy on the `PATH`
    // cannot read the tree's and try to download another compiler.
    let output = Command::new("rustc")
        .arg("-V")
        .env_clear()
        .envs(config.env().iter().map(|(name, value)| (name, value)))
        .current_dir(sandbox.work_dir())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| RequestRefusal::ToolchainMismatch)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next().unwrap_or_default();
    if output.status.success() && compiler_matches(first, &channel) {
        Ok(())
    } else {
        Err(RequestRefusal::ToolchainMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::{compiler_matches, version_channel};

    #[test]
    fn only_dotted_numbers_are_version_channels() {
        assert!(version_channel("1.99.1"));
        assert!(version_channel("1.99"));
        for other in [
            "stable",
            "nightly-2026-01-01",
            "1",
            "1.99.1.2",
            "1..1",
            "1.x",
            "",
        ] {
            assert!(!version_channel(other), "{other}");
        }
    }

    #[test]
    fn a_compiler_matches_its_channel_and_no_other() {
        let line = "rustc 1.99.1 (abcdef 2026-01-01)";
        assert!(compiler_matches(line, "1.99.1"));
        assert!(compiler_matches(line, "1.99"));
        assert!(!compiler_matches(line, "1.99.2"));
        assert!(!compiler_matches(line, "1.9"));
        assert!(!compiler_matches("rustc 1.99.10 (x)", "1.99.1"));
        assert!(!compiler_matches("", "1.99.1"));
    }
}

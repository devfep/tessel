//! Shared fixtures: a configuration for the current user and a request for a tree.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tessel_runner::{effective_login, RequestTest, RunRequest, RunnerConfig};

pub const TEST_ARGV: [&str; 3] = ["cargo", "test", "--workspace"];

pub fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// A configuration for the user running the tests, with a plain `PATH`.
pub fn config() -> RunnerConfig {
    RunnerConfig {
        user: effective_login().expect("the test user has a login"),
        timeout_secs: 1800,
        env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
    }
}

pub fn request(tree: &Path, dir: &str, words: &[&str]) -> RunRequest {
    RunRequest {
        tree: tree.display().to_string(),
        tests: vec![RequestTest {
            dir: dir.to_owned(),
            argv: argv(words),
        }],
        timeout_secs: 60,
    }
}

/// Writes a configuration file with the given text and mode.
pub fn write_config(dir: &Path, text: &str, mode: u32) -> PathBuf {
    let path = dir.join("tessel-runner.toml");
    fs::write(&path, text).expect("write config");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("set mode");
    path
}

/// The text of a valid configuration for the user running the tests.
pub fn own_config_text() -> String {
    format!(
        "[runner]\nuser = \"{}\"\n",
        effective_login().expect("the test user has a login")
    )
}

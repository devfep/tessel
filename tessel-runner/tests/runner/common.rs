//! Shared fixtures: a configuration for the current user and a request for a tree.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tessel_runner::{effective_login, RequestTest, RunRequest, RunnerConfig};

pub const TEST_ARGV: [&str; 3] = ["cargo", "test", "--workspace"];

pub fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// A configuration for the user running the tests, loaded from a file as the binary does, with a
/// plain `PATH`.
pub fn config() -> RunnerConfig {
    config_with_timeout(None)
}

pub fn config_with_timeout(timeout_secs: Option<u64>) -> RunnerConfig {
    let dir = tempfile::tempdir().expect("config dir");
    let extra = timeout_secs.map_or_else(String::new, |secs| format!("timeout_secs = {secs}\n"));
    let text = own_config_text() + &extra;
    let path = write_config(dir.path(), &text, 0o600);
    RunnerConfig::load(&path)
        .expect("a config for the current user loads")
        .with_env(vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())])
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

/// A tree to run in and a directory holding a stand-in `cargo` (and any other program a test
/// names), put first on the `PATH` the runner gives its commands.
pub struct Rig {
    pub bin: tempfile::TempDir,
    pub tree: tempfile::TempDir,
}

impl Rig {
    /// A rig whose `cargo` is a shell script with the given body.
    pub fn with_cargo(body: &str) -> Rig {
        let rig = Rig {
            bin: tempfile::tempdir().expect("bin dir"),
            tree: tempfile::tempdir().expect("tree dir"),
        };
        rig.add_program("cargo", body);
        rig
    }

    pub fn add_program(&self, name: &str, body: &str) {
        let path = self.bin.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write program");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("make executable");
    }

    pub fn path_value(&self) -> String {
        format!("{}:/usr/bin:/bin", self.bin.path().display())
    }

    pub fn config(&self) -> RunnerConfig {
        self.config_with_timeout(None)
    }

    pub fn config_with_timeout(&self, timeout_secs: Option<u64>) -> RunnerConfig {
        config_with_timeout(timeout_secs).with_env(vec![
            ("PATH".to_owned(), self.path_value()),
            ("HOME".to_owned(), "/home-for-test".to_owned()),
        ])
    }

    /// One `cargo test --workspace` in the tree.
    pub fn request(&self) -> RunRequest {
        request(self.tree.path(), "", &TEST_ARGV)
    }

    pub fn write_in_tree(&self, name: &str, text: &str) {
        fs::write(self.tree.path().join(name), text).expect("write fixture");
    }
}

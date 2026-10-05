//! The live target: a scratch repository created through the steward's admin routes, one fork and
//! one write token per agent, and an identity token per agent for the coordinator.
//!
//! Every route is built from a `ScratchRepo`, which only the target guard can produce, so this
//! module cannot address `tessel-dogfood`, `demo` or any other repository. The steward is called
//! with `curl` reading its configuration from stdin, so the admin token is never in an argument
//! list, the environment of another process or the output.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::demo::Tree;
use crate::endpoint::{Endpoint, Remote, Token};
use crate::git::{self, Git};
use crate::guard::ScratchRepo;

pub struct Steward {
    base: String,
    admin: Token,
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Steward {
    /// `base` is the steward Worker's origin, for example `https://tessel-steward.example.dev`.
    pub fn new(base: &str, admin: Token) -> Result<Self> {
        let base = base.trim_end_matches('/').to_string();
        if !(base.starts_with("https://") || base.starts_with("http://localhost")) {
            bail!(
                "the steward URL must start with https:// (or http://localhost for a dev server)"
            );
        }
        if base
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_whitespace() || c.is_control())
        {
            bail!("the steward URL has a quote, a backslash, a space or a control character");
        }
        let token = admin.expose();
        if token.is_empty()
            || token
                .chars()
                .any(|c| c == '"' || c == '\\' || c.is_control())
        {
            bail!("STEWARD_ADMIN_TOKEN is empty or has a character that cannot go in a header");
        }
        Ok(Self { base, admin })
    }

    fn post(&self, path: &str) -> Result<Value> {
        let config = format!(
            "url = \"{}{path}\"\nrequest = \"POST\"\nheader = \"Authorization: Bearer {}\"\n\
             header = \"Content-Type: application/json\"\ndata = \"{{}}\"\n",
            self.base,
            self.admin.expose()
        );
        let mut child = Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "120",
                "-K",
                "-",
                "-w",
                "\n%{http_code}",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run curl; the live target needs it to call the steward")?;
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("curl has no stdin"))?
            .write_all(config.as_bytes())?;
        let output = child.wait_with_output()?;
        let scrub = |text: &str| text.replace(self.admin.expose(), "[redacted]");
        if !output.status.success() {
            let why = String::from_utf8_lossy(&output.stderr);
            bail!("POST {path} failed: {}", scrub(why.trim()));
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
        if !status.starts_with('2') {
            let detail = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            bail!("POST {path} answered HTTP {status}: {}", scrub(&detail));
        }
        serde_json::from_str(body).with_context(|| format!("POST {path} did not answer with JSON"))
    }

    fn field(reply: &Value, key: &str, path: &str) -> Result<String> {
        reply
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("POST {path} answered without {key:?}"))
    }

    /// Creates the scratch repository; returns its git remote and a write token for it.
    pub fn create_repo(&self, repo: &ScratchRepo) -> Result<(String, Token)> {
        let path = format!("/repos/{repo}");
        let reply = self.post(&path)?;
        Ok((
            Self::field(&reply, "remote", &path)?,
            Token::new(Self::field(&reply, "token", &path)?),
        ))
    }

    pub fn create_fork(&self, repo: &ScratchRepo, agent: &str) -> Result<String> {
        let path = format!("/repos/{repo}/forks/{}", fork_name(repo, agent)?);
        Self::field(&self.post(&path)?, "remote", &path)
    }

    pub fn fork_token(&self, repo: &ScratchRepo, agent: &str) -> Result<Token> {
        let path = format!("/repos/{}/tokens", fork_name(repo, agent)?);
        Ok(Token::new(Self::field(&self.post(&path)?, "token", &path)?))
    }

    pub fn identity(&self, repo: &ScratchRepo, agent: &str) -> Result<Token> {
        if !is_name(agent) {
            bail!("{agent:?} is not a valid agent name");
        }
        let path = format!("/repos/{repo}/agents/{agent}/identity");
        Ok(Token::new(Self::field(&self.post(&path)?, "token", &path)?))
    }
}

/// The name of an agent's fork of `repo`, as the coordinator's merge request derives it.
fn fork_name(repo: &ScratchRepo, agent: &str) -> Result<String> {
    if !is_name(agent) {
        bail!("{agent:?} is not a valid agent name");
    }
    Ok(format!("{repo}--{agent}"))
}

pub struct LiveSetup<'a> {
    pub steward: &'a Steward,
    /// `ws://` or `wss://` origin of the coordinator Worker.
    pub coordinator: &'a str,
    pub repo: &'a ScratchRepo,
    pub scratch: &'a Path,
    pub base: &'a Tree,
    pub agents: &'a [String],
    /// Every name that needs an identity token: the agents, the reviewer and the observer.
    pub names: &'a [String],
}

/// Creates the repo, pushes the starting commit to its main, forks it per agent and mints the
/// tokens. Blocking: run it on a blocking thread.
pub fn provision(setup: &LiveSetup<'_>) -> Result<Endpoint> {
    let origin = setup.coordinator.trim_end_matches('/');
    if !(origin.starts_with("ws://") || origin.starts_with("wss://")) {
        bail!("the coordinator URL must start with ws:// or wss://");
    }
    let (trunk_url, trunk_token) = setup.steward.create_repo(setup.repo)?;
    let seed_dir = setup.scratch.join("seed");
    std::fs::create_dir_all(&seed_dir)?;
    git::init_repo(&Git::new(&seed_dir), setup.base)?;
    Git::new(&seed_dir)
        .with_bearer(trunk_token.expose())
        .run(&["push", "-q", "--force", &trunk_url, "HEAD:refs/heads/main"])?;
    let mut forks = HashMap::new();
    for agent in setup.agents {
        let url = setup.steward.create_fork(setup.repo, agent)?;
        let token = setup.steward.fork_token(setup.repo, agent)?;
        forks.insert(agent.clone(), (url, token));
    }
    let mut tokens = HashMap::new();
    for name in setup.names {
        tokens.insert(name.clone(), setup.steward.identity(setup.repo, name)?);
    }
    Ok(Endpoint {
        ws_url: format!("{origin}/repo/{}/ws", setup.repo),
        tokens,
        remote: Remote::Live {
            trunk_url,
            trunk_token,
            forks,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steward_needs_https_and_a_usable_token() {
        let token = || Token::new("tok".into());
        assert!(Steward::new("https://steward.example.dev/", token()).is_ok());
        assert!(Steward::new("http://localhost:8788", token()).is_ok());
        assert!(Steward::new("http://steward.example.dev", token()).is_err());
        assert!(Steward::new("https://x", Token::new(String::new())).is_err());
        for bad in [
            "https://x\"y",
            "https://x\ny",
            "https://x y",
            "https://x\\y",
            "https://x\u{7}",
        ] {
            assert!(Steward::new(bad, token()).is_err(), "{bad:?}");
        }
        assert!(Steward::new("https://x", Token::new("a\"b".into())).is_err());
    }

    #[test]
    fn fork_names_follow_the_coordinators_rule_and_reject_path_tricks() {
        let repo = ScratchRepo::parse("swarm-1").unwrap();
        assert_eq!(fork_name(&repo, "a01").unwrap(), "swarm-1--a01");
        for bad in ["", "a/b", "a b", "../demo", "a?x"] {
            assert!(fork_name(&repo, bad).is_err(), "{bad:?}");
        }
    }
}

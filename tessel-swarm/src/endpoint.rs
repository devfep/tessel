//! Where the scripted agents connect and where their git goes: a coordinator socket, a token per
//! agent, and a remote for the trunk and one fork per agent. The local target produces one of
//! these.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;

use anyhow::{anyhow, Result};

use crate::git::Git;

/// A credential. It never prints: `Debug` hides it and the only way to read it is `expose`.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// Directories on this machine: the trunk repo and a bare repo per agent fork.
#[derive(Debug, Clone)]
pub struct Remote {
    pub trunk: PathBuf,
    pub forks: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    /// `ws://` URL of the repo's socket on the coordinator.
    pub ws_url: String,
    /// Identity token by agent name, for the agents, the reviewer and the observer.
    pub tokens: HashMap<String, Token>,
    pub remote: Remote,
}

impl Endpoint {
    pub fn token_of(&self, agent: &str) -> Result<&Token> {
        self.tokens
            .get(agent)
            .ok_or_else(|| anyhow!("no identity token for agent {agent}"))
    }
}

impl Remote {
    /// An empty working directory for `agent`, brought up to the trunk's head.
    pub fn checkout(&self, dir: &Path) -> Result<(Git, String)> {
        let work = Git::new(dir);
        work.run(&["init", "-q", "-b", "main"])?;
        let head = self.sync(&work)?;
        Ok((work, head))
    }

    /// Fetches the trunk's `main`, resets the working directory to it and returns its commit id.
    pub fn sync(&self, work: &Git) -> Result<String> {
        work.run(&["fetch", "-q", &self.trunk.to_string_lossy(), "main"])?;
        work.run(&["reset", "-q", "--hard", "FETCH_HEAD"])?;
        work.run(&["rev-parse", "HEAD"])
    }

    /// Pushes the working directory's `HEAD` to `agent`'s fork as its `main`, repeating a push
    /// the remote answered with a server error or a lost connection (`Git::push`). Adds the
    /// repeats to `retries`.
    pub fn push(&self, work: &Git, agent: &str, retries: &AtomicU32) -> Result<()> {
        let fork = self.forks.join(format!("{agent}.git"));
        work.push(
            &[
                "-q",
                "--force",
                &fork.to_string_lossy(),
                "HEAD:refs/heads/main",
            ],
            retries,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_never_prints() {
        let token = Token::new("super-secret".into());
        assert!(!format!("{token:?}").contains("super-secret"));
        let endpoint = Endpoint {
            ws_url: "ws://x".into(),
            tokens: HashMap::from([("a01".to_string(), token)]),
            remote: Remote {
                trunk: "t".into(),
                forks: "f".into(),
            },
        };
        assert!(!format!("{endpoint:?}").contains("super-secret"));
        assert!(endpoint.token_of("a02").is_err());
    }
}

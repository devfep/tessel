//! The live target: a scratch repository created through the steward's admin routes, one fork and
//! one write token per agent, and an identity token per agent for the coordinator.
//!
//! Every route is built from a `ScratchRepo`, which only the target guard can produce, so this
//! module cannot address `tessel-dogfood`, `demo` or any other repository. The steward is called
//! with `curl` reading its configuration from stdin (see `Transport`), so the admin token is never
//! in an argument list, the environment of another process or the output.

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

/// What the steward answered: the HTTP status and the body text.
pub struct Reply {
    pub status: u16,
    pub body: String,
}

/// How a POST reaches the steward. The one production implementation runs `curl`; tests answer
/// from memory, because the gate's Sandbox image has no `curl`.
pub trait Transport {
    /// POSTs `{}` to `url` with `Authorization: Bearer <bearer>`.
    fn post(&self, url: &str, bearer: &str) -> Result<Reply>;
}

/// Runs `curl` with its configuration on stdin, so the bearer token is never in an argument list
/// or the environment of another process.
struct Curl;

impl Transport for Curl {
    fn post(&self, url: &str, bearer: &str) -> Result<Reply> {
        let config = format!(
            "url = \"{url}\"\nrequest = \"POST\"\nheader = \"Authorization: Bearer {bearer}\"\n\
             header = \"Content-Type: application/json\"\ndata = \"{{}}\"\n"
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
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
        let status = status
            .trim()
            .parse()
            .with_context(|| format!("curl reported the HTTP status {status:?}"))?;
        Ok(Reply {
            status,
            body: body.to_string(),
        })
    }
}

pub struct Steward {
    base: String,
    admin: Token,
    transport: Box<dyn Transport>,
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Whether `url` is plain HTTP to this machine: the host is exactly `localhost` or `127.0.0.1`
/// (a port is allowed) and there is no userinfo. The admin token travels in the clear, so
/// `http://localhost.example` and `http://localhost@example` must not pass.
fn is_local_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        Some(_) | None => authority,
    };
    matches!(host, "localhost" | "127.0.0.1")
}

impl Steward {
    /// `base` is the steward Worker's origin, for example `https://tessel-steward.example.dev`.
    pub fn new(base: &str, admin: Token) -> Result<Self> {
        Self::with_transport(base, admin, Box::new(Curl))
    }

    fn with_transport(base: &str, admin: Token, transport: Box<dyn Transport>) -> Result<Self> {
        let base = base.trim_end_matches('/').to_string();
        if !(base.starts_with("https://") || is_local_http(&base)) {
            bail!(
                "the steward URL must start with https:// (or http://localhost or \
                 http://127.0.0.1 for a dev server)"
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
        Ok(Self {
            base,
            admin,
            transport,
        })
    }

    fn post(&self, path: &str) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let scrub = |text: &str| text.replace(self.admin.expose(), "[redacted]");
        let reply = self
            .transport
            .post(&url, self.admin.expose())
            .map_err(|error| anyhow!("POST {path} failed: {}", scrub(&format!("{error:#}"))))?;
        if !(200..300).contains(&reply.status) {
            let detail = serde_json::from_str::<Value>(&reply.body)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            bail!(
                "POST {path} answered HTTP {}: {}",
                reply.status,
                scrub(&detail)
            );
        }
        serde_json::from_str(&reply.body)
            .with_context(|| format!("POST {path} did not answer with JSON"))
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

/// Creates the repo and pushes `base` to its main as one commit. Returns the trunk remote, its
/// write token and the commit id, which is the same for the same `base` on any machine.
fn seed_trunk(
    steward: &Steward,
    repo: &ScratchRepo,
    scratch: &Path,
    base: &Tree,
) -> Result<(String, Token, String)> {
    let (trunk_url, trunk_token) = steward.create_repo(repo)?;
    let seed_dir = scratch.join("seed");
    std::fs::create_dir_all(&seed_dir)?;
    let commit = git::init_repo(&Git::new(&seed_dir), base)?;
    Git::new(&seed_dir)
        .with_bearer(trunk_token.expose())
        .run(&["push", "-q", "--force", &trunk_url, "HEAD:refs/heads/main"])?;
    Ok((trunk_url, trunk_token, commit))
}

/// A named repository holding the demo's starting commit, for agents that are not scripted.
#[derive(Debug, PartialEq, Eq)]
pub struct DemoRepo {
    pub repo: String,
    pub remote: String,
    pub commit: String,
    /// Fork name and remote per agent.
    pub forks: Vec<(String, String)>,
}

/// Creates `repo`, pushes the demo's starting commit to its main and forks it once per agent.
/// No fork token is minted, and no token is in the result. Blocking: run it on a blocking thread.
pub fn create_demo_repo(
    steward: &Steward,
    repo: &ScratchRepo,
    scratch: &Path,
    agents: &[String],
) -> Result<DemoRepo> {
    let mut names = Vec::new();
    for agent in agents {
        names.push(fork_name(repo, agent)?);
    }
    let (remote, _, commit) = seed_trunk(steward, repo, scratch, &crate::demo::base_tree())?;
    let mut forks = Vec::new();
    for (agent, name) in agents.iter().zip(names) {
        forks.push((name, steward.create_fork(repo, agent)?));
    }
    Ok(DemoRepo {
        repo: repo.to_string(),
        remote,
        commit,
        forks,
    })
}

/// Creates the repo, pushes the starting commit to its main, forks it per agent and mints the
/// tokens. Blocking: run it on a blocking thread.
pub fn provision(setup: &LiveSetup<'_>) -> Result<Endpoint> {
    let origin = setup.coordinator.trim_end_matches('/');
    if !(origin.starts_with("ws://") || origin.starts_with("wss://")) {
        bail!("the coordinator URL must start with ws:// or wss://");
    }
    let (trunk_url, trunk_token, _) =
        seed_trunk(setup.steward, setup.repo, setup.scratch, setup.base)?;
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
    use std::sync::Arc;

    use super::*;

    #[test]
    fn the_steward_needs_https_and_a_usable_token() {
        let token = || Token::new("tok".into());
        assert!(Steward::new("https://steward.example.dev/", token()).is_ok());
        assert!(Steward::new("http://localhost:8788", token()).is_ok());
        assert!(Steward::new("http://127.0.0.1:8788", token()).is_ok());
        assert!(Steward::new("http://steward.example.dev", token()).is_err());
        for evil in [
            "http://localhost.evil.example",
            "http://localhost.evil.example:8788",
            "http://localhost@evil.example",
            "http://localhost:8788@evil.example",
            "http://user:pw@localhost:8788",
            "http://127.0.0.1.evil.example",
            "http://localhostevil",
        ] {
            assert!(Steward::new(evil, token()).is_err(), "{evil}");
        }
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

    const ADMIN: &str = "admin-token-that-must-not-print";
    const ORIGIN: &str = "https://steward.test";

    enum Answer {
        /// 201 with a new bare repository under the root as the remote.
        Remotes,
        Refuse(u16, &'static str),
        Fail(&'static str),
    }

    /// A steward that answers from memory and records each POST's URL and bearer token.
    struct Fake {
        root: std::path::PathBuf,
        answer: Answer,
        seen: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl Fake {
        fn new(root: &Path, answer: Answer) -> Arc<Self> {
            Arc::new(Self {
                root: root.to_path_buf(),
                answer,
                seen: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn paths(&self) -> Vec<String> {
            let seen = self.seen.lock().unwrap();
            let strip = |(url, _): &(String, String)| url.strip_prefix(ORIGIN).unwrap().to_string();
            seen.iter().map(strip).collect()
        }

        fn steward(self: &Arc<Self>, admin: &str) -> Steward {
            let transport = Box::new(Arc::clone(self));
            Steward::with_transport(ORIGIN, Token::new(admin.into()), transport).unwrap()
        }
    }

    impl Transport for Arc<Fake> {
        fn post(&self, url: &str, bearer: &str) -> Result<Reply> {
            self.seen
                .lock()
                .unwrap()
                .push((url.to_string(), bearer.to_string()));
            match &self.answer {
                Answer::Fail(why) => bail!("{why}"),
                Answer::Refuse(status, error) => Ok(Reply {
                    status: *status,
                    body: format!("{{\"error\":\"{error}\"}}"),
                }),
                Answer::Remotes => {
                    let path = url.strip_prefix(ORIGIN).unwrap();
                    let bare = self
                        .root
                        .join(path.trim_start_matches('/').replace('/', "_"));
                    let init = Command::new("git")
                        .args(["init", "-q", "--bare"])
                        .arg(&bare)
                        .status();
                    if !init.is_ok_and(|status| status.success()) {
                        bail!("cannot create a bare repository");
                    }
                    Ok(Reply {
                        status: 201,
                        body: format!(
                            "{{\"remote\":\"{}\",\"token\":\"write-token\"}}",
                            bare.display()
                        ),
                    })
                }
            }
        }
    }

    #[test]
    fn a_demo_repo_gets_the_starting_commit_and_a_fork_per_agent() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new(dir.path(), Answer::Remotes);
        let steward = fake.steward(ADMIN);
        let repo = ScratchRepo::parse("swarm-demo").unwrap();
        let agents = vec!["a1".to_string(), "a2".to_string()];
        let made = create_demo_repo(&steward, &repo, &dir.path().join("scratch"), &agents).unwrap();
        assert_eq!(
            fake.paths(),
            [
                "/repos/swarm-demo",
                "/repos/swarm-demo/forks/swarm-demo--a1",
                "/repos/swarm-demo/forks/swarm-demo--a2",
            ]
        );
        assert!(fake
            .seen
            .lock()
            .unwrap()
            .iter()
            .all(|(_, bearer)| bearer == ADMIN));
        let reference = tempfile::tempdir().unwrap();
        let commit = git::init_repo(&Git::new(reference.path()), &crate::demo::base_tree());
        assert_eq!(Some(made.commit.as_str()), commit.ok().as_deref());
        let main = Git::new(Path::new(&made.remote))
            .run(&["rev-parse", "main"])
            .unwrap();
        assert_eq!(main, made.commit, "the trunk's main is the starting commit");
        let names: Vec<&str> = made.forks.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["swarm-demo--a1", "swarm-demo--a2"]);
    }

    #[test]
    fn the_admin_token_is_scrubbed_from_a_refusal_the_steward_echoes() {
        let dir = tempfile::tempdir().unwrap();
        let echoed = "ALREADY_EXISTS for bearer admin-token-that-must-not-print";
        let fake = Fake::new(dir.path(), Answer::Refuse(409, echoed));
        let repo = ScratchRepo::parse("swarm-demo").unwrap();
        let error = create_demo_repo(&fake.steward(ADMIN), &repo, dir.path(), &[]).unwrap_err();
        assert_eq!(fake.paths(), ["/repos/swarm-demo"]);
        let message = format!("{error:#}");
        assert!(message.contains("HTTP 409"), "{message}");
        assert!(message.contains("[redacted]"), "{message}");
        assert!(!message.contains(ADMIN), "{message}");
    }

    #[test]
    fn the_admin_token_is_scrubbed_from_a_transport_failure() {
        let dir = tempfile::tempdir().unwrap();
        let why = "curl: (22) rejected bearer admin-token-that-must-not-print";
        let fake = Fake::new(dir.path(), Answer::Fail(why));
        let repo = ScratchRepo::parse("swarm-demo").unwrap();
        let error = create_demo_repo(&fake.steward(ADMIN), &repo, dir.path(), &[]).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("[redacted]"), "{message}");
        assert!(!message.contains(ADMIN), "{message}");
    }

    #[test]
    fn a_bad_agent_name_is_refused_before_anything_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::new(dir.path(), Answer::Remotes);
        let repo = ScratchRepo::parse("swarm-demo").unwrap();
        let agents = vec!["a1".to_string(), "a/b".to_string()];
        let error = create_demo_repo(&fake.steward(ADMIN), &repo, dir.path(), &agents).unwrap_err();
        assert!(
            error.to_string().contains("not a valid agent name"),
            "{error:#}"
        );
        assert!(fake.paths().is_empty(), "no request was made");
    }
}

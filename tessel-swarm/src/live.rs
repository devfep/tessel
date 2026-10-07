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
    use std::time::Duration;

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

    const SECRET: &str = "write-token-that-must-not-print";

    /// A steward on localhost that answers `count` POSTs: a repo or fork answers with a bare
    /// repository under `root` as its remote, or with HTTP 409 and `refusal` as its error text.
    /// Returns its origin and the paths it was asked for; it stops waiting after five seconds,
    /// so a request that never comes fails the test.
    fn fake_steward(
        root: &Path,
        count: usize,
        refusal: Option<&'static str>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        let root = root.to_path_buf();
        let handle = std::thread::spawn(move || {
            let mut paths = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while paths.len() < count && std::time::Instant::now() < deadline {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                let path = read_post(&mut stream);
                if let Some(error) = refusal {
                    let body = format!("{{\"error\":\"{error}\"}}");
                    let reply = format!(
                        "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(reply.as_bytes()).unwrap();
                    paths.push(path);
                    continue;
                }
                let bare = root.join(path.trim_start_matches('/').replace('/', "_"));
                let init = Command::new("git")
                    .args(["init", "-q", "--bare"])
                    .arg(&bare)
                    .status();
                assert!(init.unwrap().success());
                let body = format!(
                    "{{\"remote\":\"{}\",\"token\":\"{SECRET}\"}}",
                    bare.display()
                );
                let reply = format!(
                    "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
                paths.push(path);
            }
            paths
        });
        (origin, handle)
    }

    fn read_post(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut seen = Vec::new();
        let mut byte = [0u8; 1];
        while !seen.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            seen.push(byte[0]);
        }
        let head = String::from_utf8(seen).unwrap();
        let length = head
            .to_ascii_lowercase()
            .lines()
            .find_map(|l| {
                l.strip_prefix("content-length: ")
                    .map(|n| n.trim().parse().unwrap())
            })
            .unwrap_or(0usize);
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).unwrap();
        head.split_whitespace().nth(1).unwrap().to_string()
    }

    #[test]
    fn a_demo_repo_gets_the_starting_commit_and_a_fork_per_agent_without_a_token_in_sight() {
        let dir = tempfile::tempdir().unwrap();
        let (origin, server) = fake_steward(dir.path(), 3, None);
        let steward = Steward::new(&origin, Token::new("admin".into())).unwrap();
        let repo = ScratchRepo::parse("swarm-demo").unwrap();
        let agents = vec!["a1".to_string(), "a2".to_string()];
        let made = create_demo_repo(&steward, &repo, &dir.path().join("scratch"), &agents).unwrap();
        assert_eq!(
            server.join().unwrap(),
            [
                "/repos/swarm-demo",
                "/repos/swarm-demo/forks/swarm-demo--a1",
                "/repos/swarm-demo/forks/swarm-demo--a2",
            ]
        );
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
        let admin = "admin-token-that-must-not-print";
        let echoed = "ALREADY_EXISTS for bearer admin-token-that-must-not-print";
        let (origin, server) = fake_steward(dir.path(), 1, Some(echoed));
        let steward = Steward::new(&origin, Token::new(admin.into())).unwrap();
        let repo = ScratchRepo::parse("swarm-demo").unwrap();
        let error = create_demo_repo(&steward, &repo, dir.path(), &[]).unwrap_err();
        assert_eq!(server.join().unwrap(), ["/repos/swarm-demo"]);
        let message = format!("{error:#}");
        assert!(message.contains("HTTP 409"), "{message}");
        assert!(message.contains("[redacted]"), "{message}");
        assert!(!message.contains(admin), "{message}");
    }

    #[test]
    fn a_bad_agent_name_is_refused_before_anything_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let steward = Steward::new("http://localhost:1", Token::new("admin".into())).unwrap();
        let repo = ScratchRepo::parse("swarm-demo").unwrap();
        let agents = vec!["a1".to_string(), "a/b".to_string()];
        let error = create_demo_repo(&steward, &repo, dir.path(), &agents).unwrap_err();
        assert!(
            error.to_string().contains("not a valid agent name"),
            "{error:#}"
        );
    }
}

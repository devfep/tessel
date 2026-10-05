//! Configuration: environment variables, overridden by `.tessel/config.toml` in the worktree.

use std::fmt;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

const ENV_COORDINATOR: &str = "TESSEL_COORDINATOR";
const ENV_REPO: &str = "TESSEL_REPO";
const ENV_AGENT: &str = "TESSEL_AGENT";
const ENV_TOKEN: &str = "TESSEL_TOKEN";

/// The identity token minted by the steward. It never prints: `Debug` hides it, and the only way
/// to read it is `expose`, which the connection code calls once per handshake.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Replaces every occurrence of the token in `text`, for text that reaches a file or screen.
    pub fn redact(&self, text: &str) -> String {
        text.replace(&self.0, "[redacted]")
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `ws://` or `wss://` base URL of the coordinator Worker, without a trailing slash.
    pub coordinator: String,
    pub repo: String,
    pub agent: String,
    pub token: Token,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(
        "missing configuration: {names}; set the environment variable or add the key to \
         .tessel/config.toml"
    )]
    Missing { names: String },
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid {path}: {message}")]
    Parse { path: String, message: String },
    #[error("{name} must be {rule}")]
    Invalid {
        name: &'static str,
        rule: &'static str,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    coordinator: Option<String>,
    repo: Option<String>,
    agent: Option<String>,
    token: Option<String>,
}

impl Config {
    /// Reads the environment through `env`, then lets `<dir>/.tessel/config.toml` override it.
    pub fn load(dir: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let file = read_file(&dir.join(".tessel").join("config.toml"))?;
        let pick = |from_file: Option<String>, var: &str| {
            from_file
                .filter(|value| !value.is_empty())
                .or_else(|| env(var).filter(|value| !value.is_empty()))
        };
        let coordinator = pick(file.coordinator, ENV_COORDINATOR);
        let repo = pick(file.repo, ENV_REPO);
        let agent = pick(file.agent, ENV_AGENT);
        let token = pick(file.token, ENV_TOKEN);
        let (Some(coordinator), Some(repo), Some(agent), Some(token)) = (
            coordinator.clone(),
            repo.clone(),
            agent.clone(),
            token.clone(),
        ) else {
            let mut names = Vec::new();
            for (value, var) in [
                (&coordinator, ENV_COORDINATOR),
                (&repo, ENV_REPO),
                (&agent, ENV_AGENT),
                (&token, ENV_TOKEN),
            ] {
                if value.is_none() {
                    names.push(var);
                }
            }
            return Err(ConfigError::Missing {
                names: names.join(", "),
            });
        };
        validate_name(ENV_REPO, &repo)?;
        validate_name(ENV_AGENT, &agent)?;
        if !(coordinator.starts_with("ws://") || coordinator.starts_with("wss://")) {
            return Err(ConfigError::Invalid {
                name: ENV_COORDINATOR,
                rule: "a ws:// or wss:// URL",
            });
        }
        Ok(Self {
            coordinator: coordinator.trim_end_matches('/').to_string(),
            repo,
            agent,
            token: Token(token),
        })
    }

    /// The WebSocket endpoint of this repo's coordinator.
    pub fn socket_url(&self) -> String {
        format!("{}/repo/{}/ws", self.coordinator, self.repo)
    }
}

/// The steward's name rule: letters, digits, `.`, `_`, `-`, starting with a letter or digit.
fn validate_name(name: &'static str, value: &str) -> Result<(), ConfigError> {
    let mut chars = value.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if first_ok && rest_ok {
        return Ok(());
    }
    Err(ConfigError::Invalid {
        name,
        rule: "letters, digits, '.', '_' or '-', starting with a letter or digit",
    })
}

fn read_file(path: &Path) -> Result<FileConfig, ConfigError> {
    let shown = path.display().to_string();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileConfig::default()),
        Err(source) => {
            return Err(ConfigError::Read {
                path: shown,
                source,
            })
        }
    };
    // The parser's own message quotes the offending line, which may hold the token; keep only
    // its short message and the line number.
    toml::from_str(&text).map_err(|e| ConfigError::Parse {
        path: shown,
        message: match e.span() {
            Some(span) => format!(
                "{} (line {})",
                e.message(),
                text[..span.start.min(text.len())].matches('\n').count() + 1
            ),
            None => e.message().to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        let pairs = pairs.to_vec();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    }

    const FULL: &[(&str, &str)] = &[
        (ENV_COORDINATOR, "wss://example.test/"),
        (ENV_REPO, "demo"),
        (ENV_AGENT, "a1"),
        (ENV_TOKEN, "tok-env"),
    ];

    fn write_config(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".tessel")).unwrap();
        std::fs::write(dir.join(".tessel").join("config.toml"), body).unwrap();
    }

    #[test]
    fn environment_alone_is_enough() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(dir.path(), env_with(FULL)).unwrap();
        assert_eq!(config.coordinator, "wss://example.test");
        assert_eq!(config.agent, "a1");
        assert_eq!(config.token.expose(), "tok-env");
        assert_eq!(config.socket_url(), "wss://example.test/repo/demo/ws");
    }

    #[test]
    fn file_overrides_environment_key_by_key() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "agent = \"a9\"\ntoken = \"tok-file\"\n");
        let config = Config::load(dir.path(), env_with(FULL)).unwrap();
        assert_eq!(config.agent, "a9");
        assert_eq!(config.token.expose(), "tok-file");
        assert_eq!(config.repo, "demo");
    }

    #[test]
    fn missing_values_are_all_named() {
        let dir = tempfile::tempdir().unwrap();
        let err = Config::load(dir.path(), env_with(&[(ENV_REPO, "demo")])).unwrap_err();
        let text = err.to_string();
        for name in [ENV_COORDINATOR, ENV_AGENT, ENV_TOKEN] {
            assert!(text.contains(name), "{text}");
        }
        assert!(!text.contains(ENV_REPO), "{text}");
    }

    #[test]
    fn rejects_http_urls_and_odd_names() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "coordinator = \"https://example.test\"\n");
        assert!(matches!(
            Config::load(dir.path(), env_with(FULL)),
            Err(ConfigError::Invalid {
                name: ENV_COORDINATOR,
                ..
            })
        ));
        write_config(dir.path(), "agent = \"../x\"\n");
        assert!(matches!(
            Config::load(dir.path(), env_with(FULL)),
            Err(ConfigError::Invalid {
                name: ENV_AGENT,
                ..
            })
        ));
    }

    #[test]
    fn a_bad_file_error_does_not_quote_the_token() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "token = tok-secret-value\n");
        let text = Config::load(dir.path(), env_with(FULL))
            .unwrap_err()
            .to_string();
        assert!(!text.contains("tok-secret-value"), "{text}");
    }

    #[test]
    fn debug_output_hides_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(dir.path(), env_with(FULL)).unwrap();
        assert!(!format!("{config:?}").contains("tok-env"));
        assert_eq!(config.token.redact("a tok-env b"), "a [redacted] b");
    }
}

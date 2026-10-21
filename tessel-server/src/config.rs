//! The configuration file: where to listen, where the stores live, and which repositories to
//! serve.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tessel_coordinator::shell;

use crate::id::{Name, NameError};

/// What `tessel-server serve --config <file>` reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The address to listen on, `127.0.0.1:7878` for example.
    pub listen: SocketAddr,
    /// The directory of stores; one file per repository, and the lock that keeps a second server
    /// out.
    pub store_dir: PathBuf,
    #[serde(rename = "repository")]
    pub repositories: Vec<RepositoryConfig>,
    /// Who may connect. Without it the server answers 401 on every authenticated route.
    #[serde(default)]
    pub identity: Option<IdentityConfig>,
}

/// The `[identity]` table: where the Ed25519 keys are and which audience a token must name.
///
/// There is no deny-list: a token is refused once it expires or its key is removed from the
/// directory and the server restarted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityConfig {
    /// The directory of `<kid>.key` and `<kid>.pub` files `tessel-server admin keygen` writes.
    pub keys: PathBuf,
    /// The name a token's `aud` claim must equal, a repository-style name such as `tessel-prod`.
    pub audience: String,
}

/// A repository this server serves.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    pub tenant: String,
    pub name: String,
    /// Names the run in the evidence log of a repository with nothing stored yet.
    pub run: String,
    /// The agents that may approve or reject a submission held for review, by agent name. A
    /// token for one of these names is what lets `tessel review` decide. Empty: nobody may
    /// review, so a held submission stays held.
    #[serde(default)]
    pub reviewers: Vec<String>,
    /// Path prefixes that hold a submission for review whenever it touches one. A directory
    /// ends in `/`; there are no globs. Empty: only the other review reasons apply.
    #[serde(default)]
    pub sensitive: Vec<String>,
}

impl RepositoryConfig {
    /// `reviewers` in the comma-separated form `HostConfig` reads, or `None` when there are none.
    pub fn reviewers_list(&self) -> Option<String> {
        (!self.reviewers.is_empty()).then(|| self.reviewers.join(","))
    }

    /// `sensitive` in the comma-separated form `HostConfig` reads, or `None` when there are none.
    pub fn sensitive_list(&self) -> Option<String> {
        (!self.sensitive.is_empty()).then(|| self.sensitive.join(","))
    }

    fn check(&self, shown: &str) -> Result<(), ConfigError> {
        let refuse = |key: &'static str, entry: &str, reason: &'static str| {
            Err(ConfigError::RepositoryList {
                path: shown.to_string(),
                repository: format!("{}/{}", self.tenant, self.name),
                key,
                entry: entry.to_string(),
                reason,
            })
        };
        for entry in &self.reviewers {
            if !shell::is_agent_id(entry) {
                return refuse(
                    "reviewers",
                    entry,
                    "an agent name is letters, digits, '.', '_' and '-', starting with a letter \
                     or digit",
                );
            }
        }
        for entry in &self.sensitive {
            let parsed = shell::parse_sensitive(Some(entry));
            let one_prefix = match parsed.as_deref() {
                Ok([only]) => only == entry,
                Ok(_) | Err(_) => false,
            };
            if !one_prefix {
                return refuse(
                    "sensitive",
                    entry,
                    "a path prefix has no comma, no leading '/', no '..' segment and no '*', and \
                     no space at either end",
                );
            }
        }
        Ok(())
    }
}

impl IdentityConfig {
    fn check(&self, shown: &str) -> Result<(), ConfigError> {
        if self.keys.as_os_str().is_empty() {
            return Err(ConfigError::EmptyKeys {
                path: shown.to_string(),
            });
        }
        Name::parse(&self.audience)
            .map(drop)
            .map_err(|source| ConfigError::Audience {
                path: shown.to_string(),
                source,
            })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read the configuration {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("the configuration {path} is not valid: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    #[error("the configuration {path} names no repository; add a [[repository]] table")]
    NoRepositories { path: String },
    #[error("the configuration {path}: [identity] audience: {source}")]
    Audience { path: String, source: NameError },
    #[error("the configuration {path}: [identity] keys must not be empty")]
    EmptyKeys { path: String },
    #[error(
        "the configuration {path}: [[repository]] {repository}: {key} entry {entry:?}: {reason}"
    )]
    RepositoryList {
        path: String,
        repository: String,
        key: &'static str,
        entry: String,
        reason: &'static str,
    },
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: shown.clone(),
            source,
        })?;
        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: shown.clone(),
            source,
        })?;
        if config.repositories.is_empty() {
            return Err(ConfigError::NoRepositories { path: shown });
        }
        for repository in &config.repositories {
            repository.check(&shown)?;
        }
        if let Some(identity) = &config.identity {
            identity.check(&shown)?;
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> Result<Config, ConfigError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tessel-server.toml");
        std::fs::write(&path, text).unwrap();
        Config::load(&path)
    }

    #[test]
    fn a_configuration_names_the_address_the_directory_and_the_repositories() {
        let config = load(
            r#"
            listen = "127.0.0.1:7878"
            store_dir = "/var/lib/tessel"

            [[repository]]
            tenant = "acme"
            name = "demo"
            run = "dogfood"
            "#,
        )
        .unwrap();
        assert_eq!(config.listen.port(), 7878);
        assert_eq!(config.store_dir, Path::new("/var/lib/tessel"));
        assert_eq!(config.repositories.len(), 1);
        assert_eq!(config.repositories[0].run, "dogfood");
    }

    const BASE: &str = "listen = \"127.0.0.1:1\"\nstore_dir = \"x\"\n\n[[repository]]\n\
                        tenant = \"acme\"\nname = \"demo\"\nrun = \"r\"\n";

    #[test]
    fn an_identity_table_names_the_key_directory_and_the_audience() {
        let config = load(&format!(
            "{BASE}\n[identity]\nkeys = \"/etc/tessel/keys\"\naudience = \"tessel-prod\"\n"
        ))
        .unwrap();
        let identity = config.identity.unwrap();
        assert_eq!(identity.keys, Path::new("/etc/tessel/keys"));
        assert_eq!(identity.audience, "tessel-prod");
        assert_eq!(load(BASE).unwrap().identity, None, "the table is optional");
    }

    #[test]
    fn a_bad_identity_table_is_refused_naming_the_file_and_the_key() {
        for (table, key) in [
            ("keys = \"k\"\naudience = \"Not A Name\"", "audience"),
            ("keys = \"\"\naudience = \"prod\"", "keys"),
            ("keys = \"k\"\naudience = \"prod\"\ndeny = []", "deny"),
            ("keys = \"k\"", "audience"),
        ] {
            let error = load(&format!("{BASE}\n[identity]\n{table}\n")).unwrap_err();
            let shown = error.to_string();
            assert!(shown.contains("tessel-server.toml"), "{shown}");
            assert!(shown.contains(key), "{key}: {shown}");
        }
    }

    #[test]
    fn a_repository_names_its_reviewers_and_sensitive_paths() {
        let config = load(&format!(
            "{BASE}reviewers = [\"felix\", \"a.b_c-1\"]\nsensitive = [\"src/auth/\", \"tessel.toml\"]\n"
        ))
        .unwrap();
        let repository = &config.repositories[0];
        assert_eq!(
            repository.reviewers_list().as_deref(),
            Some("felix,a.b_c-1")
        );
        assert_eq!(
            repository.sensitive_list().as_deref(),
            Some("src/auth/,tessel.toml")
        );
        let bare = load(BASE).unwrap();
        assert_eq!(bare.repositories[0].reviewers_list(), None);
        assert_eq!(bare.repositories[0].sensitive_list(), None);
    }

    #[test]
    fn a_bad_reviewer_or_sensitive_entry_is_refused_naming_the_file_and_the_key() {
        for (line, key) in [
            ("reviewers = [\"Not A Name\"]", "reviewers"),
            ("reviewers = [\"\"]", "reviewers"),
            ("reviewers = [\"ana,bo\"]", "reviewers"),
            ("sensitive = [\"/etc\"]", "sensitive"),
            ("sensitive = [\"a/../b\"]", "sensitive"),
            ("sensitive = [\"src/*\"]", "sensitive"),
            ("sensitive = [\"\"]", "sensitive"),
            ("sensitive = [\"a.rs,b.rs\"]", "sensitive"),
            ("sensitive = [\" a.rs\"]", "sensitive"),
        ] {
            let error = load(&format!("{BASE}{line}\n")).unwrap_err();
            let shown = error.to_string();
            assert!(
                matches!(error, ConfigError::RepositoryList { .. }),
                "{line}: {shown}"
            );
            assert!(shown.contains("tessel-server.toml"), "{line}: {shown}");
            assert!(shown.contains(key), "{line}: {shown}");
            assert!(shown.contains("acme/demo"), "{line}: {shown}");
        }
    }

    #[test]
    fn a_missing_file_an_unknown_key_and_no_repository_are_each_refused_with_the_path() {
        let missing = Config::load(Path::new("/nonexistent/tessel.toml")).unwrap_err();
        assert!(matches!(missing, ConfigError::Read { .. }), "{missing}");
        assert!(missing.to_string().contains("/nonexistent/tessel.toml"));
        let unknown = load("listen = \"127.0.0.1:1\"\nstore_dir = \"x\"\nverbose = true\n");
        assert!(matches!(unknown, Err(ConfigError::Parse { .. })));
        let none = load("listen = \"127.0.0.1:1\"\nstore_dir = \"x\"\nrepository = []\n");
        assert!(matches!(none, Err(ConfigError::NoRepositories { .. })));
    }
}

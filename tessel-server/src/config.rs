//! The configuration file: where to listen, where the stores live, and which repositories to
//! serve.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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
}

/// A repository this server serves.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    pub tenant: String,
    pub name: String,
    /// Names the run in the evidence log of a repository with nothing stored yet.
    pub run: String,
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

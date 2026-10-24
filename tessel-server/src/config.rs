//! The configuration file: where to listen, where the stores live, and which repositories to
//! serve.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tessel_coordinator::shell;

use crate::credential::is_http_url;
use crate::id::{Name, NameError};
use crate::identity;
use crate::paths::{absolute, ResolveError};

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
    /// Where landings run and which runner tests them. Required by any repository with a
    /// `remote`; without a `remote` anywhere it must be absent.
    #[serde(default)]
    pub lander: Option<LanderConfig>,
}

/// The `[lander]` table: the lander and the runner, in process, for the repositories that name a
/// `remote`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanderConfig {
    /// Where clones live while a call runs, under `<work_dir>/<tenant>/<repo>/`. Each call makes
    /// its clones fresh and removes them; a restart removes any a killed server left.
    pub work_dir: PathBuf,
    /// The runner's `tessel-runner.toml`: the one user the tests run for and the longest run.
    pub runner_config: PathBuf,
    /// A file holding the git host's token, for a `remote` or `fork` that is a URL which needs
    /// one. The file must be a regular file with mode 0600 and not empty. Git reads it through
    /// the askpass hook at every command of a landing. Without it, landings use no credential.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
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
    /// The git remote main lives on, a path or a URL. With it the server lands submissions on
    /// this remote; without it every merge is refused.
    #[serde(default)]
    pub remote: Option<String>,
    /// Where an agent's fork is, with `{agent}` standing for the agent's name:
    /// `/srv/forks/{agent}.git`. Required with `remote`.
    #[serde(default)]
    pub fork: Option<String>,
}

/// The placeholder in `fork` that the agent's name replaces.
pub const FORK_AGENT_PLACEHOLDER: &str = "{agent}";

/// Whether `fork` is an http(s) URL with `{agent}` in its authority, before the first `/`.
fn agent_in_url_host(fork: &str) -> bool {
    if !is_http_url(fork) {
        return false;
    }
    let after_scheme = fork.split_once("://").map_or("", |(_, rest)| rest);
    let authority = after_scheme.split('/').next().unwrap_or_default();
    authority.contains(FORK_AGENT_PLACEHOLDER)
}

impl Config {
    /// This configuration with `store_dir` and `runner_config` made absolute, so that removing
    /// the directory the server started in does not affect them.
    pub fn with_absolute_paths(&self) -> Result<Config, ResolveError> {
        let mut resolved = self.clone();
        resolved.store_dir = absolute(&self.store_dir)?;
        if let Some(lander) = resolved.lander.as_mut() {
            lander.runner_config = absolute(&lander.runner_config)?;
        }
        Ok(resolved)
    }
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

    /// The `remote` and `fork` keys: both or neither, and both need the `[lander]` table.
    fn check_landing(&self, shown: &str, has_lander: bool) -> Result<(), ConfigError> {
        let refuse = |key: &'static str, reason: &'static str| {
            Err(ConfigError::Landing {
                path: shown.to_string(),
                table: format!("[[repository]] {}/{}", self.tenant, self.name),
                key,
                reason,
            })
        };
        let blank = |text: &str| text.trim().is_empty();
        match (&self.remote, &self.fork) {
            (None, Some(_)) => refuse(
                "fork",
                "is only read with a remote; add remote or remove it",
            ),
            (Some(_), None) => refuse(
                "fork",
                "is required with remote: where an agent's fork is, with {agent} for its name",
            ),
            (Some(remote), Some(_)) if blank(remote) => refuse("remote", "must not be empty"),
            (Some(_), Some(fork)) if fork.matches(FORK_AGENT_PLACEHOLDER).count() != 1 => refuse(
                "fork",
                "must contain {agent} exactly once, standing for the agent's name",
            ),
            (Some(_), Some(fork)) if agent_in_url_host(fork) => refuse(
                "fork",
                "must not put {agent} in the host of a URL: an agent's name would choose which \
                 host receives the token",
            ),
            (Some(_), Some(_)) if !has_lander => refuse(
                "remote",
                "needs the [lander] table with work_dir and runner_config",
            ),
            (None, None) | (Some(_), Some(_)) => Ok(()),
        }
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
            if !identity::is_server_agent_name(entry) {
                return refuse(
                    "reviewers",
                    entry,
                    "an agent name is lowercase letters, digits, '.', '_' and '-', starting with \
                     a letter or digit",
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

impl LanderConfig {
    fn check(&self, shown: &str, any_remote: bool) -> Result<(), ConfigError> {
        let refuse = |key: &'static str, reason: &'static str| {
            Err(ConfigError::Landing {
                path: shown.to_string(),
                table: "[lander]".to_string(),
                key,
                reason,
            })
        };
        if self.work_dir.as_os_str().is_empty() {
            return refuse("work_dir", "must not be empty");
        }
        if self.runner_config.as_os_str().is_empty() {
            return refuse("runner_config", "must not be empty");
        }
        if self
            .token_file
            .as_ref()
            .is_some_and(|file| file.as_os_str().is_empty())
        {
            return refuse("token_file", "must not be empty");
        }
        if !any_remote {
            return refuse(
                "[lander]",
                "is read only when a [[repository]] names a remote",
            );
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
    #[error("the configuration {path}: {table}: {key} {reason}")]
    Landing {
        path: String,
        table: String,
        key: &'static str,
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
        let has_lander = config.lander.is_some();
        for repository in &config.repositories {
            repository.check(&shown)?;
            repository.check_landing(&shown, has_lander)?;
        }
        if let Some(lander) = &config.lander {
            let any_remote = config.repositories.iter().any(|r| r.remote.is_some());
            lander.check(&shown, any_remote)?;
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
            "{BASE}reviewers = [\"felix\", \"a.b_c-1\"]\n\
             sensitive = [\"src/auth/\", \"tessel.toml\"]\n"
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
    fn a_reviewer_that_is_not_lowercase_is_refused_naming_file_repository_and_key() {
        let error = load(&format!("{BASE}reviewers = [\"Orchestrator\"]\n")).unwrap_err();
        let shown = error.to_string();
        assert!(
            matches!(error, ConfigError::RepositoryList { .. }),
            "{shown}"
        );
        for name in ["tessel-server.toml", "acme/demo", "reviewers"] {
            assert!(shown.contains(name), "{name}: {shown}");
        }
    }

    const LANDER: &str = "\n[lander]\nwork_dir = \"/var/lib/tessel/work\"\n\
                          runner_config = \"/etc/tessel/tessel-runner.toml\"\n";

    #[test]
    fn a_repository_with_a_remote_and_a_fork_is_landed_through_the_lander_table() {
        let config = load(&format!(
            "{BASE}remote = \"/srv/git/demo.git\"\nfork = \"/srv/forks/{{agent}}.git\"\n{LANDER}"
        ))
        .unwrap();
        let repository = &config.repositories[0];
        assert_eq!(repository.remote.as_deref(), Some("/srv/git/demo.git"));
        assert_eq!(repository.fork.as_deref(), Some("/srv/forks/{agent}.git"));
        let lander = config.lander.unwrap();
        assert_eq!(lander.work_dir, Path::new("/var/lib/tessel/work"));
        assert_eq!(
            lander.runner_config,
            Path::new("/etc/tessel/tessel-runner.toml")
        );
        let bare = load(BASE).unwrap();
        assert_eq!(
            (bare.repositories[0].remote.clone(), bare.lander),
            (None, None)
        );
    }

    #[test]
    fn a_bad_landing_setup_is_refused_naming_the_file_the_table_and_the_key() {
        let fork = "fork = \"/srv/forks/{agent}.git\"\n";
        for (text, table, key) in [
            ("remote = \"/r.git\"\n", "[[repository]] acme/demo", "fork"),
            (fork, "[[repository]] acme/demo", "fork"),
            (
                "remote = \"\"\nfork = \"/f/{agent}\"\n",
                "[[repository]]",
                "remote",
            ),
            (
                "remote = \"/r.git\"\nfork = \"/f/{agent}/{agent}\"\n",
                "[[repository]]",
                "fork",
            ),
            (
                "remote = \"/r.git\"\nfork = \"/f/x.git\"\n",
                "[[repository]]",
                "fork",
            ),
            (
                "remote = \"/r.git\"\nfork = \"/f/{agent}\"\n",
                "[[repository]]",
                "remote",
            ),
        ] {
            let error = load(&format!("{BASE}{text}")).unwrap_err();
            let shown = error.to_string();
            assert!(
                matches!(error, ConfigError::Landing { .. }),
                "{text}: {shown}"
            );
            assert!(shown.contains("tessel-server.toml"), "{text}: {shown}");
            assert!(shown.contains(table), "{text}: {shown}");
            assert!(shown.contains(key), "{text}: {shown}");
        }
    }

    const URLS: &str = "remote = \"https://github.com/acme/demo.git\"\n\
                        fork = \"https://github.com/acme/demo-fork-{agent}.git\"\n";

    #[test]
    fn a_url_remote_and_fork_need_no_token_file_and_a_token_file_is_a_path() {
        let public = load(&format!("{BASE}{URLS}{LANDER}")).unwrap();
        assert_eq!(public.lander.unwrap().token_file, None);
        let private = load(&format!(
            "{BASE}{URLS}{LANDER}token_file = \"/etc/tessel/github-token\"\n"
        ))
        .unwrap();
        assert_eq!(
            private.lander.unwrap().token_file.as_deref(),
            Some(Path::new("/etc/tessel/github-token"))
        );
    }

    #[test]
    fn an_empty_token_file_path_is_refused_naming_the_file_and_the_key() {
        let error = load(&format!("{BASE}{URLS}{LANDER}token_file = \"\"\n")).unwrap_err();
        let shown = error.to_string();
        assert!(matches!(error, ConfigError::Landing { .. }), "{shown}");
        assert!(shown.contains("tessel-server.toml"), "{shown}");
        assert!(
            shown.contains("[lander]") && shown.contains("token_file"),
            "{shown}"
        );
    }

    #[test]
    fn a_url_fork_is_checked_for_one_agent_placeholder_like_a_path_fork() {
        for fork in [
            "https://github.com/acme/demo-fork.git",
            "https://github.com/acme/{agent}/{agent}.git",
            "https://{agent}/acme/demo.git",
            "https://{agent}.example.com/demo.git",
            "http://git.example.com:{agent}/demo.git",
            "https://user@{agent}/demo.git",
        ] {
            let text = format!(
                "{BASE}remote = \"https://github.com/acme/demo.git\"\nfork = \"{fork}\"\n{LANDER}"
            );
            let error = load(&text).unwrap_err();
            let shown = error.to_string();
            assert!(
                matches!(error, ConfigError::Landing { .. }),
                "{fork}: {shown}"
            );
            assert!(
                shown.contains("fork") && shown.contains("{agent}"),
                "{fork}: {shown}"
            );
        }
    }

    #[test]
    fn a_bad_lander_table_is_refused_naming_the_file_and_the_key() {
        let landed = format!("{BASE}remote = \"/r.git\"\nfork = \"/f/{{agent}}\"\n");
        for (text, key) in [
            (
                "\n[lander]\nwork_dir = \"\"\nrunner_config = \"r\"\n",
                "work_dir",
            ),
            (
                "\n[lander]\nwork_dir = \"w\"\nrunner_config = \"\"\n",
                "runner_config",
            ),
            ("\n[lander]\nwork_dir = \"w\"\n", "runner_config"),
            (
                "\n[lander]\nwork_dir = \"w\"\nrunner_config = \"r\"\nuser = \"x\"\n",
                "user",
            ),
        ] {
            let error = load(&format!("{landed}{text}")).unwrap_err();
            let shown = error.to_string();
            assert!(shown.contains("tessel-server.toml"), "{text}: {shown}");
            assert!(shown.contains(key), "{text}: {shown}");
        }
        let unused = load(&format!("{BASE}{LANDER}")).unwrap_err();
        assert!(matches!(unused, ConfigError::Landing { .. }), "{unused}");
        assert!(unused.to_string().contains("[lander]"), "{unused}");
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

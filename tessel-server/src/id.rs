//! The structured id of a repository: a tenant and a repository name, each checked once, here.
//! The store path and the route are built from the two names separately and never by joining
//! them into one string, so `("a-b", "c")` and `("a", "b-c")` cannot meet (requirement S5).
//!
//! A name is lowercase letters, digits and interior hyphens, at most 63 characters. Lowercase
//! only, because the store path is made of the names and a case-insensitive file system would
//! otherwise give `Demo` and `demo` one file. No dot and no slash, so a name is never a path
//! separator or a parent segment.

use std::fmt;
use std::path::{Path, PathBuf};

/// The longest name: one DNS label.
pub const NAME_MAX_LEN: usize = 63;

/// The file extension of a repository's store.
const STORE_EXTENSION: &str = "sqlite";

/// Why a name is refused. It never carries the text that was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("a name cannot be empty")]
    Empty,
    #[error("a name is at most {NAME_MAX_LEN} characters, this one has {len}")]
    TooLong { len: usize },
    #[error("a name has only lowercase letters, digits and hyphens")]
    Character,
    #[error("a name cannot start or end with a hyphen")]
    EdgeHyphen,
}

/// A tenant or repository name that passed the check.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

impl Name {
    pub fn parse(text: &str) -> Result<Self, NameError> {
        if text.is_empty() {
            return Err(NameError::Empty);
        }
        let len = text.chars().count();
        if len > NAME_MAX_LEN {
            return Err(NameError::TooLong { len });
        }
        if !text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(NameError::Character);
        }
        if text.starts_with('-') || text.ends_with('-') {
            return Err(NameError::EdgeHyphen);
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which name is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("tenant: {0}")]
    Tenant(NameError),
    #[error("repository: {0}")]
    Repo(NameError),
}

/// A repository of a tenant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoId {
    tenant: Name,
    repo: Name,
}

impl RepoId {
    pub fn new(tenant: &str, repo: &str) -> Result<Self, IdError> {
        Ok(Self {
            tenant: Name::parse(tenant).map_err(IdError::Tenant)?,
            repo: Name::parse(repo).map_err(IdError::Repo)?,
        })
    }

    pub fn tenant(&self) -> &str {
        self.tenant.as_str()
    }

    pub fn repo(&self) -> &str {
        self.repo.as_str()
    }

    /// `<root>/<tenant>/<repo>.sqlite`: the tenant is a directory and the repository a file in
    /// it, so no choice of names makes two ids one path.
    pub fn store_file(&self, root: &Path) -> PathBuf {
        let mut file = PathBuf::from(root);
        file.push(self.tenant());
        file.push(format!("{}.{STORE_EXTENSION}", self.repo()));
        file
    }

    /// `/tenant/<tenant>/repo/<repo>`: what a route for this repository starts with.
    pub fn route_base(&self) -> String {
        format!("/tenant/{}/repo/{}", self.tenant(), self.repo())
    }
}

/// `tenant/repo`, for log lines. Both names passed the check, so there is nothing to escape.
impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.tenant(), self.repo())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_lowercase_letters_digits_and_interior_hyphens() {
        for good in [
            "a",
            "demo",
            "a-b",
            "0",
            "a1-b2-c3",
            &"x".repeat(NAME_MAX_LEN),
        ] {
            assert!(Name::parse(good).is_ok(), "{good}");
        }
        assert_eq!(Name::parse(""), Err(NameError::Empty));
        assert_eq!(
            Name::parse(&"x".repeat(NAME_MAX_LEN + 1)),
            Err(NameError::TooLong {
                len: NAME_MAX_LEN + 1
            })
        );
        for bad in [
            ".", "..", "a/b", "a.b", "A", "a_b", "a b", "é", "a\0b", "../x",
        ] {
            assert_eq!(Name::parse(bad), Err(NameError::Character), "{bad:?}");
        }
        for bad in ["-a", "a-", "-"] {
            assert_eq!(Name::parse(bad), Err(NameError::EdgeHyphen), "{bad}");
        }
    }

    #[test]
    fn an_id_says_which_name_it_refused_without_quoting_it() {
        assert_eq!(
            RepoId::new("Acme", "demo"),
            Err(IdError::Tenant(NameError::Character))
        );
        assert_eq!(
            RepoId::new("acme", ""),
            Err(IdError::Repo(NameError::Empty))
        );
        let message = RepoId::new("acme", "a/b").unwrap_err().to_string();
        assert!(!message.contains("a/b"), "{message}");
    }

    #[test]
    fn the_store_file_is_a_tenant_directory_and_a_repository_file() {
        let id = RepoId::new("acme", "demo-2").unwrap();
        assert_eq!(
            id.store_file(Path::new("/var/tessel")),
            Path::new("/var/tessel/acme/demo-2.sqlite")
        );
        assert_eq!(id.route_base(), "/tenant/acme/repo/demo-2");
        assert_eq!(id.to_string(), "acme/demo-2");
    }
}

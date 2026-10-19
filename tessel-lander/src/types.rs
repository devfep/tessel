//! Values that are checked once and then trusted: object ids and branch names.

use std::fmt;

/// A full, lowercase, 40-hex git object id. Only [`Sha::parse`] makes one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha(String);

/// The input to [`Sha::parse`] was not 40 lowercase hex characters.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{input:?} is not a git object id: expected 40 lowercase hex characters")]
pub struct InvalidSha {
    pub input: String,
}

impl Sha {
    pub fn parse(value: &str) -> Result<Sha, InvalidSha> {
        let valid = value.len() == 40
            && value
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if valid {
            Ok(Sha(value.to_owned()))
        } else {
            Err(InvalidSha {
                input: value.to_owned(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `name` is safe to put in a refspec: it starts with a letter or digit, is at most 128
/// characters of `[A-Za-z0-9._/-]`, has no `..` and does not end in `/`. A name that starts with
/// `-` can therefore never be read as an option.
pub fn is_safe_branch_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && name.len() <= 128
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
        && !name.contains("..")
        && !name.ends_with('/')
}

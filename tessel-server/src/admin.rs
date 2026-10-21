//! The `admin` subcommands: write a key pair, mint a token. Both work on the key directory the
//! `[identity]` table names, and neither starts or talks to a server.

use crate::config::{Config, IdentityConfig};
use crate::identity::{self, MintError, Role, TokenRequest};
use crate::keys::{self, KeyError};

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 60 * MINUTE_MS;
const DAY_MS: u64 = 24 * HOUR_MS;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("lifetime {text:?} must be a number and a unit, such as 30m, 12h or 7d")]
pub struct TtlError {
    text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("the configuration {path} has no [identity] table; add `keys` and `audience`")]
    NoIdentity { path: String },
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error(transparent)]
    Mint(#[from] MintError),
}

/// A lifetime such as `30m`, `12h` or `7d`, in milliseconds.
pub fn parse_ttl(text: &str) -> Result<u64, TtlError> {
    let bad = || TtlError {
        text: text.to_string(),
    };
    let (last, _) = text.char_indices().last().ok_or_else(bad)?;
    let (number, unit) = text.split_at(last);
    let unit_ms = match unit {
        "m" => MINUTE_MS,
        "h" => HOUR_MS,
        "d" => DAY_MS,
        _ => return Err(bad()),
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let count: u64 = number.parse().map_err(|_| bad())?;
    count.checked_mul(unit_ms).ok_or_else(bad)
}

/// The `[identity]` table of the configuration read from `path`.
pub fn identity_of<'a>(config: &'a Config, path: &str) -> Result<&'a IdentityConfig, AdminError> {
    config
        .identity
        .as_ref()
        .ok_or_else(|| AdminError::NoIdentity {
            path: path.to_string(),
        })
}

/// Write the key pair of `kid` into the key directory.
pub fn keygen(identity: &IdentityConfig, kid: &str) -> Result<(), AdminError> {
    Ok(keys::generate(&identity.keys, kid)?)
}

/// Who a minted token is for and for how long.
pub struct TokenArgs<'a> {
    pub kid: &'a str,
    pub tenant: &'a str,
    pub repo: &'a str,
    pub agent: &'a str,
    pub ttl_ms: u64,
    pub service: bool,
}

/// Mint a token with the secret key of `args.kid`, valid from `now_ms`.
pub fn token(
    identity: &IdentityConfig,
    args: &TokenArgs<'_>,
    now_ms: u64,
) -> Result<String, AdminError> {
    let key = keys::load_signing_key(&identity.keys, args.kid)?;
    let request = TokenRequest {
        kid: args.kid,
        audience: &identity.audience,
        tenant: args.tenant,
        repo: args.repo,
        agent: args.agent,
        role: if args.service {
            Role::Service
        } else {
            Role::Agent
        },
        ttl_ms: args.ttl_ms,
    };
    Ok(identity::mint(&key, &request, now_ms)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lifetime_is_a_number_of_minutes_hours_or_days() {
        assert_eq!(parse_ttl("30m"), Ok(30 * 60_000));
        assert_eq!(parse_ttl("12h"), Ok(12 * 3_600_000));
        assert_eq!(parse_ttl("7d"), Ok(7 * 86_400_000));
    }

    #[test]
    fn anything_else_is_not_a_lifetime() {
        let overflow = format!("{}d", u64::MAX);
        for text in [
            "", "m", "30", "30s", "30M", "-5m", "+5m", "5 m", " 5m", "1.5h", "5mm", "h5", "٣m",
            "5٣", &overflow,
        ] {
            assert!(parse_ttl(text).is_err(), "{text:?}");
        }
        assert!(parse_ttl("0m").is_ok(), "zero parses; minting refuses it");
    }
}

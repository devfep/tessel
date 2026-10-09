//! Agent identity tokens: what the steward signs and the coordinator checks at the WebSocket
//! upgrade. Pure functions, no runtime types, so every rule here is tested natively.
//!
//! # Token format
//!
//! `<payload>.<mac>`, sent as `Authorization: Bearer <token>`.
//!
//! - `payload` is base64url without padding of the JSON object
//!   `{"v":1,"repo":"<repo>","agent":"<agent>","exp_ms":<u64>}`, with the keys in that order.
//! - `mac` is base64url without padding of HMAC-SHA256 over the bytes of the `payload` string
//!   exactly as received, keyed with the secret `IDENTITY_SIGNING_KEY` (its UTF-8 bytes).
//! - `exp_ms` is a Unix time in milliseconds; the token is valid while `exp_ms > now`.
//! - Name rule, for `agent` here and for `repo` and `agent` in the steward: 1 to
//!   `MAX_AGENT_ID_BYTES` characters from `A-Z a-z 0-9 . _ -`, the first a letter or digit.
//!
//! The MAC is verified before the payload is decoded or parsed. The steward signs in
//! `tessel-steward/src/identity.ts`; both sides assert the same fixed vector in their tests.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::protocol::AgentId;
use crate::shell::is_agent_id;

/// The only token version this verifier accepts.
const TOKEN_VERSION: u32 = 1;

/// The longest token accepted. A token for the longest agent id and a repo name of a few hundred
/// bytes is under 1 KiB; anything longer is refused before it is decoded.
const MAX_TOKEN_BYTES: usize = 1024;

const BEARER_PREFIX: &str = "Bearer ";

type HmacSha256 = Hmac<Sha256>;

/// Why a token was refused. The Worker answers every one with the same 401; the variant is for
/// logs and tests and never carries token material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    /// `IDENTITY_SIGNING_KEY` is unset or empty, so every request is refused.
    NoSigningKey,
    /// No `Authorization` header, or one that is not `Bearer <token>`.
    NoBearerToken,
    /// The token is longer than `MAX_TOKEN_BYTES`.
    TooLong,
    /// The token is not `<payload>.<mac>` with both halves valid base64url.
    Malformed,
    /// The MAC does not match the payload.
    BadSignature,
    /// The signed payload is not the expected JSON object.
    BadPayload,
    /// The signed payload has a version other than 1.
    WrongVersion,
    /// `exp_ms` is not after now.
    Expired,
    /// The token was issued for another repo.
    WrongRepo,
    /// The signed agent id is empty, too long, or has characters outside the allowed set.
    BadAgent,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    v: u32,
    repo: String,
    agent: String,
    exp_ms: u64,
}

/// The agent a request's `Authorization` header proves, for the repo in the URL.
///
/// Fails closed: a missing or empty `key`, a missing header, another scheme, a bad MAC, an
/// expired token or a token for another repo never yield an agent.
pub fn verify_bearer(
    key: Option<&str>,
    authorization: Option<&str>,
    repo: &str,
    now_ms: u64,
) -> Result<AgentId, IdentityError> {
    let Some(key) = key.filter(|key| !key.is_empty()) else {
        return Err(IdentityError::NoSigningKey);
    };
    let token = authorization
        .and_then(|header| header.strip_prefix(BEARER_PREFIX))
        .filter(|token| !token.is_empty())
        .ok_or(IdentityError::NoBearerToken)?;
    verify_token(key.as_bytes(), token, repo, now_ms)
}

fn verify_token(
    key: &[u8],
    token: &str,
    repo: &str,
    now_ms: u64,
) -> Result<AgentId, IdentityError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(IdentityError::TooLong);
    }
    let Some((payload, mac)) = token.split_once('.') else {
        return Err(IdentityError::Malformed);
    };
    if mac.contains('.') {
        return Err(IdentityError::Malformed);
    }
    let mac = URL_SAFE_NO_PAD
        .decode(mac)
        .map_err(|_| IdentityError::Malformed)?;
    let mut expected = new_mac(key)?;
    expected.update(payload.as_bytes());
    expected
        .verify_slice(&mac)
        .map_err(|_| IdentityError::BadSignature)?;

    let json = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| IdentityError::BadPayload)?;
    let claims: Claims = serde_json::from_slice(&json).map_err(|_| IdentityError::BadPayload)?;
    if claims.v != TOKEN_VERSION {
        return Err(IdentityError::WrongVersion);
    }
    if claims.exp_ms <= now_ms {
        return Err(IdentityError::Expired);
    }
    if claims.repo != repo {
        return Err(IdentityError::WrongRepo);
    }
    if !is_agent_id(&claims.agent) {
        return Err(IdentityError::BadAgent);
    }
    Ok(AgentId(claims.agent))
}

fn new_mac(key: &[u8]) -> Result<HmacSha256, IdentityError> {
    HmacSha256::new_from_slice(key).map_err(|_| IdentityError::NoSigningKey)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::MAX_AGENT_ID_BYTES;
    use proptest::prelude::*;

    const KEY: &str = "test-signing-key-not-a-secret";
    const NOW: u64 = 1_000_000;
    const EXP: u64 = 2_000_000;

    /// The same vector is asserted in `tessel-steward/src/identity.test.ts`.
    const VECTOR_EXP_MS: u64 = 1_790_000_000_000;
    const VECTOR_TOKEN: &str = "eyJ2IjoxLCJyZXBvIjoiZGVtbyIsImFnZW50IjoiYWdlbnQtMSIsImV4cF9tcyI6MTc5MDAwMDAwMDAwMH0.Cz0zjmq3Tc7-jsR5M2Ss4FRRkJnwFdLgoh4_1C74Omo";

    fn sign_claims(key: &str, claims: &Claims) -> String {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_string(claims).unwrap());
        sign_payload(key, &payload)
    }

    fn sign_payload(key: &str, payload: &str) -> String {
        let mut mac = new_mac(key.as_bytes()).unwrap();
        mac.update(payload.as_bytes());
        let mac = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{payload}.{mac}")
    }

    fn sign(key: &str, repo: &str, agent: &str, exp_ms: u64) -> String {
        sign_claims(
            key,
            &Claims {
                v: TOKEN_VERSION,
                repo: repo.to_string(),
                agent: agent.to_string(),
                exp_ms,
            },
        )
    }

    fn verify(token: &str, repo: &str, now_ms: u64) -> Result<AgentId, IdentityError> {
        verify_token(KEY.as_bytes(), token, repo, now_ms)
    }

    fn valid() -> String {
        sign(KEY, "demo", "agent-1", EXP)
    }

    #[test]
    fn a_valid_token_yields_its_agent() {
        assert_eq!(
            verify(&valid(), "demo", NOW),
            Ok(AgentId("agent-1".to_string()))
        );
    }

    #[test]
    fn the_steward_vector_signs_and_verifies() {
        assert_eq!(sign(KEY, "demo", "agent-1", VECTOR_EXP_MS), VECTOR_TOKEN);
        assert_eq!(
            verify(VECTOR_TOKEN, "demo", NOW),
            Ok(AgentId("agent-1".to_string()))
        );
    }

    #[test]
    fn a_tampered_payload_is_refused_before_it_is_parsed() {
        let (_, mac) = valid()
            .split_once('.')
            .map(|(p, m)| (p.to_string(), m.to_string()))
            .unwrap();
        let forged =
            URL_SAFE_NO_PAD.encode(r#"{"v":1,"repo":"demo","agent":"admin","exp_ms":9999999}"#);
        assert_eq!(
            verify(&format!("{forged}.{mac}"), "demo", NOW),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn a_tampered_mac_is_refused() {
        let token = valid();
        let (payload, mac) = token.split_once('.').unwrap();
        let mut bytes = URL_SAFE_NO_PAD.decode(mac).unwrap();
        bytes[0] ^= 1;
        let forged = format!("{payload}.{}", URL_SAFE_NO_PAD.encode(bytes));
        assert_eq!(
            verify(&forged, "demo", NOW),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn a_token_signed_with_another_key_is_refused() {
        let token = sign("another-key", "demo", "agent-1", EXP);
        assert_eq!(
            verify(&token, "demo", NOW),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn a_token_is_valid_until_exp_ms_and_not_at_it() {
        let token = valid();
        assert!(verify(&token, "demo", EXP - 1).is_ok());
        assert_eq!(verify(&token, "demo", EXP), Err(IdentityError::Expired));
        assert_eq!(verify(&token, "demo", EXP + 1), Err(IdentityError::Expired));
    }

    #[test]
    fn a_token_for_another_repo_is_refused() {
        assert_eq!(
            verify(&valid(), "other", NOW),
            Err(IdentityError::WrongRepo)
        );
        assert_eq!(verify(&valid(), "DEMO", NOW), Err(IdentityError::WrongRepo));
    }

    #[test]
    fn another_version_is_refused() {
        for v in [0, 2] {
            let token = sign_claims(
                KEY,
                &Claims {
                    v,
                    repo: "demo".into(),
                    agent: "a1".into(),
                    exp_ms: EXP,
                },
            );
            assert_eq!(
                verify(&token, "demo", NOW),
                Err(IdentityError::WrongVersion)
            );
        }
    }

    #[test]
    fn a_signed_payload_that_is_not_the_claims_is_refused() {
        for json in [
            "not json",
            r#"{"v":1,"repo":"demo","agent":"a1"}"#,
            r#"{"v":1,"repo":"demo","agent":"a1","exp_ms":-1}"#,
            r#"{"v":1,"repo":"demo","agent":"a1","exp_ms":9999999,"admin":true}"#,
        ] {
            let token = sign_payload(KEY, &URL_SAFE_NO_PAD.encode(json));
            assert_eq!(
                verify(&token, "demo", NOW),
                Err(IdentityError::BadPayload),
                "{json}"
            );
        }
        let token = sign_payload(KEY, "!!not-base64!!");
        assert_eq!(verify(&token, "demo", NOW), Err(IdentityError::BadPayload));
    }

    #[test]
    fn a_signed_agent_id_that_cannot_be_an_agent_id_is_refused() {
        let long = "a".repeat(MAX_AGENT_ID_BYTES + 1);
        for agent in [
            "",
            "a b",
            "a\r\nb",
            "agent/1",
            "ägent",
            ".a",
            "-a",
            "_a",
            long.as_str(),
        ] {
            let token = sign(KEY, "demo", agent, EXP);
            assert_eq!(
                verify(&token, "demo", NOW),
                Err(IdentityError::BadAgent),
                "{agent:?}"
            );
        }
        let at_limit = "a".repeat(MAX_AGENT_ID_BYTES);
        assert!(verify(&sign(KEY, "demo", &at_limit, EXP), "demo", NOW).is_ok());
    }

    #[test]
    fn malformed_tokens_are_refused() {
        let token = valid();
        let (payload, mac) = token.split_once('.').unwrap();
        let cases = [
            ("no dot", payload.to_string()),
            ("extra dot", format!("{payload}.{mac}.x")),
            ("leading dot", format!(".{mac}")),
            ("empty mac", format!("{payload}.")),
            ("empty token", String::new()),
            ("padded mac", format!("{payload}.{mac}=")),
            (
                "mac with a character outside the alphabet",
                format!("{payload}.+{}", &mac[1..]),
            ),
            ("non-base64 mac", format!("{payload}.!!!")),
            ("short mac", format!("{payload}.{}", &mac[..mac.len() - 4])),
        ];
        for (label, token) in cases {
            let outcome = verify(&token, "demo", NOW);
            assert!(
                matches!(
                    outcome,
                    Err(IdentityError::Malformed | IdentityError::BadSignature)
                ),
                "{label}: {outcome:?}"
            );
        }
        assert_eq!(
            verify(&format!("{payload}.{mac}.x"), "demo", NOW),
            Err(IdentityError::Malformed)
        );
        assert_eq!(verify(payload, "demo", NOW), Err(IdentityError::Malformed));
    }

    #[test]
    fn an_oversized_token_is_refused_before_it_is_decoded() {
        let at_limit = format!("{}.x", "a".repeat(MAX_TOKEN_BYTES - 2));
        assert_ne!(verify(&at_limit, "demo", NOW), Err(IdentityError::TooLong));
        let over = "a".repeat(MAX_TOKEN_BYTES + 1);
        assert_eq!(verify(&over, "demo", NOW), Err(IdentityError::TooLong));
    }

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    #[test]
    fn the_header_form_yields_the_agent() {
        let header = bearer(&valid());
        assert_eq!(
            verify_bearer(Some(KEY), Some(&header), "demo", NOW),
            Ok(AgentId("agent-1".to_string()))
        );
    }

    #[test]
    fn a_missing_or_empty_signing_key_refuses_everything() {
        let header = bearer(&valid());
        for key in [None, Some("")] {
            assert_eq!(
                verify_bearer(key, Some(&header), "demo", NOW),
                Err(IdentityError::NoSigningKey)
            );
        }
    }

    #[test]
    fn only_a_bearer_header_is_read() {
        let token = valid();
        let basic = format!("Basic {token}");
        let lower = format!("bearer {token}");
        let joined = format!("Bearer{token}");
        let padded = format!(" Bearer {token}");
        for header in [
            None,
            Some(""),
            Some("Bearer "),
            Some("Bearer"),
            Some(token.as_str()),
            Some(basic.as_str()),
            Some(lower.as_str()),
            Some(joined.as_str()),
            Some(padded.as_str()),
        ] {
            assert_eq!(
                verify_bearer(Some(KEY), header, "demo", NOW),
                Err(IdentityError::NoBearerToken),
                "{header:?}"
            );
        }
    }

    #[test]
    fn an_oversized_header_is_refused() {
        let header = bearer(&"a".repeat(64 * 1024));
        assert_eq!(
            verify_bearer(Some(KEY), Some(&header), "demo", NOW),
            Err(IdentityError::TooLong)
        );
    }

    proptest! {
        #[test]
        fn sign_then_verify_round_trips(
            repo in ".{0,100}",
            agent in "[A-Za-z0-9][A-Za-z0-9._-]{0,127}",
            exp_ms in 1u64..(1 << 53),
        ) {
            let token = sign(KEY, &repo, &agent, exp_ms);
            prop_assert_eq!(verify(&token, &repo, exp_ms - 1), Ok(AgentId(agent)));
        }

        #[test]
        fn flipping_any_one_byte_of_a_token_fails(
            repo in "[a-z0-9-]{1,40}",
            agent in "[A-Za-z0-9][A-Za-z0-9._-]{0,127}",
        ) {
            let token = sign(KEY, &repo, &agent, EXP);
            for index in 0..token.len() {
                let mut bytes = token.clone().into_bytes();
                bytes[index] ^= 1;
                let flipped = String::from_utf8(bytes).unwrap();
                prop_assert!(verify(&flipped, &repo, NOW).is_err(), "byte {index}");
            }
        }
    }
}

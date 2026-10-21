//! Identity: Ed25519 tokens, minted by `tessel-server admin token` and checked at the routes.
//!
//! # Token format
//!
//! `<payload>.<signature>`, sent as `Authorization: Bearer <token>`.
//!
//! - `payload` is base64url without padding of the JSON object
//!   `{"v":1,"kid":..,"aud":..,"tenant":..,"repo":..,"agent":..,"role":"agent"|"service",
//!   "exp_ms":..}`, with the keys in that order.
//! - `signature` is base64url without padding of the Ed25519 signature over the bytes of the
//!   `payload` string exactly as sent. The key is the one `kid` names in the key directory.
//! - `exp_ms` is a Unix time in milliseconds; the token is valid while `exp_ms > now`.
//! - `agent` follows the name rule of the wire protocol: 1 to 128 characters from `A-Z a-z 0-9
//!   . _ -`, the first a letter or digit.
//! - `lander`, `runner` and `server` are reserved service names. A token for one has role
//!   `service`; every other token has role `agent`; a token that mixes the two is refused.
//!
//! # Order of checks
//!
//! Size (over 1 KiB is refused before it is decoded), shape (two base64url halves, claims that
//! parse), version, `kid` known, signature, audience, tenant, repository, agent name, role,
//! expiry. Each has its own fixed refusal reason, so a reason can be logged and never carries the
//! token. The `kid` and the version are read from the claims before the signature is checked,
//! because they pick the key; nothing else in them is read until it is.
//!
//! There is no deny-list: a token is valid until it expires or its `kid` is removed from the key
//! directory and the server restarted.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use data_encoding::BASE64URL_NOPAD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use tessel_coordinator::protocol::AgentId;
use tessel_coordinator::shell::is_agent_id;

use crate::id::{IdError, Name, NameError, RepoId};
use crate::keys::{self, KeyError};
use crate::ws::{Refusal, Verifier};

/// The only token version this verifier accepts.
pub const TOKEN_VERSION: u32 = 1;

/// The longest token accepted. A token for the longest agent id and the longest names is about
/// 700 bytes; anything over 1 KiB is refused before it is decoded.
pub const MAX_TOKEN_BYTES: usize = 1024;

/// The service that may tell the server the trunk moved.
pub const LANDER: &str = "lander";

const RESERVED_SERVICES: [&str; 3] = [LANDER, "runner", "server"];

const BEARER_PREFIX: &str = "Bearer ";

/// Whether `name` is one of the reserved service names.
pub fn is_reserved_service(name: &str) -> bool {
    RESERVED_SERVICES.contains(&name)
}

/// What a token's holder is: an agent working in a repository, or one of the server's services.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Agent,
    Service,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    v: u32,
    kid: String,
    aud: String,
    tenant: String,
    repo: String,
    agent: String,
    role: Role,
    exp_ms: u64,
}

fn refuse(reason: &'static str) -> Refusal {
    Refusal { reason }
}

/// Milliseconds since the Unix epoch. A clock set before 1970 reads as the far future, so every
/// token is expired rather than valid.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The token in an `Authorization` header value, if it is `Bearer <token>` with a token.
pub fn bearer_token(header: Option<&str>) -> Option<&str> {
    header
        .and_then(|value| value.strip_prefix(BEARER_PREFIX))
        .filter(|token| !token.is_empty())
}

struct Parsed<'a> {
    payload: &'a str,
    signature: Signature,
    claims: Claims,
}

fn parse(token: &str) -> Result<Parsed<'_>, Refusal> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(refuse("token is too long"));
    }
    let malformed = || refuse("token is malformed");
    let (payload, signature) = token.split_once('.').ok_or_else(malformed)?;
    let signature = BASE64URL_NOPAD
        .decode(signature.as_bytes())
        .map_err(|_| malformed())?;
    let signature = Signature::from_slice(&signature).map_err(|_| malformed())?;
    let json = BASE64URL_NOPAD
        .decode(payload.as_bytes())
        .map_err(|_| malformed())?;
    let claims = serde_json::from_slice(&json).map_err(|_| malformed())?;
    Ok(Parsed {
        payload,
        signature,
        claims,
    })
}

impl Claims {
    fn check_scope(&self, audience: &str, repo: &RepoId) -> Result<(), Refusal> {
        if self.aud != audience {
            return Err(refuse("token is for another audience"));
        }
        if self.tenant != repo.tenant() {
            return Err(refuse("token is for another tenant"));
        }
        if self.repo != repo.repo() {
            return Err(refuse("token is for another repository"));
        }
        Ok(())
    }

    fn check_subject(&self) -> Result<(), Refusal> {
        if !is_agent_id(&self.agent) {
            return Err(refuse("token names an invalid agent"));
        }
        match (self.role, is_reserved_service(&self.agent)) {
            (Role::Agent, false) | (Role::Service, true) => Ok(()),
            (Role::Agent, true) => Err(refuse("an agent token names a reserved service")),
            (Role::Service, false) => Err(refuse("a service token names no reserved service")),
        }
    }
}

/// Checks tokens against the public keys of a key directory, for one audience.
pub struct Ed25519Verifier {
    audience: String,
    keys: BTreeMap<String, VerifyingKey>,
}

impl Ed25519Verifier {
    /// Load every `.pub` key of `dir`. Fails if the audience or any key is not valid, or if there
    /// is no key.
    pub fn load(dir: &Path, audience: &str) -> Result<Self, KeyError> {
        let keys = keys::load_public_keys(dir)?;
        Ok(Self {
            audience: audience.to_string(),
            keys,
        })
    }

    /// The key ids this verifier accepts, in order.
    pub fn kids(&self) -> Vec<&str> {
        self.keys.keys().map(String::as_str).collect()
    }

    /// The agent `token` proves, for `repo`, at `now_ms`.
    pub fn verify_at(&self, token: &str, repo: &RepoId, now_ms: u64) -> Result<AgentId, Refusal> {
        let Parsed {
            payload,
            signature,
            claims,
        } = parse(token)?;
        if claims.v != TOKEN_VERSION {
            return Err(refuse("token has an unsupported version"));
        }
        let key = self
            .keys
            .get(&claims.kid)
            .ok_or_else(|| refuse("token names an unknown key"))?;
        key.verify_strict(payload.as_bytes(), &signature)
            .map_err(|_| refuse("token signature is not valid"))?;
        claims.check_scope(&self.audience, repo)?;
        claims.check_subject()?;
        if claims.exp_ms <= now_ms {
            return Err(refuse("token is expired"));
        }
        Ok(AgentId(claims.agent))
    }
}

impl Verifier for Ed25519Verifier {
    fn verify(&self, token: &str, repo: &RepoId) -> Result<AgentId, Refusal> {
        self.verify_at(token, repo, now_ms())
    }
}

/// Why a token cannot be minted.
#[derive(Debug, thiserror::Error)]
pub enum MintError {
    #[error(transparent)]
    Id(#[from] IdError),
    #[error("audience: {0}")]
    Audience(NameError),
    #[error(
        "the agent name must be 1 to 128 characters of letters, digits, '.', '_' and '-', \
         starting with a letter or digit"
    )]
    Agent,
    #[error("{name:?} is a reserved service name; mint it with --service")]
    ReservedAgent { name: String },
    #[error("--service needs one of the reserved names: lander, runner, server")]
    NotAService,
    #[error("the lifetime must be more than zero")]
    NoLifetime,
    #[error("the claims could not be encoded: {0}")]
    Encode(serde_json::Error),
}

/// What a token says, before it is signed.
pub struct TokenRequest<'a> {
    pub kid: &'a str,
    pub audience: &'a str,
    pub tenant: &'a str,
    pub repo: &'a str,
    pub agent: &'a str,
    pub role: Role,
    pub ttl_ms: u64,
}

/// Sign a token for `request`, valid from `now_ms` for the lifetime it asks for.
pub fn mint(
    key: &SigningKey,
    request: &TokenRequest<'_>,
    now_ms: u64,
) -> Result<String, MintError> {
    let TokenRequest {
        kid,
        audience,
        tenant,
        repo,
        agent,
        role,
        ttl_ms,
    } = *request;
    RepoId::new(tenant, repo)?;
    Name::parse(audience).map_err(MintError::Audience)?;
    if !is_agent_id(agent) {
        return Err(MintError::Agent);
    }
    match (role, is_reserved_service(agent)) {
        (Role::Agent, false) | (Role::Service, true) => {}
        (Role::Agent, true) => {
            return Err(MintError::ReservedAgent {
                name: agent.to_string(),
            })
        }
        (Role::Service, false) => return Err(MintError::NotAService),
    }
    if ttl_ms == 0 {
        return Err(MintError::NoLifetime);
    }
    let claims = Claims {
        v: TOKEN_VERSION,
        kid: kid.to_string(),
        aud: audience.to_string(),
        tenant: tenant.to_string(),
        repo: repo.to_string(),
        agent: agent.to_string(),
        role,
        exp_ms: now_ms.saturating_add(ttl_ms),
    };
    sign(key, &claims)
}

fn sign(key: &SigningKey, claims: &Claims) -> Result<String, MintError> {
    let json = serde_json::to_vec(claims).map_err(MintError::Encode)?;
    let payload = BASE64URL_NOPAD.encode(&json);
    let signature = key.sign(payload.as_bytes());
    Ok(format!(
        "{payload}.{}",
        BASE64URL_NOPAD.encode(&signature.to_bytes())
    ))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use tessel_coordinator::shell::MAX_AGENT_ID_BYTES;

    use super::*;
    use crate::keys;

    const NOW: u64 = 1_000_000;
    const EXP: u64 = 2_000_000;
    const AUD: &str = "tessel-test";

    /// Ed25519 is deterministic, so a fixed seed and fixed claims give one fixed token. This one
    /// was also produced by the Python `cryptography` package, signing the payload text.
    const FIXED_SEED: [u8; 32] = [7; 32];
    const FIXED_TOKEN: &str = concat!(
        "eyJ2IjoxLCJraWQiOiJrMSIsImF1ZCI6InRlc3NlbC10ZXN0IiwidGVuYW50IjoiYWNtZSIsInJlcG8iOiJkZW1v",
        "IiwiYWdlbnQiOiJhZ2VudC0xIiwicm9sZSI6ImFnZW50IiwiZXhwX21zIjoyMDAwMDAwfQ.",
        "CHmMb-YzGBKKLYE6rbHT56LmM32mbf4cRqlgTHp9TcpRE536Q9M7TEkIua252-mSqssHp4OblHe_RDwr8NXgBg"
    );

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn verifier(kids: &[(&str, u8)]) -> Ed25519Verifier {
        Ed25519Verifier {
            audience: AUD.to_string(),
            keys: kids
                .iter()
                .map(|(kid, seed)| ((*kid).to_string(), key(*seed).verifying_key()))
                .collect(),
        }
    }

    fn id() -> RepoId {
        RepoId::new("acme", "demo").unwrap()
    }

    fn claims(agent: &str, exp_ms: u64) -> Claims {
        Claims {
            v: TOKEN_VERSION,
            kid: "k1".to_string(),
            aud: AUD.to_string(),
            tenant: "acme".to_string(),
            repo: "demo".to_string(),
            agent: agent.to_string(),
            role: if is_reserved_service(agent) {
                Role::Service
            } else {
                Role::Agent
            },
            exp_ms,
        }
    }

    fn signed(key: &SigningKey, claims: &Claims) -> String {
        sign(key, claims).unwrap()
    }

    fn valid() -> String {
        signed(&key(1), &claims("agent-1", EXP))
    }

    fn check(token: &str) -> Result<AgentId, Refusal> {
        verifier(&[("k1", 1)]).verify_at(token, &id(), NOW)
    }

    fn reason(token: &str) -> &'static str {
        check(token).unwrap_err().reason
    }

    /// A payload signed with the right key, for a payload the signer should never have written.
    fn sign_payload(key: &SigningKey, json: &str) -> String {
        let payload = BASE64URL_NOPAD.encode(json.as_bytes());
        let signature = key.sign(payload.as_bytes());
        format!(
            "{payload}.{}",
            BASE64URL_NOPAD.encode(&signature.to_bytes())
        )
    }

    #[test]
    fn a_valid_token_yields_its_agent() {
        assert_eq!(check(&valid()), Ok(AgentId("agent-1".to_string())));
    }

    #[test]
    fn a_fixed_seed_signs_a_fixed_token_that_verifies() {
        let token = signed(
            &SigningKey::from_bytes(&FIXED_SEED),
            &claims("agent-1", EXP),
        );
        assert_eq!(token, FIXED_TOKEN);
        assert_eq!(
            verifier(&[("k1", 7)]).verify_at(&token, &id(), NOW),
            Ok(AgentId("agent-1".to_string()))
        );
    }

    #[test]
    fn a_payload_changed_after_signing_is_refused() {
        let token = valid();
        let (_, signature) = token.split_once('.').unwrap();
        let forged = claims("admin", 9_999_999);
        let payload = BASE64URL_NOPAD.encode(&serde_json::to_vec(&forged).unwrap());
        assert_eq!(
            reason(&format!("{payload}.{signature}")),
            "token signature is not valid"
        );
    }

    #[test]
    fn a_changed_signature_is_refused() {
        let token = valid();
        let (payload, signature) = token.split_once('.').unwrap();
        let mut bytes = BASE64URL_NOPAD.decode(signature.as_bytes()).unwrap();
        bytes[0] ^= 1;
        let forged = format!("{payload}.{}", BASE64URL_NOPAD.encode(&bytes));
        assert_eq!(reason(&forged), "token signature is not valid");
    }

    #[test]
    fn a_token_signed_with_another_key_is_refused() {
        let token = signed(&key(2), &claims("agent-1", EXP));
        assert_eq!(reason(&token), "token signature is not valid");
    }

    #[test]
    fn a_token_is_valid_until_exp_ms_and_not_at_it() {
        let token = valid();
        let verifier = verifier(&[("k1", 1)]);
        assert!(verifier.verify_at(&token, &id(), EXP - 1).is_ok());
        for now in [EXP, EXP + 1] {
            assert_eq!(
                verifier.verify_at(&token, &id(), now).unwrap_err().reason,
                "token is expired"
            );
        }
    }

    #[test]
    fn a_token_for_another_tenant_repository_or_audience_is_refused() {
        let token = valid();
        let verifier = verifier(&[("k1", 1)]);
        for (tenant, repo, expected) in [
            ("other", "demo", "token is for another tenant"),
            ("acme", "other", "token is for another repository"),
        ] {
            let other = RepoId::new(tenant, repo).unwrap();
            let got = verifier.verify_at(&token, &other, NOW).unwrap_err().reason;
            assert_eq!(got, expected);
        }
        let mut claims = claims("agent-1", EXP);
        claims.aud = "elsewhere".to_string();
        assert_eq!(
            reason(&signed(&key(1), &claims)),
            "token is for another audience"
        );
    }

    #[test]
    fn another_version_is_refused() {
        for v in [0, 2] {
            let mut claims = claims("a1", EXP);
            claims.v = v;
            assert_eq!(
                reason(&signed(&key(1), &claims)),
                "token has an unsupported version"
            );
        }
    }

    #[test]
    fn a_signed_payload_that_is_not_the_claims_is_refused() {
        let ok = r#""kid":"k1","aud":"tessel-test","tenant":"acme","repo":"demo","agent":"a1""#;
        for json in [
            "not json".to_string(),
            format!(r#"{{"v":1,{ok},"role":"agent"}}"#),
            format!(r#"{{"v":1,{ok},"role":"agent","exp_ms":-1}}"#),
            format!(r#"{{"v":1,{ok},"role":"admin","exp_ms":9999999}}"#),
            format!(r#"{{"v":1,{ok},"role":"agent","exp_ms":9999999,"admin":true}}"#),
        ] {
            assert_eq!(
                reason(&sign_payload(&key(1), &json)),
                "token is malformed",
                "{json}"
            );
        }
        let garbled = format!("!!not-base64!!.{}", valid().split_once('.').unwrap().1);
        assert_eq!(reason(&garbled), "token is malformed");
    }

    #[test]
    fn a_signed_agent_name_that_cannot_be_an_agent_id_is_refused() {
        let long = "a".repeat(MAX_AGENT_ID_BYTES + 1);
        for agent in [
            "", "a b", "a\r\nb", "agent/1", "ägent", ".a", "-a", "_a", &long,
        ] {
            let token = signed(&key(1), &claims(agent, EXP));
            assert_eq!(reason(&token), "token names an invalid agent", "{agent:?}");
        }
        let at_limit = "a".repeat(MAX_AGENT_ID_BYTES);
        assert!(check(&signed(&key(1), &claims(&at_limit, EXP))).is_ok());
    }

    #[test]
    fn malformed_tokens_are_refused() {
        let token = valid();
        let (payload, signature) = token.split_once('.').unwrap();
        let cases = [
            ("no dot", payload.to_string()),
            ("extra dot", format!("{payload}.{signature}.x")),
            ("leading dot", format!(".{signature}")),
            ("empty signature", format!("{payload}.")),
            ("empty token", String::new()),
            ("padded signature", format!("{payload}.{signature}=")),
            (
                "a character outside the alphabet",
                format!("{payload}.+{}", &signature[1..]),
            ),
            ("non-base64 signature", format!("{payload}.!!!")),
            (
                "short signature",
                format!("{payload}.{}", &signature[..signature.len() - 4]),
            ),
        ];
        for (label, token) in cases {
            assert_eq!(reason(&token), "token is malformed", "{label}");
        }
    }

    #[test]
    fn an_oversized_token_is_refused_before_it_is_decoded() {
        let at_limit = format!("{}.x", "a".repeat(MAX_TOKEN_BYTES - 2));
        assert_eq!(reason(&at_limit), "token is malformed");
        let over = "a".repeat(MAX_TOKEN_BYTES + 1);
        assert_eq!(reason(&over), "token is too long");
    }

    #[test]
    fn the_header_form_yields_the_token() {
        let token = valid();
        let header = format!("Bearer {token}");
        assert_eq!(bearer_token(Some(&header)), Some(token.as_str()));
    }

    #[test]
    fn a_verifier_with_no_key_for_the_kid_refuses_everything() {
        let bare = verifier(&[]);
        assert_eq!(
            bare.verify_at(&valid(), &id(), NOW).unwrap_err().reason,
            "token names an unknown key"
        );
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
            assert_eq!(bearer_token(header), None, "{header:?}");
        }
    }

    #[test]
    fn an_oversized_header_is_refused() {
        let header = format!("Bearer {}", "a".repeat(64 * 1024));
        let token = bearer_token(Some(&header)).unwrap();
        assert_eq!(reason(token), "token is too long");
    }

    fn request(agent: &str, role: Role, ttl_ms: u64) -> TokenRequest<'_> {
        TokenRequest {
            kid: "k1",
            audience: AUD,
            tenant: "acme",
            repo: "demo",
            agent,
            role,
            ttl_ms,
        }
    }

    #[test]
    fn a_minted_token_verifies_until_its_lifetime_is_over() {
        let verifier = verifier(&[("k1", 1)]);
        for (agent, role) in [("a1", Role::Agent), ("lander", Role::Service)] {
            let token = mint(&key(1), &request(agent, role, 60_000), NOW).unwrap();
            assert_eq!(
                verifier.verify_at(&token, &id(), NOW + 59_999),
                Ok(AgentId(agent.to_string()))
            );
            let late = verifier.verify_at(&token, &id(), NOW + 60_000);
            assert_eq!(late.unwrap_err().reason, "token is expired");
        }
    }

    #[test]
    fn minting_refuses_what_the_verifier_would_refuse() {
        let refused = |request: TokenRequest<'_>| mint(&key(1), &request, NOW).unwrap_err();
        for name in ["lander", "runner", "server"] {
            let error = refused(request(name, Role::Agent, 1));
            assert!(matches!(error, MintError::ReservedAgent { .. }), "{name}");
        }
        let error = refused(request("a1", Role::Service, 1));
        assert!(matches!(error, MintError::NotAService), "{error}");
        for name in ["", "a b", "-a", "ägent"] {
            let error = refused(request(name, Role::Agent, 1));
            assert!(matches!(error, MintError::Agent), "{name:?}");
        }
        let error = refused(request("a1", Role::Agent, 0));
        assert!(matches!(error, MintError::NoLifetime), "{error}");
        let error = refused(TokenRequest {
            tenant: "Acme",
            ..request("a1", Role::Agent, 1)
        });
        assert!(matches!(error, MintError::Id(_)), "{error}");
        let error = refused(TokenRequest {
            audience: "Not A Name",
            ..request("a1", Role::Agent, 1)
        });
        assert!(matches!(error, MintError::Audience(_)), "{error}");
    }

    #[test]
    fn a_signed_token_whose_role_and_name_disagree_is_refused() {
        let mut agent_named_lander = claims("lander", EXP);
        agent_named_lander.role = Role::Agent;
        let mut service_named_a1 = claims("a1", EXP);
        service_named_a1.role = Role::Service;
        for (claims, expected) in [
            (
                agent_named_lander,
                "an agent token names a reserved service",
            ),
            (
                service_named_a1,
                "a service token names no reserved service",
            ),
        ] {
            assert_eq!(reason(&signed(&key(1), &claims)), expected);
        }
    }

    #[test]
    fn two_kids_in_the_directory_verify_both_and_a_removed_one_is_refused_on_the_next_load() {
        let dir = tempfile::tempdir().unwrap();
        keys::generate(dir.path(), "old").unwrap();
        keys::generate(dir.path(), "new").unwrap();
        let mint_with = |kid: &str| {
            let key = keys::load_signing_key(dir.path(), kid).unwrap();
            let request = TokenRequest {
                kid,
                ..request("a1", Role::Agent, 60_000)
            };
            mint(&key, &request, NOW).unwrap()
        };
        let (old, new) = (mint_with("old"), mint_with("new"));
        let both = Ed25519Verifier::load(dir.path(), AUD).unwrap();
        assert_eq!(both.kids(), ["new", "old"]);
        for token in [&old, &new] {
            assert!(both.verify_at(token, &id(), NOW).is_ok());
        }
        std::fs::remove_file(dir.path().join("old.pub")).unwrap();
        let rotated = Ed25519Verifier::load(dir.path(), AUD).unwrap();
        assert_eq!(
            rotated.verify_at(&old, &id(), NOW).unwrap_err().reason,
            "token names an unknown key"
        );
        assert!(rotated.verify_at(&new, &id(), NOW).is_ok());
    }

    proptest! {
        #[test]
        fn sign_then_verify_round_trips(
            agent in "[A-Za-z0-9][A-Za-z0-9._-]{0,127}",
            exp_ms in 1_u64..(1 << 53),
        ) {
            prop_assume!(!is_reserved_service(&agent));
            let token = signed(&key(1), &claims(&agent, exp_ms));
            let got = verifier(&[("k1", 1)]).verify_at(&token, &id(), exp_ms - 1);
            prop_assert_eq!(got, Ok(AgentId(agent)));
        }
    }

    proptest! {
        // Every case checks every byte of a token, so a few dozen cases cover the agent names.
        #![proptest_config(ProptestConfig::with_cases(24))]

        #[test]
        fn flipping_any_one_byte_of_a_token_fails(
            agent in "[A-Za-z0-9][A-Za-z0-9._-]{0,127}",
        ) {
            prop_assume!(!is_reserved_service(&agent));
            let token = signed(&key(1), &claims(&agent, EXP));
            for index in 0..token.len() {
                let mut bytes = token.clone().into_bytes();
                bytes[index] ^= 1;
                let flipped = String::from_utf8(bytes).unwrap();
                prop_assert!(check(&flipped).is_err(), "byte {index}");
            }
        }
    }
}

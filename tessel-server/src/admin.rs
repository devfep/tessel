//! The `admin` subcommands: write a key pair, mint a token, resolve a `Land` in doubt. The first
//! two work on the key directory the `[identity]` table names; the third works on a repository's
//! store, which the running server holds locked, so it is run with the server stopped. None of
//! them starts or talks to a server.

use tessel_coordinator::coordinator::NoLandInDoubt;
use tessel_coordinator::protocol::ClaimId;
use tessel_coordinator::shell::{self, LoadError};

use crate::config::{Config, IdentityConfig, RepositoryConfig};
use crate::id::{IdError, RepoId};
use crate::identity::{self, MintError, Role, TokenRequest};
use crate::keys::{self, KeyError};
use crate::store::{StoreDir, StoreError};

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
    #[error(
        "the configuration lists no repository {tenant}/{repo}; its repositories are: {listed}"
    )]
    NoRepository {
        tenant: String,
        repo: String,
        listed: String,
    },
    #[error(
        "the configuration lists {count} repositories; name one with --tenant and --repo ({listed})"
    )]
    WhichRepository { count: usize, listed: String },
    #[error(transparent)]
    Id(#[from] IdError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{path} holds no stored state for the repository; there is no Land to resolve")]
    NoState { path: String },
    #[error("the stored state of the repository cannot be loaded: {0}")]
    Load(LoadError),
    #[error("{0}; nothing was changed")]
    NothingInDoubt(#[from] NoLandInDoubt),
    #[error("cannot write the state the answer produced: {0}")]
    Serialize(#[from] serde_json::Error),
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

/// The repository `resolve` works on: the one named, or the only one the configuration lists.
fn repository_of<'a>(
    config: &'a Config,
    named: (Option<&str>, Option<&str>),
) -> Result<&'a RepositoryConfig, AdminError> {
    let listed = || {
        let names: Vec<String> = config
            .repositories
            .iter()
            .map(|repository| format!("{}/{}", repository.tenant, repository.name))
            .collect();
        names.join(", ")
    };
    match named {
        (Some(tenant), Some(repo)) => config
            .repositories
            .iter()
            .find(|repository| repository.tenant == tenant && repository.name == repo)
            .ok_or_else(|| AdminError::NoRepository {
                tenant: tenant.to_string(),
                repo: repo.to_string(),
                listed: listed(),
            }),
        (None, None) => match config.repositories.as_slice() {
            [only] => Ok(only),
            many => Err(AdminError::WhichRepository {
                count: many.len(),
                listed: listed(),
            }),
        },
        (Some(_), None) | (None, Some(_)) => Err(AdminError::WhichRepository {
            count: config.repositories.len(),
            listed: listed(),
        }),
    }
}

/// What `resolve` did, for the log line.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    pub repository: String,
    pub claim: u64,
    pub landed: bool,
    /// Events the answer appended to the repository's log.
    pub events: usize,
}

/// Settle the `Land` of `claim` that is in doubt, offline: take the store directory's lock (so a
/// running server refuses it), load the stored core, feed it the answer a `Reconcile` would have
/// given (`landed`: the head is on main; otherwise it is not and the claim goes back to `Prepare`)
/// and store the result with its events in one transaction. The core's rules still apply: it
/// refuses a claim with no `Land` in flight, and nothing is written then.
pub fn resolve(
    config: &Config,
    (tenant, repo): (Option<&str>, Option<&str>),
    (claim, landed): (u64, bool),
    now_ms: u64,
) -> Result<Resolved, AdminError> {
    let repository = repository_of(config, (tenant, repo))?;
    let id = RepoId::new(&repository.tenant, &repository.name)?;
    let dir = StoreDir::open(&config.store_dir)?;
    let file = dir.repo_file(&id);
    if !file.is_file() {
        return Err(AdminError::NoState {
            path: file.display().to_string(),
        });
    }
    let mut store = dir.open_repo(&id)?;
    let stored = store.load()?.ok_or_else(|| AdminError::NoState {
        path: file.display().to_string(),
    })?;
    let reviewers = shell::reviewers_or_none(repository.reviewers_list().as_deref(), |_| {});
    let sensitive = shell::sensitive_or_all(repository.sensitive_list().as_deref(), |_| {});
    let mut core = shell::load_core(
        Some(&stored),
        Some(&repository.run),
        None,
        reviewers,
        sensitive,
    )
    .map_err(AdminError::Load)?;
    let effects = core.resolve_doubt(ClaimId(claim), landed, now_ms)?;
    let (events, _unsent) = shell::split_effects(effects);
    let entries = shell::persist_entries(&core, &events)?;
    store.commit(&entries)?;
    Ok(Resolved {
        repository: id.to_string(),
        claim,
        landed,
        events: events.len(),
    })
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
mod resolve_tests {
    use std::path::Path;

    use tessel_coordinator::coordinator::Effect;
    use tessel_coordinator::merge::MergeOutcome;
    use tessel_coordinator::protocol::{
        AgentId, ClientMsg, CommitId, DecisionRecord, EventKind, Intent, Mode, OnConflict,
        RequestId, Scope, ScopeClaim, ServerMsg,
    };

    use super::*;

    const OLD: &str = "dddddddddddddddddddddddddddddddddddddddd";
    const HEAD: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    fn write_config(dir: &Path, repositories: &[(&str, &str)]) -> Config {
        let mut text = format!(
            "listen = \"127.0.0.1:0\"\nstore_dir = {:?}\n",
            dir.join("stores").display().to_string()
        );
        for (tenant, name) in repositories {
            let table =
                format!("\n[[repository]]\ntenant = {tenant:?}\nname = {name:?}\nrun = \"t\"\n");
            text.push_str(&table);
        }
        let path = dir.join("tessel-server.toml");
        std::fs::write(&path, text).unwrap();
        Config::load(&path).unwrap()
    }

    /// A stored core whose claim 1 has its `Land` out when the server stops (`answered`: the
    /// `Land` came back `Unknown`; otherwise it was cut off), and its events.
    fn store_with_a_land_in_doubt(config: &Config, id: &RepoId) -> u64 {
        let mut core = shell::load_core(None, Some("t"), None, Vec::new(), Vec::new()).unwrap();
        let who = AgentId("a1".to_string());
        let scope = ScopeClaim {
            scope: Scope::File {
                path: "x.rs".to_string(),
            },
            mode: Mode::EditBody,
        };
        let claim = ClientMsg::Claim {
            req: RequestId(1),
            intent: Intent {
                summary: "s".to_string(),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes: vec![scope.clone()],
            on_conflict: OnConflict::Fail,
        };
        let mut effects = core.handle(&who, claim, 1_000);
        let granted = effects.iter().find_map(|effect| match effect {
            Effect::Reply(ServerMsg::Granted { claim, fence, .. }) => Some((*claim, *fence)),
            Effect::Reply(_) | Effect::Notify { .. } | Effect::Log(_) => None,
        });
        let (claim, fence) = granted.expect("granted");
        let submit = ClientMsg::Submit {
            req: RequestId(2),
            claim,
            fence,
            fork_commit: CommitId("ab".repeat(20)),
            touched: vec![scope],
            decisions: DecisionRecord {
                evidence: vec!["tests passed".to_string()],
                ..DecisionRecord::default()
            },
        };
        effects.extend(core.handle(&who, submit, 1_000));
        let prepare = core.begin_merge(1_000).expect("a Prepare");
        let prepared = MergeOutcome::Prepared {
            base: CommitId(OLD.to_string()),
            head: CommitId(HEAD.to_string()),
        };
        effects.extend(core.merge_outcome(prepare.id, &prepared, 1_001));
        let test = core.begin_merge(1_001).expect("a Test");
        effects.extend(core.merge_outcome(test.id, &MergeOutcome::Tested {}, 1_002));
        let land = core.begin_merge(1_002).expect("a Land");
        effects.extend(core.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, 1_003));
        let (events, _) = shell::split_effects(effects);
        let entries = shell::persist_entries(&core, &events).unwrap();
        let dir = StoreDir::open(&config.store_dir).unwrap();
        dir.open_repo(id).unwrap().commit(&entries).unwrap();
        claim.0
    }

    fn kinds_of(config: &Config, id: &RepoId) -> Vec<EventKind> {
        let dir = StoreDir::open(&config.store_dir).unwrap();
        let store = dir.open_repo(id).unwrap();
        let events = store.read_events(0, 1_000).unwrap();
        events.into_iter().map(|event| event.kind).collect()
    }

    fn core_of(config: &Config, id: &RepoId) -> serde_json::Value {
        let dir = StoreDir::open(&config.store_dir).unwrap();
        let stored = dir.open_repo(id).unwrap().load().unwrap().unwrap();
        serde_json::from_str(&stored).unwrap()
    }

    fn setup() -> (tempfile::TempDir, Config, RepoId, u64) {
        let dir = tempfile::tempdir().unwrap();
        let config = write_config(dir.path(), &[("acme", "demo")]);
        let id = RepoId::new("acme", "demo").unwrap();
        let claim = store_with_a_land_in_doubt(&config, &id);
        (dir, config, id, claim)
    }

    #[test]
    fn landed_merges_the_claim_and_stores_the_event() {
        let (_dir, config, id, claim) = setup();
        let done = resolve(&config, (None, None), (claim, true), 5_000).unwrap();
        assert_eq!((done.landed, done.claim), (true, claim));
        assert!(done.events > 0);
        let kinds = kinds_of(&config, &id);
        let merged = kinds
            .iter()
            .filter(|kind| matches!(kind, EventKind::Merged { head, .. } if head.0 == HEAD))
            .count();
        assert_eq!(merged, 1, "{kinds:?}");
        let core = core_of(&config, &id);
        assert!(core["merge_in_flight"].is_null());
        assert!(core["claims"].as_object().unwrap().is_empty());
        assert_eq!(core["head"], HEAD);
    }

    #[test]
    fn not_landed_sends_the_claim_back_to_prepare_and_keeps_it() {
        let (_dir, config, id, claim) = setup();
        resolve(&config, (None, None), (claim, false), 5_000).unwrap();
        let kinds = kinds_of(&config, &id);
        assert!(!kinds
            .iter()
            .any(|kind| matches!(kind, EventKind::Merged { .. })));
        let core = core_of(&config, &id);
        assert!(core["merge_in_flight"].is_null(), "the doubt is over");
        let work = &core["claims"][claim.to_string()]["work"];
        assert_eq!(work["in_doubt"], false);
        assert_eq!(work["infra_failures"], 0);
        assert_eq!(work["moved"], 0, "nobody said main moved, and no attempt");
    }

    #[test]
    fn a_claim_with_no_land_in_doubt_is_refused_and_nothing_is_written() {
        let (_dir, config, id, claim) = setup();
        let before = core_of(&config, &id);
        let events = kinds_of(&config, &id).len();
        let wrong = resolve(&config, (None, None), (claim + 1, true), 5_000).unwrap_err();
        assert_eq!(
            wrong.to_string(),
            format!(
                "claim {} has no Land in doubt; nothing was changed",
                claim + 1
            )
        );
        assert_eq!(core_of(&config, &id), before);
        assert_eq!(kinds_of(&config, &id).len(), events);
        resolve(&config, (None, None), (claim, true), 5_000).unwrap();
        let again = resolve(&config, (None, None), (claim, true), 5_001).unwrap_err();
        assert!(matches!(again, AdminError::NothingInDoubt(_)), "{again}");
    }

    #[test]
    fn a_store_held_by_a_running_server_is_refused_and_untouched() {
        let (_dir, config, id, claim) = setup();
        let before = core_of(&config, &id);
        let held = StoreDir::open(&config.store_dir).unwrap();
        let refused = resolve(&config, (None, None), (claim, true), 5_000).unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("is in use by tessel-server pid"),
            "{refused}"
        );
        drop(held);
        assert_eq!(core_of(&config, &id), before);
    }

    #[test]
    fn a_repository_is_named_when_there_are_several_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let config = write_config(dir.path(), &[("acme", "one"), ("acme", "two")]);
        let which = resolve(&config, (None, None), (1, true), 1).unwrap_err();
        assert!(
            matches!(which, AdminError::WhichRepository { count: 2, .. }),
            "{which}"
        );
        let missing = resolve(&config, (Some("acme"), Some("three")), (1, true), 1).unwrap_err();
        assert!(
            missing.to_string().contains("acme/one, acme/two"),
            "{missing}"
        );
        let empty = resolve(&config, (Some("acme"), Some("one")), (1, true), 1).unwrap_err();
        assert!(matches!(empty, AdminError::NoState { .. }), "{empty}");
        assert!(
            !dir.path().join("stores/acme/one.sqlite").exists(),
            "resolve never creates a store"
        );
    }
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

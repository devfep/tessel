//! A store as a stopped server leaves it with a `Land` in doubt, for the tests of `admin resolve`.

#![expect(
    clippy::expect_used,
    reason = "a failed expect is a failed test; the helpers are not #[test] functions"
)]

use std::path::Path;

use tessel_coordinator::coordinator::Effect;
use tessel_coordinator::merge::MergeOutcome;
use tessel_coordinator::protocol::{
    AgentId, ClientMsg, CommitId, DecisionRecord, Intent, Mode, OnConflict, RequestId, Scope,
    ScopeClaim, ServerMsg,
};
use tessel_coordinator::shell;
use tessel_server::id::RepoId;
use tessel_server::store::StoreDir;

/// What `store_a_land_in_doubt` left in the store: the claim whose `Land` is out, and the base
/// and head that `Land` journaled.
pub struct LandInDoubt {
    pub claim: u64,
    pub base: String,
    pub head: String,
}

/// Writes, as a stopped server would have left it, a store whose claim 1 was submitted, prepared
/// and tested, and whose `Land` came back `Unknown`: the merge is in flight and the claim is in
/// doubt. `with_waiter` also queues agent `a2` behind it for the same file.
pub fn store_a_land_in_doubt(
    store_dir: &Path,
    (tenant, repo): (&str, &str),
    with_waiter: bool,
) -> LandInDoubt {
    let base = "d".repeat(40);
    let head = "e".repeat(40);
    let mut core =
        shell::load_core(None, Some("test"), None, Vec::new(), Vec::new()).expect("a fresh core");
    let who = AgentId("a1".to_string());
    let scope = ScopeClaim {
        scope: Scope::File {
            path: "x.rs".to_string(),
        },
        mode: Mode::EditBody,
    };
    let intent = Intent {
        summary: "s".to_string(),
        task_ref: None,
        assumptions: Vec::new(),
    };
    let claim = ClientMsg::Claim {
        req: RequestId(1),
        intent: intent.clone(),
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
        touched: vec![scope.clone()],
        decisions: DecisionRecord {
            evidence: vec!["tests passed".to_string()],
            ..DecisionRecord::default()
        },
    };
    effects.extend(core.handle(&who, submit, 1_000));
    if with_waiter {
        let waiter = ClientMsg::Claim {
            req: RequestId(3),
            intent,
            scopes: vec![scope],
            on_conflict: OnConflict::Wait,
        };
        effects.extend(core.handle(&AgentId("a2".to_string()), waiter, 1_000));
    }
    let prepare = core.begin_merge(1_000).expect("a Prepare");
    let prepared = MergeOutcome::Prepared {
        base: CommitId(base.clone()),
        head: CommitId(head.clone()),
    };
    effects.extend(core.merge_outcome(prepare.id, &prepared, 1_001));
    let test = core.begin_merge(1_001).expect("a Test");
    effects.extend(core.merge_outcome(test.id, &MergeOutcome::Tested {}, 1_002));
    let land = core.begin_merge(1_002).expect("a Land");
    effects.extend(core.merge_outcome(land.id, &MergeOutcome::ServiceUnavailable, 1_003));
    let (events, _) = shell::split_effects(effects);
    let entries = shell::persist_entries(&core, &events).expect("the core serialises");
    let id = RepoId::new(tenant, repo).expect("a repository id");
    let dir = StoreDir::open(store_dir).expect("the store directory opens");
    dir.open_repo(&id)
        .expect("the store opens")
        .commit(&entries)
        .expect("the store takes the state");
    LandInDoubt {
        claim: claim.0,
        base,
        head,
    }
}

//! The loaded core and the stored size the soft limit judges growth against (R14 to R18, R50).
//!
//! Nothing here stores or sends: `apply` runs a step on the core and says what to store, and it
//! drops the core whenever the core has moved on from storage and the step's result will not be
//! stored (the next call reloads).

use crate::coordinator::{Coordinator as Core, Effect, HeadRead};
use crate::host::HostConfig;
use crate::protocol::{AgentId, Event};
use crate::shell::{self, LoadError, Outbound, StoreDecision, StoredSize, Work};

/// What this instance is waiting on, for the wake the core computes (R16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    pub merging: bool,
    pub verifying: bool,
}

/// One step's result, ready to be stored and then sent.
pub struct Applied {
    /// (key, JSON) pairs for one transaction: the core state, then each event.
    pub entries: Vec<(String, String)>,
    pub events: Vec<Event>,
    pub outbound: Vec<Outbound>,
    pub next_wake_ms: Option<u64>,
}

/// What `apply` made of a step: something to store and send, or a client call refused because it
/// would grow the state too far.
pub enum Prepared {
    Ready(Applied),
    Refused,
}

/// Why a step could not be applied: the operation that failed and why.
#[derive(Debug, PartialEq, Eq)]
pub struct ApplyError {
    pub operation: &'static str,
    pub cause: String,
}

type Entries = Vec<(String, String)>;

#[derive(Default)]
pub struct CoreSlot {
    core: Option<Core>,
    stored: StoredSize,
}

impl CoreSlot {
    pub fn is_loaded(&self) -> bool {
        self.core.is_some()
    }

    /// Keep `core`, built from `stored`, unless a core is already loaded: a load that finishes
    /// second changes nothing, and the stored size is set on the first load only (R15, R50).
    pub fn install(&mut self, core: Core, stored: Option<&str>) {
        if self.core.is_none() {
            self.core = Some(core);
            self.stored = StoredSize::on_load(stored);
        }
    }

    /// The core has moved on from storage, or storage may have: the next use reloads.
    pub fn drop_core(&mut self) {
        self.core = None;
    }

    pub fn has_due_expiry(&self, now_ms: u64) -> bool {
        self.core
            .as_ref()
            .is_some_and(|core| core.has_due_expiry(now_ms))
    }

    pub fn has_merge_in_flight(&self) -> bool {
        self.core
            .as_ref()
            .is_some_and(|core| core.has_merge_in_flight())
    }

    pub fn has_verification_in_flight(&self) -> bool {
        self.core
            .as_ref()
            .is_some_and(|core| core.has_verification_in_flight())
    }

    /// The head read the core offers, if any (R22).
    pub fn begin_head_read(&self) -> Option<HeadRead> {
        self.core.as_ref().and_then(|core| core.begin_head_read())
    }

    /// The wake the core wants, with this instance's waits (R16).
    pub fn next_wake_ms(&self, waits: Waits, now_ms: u64) -> Option<u64> {
        let core = self.core.as_ref()?;
        core.next_wake_ms(waits.merging, waits.verifying, now_ms)
    }

    pub fn has_queued_request(&self, agent: &AgentId) -> bool {
        self.core
            .as_ref()
            .is_some_and(|core| core.has_queued_request(agent))
    }

    /// The size of the stored state after `entries` were written (R50).
    pub fn written(&mut self, entries: &[(String, String)]) {
        self.stored = self.stored.on_write(entries);
    }

    /// Run `step` on the core and serialize the result. Nothing is stored or sent yet. If
    /// serialization fails the core is dropped. A client call that would grow the state or log an
    /// event past the soft limit is refused and the core is dropped too. Any work past the hard
    /// limit is an error and drops the core. The wake is computed from the core as the step left
    /// it, with this instance's waits (R16).
    pub fn apply(
        &mut self,
        work: Work,
        waits: Waits,
        now_ms: u64,
        step: impl FnOnce(&mut Core) -> Vec<Effect>,
    ) -> Result<Prepared, ApplyError> {
        self.apply_serialized(work, waits, now_ms, step, shell::persist_entries)
    }

    fn apply_serialized(
        &mut self,
        work: Work,
        waits: Waits,
        now_ms: u64,
        step: impl FnOnce(&mut Core) -> Vec<Effect>,
        serialize: impl FnOnce(&Core, &[Event]) -> Result<Entries, serde_json::Error>,
    ) -> Result<Prepared, ApplyError> {
        let Some(core) = self.core.as_mut() else {
            return Err(ApplyError {
                operation: "apply",
                cause: "core is not loaded".to_string(),
            });
        };
        let effects = step(core);
        let next_wake_ms = core.next_wake_ms(waits.merging, waits.verifying, now_ms);
        let (events, outbound) = shell::split_effects(effects);
        let entries = match serialize(core, &events) {
            Ok(entries) => entries,
            Err(e) => {
                self.core = None;
                return Err(ApplyError {
                    operation: "serialize state",
                    cause: e.to_string(),
                });
            }
        };
        match self.stored.judge(work, &entries) {
            StoreDecision::Store => Ok(Prepared::Ready(Applied {
                entries,
                events,
                outbound,
                next_wake_ms,
            })),
            StoreDecision::Refuse => {
                self.core = None;
                Ok(Prepared::Refused)
            }
            StoreDecision::OverHard => {
                self.core = None;
                Err(ApplyError {
                    operation: "store state",
                    cause: "state is over the storage limit".to_string(),
                })
            }
        }
    }
}

/// Build the core from what `Output::Load` read (R14). Fails loudly on corrupt state or a bad
/// config and never starts empty over stored data. `run` and `shadow_enabled` matter only when
/// nothing is stored yet; the reviewers and the sensitive paths are applied on every load. A bad
/// reviewers list leaves nobody able to review and a bad sensitive list holds every submission:
/// each is reported in `reports`.
pub fn build_core(
    config: &HostConfig,
    stored: Option<&str>,
    reports: &mut Vec<String>,
) -> Result<Core, LoadError> {
    let repo = &config.repo;
    let reviewers = shell::reviewers_or_none(config.reviewers.as_deref(), |e| {
        reports.push(format!("coordinator {repo}: {e}; nobody can review"));
    });
    let sensitive = shell::sensitive_or_all(config.sensitive.as_deref(), |e| {
        reports.push(format!("coordinator {repo}: {e}; every submission is held"));
    });
    let mut core = shell::load_core(
        stored,
        config.run.as_deref(),
        config.shadow_enabled.as_deref(),
        reviewers,
        sensitive,
    )?;
    if let Some(range) = config.protocol_range {
        core.set_protocol_range(range);
    }
    Ok(core)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAITS: Waits = Waits {
        merging: false,
        verifying: false,
    };

    fn loaded() -> CoreSlot {
        let core = shell::load_core(None, Some("t"), None, Vec::new(), Vec::new()).unwrap();
        let mut slot = CoreSlot::default();
        slot.install(core, None);
        slot
    }

    fn unserializable(_: &Core, _: &[Event]) -> Result<Entries, serde_json::Error> {
        Err(serde::ser::Error::custom("no"))
    }

    #[test]
    fn host_a_state_that_cannot_be_serialised_drops_the_core() {
        let mut slot = loaded();
        let result = slot.apply_serialized(Work::Plain, WAITS, 1, |_| Vec::new(), unserializable);
        let error = result.err().expect("serialization failed");
        assert_eq!(error.operation, "serialize state");
        assert!(!slot.is_loaded(), "the core is ahead of storage");
    }

    fn lease_of(config: &HostConfig, stored: Option<&str>) -> Result<u64, shell::LoadError> {
        let mut reports = Vec::new();
        let core = build_core(config, stored, &mut reports)?;
        let state = serde_json::to_value(&core).unwrap();
        Ok(state["config"]["lease_ms"].as_u64().unwrap())
    }

    fn config_with(lease_ms: Option<u64>) -> HostConfig {
        HostConfig {
            repo: "r".to_string(),
            run: Some("t".to_string()),
            lease_ms,
            ..HostConfig::default()
        }
    }

    #[test]
    fn a_repo_with_no_lease_setting_keeps_the_thirty_second_lease() {
        assert_eq!(lease_of(&config_with(None), None), Ok(shell::LEASE_MS));
    }

    #[test]
    fn the_lease_setting_sets_the_lease_of_a_new_repo_and_of_a_stored_one() {
        assert_eq!(lease_of(&config_with(Some(5_000)), None), Ok(5_000));
        let stored = serde_json::to_string(&loaded_core()).unwrap();
        assert_eq!(
            lease_of(&config_with(Some(5_000)), Some(&stored)),
            Ok(5_000)
        );
        assert_eq!(
            lease_of(&config_with(None), Some(&stored)),
            Ok(shell::LEASE_MS)
        );
    }

    fn loaded_core() -> Core {
        shell::load_core(None, Some("t"), None, Vec::new(), Vec::new()).unwrap()
    }

    #[test]
    fn a_lease_below_a_second_or_above_ten_minutes_is_refused() {
        for lease in [0, 999, 600_001] {
            let result = lease_of(&config_with(Some(lease)), None);
            assert!(
                matches!(result, Err(shell::LoadError::InvalidConfig(_))),
                "{lease} ms was accepted: {result:?}"
            );
        }
        assert_eq!(lease_of(&config_with(Some(1_000)), None), Ok(1_000));
        assert_eq!(lease_of(&config_with(Some(600_000)), None), Ok(600_000));
    }

    #[test]
    fn a_step_on_an_unloaded_slot_is_an_error() {
        let mut slot = CoreSlot::default();
        let error = slot
            .apply(Work::Plain, WAITS, 1, |_| Vec::new())
            .err()
            .expect("no core");
        assert_eq!(error.operation, "apply");
    }
}

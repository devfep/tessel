# Roadmap

Task list for the work in `PLAN.md`, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope
and order; `docs/proposals/real-use.md` holds the requirements, invariants and done-checks this
file's tasks refer to. Only the orchestrator edits this file. The contest-era roadmap and build log
are in git history (`78bcb9a` and earlier).

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-17 evening.
**Ready for Felix to merge:** PR #19 (P0-CI; Opus review "With fixes" twice, all fixed but the
steward image, ruled out of scope; all six jobs green on GitHub, fast lane 1m9s cold) and PR #20
(P0-MQ-PROPTEST; Opus "Yes" after one fix pass; 19 mutants killed, no bugs found). After #19
merges, #20 merges `main` in so CI runs on it.
**Next to dispatch:** P0-DISPATCH-ID and P0-CLI-REPLAY-UNKNOWN-EVENT (disjoint files).
**Pending from Felix:** the spike organisation and App (P0-SPIKE); a ruleset making the six CI
jobs required on `main`.

## Phase 0: foundations

- [ ] **P0-CI** — GitHub Actions, every action pinned to a full SHA with its version in a comment,
  `persist-credentials: false`, clean under `actionlint` and `zizmor`. Fast lane (target under
  three minutes): `cargo fmt --check`; clippy `-D warnings` native and for `wasm32-unknown-unknown`
  with `--features runtime`; the core library's tests. Slow lane: the workspace through
  cargo-nextest; the steward's `pnpm install --frozen-lockfile`, typecheck, lint and tests;
  `cargo deny check advisories bans`. A pull request template with a field for the new test's
  failing output (CLAUDE.md rule 9). Known flakes the slow lane will meet are fixed or quarantined
  with a named task, never retried silently (SWARM-ONLOCAL-LOAD-2 below). Done: both lanes green
  on the task's pull request, with measured durations in the report.
- [ ] **P0-MQ-PROPTEST** — property tests for the merge queue (`src/coordinator/merging.rs`)
  interleaving every `MergeOutcome` (`Merged`, `Conflict`, `TestsFailed`, `ServiceUnavailable`,
  `MainMoved`, `AlreadyMerged`), cut-off recovery and the watchdog with claims, leases, review
  holds and races. Checks: at most one `Merged` per claim; a held submission is never dispatched;
  attempt counts and bounds (`MAX_MAIN_MOVED`, `MAX_INFRA_RETRIES`) hold; state round-trips
  through serde at every step; the queue never wedges (if a submission is eligible, `begin_merge`
  or the next wake dispatches it). Tests only, no behaviour change; a property that exposes a
  bug is recorded as a new task, not fixed in this lane. Done: green at 10,000 cases, with the
  run time reported.
- [ ] **P0-DISPATCH-ID** — a persisted, monotonic dispatch id on every merge and verification
  dispatch, carried by the shell's call, and matched in `merge_outcome` and the verification
  outcome; a stale answer changes nothing (proposal, correctness requirement 5 and I7). Done: a
  property test showing a stale answer is ignored, failing before the change.
- [ ] **P0-LANDED-COMMIT** — `already_merged` no longer becomes `Landed { head: base }`
  (`src/merge.rs`); the outcome carries the landed commit when known, and no verification or
  trial is queued until it is (proposal, fixes table). Done: a test that no verification compares
  main with itself, failing before the change.
- [ ] **P0-CLI-REPLAY-UNKNOWN-EVENT** — carried over: `tessel-cli/src/daemon.rs` aborts replay or
  drops the socket on one event it cannot parse, so any new event kind breaks older daemons.
  Decode the envelope tolerantly (seq plus a possibly unknown kind) and keep replaying. Must land
  before P0-VERIFY-SLOT adds an event kind. Done: a test replaying a log with an unknown kind.
- [ ] **P0-VERIFY-SLOT** — after N merges (setting, default 4) the next merge waits if the head
  verification is ready and not in backoff; `begin_merge` respects a verification in flight;
  dropped verifications are logged as a new `VerificationDropped` event and counted in `Summary`
  (proposal, fixes table, I12). Needs P0-CLI-REPLAY-UNKNOWN-EVENT. Done: a property test that a
  ready verification runs within N merges under continuous merge load.
- [ ] **P0-DOC-MAIN-MOVED** — from the P0-MQ-PROPTEST review: `src/merge.rs` documents
  `MAX_MAIN_MOVED` as answers "in a row", but the code counts them per submission (an infra failure
  does not reset the count, which is the safer rule), and `merging.rs` says "up to MAX_MAIN_MOVED
  times" where there are MAX_MAIN_MOVED - 1 re-dispatches. Fix the docs to match the code. Small.
- [ ] **P0-FLAKE-WALLCLOCK** — at a local load average near 240, two tests fail on wall-clock
  checks in code no Phase 0 change touched: `tessel-swarm`
  `off::tests::agents_work_at_the_same_time` ("branches were built one after another") and
  `tessel-cli` `heartbeats_keep_a_claim_alive_past_its_lease` (once; passed on rerun). Both pass on GitHub. Replace the timing assertions with ordering or
  explicit signals, or move them behind the daemon clock seam planned for Phase 1 (proposal,
  "Flake control"); never add retries.
- [ ] **P0-UNVERIFIABLE-LANDING** — from the P0-LANDED-COMMIT review: when a recovered merge
  lands with no known baseline, its assumption verifications are not queued and nothing records
  that they were skipped. Make it visible (rule 7, invariant 10), which needs a protocol decision
  from Felix: a new `VerificationDropped` reason or a separate counter.
- [ ] **COORD-HEAD-FROM-AGENT** — from the same review: until the first landing or head read,
  the coordinator's `head` is the base the first agent declared in its `Hello`
  (`src/coordinator.rs`), and it becomes the baseline of the first dispatch unchecked. Phase 1:
  read the trunk head from the git host when the repo loads, before any dispatch.
- [ ] **P0-SPIKE** — the GitHub spike in the proposal ("The GitHub adapter", spike paragraph):
  does a fork pull request's check satisfy a required-checks ruleset for a direct `updateRefs` of
  that commit; is a stale `beforeOid` refused with the App as bypass actor; check and webhook
  latencies. Done: a written answer in `docs/proposals/` and a script that reproduces it. Blocked
  on Felix (test organisation and App).

## Carried over from the contest build

Kept, and scheduled where they fit:

- [x] **SWARM-ONLOCAL-LOAD-2** — already done before Phase 0 (the test asserts the order of the two
  `Submitted` events, `tessel-swarm/tests/on_local.rs`); the contest roadmap had left the box open.
- [x] **CLI-GITHOOK-REINSTALL** — already done before Phase 0 (`cdb445c`: a different hook moves the
  old backup to a dated name and is chained; tests in `tessel-cli/tests/githook.rs`); the contest
  roadmap had left the box open.
- [x] **SWARM-PUSH-PHRASES** — already done before Phase 0 (all four phrases are in the transient
  set with tests, `tessel-swarm/src/git.rs`); the contest roadmap had left the box open.
- [ ] **COORD-UNDELIVERED-GRANT** — a `Granted` that fails to send closes the socket but leaves the
  claim held until its lease lapses. Phase 1, in the host (send failure handling).
- [x] **COORD-APPLY-LOADED** — already done before Phase 0 (`apply_loaded` reloads the core before
  applying a steward outcome, `src/runtime.rs`); the contest roadmap had left the box open.
- [ ] **COORD-WAKE-WHILE-VERIFYING** — from the P0-MQ-PROPTEST review: `runtime.rs` (~661)
  calls `next_wake_ms(true, false, ..)` even while this instance is verifying, which looks like an
  immediate-alarm loop during a verification. Phase 1: a host test (I4, "the wake is in the
  future") pins it when the orchestration moves.
- [x] **REVIEW-SENSITIVE** — already done before Phase 0 (`ae62618`: submissions touching paths in
  the `SENSITIVE` variable are held); the contest roadmap had left the box open.
- [ ] **REVIEW-NOTE** — a reviewer's note never reaches the agent. Needs a design (untrusted text,
  rule 3) and Felix's ruling. Phase 3.

Dropped, because the code they concern is deleted in Phase 1 and nothing runs it:
STEWARD-ACCESS-STALE-KEYS, GATE-PATH, GATE-OUTPUT.

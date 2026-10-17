# Roadmap

Task list for the work in `PLAN.md`, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope
and order; `docs/proposals/real-use.md` holds the requirements, invariants and done-checks this
file's tasks refer to. Only the orchestrator edits this file. The contest-era roadmap and build log
are in git history (`78bcb9a` and earlier).

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-17. Phase 0 started after Felix merged PR #16.
**Running:** P0-CI (lane A), P0-MQ-PROPTEST (lane B).
**Pending from Felix:** P0-SPIKE needs a GitHub test organisation (or a public test repository)
and permission to create a GitHub App in it; making CI checks required on `main` once P0-CI lands.

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
- [ ] **P0-SPIKE** — the GitHub spike in the proposal ("The GitHub adapter", spike paragraph):
  does a fork pull request's check satisfy a required-checks ruleset for a direct `updateRefs` of
  that commit; is a stale `beforeOid` refused with the App as bypass actor; check and webhook
  latencies. Done: a written answer in `docs/proposals/` and a script that reproduces it. Blocked
  on Felix (test organisation and App).

## Carried over from the contest build

Kept, and scheduled where they fit:

- [ ] **SWARM-ONLOCAL-LOAD-2** — `tessel-swarm/tests/on_local.rs`
  `a_shadow_submission_is_on_record_early_and_its_work_time_is_still_counted` fails under load: it
  compares two agents' claim-to-submit spans, which depend on uneven git timing. Assert the order
  of the two `Submitted` events instead, and that the shadow's result still counts `work_ms`. Also
  seen once: `local::tests::work_that_already_fails_on_main...` (passes alone). With P0-CI.
- [ ] **CLI-GITHOOK-REINSTALL** — `tessel-cli/src/githook.rs`: re-running `tessel hook install`
  after another tool overwrote the shim bails because `<hook>.pre-tessel` exists. Move the old
  backup aside with a date, chain the new hook, report both. Test first. Any free lane.
- [ ] **SWARM-PUSH-PHRASES** — `tessel-swarm/src/git.rs`: add "the remote end hung up
  unexpectedly", "unexpected disconnect while reading sideband packet", "connection timed out" and
  "empty reply from server" to the transient set, with tests. Any free lane.
- [ ] **COORD-UNDELIVERED-GRANT** — a `Granted` that fails to send closes the socket but leaves the
  claim held until its lease lapses. Phase 1, in the host (send failure handling).
- [ ] **COORD-APPLY-LOADED** — `runtime.rs` applies a steward outcome after an await without
  reloading the core. Phase 1: the host's one-outstanding-commit model removes it; a host test
  pins it.
- [ ] **REVIEW-SENSITIVE** — the sensitive-path review reason can never fire (no list is
  configured). Phase 3, with per-repo review configuration.
- [ ] **REVIEW-NOTE** — a reviewer's note never reaches the agent. Needs a design (untrusted text,
  rule 3) and Felix's ruling. Phase 3.

Dropped, because the code they concern is deleted in Phase 1 and nothing runs it:
STEWARD-ACCESS-STALE-KEYS, GATE-PATH, GATE-OUTPUT.

# Phase 1 plan: portable host, `tessel-server` and the Rust lander

Status: draft for Felix's review, 18 October 2026, revised after two adversarial reviews the same
day. It plans Phase 1 of `PLAN.md` without changing its scope or order (`CLAUDE.md`, rule 10);
where this plan would change them, it says so under "Proposed changes for Felix". Requirements
(C1 to C11, S1 to S6, O1 to O6) and invariants (I1 to I12) are those of
`docs/proposals/real-use.md`.

P0-UNVERIFIABLE-LANDING is decided (Felix delegated it) and implemented in `6c924d9`, under
review: two optional lists on `EventKind::Merged`, `unverified: Vec<UnverifiedAssumption>` and
`untried_shadows: Vec<ClaimId>`, both `#[serde(default, skip_serializing_if = "Vec::is_empty")]`,
and `Summary.verifications_unverifiable`. This plan builds on that shape and assumes it has merged
before any Phase 1 task touches `src/protocol.rs`, `src/coordinator*.rs` or `src/shell.rs`.

Phase 1 scope, from `PLAN.md`: `runtime.rs` moved step for step into the sans-IO host; the
simulator with invariants I1 to I12 and failpoints; `tessel-server` with SQLite, versioned state,
the protocol range, metrics, Ed25519 identity and token minting; the Rust lander and a local test
runner; minimal advisory mode; Cloudflare and TypeScript code deleted. About 10 to 12 weeks.

## Done-check for the phase

At the end of Phase 1, CI on `main` proves:

1. `cargo test --workspace` passes with the CLI and swarm suites driving the host (through
   `tessel-server` or the host's test driver), with no `worker` crate in `cargo tree`, no
   `wasm32-unknown-unknown` target in `rust-toolchain.toml`, and no `tessel-steward/` directory.
2. The simulator lane passes on every pull request with all twelve invariant checkers on, in both
   landing and advisory mode, and the nightly seed sweep has passed on seven nights in a row.
3. The `tessel-server` adapter suite passes: kill -9 loop with client transcripts checked, the
   store lock, a slow watcher, a panicking repo task, injected store errors, WebSocket framing and
   close codes, and the CLI of the commit before the protocol range field against the new server.
4. The lander suite passes: the steward's unit and real-git integration tests ported to Rust, the
   ancestry read-back, the isolation cases of S3, and a pinned-commit test of the deterministic
   rebase (C7).
5. The advisory-mode end-to-end test passes: a merge commit that contains a submitted commit
   releases its claim without raising `Summary.merges`; a squash of the same branch does not
   release it, and the claim is reported as held.
6. The per-pull-request mutation check on changed lines is required and green; the nightly
   proptest run at 10,000 cases is green.
7. Every Phase 1 row of the proposal's "Requirement to test" table names a test that exists,
   checked by the same script that checks the `runtime.rs` rules list (P1-RULES).
8. `cargo tree -p tessel-server` contains neither `tessel-lander` nor `git2` or `gix` (rule 4:
   the coordinator never runs git), and no source file reads a `FLY_` environment variable.

Outside CI, recorded in `docs/ROADMAP.md`: this repository was coordinated in advisory mode by a
hosted `tessel-server` for at least five merged pull requests, with at least one redeploy while a
submitted claim was held, that claim released by head sync afterwards, before the TypeScript
steward was deleted (decision 6; the Cloudflare Rust glue goes earlier, decision 13).

## Lanes and file ownership

At most two lanes build or test at once, and only on disjoint files (`docs/BUILD-PROTOCOL.md`,
section 5). Heavy runs (mutation, simulator sweeps, proptest at 10,000 cases, kill -9 loops) run
on GitHub runners only, never on the Mac.

| Lane | Owns |
|---|---|
| A, host and server | `src/host/**` (new), `src/sim/**` (new), `src/lib.rs`, `src/shell.rs`, `src/protocol.rs` (additive changes only), `src/coordinator.rs`, `src/coordinator/**` and `src/merge.rs` except in M3, `docs/reference/runtime-rules.md` (new), `tests/runtime_rules.rs` (new), `scripts/ci/check-named-tests.sh` (new), `tests/simulator.rs` (new), `tessel-cli/tests/support/mod.rs`, `tessel-cli/src/daemon.rs` (the range warning, M4), `tessel-swarm/src/local.rs`, `tessel-server/Cargo.toml` and `tessel-server/src/{main,repo,store,ws,metrics}.rs` (new; `main.rs` wires lane B's subcommands) |
| B, lander, merge path, CI and identity | `.github/workflows/**`, `.cargo/**` (new), `scripts/ci/**` (new) except the named-tests check, `tessel-lander/**` (new), in M3 `src/merge.rs`, `src/coordinator.rs` and `src/coordinator/**`, `tessel-server/src/{identity,admin}.rs` (new), `tessel-cli/src/init.rs`, `deploy/dogfood/**` (new), the deletions and doc rewrites of P1-DROP-WORKER (M3: `src/runtime.rs`, `src/store.rs`, `src/identity.rs`, `wrangler.toml`, `tessel-swarm/src/{live,guard,main}.rs`, `README.md`, `docs/how-tessel-works.md`, `docs/reference/coordinator.md`, `docs/configuration.md`, a proposed `CLAUDE.md` edit) and of `tessel-steward/` (M6) |

Shared files: the root `Cargo.toml` (workspace members, features) and `Cargo.lock`. A lane that
adds a member or a dependency does it in its own small commit, and the orchestrator does not run
two such commits at once. `.github/workflows/ci.yml` is lane B's, but lane A adds one step to it
in P1-RULES and P1-CI-SIM; whichever of two such pull requests merges second merges `main` first.
The core and `src/merge.rs` pass from lane A to lane B for M3 (the staged landing) and back when
P1-CORE-RECONCILE merges. M3's cross edits, in order:

1. P1-DROP-WORKER (lane B) deletes the Cloudflare Rust glue before any core API changes
   (decision 13).
2. P1-CORE-STAGED-LANDING, then P1-CORE-RECONCILE (lane B). P1-CORE-RECONCILE also makes its own
   edits in `src/host/calls.rs` (the `Reconcile` on `Loaded`, the flipped R24 test); lane A does
   not touch that file or `src/host/wake.rs` until it merges. Meanwhile lane A works in
   `src/sim/**` and `src/host/batch.rs` (P1-SIM-CORE, P1-PERSIST-RETRY).
3. P1-ONE-BATCH (lane A) runs after P1-CORE-RECONCILE merges, when the core is lane A's again,
   because it changes the core's `next_wake_ms` as well as `src/host/wake.rs`.

The merge-path tasks (P1-CORE-STAGED-LANDING, P1-CORE-RECONCILE, P1-LANDER-ANCESTRY) go to Felix
for review, as `PLAN.md` keeps merge-path changes with him. `src/runtime.rs` stays unchanged
through M2, so the reference for the move stays readable and the wasm lane keeps checking it.

## Milestones

Weeks are calendar weeks with two lanes; neighbouring milestones overlap by a week where one lane
finishes first. Each milestone ends with a check CI runs; the first two are planned to task level
below, the rest in outline, re-planned at each milestone boundary.

| Milestone | Weeks | Contents | Done-check (CI) |
|---|---|---|---|
| M1. Guardrails and the host's types | 1-2 | Mutation check per pull request; nightly job; `runtime.rs` rules list with in-place tests; the host's `Input`, `Output` and `Call` types; lander git layer | Mutation job required and green; nightly green once; rules check passes; host types round-trip; lander git tests pass |
| M2. The move | 3-5 | `runtime.rs` moved step for step into `src/host/`, quirks included; the CLI's fake coordinator rebuilt on the host; lander `prepare`, `land` and `trial` ported | No rules-list row left "after move"; CLI suite green on the host; ported steward tests green |
| M3. Simulator and the staged landing | 6-8 | Cloudflare Rust glue deleted first (P1-DROP-WORKER); simulator in two tasks, I1-I12 checkers, failpoints; the core's landing split into `Prepare`, journaled head, `Test`, `Land`; `Unknown` and `Reconcile`; ancestry read-back; persist retry; one batch per wake | Simulator lane required and green; each behaviour change flips its named test |
| M4. Host changes and `tessel-server` | 8-10 | Exact replay, half-lease persistence, two carried-over tasks; axum, one task per repo, SQLite store, store lock, `state_version` and migrations, protocol range; Ed25519 identity and token minting; local test runner | Adapter suite green; each host change flips its named test |
| M5. Lander behind the server; advisory mode | 10-11 | Lander and runner as separate binaries over a local authenticated channel; evidence records; metrics, logs, trace id; swarm local target rebuilt on the server; advisory head sync; reviewers on the server; hosted dogfood server | End-to-end landing on a local git remote; advisory end-to-end test; metrics contract test; `tessel review` approves through the server |
| M6. Dogfood, then delete | 11-12 | Five merged pull requests of this repo in advisory mode, one redeploy with a claim held across it; the TypeScript steward deleted; ruleset changed in the same step | Phase done-check above |

## M1 tasks

Order: P1-CI-MUTANTS and P1-RULES start together (the first builds nothing locally, so the two
lanes rule holds); P1-CI-NIGHTLY follows in lane B; P1-HOST-TYPES follows P1-RULES in lane A;
P1-LANDER-GIT starts in lane B once the CI tasks have merged.

The order asked for, (a) CI checks, (b) the rules list, (c) the host types, is kept, with three
changes and their reasons:

- (a) and (b) run at the same time. (a) is workflow files and is checked on GitHub, so it adds no
  local load; waiting for it would idle a lane for no gain.
- (a) is split into the per-pull-request check (P1-CI-MUTANTS) and the nightly job
  (P1-CI-NIGHTLY). The required check should not wait on the full-mutation baseline work. The
  simulator lane is not created in M1: there is nothing for it to run until P1-SIM-CORE, and an
  empty job would be a phantom check.
- (c) follows (b), not the other way round: the rules list is the checklist the types are
  reviewed against (every rule that lives only in `runtime.rs` must have an input or output that
  carries it).

The lander port is a parallel lane from M1, on separate files (`tessel-lander/**`). It is the
longest path to the deletion step, and it shares no files with the host.

- [ ] **P1-CI-MUTANTS** — Lane B. Files: `.github/workflows/ci.yml`, `.cargo/mutants.toml`.
  On `pull_request`: a matrix of N shard jobs, each `cargo mutants --in-diff <diff> --package
  tessel-coordinator --test-tool nextest --jobs 2 --shard k/N`, and one job named "Slow lane,
  mutants on changed lines" that `needs` the shards and fails unless each passed; that job is the
  required check, so the shard count can change without a ruleset edit. Each shard: checkout with
  `fetch-depth: 0` and `persist-credentials: false`; `taiki-e/install-action` (pinned SHA, as
  today) with `cargo-mutants@27.1.0` (current release, 2 June 2026, checked upstream) and the
  existing `cargo-nextest@0.9.148`; `git diff "origin/$BASE_REF...HEAD"` into `$RUNNER_TEMP`, with
  `BASE_REF` passed through `env:` rather than expanded in `run:` (zizmor's template-injection
  rule); `timeout-minutes: 30`. Exit 0, or 3 (timeouts only, reported apart from missed mutants),
  passes; any other code fails (2 missed, 4 baseline failed, 5 and 6 a bad diff). An empty or
  Rust-free diff exits 0. No path filter: a docs-only pull request still reports the check, as a
  required check must. N is set from a measurement taken in this task: the run time of a diff
  that adds a whole existing file (`src/coordinator/trunk.rs` against an empty tree), scaled so
  that a new file of about 600 lines, the size of a P1-MOVE task, fits 30 minutes per shard; the
  report states the measurement and N. A `workflow_dispatch` trigger with `files` and `regex`
  inputs (passed through `env:`) runs `cargo mutants -f <files> --re <regex>` on any branch, for
  tasks whose tests pin code the pull request does not change. `.cargo/mutants.toml` excludes
  `src/runtime.rs` (wasm glue with no tests, deleted in M6) with the reason in a comment.
  Packages are added as `tessel-lander` and `tessel-server` appear; the CLI and swarm crates stay
  out, because their tests run the real binary and take minutes per mutant. Done: on the task's
  pull request, a planted commit that adds an untested branch to `src/merge.rs` fails the
  required job with the mutant named, and the job passes after it is reverted; a dispatch run
  with `files` set finds a planted untested branch in an unchanged file; `actionlint` and
  `zizmor` clean. Then Felix adds the check to the ruleset (step 1 under "Ruleset steps").
- [ ] **P1-RULES** — Lane A. Files: `docs/reference/runtime-rules.md`, `tests/runtime_rules.rs`,
  `scripts/ci/check-named-tests.sh`, one step in the fast lane of `ci.yml`. Name every rule in
  `src/runtime.rs` at base `6efe835`, starting from the appendix of this plan, with: the lines it
  lives on, where its decision lives (`shell`, the core, or `runtime.rs` only), and its status:
  "now" (a test exists or is written in this task, against the public `shell` and core API, in
  `tests/runtime_rules.rs`), "after move" (the host test that will pin it, by name, written in
  M2), or "dropped" (no counterpart outside Cloudflare, with the reason). The script reads the
  table and fails if a "now" row names a test missing from `cargo test -p tessel-coordinator --
  --list`. Done: the script fails on the base (no table) and passes after; every `fn` in
  `runtime.rs` appears in at least one row; and, because the per-pull-request job mutates only
  changed lines and `src/shell.rs` does not change here, a `workflow_dispatch` run of the mutation
  job with `files=src/shell.rs` and `regex` set to the functions the "now" rows pin shows each
  new test killing at least one mutant of its function (the run is linked in the report).
- [ ] **P1-CI-NIGHTLY** — Lane B. Files: `.github/workflows/nightly.yml`,
  `scripts/ci/new-missed-mutants.sh`, `scripts/ci/new-missed-mutants.test.sh`,
  `.cargo/mutants-missed-baseline.txt`. `schedule` (once a day) and `workflow_dispatch`, plus a
  `pull_request` trigger filtered to the workflow's own paths so the task's pull request runs it.
  Jobs: "Nightly, proptest at 10,000 cases" (`PROPTEST_CASES=10000 cargo nextest run -p
  tessel-coordinator --lib`); "Nightly, full mutants" as a four-shard matrix on the core, whose
  missed mutants are compared with the committed baseline after line and column numbers are
  stripped. Stripping can make two mutants identical, so the comparison counts each name
  (`sort | uniq -c`) and fails when any name occurs more often than in the baseline; the baseline
  only shrinks. Done: the comparison script's test fails on a planted new missed mutant and on a
  planted second copy of a baseline name, and passes on an unchanged list; the workflow's run on
  the task's pull request is green; `actionlint`, `zizmor` and `shellcheck` clean.
- [ ] **P1-HOST-TYPES** — Lane A, after P1-RULES. Files: `src/host/mod.rs`, `src/host/call.rs`,
  `src/lib.rs`. The host's `Input`, `Output`, `Call` and `CallAnswer` types as frozen in the
  proposal ("Calls"), with no behaviour: no `step` function yet. `src/host` builds without the
  `runtime` feature. `Resumed { sockets }` from the proposal's input list models Durable Object
  hibernation, which `tessel-server` does not have; it is left out unless a rules-list row needs
  it. Each "after move" row of the rules list gains the input or output that carries it. Done: a
  proptest that every persisted part (an outstanding call with its dispatch id, request kind and
  deadline; every `CallAnswer`, `Unknown` included) round-trips through serde, which cannot pass
  before the types exist; no `_` arm in any match on the new enums (clippy's existing
  `wildcard_enum_match_arm` deny).
- [ ] **P1-LANDER-GIT** — Lane B. Files: `tessel-lander/**`, root `Cargo.toml` (member).
  A library crate: the git command layer and coverage check ported from `merge-commands.ts` and
  `merge-coverage.ts`, running the `git` binary (as the steward does, so its behaviour is the
  reference) with a fixed environment: no system or global config, `core.hooksPath` set to an
  empty directory, `transfer.fsckObjects`, no attribute-driven filters or drivers. The rebase
  uses a fixed committer name, email and date (C7). Mode checks call `protocol::Mode::permits`
  instead of porting `modePermits`, so the lander and the coordinator cannot disagree. Done: the
  11 tests of `merge-commands.test.ts` and the 17 of `merge-coverage.test.ts` (14 `it` and 3
  `it.each`, each row ported as its own case), ported one for one; a test that rebases a fixture
  repository and gets a commit id written in the test, which fails if any committer field or the
  date is taken from the environment.

## M2 tasks

Lane A moves `runtime.rs` into `src/host/`, one area per task, keeping today's behaviour,
including its quirks. Lane B ports the steward's merge and trial. `src/host/testkit.rs` (from
P1-MOVE-CALLS) runs the host against an in-memory store and a simulated clock, executes its
outputs and records each client's transcript; the CLI's test support and, in M3, the simulator are
built on it.

Today's ordering comes from the Durable Object's input gate: while a storage operation is
outstanding no other input runs, and a steward `fetch` releases the gate (`runtime.rs`, 433-437
and 980-986). The host keeps that: an input that arrives while a commit is outstanding is queued
and applied after `Committed`; an input that arrives while a `Call` is outstanding is applied at
once. In M2 the outstanding-call table is host memory, like the `merging` and `verifying` cells it
replaces, so after a reload every in-flight marker the core holds counts as a cut-off, exactly as
today (R24). From P1-CORE-RECONCILE (M3), a marker found on `Loaded` is reconciled instead.

- [ ] **P1-MOVE-CALLS** — Lane A. Files: `src/host/{mod,batch,core_slot,testkit}.rs`. The call
  path: load (R14, R15), apply with the soft and hard limits (R16 to R18), one commit at a time
  with sends held until `Committed` (C1; only `Committed` builds the token that releases sends),
  inputs queued behind an outstanding commit and not behind an outstanding call (R42), the reload
  before an answer is applied (R19), a failed commit dropping the core (R30, today's quirk), bind
  after `Welcome` (R29), settle (R31, R32), expiry before a client call (R28). Done: rows R14 to
  R19, R28 to R32 and R42 move to "now", each with a host test; two of them are a `Frame` that
  arrives during an outstanding commit and is applied only after `Committed`, and one that
  arrives during an outstanding `Call` and is applied at once. Each new test is shown failing on
  a planted mutant of its rule (commit, red CI run linked, revert), because the moved code is new
  and the mutation job mutates all of it.
- [ ] **P1-MOVE-SOCKETS** — Lane A. Files: `src/host/sockets.rs`. Sessions and the socket table:
  `Opened { sock, verified }`, `Frame`, `Closed`; binary and malformed frames (R7, R8); identity
  mismatch closing with 1008 (R9); errors closing with 1011 (R10, R32); withdrawal (R11 to R13).
  Done: rows R6 to R13 "now" or "dropped" with reasons; the mutation job green on the new file.
- [ ] **P1-MOVE-WAKE** — Lane A. Files: `src/host/{wake,calls}.rs`. The wake, in today's order
  and shape (R20): expiry, merge recovery, verification recovery, one merge, one verification, the
  head read, then the next wake, each step its own batch. Today the merge is awaited inside the
  alarm, so the verification and head-read steps of that wake run only after the merge's answer,
  and the wake set while it waits is the watchdog; the host keeps that by suspending the wake at
  its merge `Call` and resuming it on `Replied`, with no other wake while it waits except the
  watchdog. Also: verification dispatch with its marker persisted first (R25, R27); cut-off
  recovery (R24); head reads and trunk pokes (R21 to R23); call deadlines enforced by the adapter
  as today's timeouts (R26, R38); a failed wake rescheduled, which Cloudflare does today by
  retrying an alarm that returned an error (R41, C9). COORD-WAKE-WHILE-VERIFYING is decided here:
  a host test of I4 ("if work is due, exactly one wake is pending, and it is in the future") runs
  a merge dispatch while a verification is in flight. If the moved code schedules a wake at `now`
  in a loop, the test lands with `#[ignore = "COORD-WAKE-WHILE-VERIFYING: fixed in the next
  commit"]` and the fix is a separate commit that removes the attribute. Done: rows R20 to R27,
  R38 and R41 "now"; a test that a wake which dispatched a merge runs its verification step only
  after `Replied`; a test that a step failing inside a wake leaves exactly one wake pending, in
  the future; the I4 test passes with no `#[ignore]` left.
- [ ] **P1-MOVE-REPLAY** — Lane A. Files: `src/host/replay.rs`. `Watch` replay through
  `ReadEvents` and `ReplayPage`, with every other input queued until the replay ends, which is
  what the input gate gives today (R36). Done: row R36 "now"; a host test that a `Hello` sent
  after `Watch { from_seq: 0 }` is answered only after the last page.
- [ ] **P1-CLI-ON-HOST** — Lane A, after the four above. Files:
  `tessel-cli/tests/support/mod.rs`. The CLI's fake coordinator keeps its WebSocket plumbing and
  fault injection (`go_silent`, `go_deaf`) and replaces its copy of the orchestration with the
  host and the test driver. Done: the CLI suite passes with the same test count. The core's
  mutation job does not run the CLI tests, so the check that the suite characterises the host at
  the wire is a planted mutant instead: a commit that breaks one moved rule (R29, bind after
  `Welcome`) turns at least one CLI test red on CI, and is reverted; the run is linked.
- [ ] **P1-LANDER-MERGE** — Lane B. Files: `tessel-lander/**`. `run_merge` ported from
  `merge-steps.ts`, split into the two calls the proposal's C6 requires: `prepare` (fetch, verify
  the commit, pin main, deterministic rebase, coverage and gate-path checks; returns the head;
  runs no repo code) and `land` (compare-and-swap push of main from base to the journaled head on
  a git remote reached by path or URL, then a read-back of main). The read-back keeps the
  steward's equality rule in this task, so the ported tests pass unchanged; ancestry is the
  intended change P1-LANDER-ANCESTRY. Done, ported one for one: the 47 tests of
  `merge-steps.test.ts` (44 `it` and 3 `it.each`, each row its own case), with a fake git runner,
  among them the step order and token lifetimes (no credential is live while repo code runs; the
  write token is minted only after the tests and revoked when the push ends or throws); the
  outcome read back from main, never taken from the push's exit code (4 cases); `main_moved`
  checked before the commit is verified or rebased; `already_merged`; `gate_changed`,
  `gate_invalid` and `gate_only` under the admin policy; at most `LOCAL_GIT_COMMANDS_MAX` local
  git commands on the longest path; a stopped rebase as `conflict` with its files and a failed
  rebase with none as `git_failed`; the coverage check on the rebased range with no external diff
  drivers and at most 50 names reported. Then the 11 `runMerge` tests of
  `merge-git.integration.test.ts` (6 merge, 5 coverage) and the 3 of
  `gate-trunk.integration.test.ts`, against real git. Git commands that read a repository's own
  config run only in a `Workspace`, a type that only the lander's clone step can produce, so a
  filter, merge driver or hook in that config can never run (rule 4; P1-LANDER-GIT's review
  showed an in-place rebase runs them and a fresh clone does not). Decided by Felix on
  19 October. Done also: no public function takes a repository path where it needs a
  `Workspace`, and a test that a `Workspace` cannot be built outside the clone step.
- [ ] **P1-LANDER-TRIAL** — Lane B. Files: `tessel-lander/**`. `run_trial` and the two-sided
  report (baseline, then the new main) ported, with the test step behind a trait the runner
  implements in M4. Done: the 6 `runTrial` and 3 `reportTrial` tests of
  `merge-git.integration.test.ts`, and the unit tests of `merge-trial.test.ts`, ported pass.
- [ ] **P1-LANDER-ISOLATION** — Lane B. Files: `tessel-lander/**`. S3: a fork that carries
  hooks, `.gitattributes` filters and drivers, a malformed pack, and a file name with control
  characters. Done: tests that each is refused or inert, each failing when the matching part of
  the fixed git environment is removed.

M2's done-check: no rules-list row is "after move"; `cargo test --workspace` green with the CLI
suite on the host; the wasm lane still green (`runtime.rs` unchanged).

## M3 to M6, in outline

**M3. Simulator and the staged landing.** Lane A in `src/sim/**` and `src/host/**`; lane B owns
the core and `src/merge.rs` until P1-CORE-RECONCILE merges (order under "Lanes and file
ownership").
- P1-DROP-WORKER (lane B, first in M3, decision 13): deletes `src/runtime.rs`, `src/store.rs`,
  `wrangler.toml`, the `worker` and `console_error_panic_hook` dependencies, the `cdylib` crate
  type, the `wasm32-unknown-unknown` target and the "Fast lane, wasm clippy" job, with ruleset
  step 2 made right before it merges. It also deletes what only the Worker called:
  `src/identity.rs`, the `#[cfg(feature = "runtime")]` items in `src/shell.rs` (`Denied`,
  `Inbound`, `authorize`, `only_method` and the `routes` test module) and the `runtime` feature
  itself, so no feature is left holding dead code from M3 to M4; the Ed25519 identity in M4 is
  written fresh in `tessel-server`, and rule R1 moves to "adapter" with it (`parse_route` stays
  in `shell` with its tests). Renaming the feature or keeping it for the
  three weeks would keep code with no caller (`PLAN.md` and `CLAUDE.md`: replace, don't
  deprecate). It deletes `tessel-swarm`'s live target (`src/live.rs`, the `--target live` path in
  `src/main.rs`, and the Cloudflare host names in `src/guard.rs`, keeping `ScratchRepo`): it
  needs a Cloudflare coordinator and steward that can no longer exist, and a live target against
  a hosted `tessel-server` is added when an A/B run needs one, not before. And it rewrites the
  docs that describe the deleted code, so no agent follows them from M3 to M6: `README.md`,
  `docs/how-tessel-works.md`, `docs/reference/coordinator.md`, `docs/configuration.md`, and,
  proposed in the same pull request for Felix to merge, `CLAUDE.md`'s Layout (`runtime.rs`,
  `store.rs`, `identity.rs`) and Commands (`npx wrangler dev`); `docs/BUILD-PROTOCOL.md`'s gate
  row for the wasm warning count is the orchestrator's to change. The TypeScript steward and its
  CI job stay until M6 as the lander's reference. Done: CI green without the wasm job;
  `cargo build -p tessel-coordinator` and its clippy with default features pass natively, and
  `Cargo.toml` has no `runtime` feature; `cargo tree -p tessel-coordinator` has no `worker`,
  `hmac` or `sha2`; a search of the docs above finds no `wrangler`, `Durable Object` or
  `runtime.rs` outside history notes; the rules list's "Lives in" column still points at
  `6efe835`, where the deleted code can be read.
- P1-SIM-CORE (lane A): `src/sim/` (SplitMix64 moved from `tessel-swarm/src/rng.rs`); the driver
  on the test kit; a clock that jumps; a store that loses uncommitted writes on crash and can fail
  a commit without crashing; a crash after any output and a reload; client traffic (claims,
  amends, releases, heartbeats, reviews, races, watchers); checkers I1, I2, I4, I5, I8, I9, I10.
  Each seed runs twice and the two transcripts must be equal, which catches nondeterminism such
  as iteration over the core's `HashMap` lock table (`coordinator.rs`, `LockTable`); a lint bans
  `SystemTime::now` and `Instant::now` inside `src/host` and `src/sim` (clippy's
  `disallowed_methods`, scoped in the task so other crates keep their clocks). Each checker has a
  test with a planted bug in a host variant that the checker must catch.
- P1-SIM-MERGE (lane A, after P1-SIM-CORE and P1-CORE-STAGED-LANDING): an asynchronous merge path
  that delays, reorders, duplicates and drops answers, times out, pushes then loses its reply,
  reports for stale commits, flips on re-run, returns rate-limit and compare-and-swap refusals,
  answers `Unknown` (also to `Reconcile`, many times in a row), and delivers a late `Land` after a
  later one has landed; checkers I3, I6, I7, I11, I12; the advisory mode variants (table below).
- P1-FAILPOINTS (lane A): crash windows of a landing: dispatch persisted; `Prepare` running; head
  journaled; tested, not landed; `Land` sent, outcome not persisted; outcome persisted, not
  delivered; rescheduling; landed, evidence note not written; and a `Trial` and a `ReadHead` in
  flight.
- P1-CI-SIM (lane A): "Fast lane, simulator" per pull request (a fixed number of seeds tuned to
  about two minutes, seeded from the commit sha so each push explores new seeds, failing seeds
  printed for reproduction; no path filter) and a nightly sweep; failing seeds are kept as
  regression cases.
- P1-CORE-STAGED-LANDING (lane B, Felix reviews): the core's one merge call (`begin_merge` and
  `merge_outcome`, `merging.rs`; `MergeOutcome`, `merge.rs`) becomes `Prepare` → the head
  journaled in the in-flight record → `Test` → `Land` (C6), each its own dispatch id. Done: a
  core test that no `Land` is dispatched before its head is persisted, and the existing
  merge-queue property tests rewritten for the stages and green at 10,000 cases on CI.
- P1-CORE-RECONCILE (lane B, Felix reviews): `Unknown` and `Reconcile` (C5) as specified in the
  proposal ("Calls"), using the core's `in_doubt` and `unanswered_from`. Done: core tests that a
  retried `Land` reuses its dispatch id, base and head; that `Unknown` after `Land` never counts
  toward `MAX_INFRA_RETRIES`; that a marker found on `Loaded` is reconciled, not counted as a
  cut-off (the M2 rule R24 test is flipped in this commit); that a landing `Reconcile` finds
  also gets its evidence note (P1-EVIDENCE writes it the same way, whichever call found it). A
  `Land` in doubt is shown in status and metrics with how long it has been in doubt, and
  `tessel-server admin resolve` (proposed change A) is the operator's way out.
- P1-LANDER-ANCESTRY (lane B, Felix reviews): the read-back changes from main equal to the head
  to the journaled head being an ancestor of main (C6). Done: a lander test where main moved past
  the landed head before the read-back, answered `landed`, which fails under equality; the ported
  case "main_moved when the push exits 0 but main is at neither base nor head" now needs the head
  not to be an ancestor, and says so in its name.
- P1-PERSIST-RETRY (lane A, C8, `src/host/batch.rs`): a failed commit retried with the batch
  still in memory instead of dropping the core. Flips R30's test.
- P1-ONE-BATCH (lane A, after P1-CORE-RECONCILE merges; `src/host/wake.rs` and the core's
  `next_wake_ms`): expiry, recovery and dispatch folded into one batch per wake, with a
  verification or head read that is due after a merge's answer getting a wake at once instead of
  riding the suspended wake. Flips R20's test.

**M4. Host changes and `tessel-server`.**
- Lane A, host: exact replay without stalling the repo (C3, flips R36's test); lease renewal
  persisted below half the lease (C10); COORD-HEAD-FROM-AGENT (on `Loaded`, a `ReadHead` call
  before any dispatch; a simulator check that no dispatch uses an agent-declared base);
  COORD-UNDELIVERED-GRANT (decision 7).
- Lane A, server: axum 0.8 (current 0.8.9) on tokio, one task per repo owning its host (C2),
  rusqlite (current 0.40.2) in WAL mode with `synchronous=FULL` (C1), an exclusive lock on the
  store (C2), `state_version` with a migration chain and a fixture per shape (O1; version 1 is the
  first shape, since nothing was ever deployed to migrate from), the protocol range announced in
  a new optional field of `Welcome` with the CLI warning outside it (O2), structured
  `(tenant, repo)` ids (S5).
- Lane B: Ed25519 identity with `kid`, `aud`, tenant claims and reserved service names, minted by
  `tessel-server admin token`, replacing the steward's HMAC minting (the Rust HMAC check went in
  P1-DROP-WORKER) and `tessel init --steward` (S1), with the route and token refusals of R1 as
  adapter tests that port the `routes` cases P1-DROP-WORKER deletes from `src/shell.rs` one for
  one (404; 405 with `Allow`; 403; 401 with the fixed body), each named in the rules list, so
  R1 ends the gap with the coverage it had; ed25519-dalek (current 3.0.0) is the candidate,
  chosen in the task under the dependency rule. P1-RUNNER: the local test runner as its own
  binary, refusing to start unless configured for a single trusted user.

**M5. Lander behind the server; advisory mode.** The lander and the runner as their own binaries
(`tessel-lander`, `tessel-runner`), started by `tessel-server` from configured paths, accepting
work only from it over a local authenticated channel and taking fork and scopes from the
persisted dispatch (S1, S3); `tessel-server` does not link the lander crate (done-check 8).
P1-EVIDENCE: after the read-back finds the head landed, the lander writes the landing's evidence
record (the `MergeRecord` the dispatch carries, the runner's gate steps as they ran, base and
head) as a git note on the landed commit under `refs/notes/tessel-evidence`, with no credential
live while repo code runs (decision 9); at most one per landed commit (I3). The note is a second
remote write after the landing, so it can be lost to a crash ("landed, note not written" is a
failpoint). The write is idempotent: the lander reads the notes ref first, writes the note if it
is absent, does nothing if the same note is there, and reports a different note as an error
without overwriting it; the notes ref is updated by compare-and-swap like main. It is written
from a landing `Land` reports and from one `Reconcile` finds, and the core keeps a landing
"evidence pending" until the lander confirms the note, retrying on the next wake. Reviewers move
to the server's config (`REVIEWERS` in `wrangler.toml` at `6efe835`), so `tessel review` approves
held submissions after the steward's dashboard is deleted; the review page itself stays Phase 3.
`/metrics`, structured logs keyed by repo, claim and seq, and one trace id from submission to
landing (O3); `tessel-swarm/src/local.rs` rebuilt on the server; minimal advisory mode (below);
hook behaviour with the server unreachable (decision 10, O6); the dogfood server deployed.

**M6. Dogfood, then delete.** Five merged pull requests of this repository with claims released
by head sync, including one redeploy while a submitted claim is held; then one pull request that
deletes `tessel-steward/` and the "Slow lane, steward" job, with ruleset step 5. (The Cloudflare
Rust glue went in M3, P1-DROP-WORKER; the HMAC identity with M4's Ed25519 identity.) Docs that
describe the Cloudflare deployment are rewritten in the same pull request.

## Invariants and the simulator checks that prove them

Every checker runs after every step, every crash and reload, and every migration, on what clients
received (their transcripts), not on what the host intended to send. The simulator runs in both
modes; where an invariant reads differently for an advisory release, the last column says how.

| Invariant | The checker compares | Faults that exercise it | Phase 1 scope; advisory mode |
|---|---|---|---|
| I1 | each grant and event in a client transcript against the state and log reloaded from the simulated store | crash after any output; commit failure without crash | full; same |
| I2 | each watcher's received events against `log[from..]`: in order, no gap, no duplicate, including a `Hello` sent mid-replay | crash mid-replay; slow and dropped sockets | full, except `Truncated`, which arrives with retention (Phase 3); same |
| I3 | `Merged` count per claim (at most one), each submission on the simulated main's first-parent history (at most once), evidence records per landed commit (at most one) | duplicated and lost lander replies; push then lost reply; a late `Land` after a later one landed | full; for an advisory release: at most one `Merged` per claim, its submitted commit an ancestor of the synced head (it is a second parent, not first-parent history), and no evidence record |
| I4 | after each step: if the core has due work, exactly one wake is pending and it is later than `now` | clock jumps; failed wakes | full; same |
| I5 | state against the real store codec and every migration: decode of encode is equal | every step | full; same |
| I6 | a submission whose dispatch is cut off before `Land` every time is rejected after `MAX_INFRA_RETRIES`; no other claim is rejected because of it | failpoints "`Prepare` running" and "tested, not landed" on one submission, always | full; not applicable (nothing is dispatched) |
| I7 | a snapshot of state before and after an answer for any dispatch id not in flight: equal | reordered, duplicated and stale answers | full; same, for `ReadHead` answers |
| I8 | every pair of granted claims after each step: no conflicting locks (`Lock::conflicts_with`) | random claim, amend, release, expiry and race traffic | full; same |
| I9 | every fence, claim id and seq ever seen by any client: no duplicates; a retired fence is refused | crash and reload; migrations | reloads and migrations; restores arrive with backup (Phase 3); same |
| I10 | dispatches against held-for-review submissions (none); `Merged` against uncovered submissions (none); each claim's expiry against heartbeat inputs (no extension without one) | review traffic; uncovered submits; clock jumps | full; same |
| I11 | the host's incremental counters against `Summary::from_events` over the log; each landing's evidence head against the head the lander journaled and tested | lander reports for stale commits; flipped re-runs | full, over the live log (no archive until retention); advisory releases count in `advisory_releases`, never in `merges`, and carry no evidence |
| I12 | a ready verification is dispatched within N merges under continuous merge load; `rank_entries` is unchanged by any permutation of its input | continuous submissions; races | full; the `rank_entries` half stays a proptest, as it is pure; the checker counts landings, and a `Land` in doubt is exempt (no verification is expected while it is outstanding, since `begin_verification` waits for the merge in flight), so seeds with long runs of `Unknown` do not fail it; advisory releases dispatch no verification |

## CI: enforcing rule 9

- Per pull request, required: the mutation check on changed lines (P1-CI-MUTANTS), sharded with
  one aggregating job; from M3, the simulator lane (P1-CI-SIM). Neither has a path filter, so
  every pull request reports both. The pull request template's field for the new test's failing
  output stays. Where the per-pull-request job cannot show a new test failing (tests that pin
  unchanged code, or CLI tests of the host), the task uses the mutation job's `workflow_dispatch`
  run or a planted mutant with a linked red run.
- Nightly: proptest at 10,000 cases and full mutants against a shrinking baseline (P1-CI-NIGHTLY);
  from M3, the simulator seed sweep. A nightly failure is fixed as a task, never retried away.
- From M5, a fast-lane step for done-check 8 (`cargo tree -p tessel-server` and a search for
  `FLY_`).
- Versions named here were checked on 18 October 2026: cargo-mutants 27.1.0,
  `taiki-e/install-action` v2.87.29 (`8cca0918f8471c33515202b73f56ab27f2bed283`, already pinned in
  `ci.yml`), `actions/checkout` v7.0.1 and `Swatinem/rust-cache` v2.9.2 (both already pinned). A
  task that adds an action or tool looks its version up again and pins it by SHA with the version
  in a comment.

## Characterisation, and the `wrangler dev` question

`runtime.rs` builds only for wasm and has no tests, so it cannot be characterised in place.
P1-RULES names every rule; rules whose decision lives in `shell` or the core are tested now,
through the public API; the rest are pinned by host tests as each area moves (M2), and the CLI
suite, run against the host, checks the move at the wire. Rules with no counterpart outside
Cloudflare are listed as dropped, with the reason.

`wrangler dev` comparison captures (running the CLI scripts against `wrangler dev` with a stub
steward, then against the host, and comparing transcripts): recommended **drop** (decision 1).
They would need a stub steward behind a service binding and Node and wrangler running locally on
the shared Mac; transcripts carry wall-clock times and ids that differ between runs, so they need
normalising before they compare; and a capture shows one interleaving, while the rules that matter
most (input gates, replay, cut-offs) are about interleavings, which the host tests and later the
simulator cover. Nothing is deployed, so no user depends on a quirk that the rules list misses.
Cost if kept: about three to five days in M2.

## Advisory mode and dogfooding

Minimal advisory mode (M5): the server runs with landing off. A submitted claim is not dispatched
to the lander; a periodic `ReadHead` call asks the lander for main's head and which submitted
commits are ancestors of it, and the core releases those claims. Ancestry holds for merge commits
only: a squash or rebase merge writes new commits, so the submitted commit never becomes an
ancestor and its claim, which never expires once submitted (protocol invariant 5), would be held
forever.

Options: restrict merge methods to merge commits through the ruleset, or match pull requests by
number through the GitHub API. Recommended: **the ruleset** (decision 3). It needs no code and no
token, works with the merge commits this repository already uses (`git log` shows "Merge pull
request #31"), and matching by pull request number is the GitHub advisory mode of Phase 3
(`merge_commit_sha`); building it now would build Phase 3 early. A force-pushed pull request
branch has the same effect as a squash, which `docs/BUILD-PROTOCOL.md` already rules out (lanes
merge `main` into their branch, never rebase). For the case that slips through anyway, see
proposed change A.

The release is an `EventKind::Merged` with a new field, `advisory: bool`, `#[serde(default,
skip_serializing_if = "is_false")]`, beside P0-UNVERIFIABLE-LANDING's two lists (decision 4). For
an advisory release, `head` is main as the head sync read it, not a commit Tessel landed.
`Summary::from_events` and `SummaryTally` count it in a new `advisory_releases` counter and leave
`merges` unchanged, with a test that fails without the change, so no number reports it as a
landing Tessel made or as evidence that required checks passed (`CLAUDE.md`, rule 6). Main at the
sync can hold several merges, so the release has no known baseline: the assumptions it challenges
go into `unverified` and count as `verifications_unverifiable`, which is what that list means,
and the submitted shadow claims it blocked go into `untried_shadows`, settled without a
`DenialVerified`, as for a landing with no baseline. Older daemons ignore the field and treat
the event as a release, which is right for them.

Protocol invariant 5 says only the coordinator writes main and that a submitted claim is held
"until `Merged` or `SubmitRejected`". In advisory mode humans and CI write main, so the invariant
needs a second sentence; that is protocol text, so it is decision 11.

When the server is unreachable, the `pre-edit` hook fails closed today (`tessel-cli/src/hook.rs`,
`NoDaemon` and `DaemonUnreachable`); the inbox and stop hooks already fail open. Recommended
(decision 10): a setting in `tessel.toml` on the trunk, fail-closed by default, which this
repository sets to fail open while it runs in advisory mode, so a dogfood outage slows lanes down
instead of stopping them, and the CLI reconciles claims on reconnect. The hook reads the setting
from the trunk ref, `git show origin/main:tessel.toml` as last fetched, never from the worktree,
and fails closed if that read fails; a test edits the worktree's `tessel.toml` to fail open and
shows the hook still failing closed. An agent can then change the setting only by landing the
change, and `tessel.toml` is on the `SENSITIVE` list, so that pull request is held for review,
which answers O6's "who can change it". Today no hook reads `tessel.toml`; only the steward's
gate does, and it reads main.

Where the dogfood server runs. It needs to be always on, with a persistent disk for SQLite, an
inbound WebSocket with TLS, and outbound HTTPS to GitHub. The repository is public, so head sync
reads it without a token, and the server holds one secret, its Ed25519 signing key. Felix's Mac
is not an option, nor is any other machine at home, for the same reason.

| Option | Cost a month (checked 18 October 2026) | Notes |
|---|---|---|
| Fly.io, `shared-cpu-1x` with 512 MB, 1 GB volume | about $3.84 ($3.69 + $0.15), in `iad` | builds the image remotely (no Mac load); restarts on crash; TLS on the shared IPv4 at no charge; a volume lives on one host and is not replicated |
| Hetzner Cloud CX23 (2 vCPU, 4 GB), systemd | €5.49 plus €0.50 for IPv4, excluding VAT | a plain VM, closest to how a team would run the binary; Hetzner's cost-optimised plans were reported out of stock through October 2026, and its other shared plans cost more |
| Oracle Cloud Always Free (Ampere A1) | $0 | Oracle reclaims idle Always Free instances, and this server idles most of the time |

Recommended: **Fly.io** (decision 2): the lowest cost that is orderable today, nothing to
administer, and remote builds keep image builds off the Mac. Fallback if Felix declines Fly
billing: Hetzner CX23 when it is in stock; if neither is available, M6's dogfood gate waits and
the deletion with it, while Phase 2 design does not depend on it (proposed change D).

The Fly configuration is the dogfood instance's own (`deploy/dogfood/fly.toml`, with
`auto_stop_machines = "off"` and one machine always running, since head sync stops while a
machine sleeps); the code reads no `FLY_` variable (done-check 8), and the image is the plain
server binary.

Volume snapshots are not a restore path in Phase 1. A restored snapshot rewinds the fence, claim
id and seq counters, which breaks I9 and protocol invariant 4; the high-water mark that makes a
restore safe (C11) arrives with backup in Phase 3. If the volume is lost, the server starts with
a fresh repo state and agents claim again; claims held at the loss are gone, and their pull
requests are unaffected. Moving the server to another host is done by stopping it, copying the
SQLite file, and never starting the old instance again.

## Ruleset steps

Each step is Felix's, made on the date its trigger is met, and recorded with that date in
`docs/ROADMAP.md`. In order:

1. After P1-CI-MUTANTS merges (M1): add "Slow lane, mutants on changed lines".
2. At the start of M3, right before P1-DROP-WORKER merges, with no other pull request open:
   remove "Fast lane, wasm clippy" (decision 13). A required check that never reports blocks
   every pull request, the deleting one included.
3. After P1-CI-SIM merges (M3): add "Fast lane, simulator".
4. In M5, before the first pull request is coordinated in advisory mode: set
   `allowed_merge_methods` to `["merge"]` on the `pull_request` rule (decision 3).
5. In M6, right before the steward's deletion merges, with no other pull request open: remove
   "Slow lane, steward".

Step 5, as a command (step 2 is the same with "Fast lane, wasm clippy"; `+= [{"context": "...",
"integration_id": 15368}]` on `required_status_checks` adds a check, and
`.parameters.allowed_merge_methods = ["merge"]` on the `pull_request` rule sets the methods,
which today allow `merge`, `squash` and `rebase`):

```
gh api repos/devfep/tessel/rulesets/25345043 \
  | jq '{name, target, enforcement, conditions, bypass_actors,
         rules: [.rules[] | if .type == "required_status_checks"
           then .parameters.required_status_checks |= map(select(
             .context != "Slow lane, steward"))
           else . end]}' \
  | gh api -X PUT repos/devfep/tessel/rulesets/25345043 --input -
```

The steward is deleted only after the Rust lander suite passes (M2, M3) and the dogfood gate is
met (M6, decision 6). The Cloudflare Rust glue goes earlier, at the start of M3 (decision 13).

## Carried-over tasks

| Task | Slot | How |
|---|---|---|
| COORD-WAKE-WHILE-VERIFYING | M2, P1-MOVE-WAKE | an I4 host test pins the moved behaviour; a fix, if needed, is its own commit that removes the test's `#[ignore]` |
| COORD-HEAD-FROM-AGENT | M4 | a `ReadHead` call on `Loaded`, before any dispatch; simulator check that no dispatch uses an agent-declared base |
| COORD-UNDELIVERED-GRANT | M4 | decision 7; a host test that the scope is free again within one lease and the fence is refused after it |

## Risks

| Risk | Mitigation |
|---|---|
| The move drops a rule nobody wrote down | every `fn` of `runtime.rs` in the rules list, input-gate and alarm-retry rules included; CLI suite on the host; planted mutants of moved rules must fail a test |
| `runtime.rs` is deleted at the start of M3, before the staged landing | M2's done-check leaves no rule without a host test; the code stays readable at `6efe835`; decision 13 |
| A simulator checker passes without checking anything | each checker has a planted-bug test that must fail |
| A seed does not reproduce | each seed runs twice with transcripts compared; wall-clock calls banned in `src/host` and `src/sim` |
| The lander port drifts from the steward's behaviour | the steward's 47 unit tests and real-git cases are ported first and are the lander's done-checks; the TypeScript stays as reference until M6 |
| A late `Land` overwrites a later landing | compare-and-swap on the base, one `Land` in flight per repo, deterministic rebase; the simulator delivers late `Land`s |
| The lander runs git on objects an agent wrote | fixed git environment; S3 tests with hooks, filters and malformed packs |
| SQLite loses a commit on a crash | `synchronous=FULL`; kill -9 loop on CI with transcripts checked against I1 |
| Advisory mode holds a claim forever (squash, force push) | ruleset allows merge commits only; lanes never rebase pushed branches; proposed change A |
| The dogfood server is down or loses its volume | hooks fail open in advisory mode (decision 10); claims reconcile on reconnect; a lost volume means a fresh state and new claims, never a snapshot restore |
| The mutation check is slow or flaky | changed lines only, core crates only, sharded with a measured shard count, 30 minutes per shard; flakes are fixed, never retried |
| The shared Mac is overloaded | heavy runs on CI only; at most two lanes; no local mutation or sweep runs |
| Phase 1 runs past 12 weeks | re-plan at each milestone boundary; M3 and M4 carry the most and are re-sized first |

## Decisions for Felix

1. **`wrangler dev` captures: drop.** They need a stub steward and Node on the shared Mac, compare
   one interleaving where the risky rules are about interleavings, and nothing deployed depends
   on the quirks; the rules list, host tests and CLI suite check the move.
2. **Dogfood server on Fly.io**, `shared-cpu-1x` 512 MB with a 1 GB volume, about $3.84 a month.
   The cheapest orderable option, with nothing to administer and builds off the Mac. Needs an
   account and billing. Fallback: Hetzner CX23 when in stock.
3. **Merge commits only on `main`**, set through the ruleset (`allowed_merge_methods: ["merge"]`)
   in M5, before advisory mode starts (ruleset step 3). No code, no token; matching by pull
   request number is Phase 3.
4. **Advisory releases are `Merged` with a new optional `advisory` field**, counted in
   `advisory_releases` and never in `merges`. A new event kind would bump `PROTOCOL_VERSION`
   (invariant 13), and older daemons, which skip unknown kinds, would treat their log reads as
   incomplete and lose their reconnect repairs; with a field, they read the event as a release,
   which it is.
5. **Required checks**: the five ruleset steps above, each Felix's edit.
6. **The dogfood gate before the deletion: five merged pull requests and one redeploy** while a
   submitted claim is held, that claim released by head sync afterwards, none released by hand.
   Five pull requests show merge commits from different lanes; the redeploy shows that a held
   claim survives a restart, which pull requests alone would not.
7. **COORD-UNDELIVERED-GRANT: keep the claim until its lease lapses** (30 s). The server cannot
   tell a grant that was never received from one whose write failed after the bytes left; keeping
   it lets an agent that did receive it heartbeat and carry on, and the cost of the other case is
   at most one lease of a blocked scope.
8. **The lander lane runs in parallel from M1.** Scheduling within the phase, not a scope change:
   the lander is the longest path to the deletion and shares no files with the host.
9. **Evidence records as git notes** on the landed commit under `refs/notes/tessel-evidence`,
   written by the lander after the read-back. Storage stays in git, as `PLAN.md` says, with no
   record repository to provision; the steward wrote to a separate record repository on Artifacts,
   which is deleted.
10. **Hooks with the server unreachable: a `tessel.toml` setting**, fail-closed by default, set to
    fail open by this repository while it runs in advisory mode. The hook reads it from
    `origin/main:tessel.toml`, never the worktree, so it changes only through a landed pull
    request, which is held for review because `tessel.toml` is sensitive. A server-side setting
    could not be read while the server is unreachable.
11. **Protocol invariant 5 gains a sentence for advisory mode**: "In advisory mode humans and CI
    write main; the coordinator releases a submitted claim with an advisory `Merged` once its
    commit is an ancestor of main, and the lander writes nothing." Protocol text is Felix's.
12. **The `Call` design is frozen** (`real-use.md`, Architecture, "Calls"), from the spike's
    answers. Merging this plan's pull request approves the freeze; after it, a change to `Call`
    needs Felix.
13. **Delete the Cloudflare Rust glue at the start of M3, the TypeScript steward in M6.** This
    moves part of the deletion earlier than the plan first promised (after the lander and
    advisory mode). M3's staged landing changes the core API that `runtime.rs` calls
    (`begin_merge`, `merge_outcome`, `recover_merge`, `has_merge_in_flight`, `next_wake_ms`,
    `MergeOutcome::from_response`). Keeping `runtime.rs` compiling would mean mapping the staged
    `Prepare`, `Test` and `Land` onto the steward's one-call `/merge`, which is a compatibility
    shim, untested because `runtime.rs` has no tests. The Durable Object is no fallback either:
    nothing is deployed (the instance was deleted on 17 October), and it could land nothing
    without the steward on Cloudflare. After M2's done-check, every rule of `runtime.rs` has a
    host test, and the code stays readable at `6efe835`. What the later gate protects, the
    lander's reference and its CI check, is the TypeScript steward, which stays until M6. This
    matches `PLAN.md`'s decision 2 ("removed as soon as the portable host replaces it") and keeps
    the phase order. It is reversible: P1-DROP-WORKER is one pull request that `git revert`
    undoes, and nothing deployed needs restoring. If Felix declines, the staged landing waits for
    the deletion in M6, after advisory mode, which pushes M3's merge-path work about four weeks
    later.
14. **The native test hosts are rebuilt earlier**: the CLI's in M2, as the wire-level check of
    the move, and the swarm's in M5. The proposal lists them with the deletion (Phase 1, item
    7). Like decision 13, this reorders items within Phase 1 and leaves `PLAN.md`'s scope and
    phase order as they are, so both are filed here as decisions rather than as proposed changes
    to `PLAN.md`; recommended for both, for the reasons given. It is reversible the same way.

## Proposed changes for Felix

These change `PLAN.md` scope or order, so they wait for Felix (rule 10):

- **A. Two admin commands in Phase 1**, moved forward from Phase 3's `tessel admin`.
  `tessel-server admin release <claim>` retires the claim's fence; without it, a claim that
  advisory mode cannot release (a squash that slipped through, a force-pushed branch) is held
  until someone edits the database by hand. `tessel-server admin resolve <claim> --landed` or
  `--not-landed` settles a `Land` stuck in doubt after the operator has read main; without it,
  a git host that never answers `Reconcile` holds the repository's queue for good. `resolve`
  only feeds the core the answer `Reconcile` would have given, so the core's rules still apply.
  If Felix declines `resolve`, a `Land` stuck in doubt holds the queue for good: no submission
  of that repository lands again until the git host answers, and the only other way out is
  editing the database by hand.
- **B. A deployment for the dogfood server in Phase 1**: a plain container build of the server
  binary, and `deploy/dogfood/fly.toml` as that instance's own configuration, ahead of Phase 3's
  signed image. It is not a release and is not signed.
- **C. I2's `Truncated` clause and I9's restore clause are checked when retention and backup
  arrive (Phase 3).** `PLAN.md` says "the simulator with invariants I1 to I12"; the rest of each
  is checked in Phase 1 (table above).
- **D. Phase 2 design may start before the deletion.** If M6 slips (the dogfood gate, hosting),
  Phase 2's GitHub adapter design can start, since it depends on the host and the lander, not on
  the deletion. Building Phase 2 still waits for Phase 1's done-check.

## Appendix: starting list for P1-RULES

Lines are `src/runtime.rs` at `6efe835`. "Lives in" says where the decision is made today; "Then"
says how the rule is checked: "now" (a test of `shell` or the core), a host test in M2, an
adapter test in `tessel-server` (M4), or dropped. P1-RULES checks and completes this list.

| Rule | Lines | Statement | Lives in | Then |
|---|---|---|---|---|
| R1 | 128-131, 149-171 | Routes and token checks decide 404, 405 with `Allow`, 403, 401 with a fixed body | `shell::authorize`; status mapping in runtime | now until P1-DROP-WORKER deletes `authorize`, then adapter (M4) |
| R2 | 136-139 | The Worker drops `Authorization` and sets the verified agent itself; a caller cannot choose it | runtime | adapter: a client-sent agent header is ignored |
| R3 | 162-169 | A refusal is logged with its reason and path, never the token or key | runtime | adapter log test |
| R4 | 208-219 | Summary and trunk-moved need the verified agent (401 otherwise) | runtime | adapter |
| R5 | 220-222 | A non-upgrade request to `ws` is refused with 426 | runtime | adapter |
| R6 | 229-234 | A socket's session holds its verified agent before its first message | runtime | host |
| R7 | 322 | A binary frame gets a rejection reply; the socket stays open | `shell::binary_rejection` | now, then host |
| R8 | 324-327 | An unparsable or oversized frame gets a fault reply | `shell::parse_client_msg` | now |
| R9 | 329-336 | A `Hello` for another agent is answered, closed with 1008 and withdrawn | `shell::decide`; close in runtime | now, then host |
| R10 | 248-253 | An error handling a message closes the socket with 1011 and withdraws | runtime | host |
| R11 | 257-275 | A closed or failed socket withdraws its agents | runtime | host |
| R12 | 304-317 | A socket closed by the coordinator is marked closed first, so it shields no agent | runtime | host |
| R13 | 775-805 | Only an agent with no other open socket and a queued request is withdrawn; each withdrawal is applied, persisted and settled; the first error is returned | `shell::agents_to_withdraw`; loop in runtime | now, then host |
| R14 | 347-385 | Load: stored state, or a new core from `RUN` and `SHADOW_ENABLED`; reviewers and sensitive paths applied on every load; a bad reviewers list leaves nobody able to review; a bad sensitive list holds everything; corrupt state fails and never starts empty | `shell::load_core` and helpers | now |
| R15 | 379-383 | A load that finishes second keeps the core already loaded | runtime | host |
| R16 | 397-401 | The wake is computed after each step, with this instance's merge and verification waits | runtime and core | host |
| R17 | 403-409 | A state that cannot be serialised drops the core | runtime | host |
| R18 | 410-431 | The soft limit refuses a client call with its `req` and drops the core; over the hard limit is an error that drops the core | `StoredSize::judge`; runtime | now, then host |
| R19 | 434-446 | After a steward call, the core is reloaded before an answer is applied, because a client message handled during the call may have dropped it | runtime | host |
| R20 | 448-463 | A wake runs expiry, merge recovery, verification recovery, one merge, one verification, the head read, then sets the wake; each step is its own write; the merge is awaited inside the wake, so the later steps run after its answer | runtime | host (quirk; one batch per wake in M3) |
| R21 | 469-486 | A trunk poke for a repo with no state answers 204 and stores nothing; otherwise it is counted, stored, and answered 202 | `shell::poke_reply`, core; runtime | now, then host and adapter |
| R22 | 491-507 | A head read runs only when the core offers one; its answer is stored before anyone is told | core; runtime | host |
| R23 | 511-533 | A failed, timed-out or headless head answer is `None`, and logged | `parse_head_response`; runtime | now, then host |
| R24 | 537-551, 626-640 | A stored in-flight marker with no call waiting in this instance's memory is a cut-off and counts an attempt | `shell::merge_cut_off`, core | now, then host (reconciled instead from M3) |
| R25 | 555-595 | A verification runs only when no merge or verification waits here; its marker is stored before the call | runtime | host |
| R26 | 599-621, 685-707 | A failed call, a timeout, a non-200 or an unknown answer is `ServiceUnavailable` | `from_response`; runtime | now, then host |
| R27 | 644-683 | A merge runs only when none waits here; its marker is stored before the call; while it waits, the wake is the watchdog | runtime and core | host (COORD-WAKE-WHILE-VERIFYING) |
| R28 | 723-743 | Due leases expire, in their own write, before a client message is handled; nothing is written when none is due | runtime | host |
| R29 | 833-869 | A client call: load, expire, handle, persist, bind on `Welcome` after the persist, settle with the sender | runtime; `shell::bind_on_welcome` | host |
| R30 | 816-831 | State and events are written in one transaction; on failure the core is dropped and nothing is sent | runtime and `store` | host (quirk; persist retry in M3) |
| R31 | 871-884 | Settle delivers, then reschedules even if delivery failed; a failed wake write is logged, not returned; dead sockets are withdrawn after | runtime | host |
| R32 | 899-947 | Only a failed send to the sender is an error; a failed send elsewhere closes that socket with 1011 and delivery goes on | `shell::plan_delivery`; runtime | now, then host |
| R33 | 908-912 | Sessions are read only when the plan needs them | `shell::needs_sessions` | dropped: a cost of reading attachments |
| R34 | 949-964 | An unreadable session closes its socket and counts as unbound | runtime | dropped: the host's socket table cannot fail to read |
| R35 | 966-976 | The wake time comes from `alarm_at_ms`; none deletes the alarm | `shell::alarm_at_ms` | now, then host |
| R36 | 978-1012 | Replay pages from `from_seq`, then marks the watcher; no other input runs until the replay ends | `shell::after_page`; input gate | now, then host (quirk; exact replay in M4) |
| R37 | 1014-1042 | The summary is a plain read with an incremental tally; no core, event, wake or socket; `no-store`; 405 for any method but `GET` | `SummaryTally`; runtime | now, then adapter |
| R38 | 87-91 | A head call waits 30 s and a steward call 13 minutes | runtime | host (deadlines sized for CI in M3) |
| R39 | 227-229 | Hibernation keeps sockets, with sessions in their attachments | runtime | dropped: `tessel-server` does not hibernate |
| R40 | 235-239, 264-273 | Accept, close and error lines carry the agent | `shell` log helpers | now, then adapter |
| R41 | 277-280, 886-897 | A wake that fails returns its error, and Cloudflare runs the alarm again; the wake ends by setting the next one, with its error returned for that retry | runtime | host: a failed step leaves one future wake (C9, I4) |
| R42 | 433-446, 980-986 | While a storage operation is outstanding no other input runs; a steward call releases that hold, so client messages run while a merge, trial or head read waits | input gate | host: inputs queue behind a commit, not behind a call |

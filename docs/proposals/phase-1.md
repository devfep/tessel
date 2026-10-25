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
| A, host and server | `src/host/**` (new), `src/sim/**` (new), `src/lib.rs`, `src/shell.rs`, `src/protocol.rs` (additive changes only), `src/coordinator.rs`, `src/coordinator/**` and `src/merge.rs` except while lane B has them in M5, `docs/reference/runtime-rules.md` (new), `tests/runtime_rules.rs` (new), `scripts/ci/check-named-tests.sh` (new), `tests/simulator.rs` (new), `tessel-cli/tests/support/mod.rs`, `tessel-cli/src/daemon.rs` (the range warning, M3), `tessel-swarm/src/local.rs`, `tessel-server/Cargo.toml` and `tessel-server/src/{main,server,config,repo,store,ws,metrics}.rs` (new; `main.rs` wires lane B's subcommands) |
| B, lander, merge path, CI and identity | `.github/workflows/**`, `.cargo/**` (new), `scripts/ci/**` (new) except the named-tests check, `tessel-lander/**` (new), in M5 `src/merge.rs`, `src/coordinator.rs` and `src/coordinator/**`, `tessel-server/src/{identity,admin,credential}.rs` and `tessel-server/src/landing/**` (new), `tessel-runner/**` (new), `tessel-cli/src/init.rs`, `deploy/dogfood/**` (new), the deletions and doc rewrites of P1-DROP-WORKER (M3: `src/runtime.rs`, `src/store.rs`, `src/identity.rs`, `wrangler.toml`, `tessel-swarm/src/{live,guard,main}.rs`, `README.md`, `docs/how-tessel-works.md`, `docs/reference/coordinator.md`, `docs/configuration.md`, a proposed `CLAUDE.md` edit) and of `tessel-steward/` (M6) |

Shared files: the root `Cargo.toml` (workspace members, features) and `Cargo.lock`. A lane that
adds a member or a dependency does it in its own small commit, and the orchestrator does not run
two such commits at once. `.github/workflows/ci.yml` is lane B's, but lane A adds one step to it
in P1-RULES and P1-CI-SIM; whichever of two such pull requests merges second merges `main` first.
The core and `src/merge.rs` pass from lane A to lane B from P1-CORE-STAGED-LANDING until
P1-CORE-RECONCILE lands, and some of lane B's M5 tasks edit lane A's files while they run; the
M5 outline below lists those cross edits and their order (P1-DROP-WORKER, lane B, deleted the
Cloudflare Rust glue in M3, before any core API change, decision 13).

The merge-path tasks (P1-SERVER-STAGED with P1-LANDER-ANCESTRY, P1-CORE-STAGED-LANDING,
P1-CORE-RECONCILE) go to Felix for review, as `PLAN.md` keeps merge-path changes with him
(MERGE-PATH-REVIEW, M5). `src/runtime.rs` stays unchanged
through M2, so the reference for the move stays readable and the wasm lane keeps checking it.

## Milestones

Weeks are calendar weeks with two lanes; neighbouring milestones overlap by a week where one lane
finishes first. Each milestone ends with a check CI runs; the first two are planned to task level
below, the rest in outline, re-planned at each milestone boundary.

| Milestone | Weeks | Contents | Done-check (CI) |
|---|---|---|---|
| M1. Guardrails and the host's types | 1-2 | Mutation check per pull request; nightly job; `runtime.rs` rules list with in-place tests; the host's `Input`, `Output` and `Call` types; lander git layer | Mutation job required and green; nightly green once; rules check passes; host types round-trip; lander git tests pass |
| M2. The move | 3-5 | `runtime.rs` moved step for step into `src/host/`, quirks included; the CLI's fake coordinator rebuilt on the host; lander `prepare`, `land` and `trial` ported | No host row of the rules list left "after move"; CLI suite green on the host; ported steward tests green |
| M3. The local server | 6-8 | Cloudflare Rust glue deleted first (P1-DROP-WORKER); axum, one task per repo, SQLite store, store lock, `state_version`, protocol range; Ed25519 identity and token minting; local test runner; the host's one `Prepare` call answered by prepare, tests and land in process; the adapter rules R1-R5 and R49; `tessel init` against a local server; the single-developer quickstart | No rules-list row left "after move"; an end-to-end landing on CI: a local `tessel-server`, a bare git remote, two CLI agents, one commit landed by compare-and-swap after the runner's command ran on it; `tessel review` approves through the server; a kill -9 loop with claims reconciled |
| M4. Dogfood on the local server | 8-9 | This repository coordinated and landed by a `tessel-server` on Felix's machine: lanes claim before editing, the runner runs the workspace gate, the lander lands `main`; the build protocol's section 2 rewritten | Five landings of this repository with their notes, claims released by `Merged`; one server restart while a submitted claim is held |
| M5. Hardening: simulator, staged landing, reconciliation | 10-14 | Simulator in two tasks, I1-I12 checkers (evidence and advisory clauses in M6), failpoints; the core's landing split into `Prepare`, journaled head, `Test`, `Land`, the head kept on a candidate ref; `Unknown` and `Reconcile`; ancestry read-back; persist retry; one batch per wake; exact replay, half-lease persistence, COORD-UNDELIVERED-GRANT; the adapter suite; repo code under test as its own macOS user; swarm local target rebuilt on the server; the dogfood server upgraded to the staged landing | Simulator lane required and green, its short seed set in every landing; each behaviour change flips its named test; adapter suite green; a CI test of the runner's own uid; three staged landings of this repository |
| M6. Team mode, then delete | 14-16 | The lander as its own binary; evidence records; metrics, logs, trace id; minimal advisory mode with head sync; hooks with the server unreachable; the hosted dogfood server; five merged pull requests in advisory mode and one redeploy with a claim held across it; the TypeScript steward deleted; ruleset changed in the same step | `cargo tree -p tessel-server` has no lander crate; advisory end-to-end test; metrics contract test; phase done-check above |

## M3 to M6, in outline

Re-ordered on 20 October (decision 15): the single-developer local setup comes first, then its
dogfood, then the hardening the first order began with.

**M3. The local server.** Lane A owns `src/host/**` and `tessel-server/src/{main,repo,store,ws}.rs`;
lane B owns `tessel-lander/**` and `tessel-server/src/{identity,admin,runner}.rs`.
- P1-DROP-WORKER (lane B, first in M3, decision 13): deletes `src/runtime.rs`, `src/store.rs`,
  `wrangler.toml`, the `worker` and `console_error_panic_hook` dependencies, the `cdylib` crate
  type, the `wasm32-unknown-unknown` target and the "Fast lane, wasm clippy" job, with ruleset step
  2 made right before it merges. It also deletes what only the Worker called: `src/identity.rs`, the
  `#[cfg(feature = "runtime")]` items in `src/shell.rs` (`Denied`, `Inbound`, `authorize`,
  `only_method` and the `routes` test module) and the `runtime` feature itself, so no feature is
  left holding dead code in this milestone; the Ed25519 identity is written fresh in `tessel-server`
  (P1-IDENTITY), and rule R1 moves to "adapter" with it (`parse_route` stays in `shell` with its
  tests). Renaming the feature or keeping it for the three weeks would keep code with no caller
  (`PLAN.md` and `CLAUDE.md`: replace, don't deprecate). It deletes `tessel-swarm`'s live target
  (`src/live.rs`, the `--target live` path in `src/main.rs`, and the Cloudflare host names in
  `src/guard.rs`, keeping `ScratchRepo`): it needs a Cloudflare coordinator and steward that can no
  longer exist, and a live target against a hosted `tessel-server` is added when an A/B run needs
  one, not before. And it rewrites the docs that describe the deleted code, so no agent follows them
  from M3 to M6: `README.md`, `docs/how-tessel-works.md`, `docs/reference/coordinator.md`,
  `docs/configuration.md`, and, proposed in the same pull request for Felix to merge, `CLAUDE.md`'s
  Layout (`runtime.rs`, `store.rs`, `identity.rs`) and Commands (`npx wrangler dev`);
  `docs/BUILD-PROTOCOL.md`'s gate row for the wasm warning count is the orchestrator's to change.
  The TypeScript steward and its CI job stay until M6 as the lander's reference. Done: CI green
  without the wasm job; `cargo build -p tessel-coordinator` and its clippy with default features
  pass natively, and `Cargo.toml` has no `runtime` feature; `cargo tree -p tessel-coordinator` has
  no `worker`, `hmac` or `sha2`; a search of the docs above finds no `wrangler`, `Durable Object` or
  `runtime.rs` outside history notes; the rules list's "Lives in" column still points at `6efe835`,
  where the deleted code can be read.
- P1-SERVER (lane A): axum 0.8 (current 0.8.9) on tokio, one task per repo owning its host (C2),
  rusqlite (current 0.40.2) in WAL mode with `synchronous=FULL` (C1), an exclusive lock on the
  store (C2), `state_version` with a migration chain and a fixture per shape (O1; version 1 is the
  first shape, since nothing was ever deployed to migrate from), the protocol range announced in
  a new optional field of `Welcome` with the CLI warning outside it (O2), structured
  `(tenant, repo)` ids (S5).
- P1-IDENTITY (lane B): Ed25519 identity with `kid`, `aud`, tenant claims and reserved service
  names, minted by `tessel-server admin token`, replacing the steward's HMAC minting (the Rust HMAC
  check went in P1-DROP-WORKER) and `tessel init --steward` (S1), with the route and token refusals
  of R1 as adapter tests that port the `routes` cases P1-DROP-WORKER deletes from `src/shell.rs` one
  for one (404; 405 with `Allow`; 403; 401 with the fixed body), each named in the rules list, so R1
  ends the gap with the coverage it had; ed25519-dalek (current 3.0.0) is the candidate, chosen in
  the task under the dependency rule.
- P1-RUNNER (lane B): the local test runner as its own binary, refusing to start unless
  configured for a single trusted user. It runs the repository's test command on a `TestTree`
  and reports pass or fail with the first failure quoted and bounded (rule 3).

- P1-SERVER-LANDING (lane B, after P1-SERVER and P1-RUNNER; Felix reviews): the adapter half of
  HOST-MERGE-STANDIN. The host's one `Prepare` call stands for the whole merge until M5, so the
  local adapter answers it by running the lander's `prepare`, the runner's command on the
  `TestTree`, and `land`, in process, and replies `Prepared { base, head }` only after the
  read-back finds the head on main; a refused rebase, a failed test run or a push that lost the
  race answers `Refused` or `Unknown` as the core expects (rule 6: nothing is reported landed
  that did not land). No evidence note yet (P1-EVIDENCE, M6); the submitted commit carries the
  CLI's intent note. Done: an adapter test per outcome, and the end-to-end landing in the
  done-check.
- P1-ADAPTER-RULES (lane B, with P1-SERVER): the adapter rows R1-R5 and R49 become "now", each
  with its named `tessel-server` test (the route and token refusals, the verified agent set by
  the adapter, refusals logged without the token, 401 and 426, one host per repo).
- P1-LOCAL-SETUP (lane A, last): `tessel init` against a local server (address and token from
  configuration, `--steward` minting gone), reviewers in the server's config so `tessel review`
  approves held submissions through it, and `docs/local-setup.md`: one developer, one binary,
  a SQLite file, their own remote, in at most a page. Done: the quickstart is followed on CI from
  a clean checkout to a landed commit.

**M4. Dogfood on the local server.** This repository is coordinated and landed by a
`tessel-server` that Felix starts on his machine while lanes work, like a dev server; its SQLite
file keeps claims and held submissions across stops. Lanes claim before editing (`CLAUDE.md`'s
dogfooding section and `skills/tessel/SKILL.md`); the runner runs `cargo test --workspace` and
the rest of the gate on the rebased commit; the lander lands `main` on GitHub by compare-and-swap
push over HTTPS with Felix's token as a ruleset bypass actor (ruleset step 4); the orchestrator's
gate on the merged tree becomes the runner's run, and `docs/BUILD-PROTOCOL.md` section 2 is
rewritten for it, as that section says, with Felix's approval of the landing shape before the
first landing. Done: five landings of this repository with their notes and claims released by
`Merged`, none released by hand; one server restart while a submitted claim is held, that claim
landed afterwards. The hosted server and advisory mode are M6.

**M5. Hardening: simulator, staged landing, reconciliation.** Re-planned on 25 October against
what M3 and M4 changed; Felix decided M5-1 to M5-8 (under "Decisions for Felix") as recommended,
after an adversarial review of the re-plan.

What changed since the outline:
1. The server lands this repository, so a change to the merge path is also a change to the tool
   that lands it. A landed change that breaks landing cannot have its fix landed through the
   server; DOGFOOD-UPGRADE sets the rules for upgrades and rollbacks.
2. Server landings use the ruleset bypass, so the per-pull-request mutants job does not run on
   them; the nightly full-mutants run covers every landed line about a day late, and clippy and
   fmt run on main's CI after the push. Only the runner's `cargo test --workspace` gates a
   landing.
3. D4 tied the server as its own macOS user to M5. RUNNER-SANDBOX and M4-SANDBOX-WORKSPACE are
   the same problem: lane agents, the server with the lander's token, and repo code under test
   all run as one uid.
4. `tessel-runner` is already a binary (one JSON request on stdin, one result on stdout), but the
   server calls its library in process. The server links `tessel_lander` in seven files, and the
   askpass hook is the server's own executable.
5. A landing's state lives in throwaway `call-N` directories that `clear_leftovers` deletes at
   start, and `Request::Land` carries only `{base, head}`. Under the staged landing the head that
   `Prepare` makes must outlive the call and a restart (CANDIDATE-REF).
6. Done since the outline: COORD-HEAD-FROM-AGENT's `ReadHead` on `Loaded` (#106; its simulator
   check moves to P1-SIM-MERGE), SERVER-KILL-AFTER-PUSH-TEST and SERVER-FRAME-LIMIT-TESTS (#103),
   HOST-CALL-DEADLINE-BACKSTOP (#84). The adapter already answers `ReadHead` and `Reconcile` by
   ancestry, so P1-CORE-RECONCILE needs no adapter change.
7. Advisory mode and evidence records do not exist yet, so the simulator's advisory variants, the
   evidence clauses of I3 and I11 and the failpoint "landed, evidence note not written" move to
   M6, beside the code they check.

Two lanes: lane A the host, the server adapter and the simulator; lane B the merge path, the
lander, the runner and CI. The core (`src/coordinator.rs`, `src/coordinator/**`) and
`src/merge.rs` are lane B's from P1-CORE-STAGED-LANDING until P1-CORE-RECONCILE lands, then lane
A's again. Lane B's tasks also edit these files of lane A's, and lane A stays out of each file
while the task that edits it runs:
- P1-SERVER-STAGED: `src/host/call.rs`, `src/host/calls.rs`, `tessel-server/src/repo.rs`.
- P1-CORE-STAGED-LANDING: `src/host/calls.rs`, `src/host/mod.rs`, `src/host/tests.rs` and
  `tessel-cli/tests/support/mod.rs`.
- P1-CORE-RECONCILE: `src/host/calls.rs` (the `Reconcile` on `Loaded`) and R24's named tests in
  `src/host/tests.rs`, `tests/runtime_rules.rs` and `src/shell.rs`. P1-HOST-HARDENING, which runs
  beside it, waits for it before editing those files.
- RUNNER-OWN-UID: `tessel-server/src/{main,server,config}.rs` and `Cargo.toml`.

Workflow changes cannot land through the server (the lander's token has no Workflows
permission), so each task's workflow part is its own pull request that Felix merges, one at a
time, in this order: P1-SIM-CORE (the seed count in the mutants jobs), P1-CI-SIM,
P1-ADAPTER-SUITE (the CI-only kill loop and the old-CLI build), RUNNER-OWN-UID (the uid job). No
pull request is merged while a landing is in flight. Sizes: S about a day of lane time, M two to
three, L four or more, XL more than a week.

Merge-path reviews: `PLAN.md` keeps merge-path changes with Felix, while in the dogfood setup the
orchestrator is the only reviewer and decides held submissions alone. For tasks marked "Felix
reviews", the orchestrator approves the held submission only after Felix's written yes naming
the fork commit's sha, and a fix pass needs a new yes; the standing buy-in order does not apply
to them (MERGE-PATH-REVIEW). SENSITIVE-MERGE-PATH makes sure those submissions are held.

Lane A:
- P1-SIM-CORE (L): `src/sim/` (SplitMix64 moved from `tessel-swarm/src/rng.rs`); the driver on
  the test kit; a clock that jumps; a store that loses uncommitted writes on crash and can fail a
  commit without crashing; a crash after any output and a reload; client traffic (claims, amends,
  releases, heartbeats, reviews, races, watchers); checkers I1, I2, I4, I5, I8, I9, I10, each
  with a test with a planted bug in a host variant that the checker must catch. Each seed runs
  twice and the two transcripts must be equal, which catches nondeterminism such as iteration
  over the core's `HashMap` lock table; clippy's `disallowed_methods` bans `SystemTime::now` and
  `Instant::now` inside `src/host` and `src/sim`. Files: `src/sim/**`, `tests/simulator.rs`,
  `src/lib.rs`, the clippy config. The default seed count is the short set (about 30 s), so
  `cargo test --workspace`, and with it the runner, runs the simulator on every landing; a
  `TESSEL_SIM_SEEDS` variable raises it on CI and lowers it in the mutants jobs, where 30 s more
  per mutant would break the 30-minute pull-request shards and the 120-minute nightly ones. The
  workflow pull request that sets it low there merges before any of the task's code lands. The
  task records the nightly duration once its code has landed. If it runs long, it splits in two:
  the driver and store with I1, I5 and I9, then the traffic checkers.
- P1-CI-SIM (S, after P1-SIM-CORE): "Fast lane, simulator" on pull requests and on pushes to main
  (the two-minute set, seeds from the commit sha, failing seeds printed for reproduction and kept
  as regression cases, no path filter) and a nightly sweep.
- P1-SWARM-ON-SERVER (M, after P1-CI-SIM; decision 14): `tessel-swarm/src/local.rs` rebuilt on
  the server. It calls the core's `begin_merge` and `merge_outcome` itself today; once it drives
  the server, P1-CORE-STAGED-LANDING does not touch it. The swarm tests then start a real server
  inside the runner's `cargo test`, so the task measures a landing's time against the runner's
  720 s limit and reports it.
- P1-HOST-HARDENING (M, after P1-CORE-STAGED-LANDING lands): P1-PERSIST-RETRY (C8,
  `src/host/batch.rs`: a failed commit retried with the batch still in memory instead of dropping
  the core; flips R30's test); exact replay without stalling the repo (C3, flips R36's test);
  COORD-UNDELIVERED-GRANT (decision 7: the claim kept until its lease lapses, one test).
- P1-ADAPTER-SUITE (M; first part after P1-SWARM-ON-SERVER, the rest after P1-HOST-HARDENING):
  the parts of the phase's done-check 3 with no test yet. First part, which does not touch
  `src/host/`: a kill -9 loop with client transcripts checked against I1, in a job that runs only
  on CI; the CLI release before the protocol range field run against the new server, that old
  binary built only in a CI job, never in `cargo test`. The rest: a slow watcher, a panicking
  repo task, injected store errors.
- P1-SIM-MERGE (L, after P1-SIM-CORE and P1-CORE-STAGED-LANDING): an asynchronous merge path that
  delays, reorders, duplicates and drops answers, times out, pushes then loses its reply, reports
  for stale commits, flips on re-run, returns rate-limit and compare-and-swap refusals, answers
  `Unknown` (also to `Reconcile`, many times in a row), and delivers a late `Land` after a later
  one has landed; checkers I3, I6, I7, I11, I12, without their evidence and advisory clauses;
  and COORD-HEAD-FROM-AGENT's check that no dispatch uses an agent-declared base.
- P1-FAILPOINTS (M, after P1-SIM-MERGE and P1-CORE-RECONCILE): crash windows of a landing:
  dispatch persisted; `Prepare` running; candidate ref pushed, head not journaled; head
  journaled; tested, not landed; `Land` sent, outcome not persisted; outcome persisted, not
  delivered; rescheduling; and a `Trial` and a `ReadHead` in flight.
- P1-ONE-BATCH (M, after P1-CORE-RECONCILE, when the core is lane A's again; `src/host/wake.rs`
  and the core's `next_wake_ms`): expiry, recovery and dispatch folded into one batch per wake,
  with a verification or head read that is due after a merge's answer getting a wake at once
  instead of riding the suspended wake (flips R20's test); lease renewal persisted below half the
  lease (C10, the core's `heartbeat`).

Lane B:
- CANDIDATE-REF (S; an adversarial review, then Felix, decision M5-7): a design note on where the
  prepared head lives between `Prepare`, `Test` and `Land`. Recommended shape: `Prepare` pushes
  the rebased head to `refs/heads/tessel/candidate/<dispatch id>` on a fork the lander owns, one
  per repository and not an agent's; `Test` clones that ref; `Land` pushes the head by
  compare-and-swap on main and deletes the ref once the ancestry read-back finds the head landed.
  A kill at any point leaves the ref, so a retried `Land` pushes the same head. A `Land` whose
  ref is gone answers `Landed` if main contains the head, and otherwise a new outcome that sends
  the submission back to `Prepare`: `Refused` would reject good work, and `Unknown` would loop
  (with main still at base, `Reconcile` resends `Land`, and nothing after `Land` counts toward
  the retry limit). This is the shape of Phase 2's candidate pull request. The note also covers
  cleanup of orphaned refs, the credential for the candidate push, and whether `prepare` and
  `land` stay public in the lander crate.
- P1-SERVER-STAGED (M, Felix reviews, after CANDIDATE-REF and MERGE-PATH-REVIEW): the server
  answers `Test` (the runner on a clone of the candidate ref) and `Land` (the compare-and-swap
  push and the ancestry read-back: the journaled head must be an ancestor of main, not equal to
  it, which is P1-LANDER-ANCESTRY, folded in). It adds the new outcome for a missing candidate
  ref as a variant in `src/host/call.rs` with its match arms (rule 2) in `src/host/calls.rs` and
  `tessel-server/src/repo.rs` (`moves_main`); until P1-CORE-STAGED-LANDING the core reads it as
  infrastructure, which is harmless because the core never sends `Land` before then, so neither
  main's behaviour nor the dogfood server changes. Done: an adapter test for each call and
  outcome, including a `Land` whose ref is missing; a lander test where main moved past the
  landed head before the read-back, answered `landed`, which fails under equality; the ported
  case "main_moved when the push exits 0 but main is at neither base nor head" now needs the head
  not to be an ancestor, and says so in its name.
- P1-CORE-STAGED-LANDING (L, Felix reviews, after P1-SERVER-STAGED, P1-SWARM-ON-SERVER and
  SENSITIVE-MERGE-PATH): the core's one merge call (`begin_merge` and `merge_outcome`,
  `merging.rs`; `MergeOutcome`, `merge.rs`) becomes `Prepare` → the head journaled in the
  in-flight record → `Test` → `Land` (C6), each its own dispatch id, and `Prepare` becomes
  rebase-and-push-candidate only. In the same commit, so main is never red: `src/host/calls.rs`,
  `src/host/mod.rs`, `src/host/tests.rs` and the CLI's fake coordinator
  (`tessel-cli/tests/support/mod.rs`, which answers `Prepare` as the whole merge). Done: a core
  test that no `Land` is dispatched before its head is persisted; the merge-queue property tests
  rewritten for the stages and green at 10,000 cases on CI; the end-to-end and quickstart CI jobs
  green on the staged path. HOST-MERGE-STANDIN's stand-in half closes here.
- P1-CORE-RECONCILE (M, Felix reviews, after P1-CORE-STAGED-LANDING; the core and the host, no
  adapter change): `Unknown` and `Reconcile` (C5) as specified in the proposal ("Calls"), using the
  core's `in_doubt` and `unanswered_from`. Done: core tests that a retried `Land` reuses its
  dispatch id, base, head and candidate ref; that `Unknown` after `Land` never counts toward
  `MAX_INFRA_RETRIES`; that a marker found on `Loaded` is reconciled, not counted as a cut-off
  (R24's test flipped in this commit); DOGFOOD-UPGRADE's fixture test (c). A `Land` in doubt is
  shown in status with how long it has been in doubt, as an optional wire field with
  `#[serde(default)]` (rule 1), and `tessel-server admin resolve`, run offline (stop, resolve,
  start), is the operator's way out (decision M5-3).
- GATE-PARITY-FMT (S, after P1-CORE-RECONCILE): the gate's strict command check
  (`tessel-lander/src/gate.rs`) accepts `cargo fmt --all --check`, which compiles nothing, and
  Felix's pull request adds it to `tessel.toml`. Clippy stays on CI: it would add a second full
  compile per landing on a Mac already at load 55-75.
- OWN-USERS-DESIGN (S, an adversarial security review, then Felix; while P1-SWARM-ON-SERVER
  runs): a design note whose scope is keeping the lander's token and workspace away from repo
  code under test; keeping main safe from the lanes, which still run as Felix, is not in scope.
  It settles: hidden service accounts made with `dscl` (a UID below 500, their own group, shell
  `/usr/bin/false`, home `/var/empty`; not `sysadminctl`, whose users join `staff` and can read
  Felix's home), `_tessel` for the server and the lander, alone reading the token and the store,
  and `_tessel-runner` for repo code; a one-shot runner started through a narrow sudoers rule, its
  stdin and stdout JSON the channel and who may sudo the only authentication;
  cleanup by `sudo -u _tessel-runner kill -KILL -1`, with runner calls serialized across the
  server; the runner uid's own `CARGO_HOME` and toolchain, and who runs `cargo fetch` when
  `Cargo.lock` changes; who cleans the test tree across uids; upgrades as sudo steps for Felix,
  unless the bin directory stays writable by Felix, which undoes the boundary;
  escape routes tested on macOS (`at`, `cron`, `launchctl` in the runner uid's domain). Dropped:
  a warm `CARGO_TARGET_DIR` (one submission could poison the next one's tests, rule 6) and
  systemd units.
- RUNNER-OWN-UID (M to L, after OWN-USERS-DESIGN and GATE-PARITY-FMT): the server runs the
  existing `tessel-runner` binary through the sudo rule instead of its library. A Linux CI job,
  where runners have passwordless sudo, creates the user and checks that the token is
  unreadable, the workspace unwritable and a `setsid` child killed. Closes RUNNER-SANDBOX and
  M4-SANDBOX-WORKSPACE. Then the dogfood server moves to its own users (sudo steps for Felix),
  and D4's "process control, not a security boundary" is retired for the token.

Orchestrator rows (no lane):
- MERGE-PATH-REVIEW (before P1-SERVER-STAGED; a pull request Felix merges):
  `docs/BUILD-PROTOCOL.md` section 6 gains the merge-path review rule above.
- SENSITIVE-MERGE-PATH (before P1-CORE-STAGED-LANDING; a pull request to
  `scripts/dogfood/setup.sh` Felix merges, then the server's config): `src/merge.rs`,
  `src/coordinator.rs` and `src/coordinator/` join `sensitive`, done with no dispatch in flight
  because it needs a restart. It also holds P1-ONE-BATCH, which the orchestrator decides as usual.
- DOGFOOD-UPGRADE (a section of `docs/dogfood.md`, before the first upgrade): (a) no upgrade to the
  staged landing until P1-SERVER-STAGED, P1-CORE-STAGED-LANDING and P1-CORE-RECONCILE have all
  landed; (b) upgrade only with no dispatch in flight (held submissions are fine), since a stop
  mid-test orphans the cargo process group, and an old whole-merge call may already have pushed,
  which the new core would read as a cut-off before `Land`; (c) a fixture test, part of
  P1-CORE-RECONCILE: a store written before the staged landing with a merge in flight is decoded by
  the new core and treated as in doubt (O1, I5); (d) rollback only with an idle queue: M5 bumps
  `state_version` only if the old binary can still read the new state, otherwise rollback means a
  fresh store and new claims, never a snapshot restore, which can reissue fences (C11); (e) before
  the swap, the new binary lands once on `tessel-sandbox/m4-trunk` over HTTPS through the askpass
  hook; (f) the binary is built only from a landed main with green CI, the old `bin/` is kept as
  `bin.prev`, the swap is stop, swap and start from inside `~/tessel-dogfood`, and a fix that cannot
  land through the server goes through a pull request Felix merges.
- M5-DOGFOOD-STAGED: after the upgrade, three landings of this repository through the staged
  path, with notes and claims released by `Merged`.

Order: lane A runs P1-SIM-CORE, P1-CI-SIM, P1-SWARM-ON-SERVER, P1-ADAPTER-SUITE's first part,
then (after P1-CORE-STAGED-LANDING) P1-HOST-HARDENING, P1-SIM-MERGE, the rest of
P1-ADAPTER-SUITE, and (after P1-CORE-RECONCILE) P1-FAILPOINTS and P1-ONE-BATCH. Lane B runs
CANDIDATE-REF, P1-SERVER-STAGED, OWN-USERS-DESIGN, P1-CORE-STAGED-LANDING, P1-CORE-RECONCILE,
GATE-PARITY-FMT and RUNNER-OWN-UID; it may sit idle for a few days waiting for
P1-SWARM-ON-SERVER, which is accepted rather than putting lane B in lane A's `local.rs`. The
critical path is P1-SIM-CORE → P1-CI-SIM → P1-SWARM-ON-SERVER → P1-CORE-STAGED-LANDING →
P1-CORE-RECONCILE → P1-FAILPOINTS and P1-ONE-BATCH. Lane A comes to about 20 to 24 lane-days and
lane B about 15 to 20, over the outline's three to four weeks.

Done: (1) the simulator lane is green with all twelve checkers on, their evidence and advisory
clauses in M6, and its short seed set runs in every landing's `cargo test --workspace`; (2) each
behaviour change flips its named test: R20, R24, R30, R36; (3) P1-ADAPTER-SUITE passes on CI;
(4) repo code under test runs as `_tessel-runner` on the dogfood server, with a CI test that the
token is unreadable, the workspace unwritable and a `setsid` child killed (not claimed: that
lanes cannot reach main); (5) M5-DOGFOOD-STAGED. Cut at the re-plan: running the
pull-request checks on each landing (the nightly full mutants already covers every landed line),
and M5-PROOF-CANDIDATE, moved to Phase 3 (decision M5-5).

**M6. Team mode, then delete.** P1-EVIDENCE: after the read-back finds the head landed, the lander
writes the landing's evidence record (the `MergeRecord` the dispatch carries, the runner's gate
steps as they ran, base and head) as a git note on the landed commit under
`refs/notes/tessel-evidence`, with no credential live while repo code runs (decision 9); at most one
per landed commit (I3). The note is a second remote write after the landing, so it can be lost to a
crash ("landed, note not written" is a failpoint). The write is idempotent: the lander reads the
notes ref first, writes the note if it is absent, does nothing if the same note is there, and
reports a different note as an error without overwriting it; the notes ref is updated by
compare-and-swap like main. It is written from a landing `Land` reports and from one `Reconcile`
finds, and the core keeps a landing "evidence pending" until the lander confirms the note, retrying
on the next wake. Reviewers stay in the server's config from M3; the review page itself is Phase 3.
`/metrics`, structured logs keyed by repo, claim and seq, and one trace id from submission to
landing (O3); minimal advisory mode (below); hook behaviour with the server unreachable (decision
10, O6); the hosted dogfood server deployed (decision 2); five merged pull requests of this
repository in advisory mode with one redeploy while a submitted claim is held (decision 6); then one
pull request that deletes `tessel-steward/` and the "Slow lane, steward" job, with ruleset step 6.
Docs that describe the Cloudflare deployment are rewritten in the same pull request.
P1-LANDER-OUT (lane B, XL, after RUNNER-OWN-UID and P1-CORE-RECONCILE; moved from M5, decision
M5-6): `ReadHead`, `Reconcile`, `Trial`, `clone_main`, `Prepare`, `Test`, `Land` and the askpass
hook move to a `tessel-lander` binary behind a local authenticated channel, which takes fork and
scopes from the persisted dispatch (S1, S3); then `cargo tree -p tessel-server` has no lander crate,
and a fast-lane step checks that and searches for `FLY_` (done-check 8), in a workflow pull request
Felix merges. M6 is re-sized at its own re-plan. Also moved from M5: the simulator's advisory
variants and the evidence clauses of I3 and I11, on P1-SIM-MERGE's merge path; the failpoint
"landed, evidence note not written"; and the merge record's route, an optional `record` on the
host's `Call::Land`, host-internal, added with P1-EVIDENCE with its free text bounded (rule 3;
decision M5-1), which closes HOST-MERGE-STANDIN's record half.

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
  one aggregating job; from M5, the simulator lane (P1-CI-SIM). Neither has a path filter, so
  every pull request reports both. The pull request template's field for the new test's failing
  output stays. Where the per-pull-request job cannot show a new test failing (tests that pin
  unchanged code, or CLI tests of the host), the task uses the mutation job's `workflow_dispatch`
  run or a planted mutant with a linked red run.
- Nightly: proptest at 10,000 cases and full mutants against a shrinking baseline (P1-CI-NIGHTLY);
  from M5, the simulator seed sweep. A nightly failure is fixed as a task, never retried away.
- From M6 (P1-LANDER-OUT), a fast-lane step for done-check 8 (`cargo tree -p tessel-server` and a
  search for `FLY_`).
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

Minimal advisory mode (M6): the server runs with landing off. A submitted claim is not dispatched
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
3. After P1-CI-SIM merges (M5): add "Fast lane, simulator". It applies only to pull requests;
   server landings use the bypass (decision M5-4).
4. In M4, before the local lander's first landing: add Felix's account as a bypass actor of the
   required-checks and update-restriction rules, so the lander's compare-and-swap push of `main`
   from his machine passes (decision 15); the runner has run the gate on that commit.
5. In M6, before the first pull request is coordinated in advisory mode: set
   `allowed_merge_methods` to `["merge"]` on the `pull_request` rule (decision 3).
6. In M6, right before the steward's deletion merges, with no other pull request open: remove
   "Slow lane, steward".

Step 6, as a command (step 2 is the same with "Fast lane, wasm clippy"; `+= [{"context": "...",
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

The steward is deleted only after the Rust lander suite passes (M2, M5) and the dogfood gate is
met (M6, decision 6). The Cloudflare Rust glue goes earlier, at the start of M3 (decision 13).

## Carried-over tasks

| Task | Slot | How |
|---|---|---|
| COORD-WAKE-WHILE-VERIFYING | M2, P1-MOVE-WAKE | an I4 host test pins the moved behaviour; a fix, if needed, is its own commit that removes the test's `#[ignore]` |
| COORD-HEAD-FROM-AGENT | `ReadHead` half done (#106); the simulator check in M5, P1-SIM-MERGE | a `ReadHead` call on `Loaded`, before any dispatch; simulator check that no dispatch uses an agent-declared base |
| COORD-UNDELIVERED-GRANT | M5, P1-HOST-HARDENING | decision 7; a host test that the scope is free again within one lease and the fence is refused after it |

## Risks

| Risk | Mitigation |
|---|---|
| The move drops a rule nobody wrote down | every `fn` of `runtime.rs` in the rules list, input-gate and alarm-retry rules included; CLI suite on the host; planted mutants of moved rules must fail a test |
| `runtime.rs` is deleted at the start of M3, before the staged landing | M2's done-check leaves no host rule without a host test; the adapter rules (R2-R5, R49) have no code to protect once `runtime.rs` is gone and are M3's done-check; the code stays readable at `6efe835`; decision 13 |
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
| Phase 1 runs past 12 weeks | re-plan at each milestone boundary; M3 and M5 carry the most and are re-sized first; the phase is sized at 13 weeks since decision 15, and about 16 since M5's re-plan, until M6's own re-plan |

## Decisions for Felix

1. **`wrangler dev` captures: drop.** They need a stub steward and Node on the shared Mac, compare
   one interleaving where the risky rules are about interleavings, and nothing deployed depends
   on the quirks; the rules list, host tests and CLI suite check the move.
2. **Dogfood server on Fly.io**, `shared-cpu-1x` 512 MB with a 1 GB volume, about $3.84 a month.
   The cheapest orderable option, with nothing to administer and builds off the Mac. Needs an
   account and billing. Fallback: Hetzner CX23 when in stock. Needed in M6 (decision 15).
3. **Merge commits only on `main`**, set through the ruleset (`allowed_merge_methods: ["merge"]`)
   in M6, before advisory mode starts (ruleset step 5). No code, no token; matching by pull
   request number is Phase 3.
4. **Advisory releases are `Merged` with a new optional `advisory` field**, counted in
   `advisory_releases` and never in `merges`. A new event kind would bump `PROTOCOL_VERSION`
   (invariant 13), and older daemons, which skip unknown kinds, would treat their log reads as
   incomplete and lose their reconnect repairs; with a field, they read the event as a release,
   which it is.
5. **Required checks**: the six ruleset steps above, each Felix's edit.
6. **The dogfood gate before the deletion: five merged pull requests and one redeploy** while a
   submitted claim is held, that claim released by head sync afterwards, none released by hand.
   Five pull requests show merge commits from different lanes; the redeploy shows that a held
   claim survives a restart, which pull requests alone would not.
7. **COORD-UNDELIVERED-GRANT: keep the claim until its lease lapses** (30 s). The server cannot
   tell a grant that was never received from one whose write failed after the bytes left; keeping
   it lets an agent that did receive it heartbeat and carry on, and the cost of the other case is
   at most one lease of a blocked scope. Decided by Felix on 25 October (M5-8): as
   recommended.
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
13. **Delete the Cloudflare Rust glue at the start of M3, the TypeScript steward in M6.** This moves
    part of the deletion earlier than the plan first promised (after the lander and advisory mode).
    M3's staged landing changes the core API that `runtime.rs` calls (`begin_merge`,
    `merge_outcome`, `recover_merge`, `has_merge_in_flight`, `next_wake_ms`,
    `MergeOutcome::from_response`). Keeping `runtime.rs` compiling would mean mapping the staged
    `Prepare`, `Test` and `Land` onto the steward's one-call `/merge`, which is a compatibility
    shim, untested because `runtime.rs` has no tests. The Durable Object is no fallback either:
    nothing is deployed (the instance was deleted on 17 October), and it could land nothing without
    the steward on Cloudflare. After M2's done-check, every host rule of `runtime.rs` has a host
    test; the adapter rules (R2-R5, R49) are written down with their named adapter tests (M3 after
    decision 15) and have no code to protect until the adapter exists; and the code stays readable
    at `6efe835`. What the later gate protects, the lander's reference and its CI check, is the
    TypeScript steward, which stays until M6. This matches `PLAN.md`'s decision 2 ("removed as soon
    as the portable host replaces it") and keeps the phase order. It is reversible: P1-DROP-WORKER
    is one pull request that `git revert` undoes, and nothing deployed needs restoring. If Felix
    declines, the staged landing waits for the deletion in M6, after advisory mode, which pushes
    M3's merge-path work about four weeks later.
14. **The native test hosts are rebuilt earlier**: the CLI's in M2, as the wire-level check of
    the move, and the swarm's in M5. The proposal lists them with the deletion (Phase 1, item
    7). Like decision 13, this reorders items within Phase 1 and leaves `PLAN.md`'s scope and
    phase order as they are, so both are filed here as decisions rather than as proposed changes
    to `PLAN.md`; recommended for both, for the reasons given. It is reversible the same way.
    Decided by Felix on 20 October: as recommended.
15. **Local first.** Felix asked on 20 October that the single-developer local setup take priority.
    Within Phase 1, M3 becomes the local server (`tessel-server` on SQLite, identity, the local test
    runner, the host's one `Prepare` call answered by prepare, tests and land in process, the
    adapter rules, `tessel init` against it and a quickstart), M4 the dogfood of this repository on
    that server, M5 the hardening the first order put in M3 and M4 (the simulator, the staged
    landing, reconciliation, the host changes, the lander and runner as separate processes, the
    lander's half moved to M6 by decision M5-6), and M6 team mode (evidence notes, metrics, advisory
    mode, the hosted server) and the deletion. The first local release lands through the single call
    the host has today instead of the staged `Prepare`, `Test`, `Land`: for one developer landing
    into their own repository that is the contest-era behaviour, with the host's cut-off recovery on
    reload, and the staged landing follows in M5 before any shared server. What it costs: the server
    ships before the simulator proves the invariants, and the phase is sized at about 13 weeks
    instead of 12. `PLAN.md`'s scope is unchanged; its phase row and dogfooding section are reworded
    to say the local setup comes first. Decided by Felix on 20 October: as proposed (the single-call
    merge for the first local release included).

**Decided at M5's re-plan.** Felix decided these on 25 October, as recommended, after an
adversarial review of the re-plan:
- **M5-1. The merge record's route:** an optional `record` on the host's `Call::Land`,
  host-internal rather than wire protocol, added with P1-EVIDENCE in M6, its free text bounded
  (rule 3).
- **M5-2. The simulator in the gate:** the short seed set (about 30 s) is the default, so every
  landing runs it inside `cargo test --workspace` with no `tessel.toml` change; CI runs the
  two-minute set through `TESSEL_SIM_SEEDS`.
- **M5-3. Proposed change A:** only `tessel-server admin resolve`, for a `Land` in doubt, run
  offline (stop the server, resolve, start), since the store is exclusively locked while the
  server runs; `release` waits.
- **M5-4. Ruleset step 3** stays, after P1-CI-SIM lands. It applies only to pull requests.
- **M5-5. M5-PROOF-CANDIDATE moves to Phase 3.** It is not Phase 1 scope in `PLAN.md`, and
  Verus with Z3 is heavy on the shared Mac.
- **M5-6. P1-LANDER-OUT goes in M6**, before the hosted server, which needs it most. The phase's
  done-check 8 needs it before Phase 1 closes either way.
- **M5-7. CANDIDATE-REF:** Felix approves the shape after its adversarial review, including the
  new outcome for a missing candidate ref if it changes `Call` (decision 12). Creating the
  lander's fork repository and adding it to the fine-grained token "tessel-dogfood-lander" are
  his steps.
- **M5-8. Decision 7** is accepted as written; P1-HOST-HARDENING adds its test.

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
  editing the database by hand. Decided by Felix on 25 October (M5-3): `resolve` only, run
  offline, in M5; `release` waits.
- **B. A deployment for the dogfood server in Phase 1 (M6 after decision 15)**: a plain container
  build of the server binary, and `deploy/dogfood/fly.toml` as that instance's own configuration,
  ahead of Phase 3's signed image. It is not a release and is not signed.
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
| R1 | 128-131, 149-171 | Routes and token checks decide 404, 405 with `Allow`, 403, 401 with a fixed body | `shell::authorize`; status mapping in runtime | now until P1-DROP-WORKER deletes `authorize`, then adapter (M3) |
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
| R20 | 448-463 | A wake runs expiry, merge recovery, verification recovery, one merge, one verification, the head read, then sets the wake; each step is its own write; the merge is awaited inside the wake, so the later steps run after its answer | runtime | host (quirk; one batch per wake in M5) |
| R21 | 469-486 | A trunk poke for a repo with no state answers 204 and stores nothing; otherwise it is counted, stored, and answered 202 | `shell::poke_reply`, core; runtime | now, then host and adapter |
| R22 | 491-507 | A head read runs only when the core offers one; its answer is stored before anyone is told | core; runtime | host |
| R23 | 511-533 | A failed, timed-out or headless head answer is `None`, and logged | `parse_head_response`; runtime | now, then host |
| R24 | 537-551, 626-640 | A stored in-flight marker with no call waiting in this instance's memory is a cut-off and counts an attempt | `shell::merge_cut_off`, core | now, then host (reconciled instead from M5) |
| R25 | 555-595 | A verification runs only when no merge or verification waits here; its marker is stored before the call | runtime | host |
| R26 | 599-621, 685-707 | A failed call, a timeout, a non-200 or an unknown answer is `ServiceUnavailable` | `from_response`; runtime | now, then host |
| R27 | 644-683 | A merge runs only when none waits here; its marker is stored before the call; while it waits, the wake is the watchdog | runtime and core | host (COORD-WAKE-WHILE-VERIFYING) |
| R28 | 723-743 | Due leases expire, in their own write, before a client message is handled; nothing is written when none is due | runtime | host |
| R29 | 833-869 | A client call: load, expire, handle, persist, bind on `Welcome` after the persist, settle with the sender | runtime; `shell::bind_on_welcome` | host |
| R30 | 816-831 | State and events are written in one transaction; on failure the core is dropped and nothing is sent | runtime and `store` | host (quirk; persist retry in M5) |
| R31 | 871-884 | Settle delivers, then reschedules even if delivery failed; a failed wake write is logged, not returned; dead sockets are withdrawn after | runtime | host |
| R32 | 899-947 | Only a failed send to the sender is an error; a failed send elsewhere closes that socket with 1011 and delivery goes on | `shell::plan_delivery`; runtime | now, then host |
| R33 | 908-912 | Sessions are read only when the plan needs them | `shell::needs_sessions` | dropped: a cost of reading attachments |
| R34 | 949-964 | An unreadable session closes its socket and counts as unbound | runtime | dropped: the host's socket table cannot fail to read |
| R35 | 966-976 | The wake time comes from `alarm_at_ms`; none deletes the alarm | `shell::alarm_at_ms` | now, then host |
| R36 | 978-1012 | Replay pages from `from_seq`, then marks the watcher; no other input runs until the replay ends | `shell::after_page`; input gate | now, then host (quirk; exact replay in M5) |
| R37 | 1014-1042 | The summary is a plain read with an incremental tally; no core, event, wake or socket; `no-store`; 405 for any method but `GET` | `SummaryTally`; runtime | now, then adapter |
| R38 | 87-91 | A head call waits 30 s and a steward call 13 minutes | runtime | host (deadlines sized for CI in M5) |
| R39 | 227-229 | Hibernation keeps sockets, with sessions in their attachments | runtime | dropped: `tessel-server` does not hibernate |
| R40 | 235-239, 264-273 | Accept, close and error lines carry the agent | `shell` log helpers | now, then adapter |
| R41 | 277-280, 886-897 | A wake that fails returns its error, and Cloudflare runs the alarm again; the wake ends by setting the next one, with its error returned for that retry | runtime | host: a failed step leaves one future wake (C9, I4) |
| R42 | 433-446, 980-986 | While a storage operation is outstanding no other input runs; a steward call releases that hold, so client messages run while a merge, trial or head read waits | input gate | host: inputs queue behind a commit, not behind a call |

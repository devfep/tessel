# Phase 1 plan: portable host, `tessel-server` and the Rust lander

Status: draft for Felix's review, 18 October 2026. It plans Phase 1 of `PLAN.md` without changing
its scope or order (`CLAUDE.md`, rule 10); where this plan would change them, it says so under
"Proposed changes for Felix". Requirements (C1 to C11, S1 to S6, O1 to O6) and invariants (I1 to
I12) are those of `docs/proposals/real-use.md`. It assumes P0-UNVERIFIABLE-LANDING (optional
`unverified` and `untried_shadows` lists on `EventKind::Merged`, a `verifications_unverifiable`
counter in `Summary`) has merged before any Phase 1 task touches `src/protocol.rs`,
`src/coordinator*.rs` or `src/shell.rs`.

Phase 1 scope, from `PLAN.md`: `runtime.rs` moved step for step into the sans-IO host; the
simulator with invariants I1 to I12 and failpoints; `tessel-server` with SQLite, versioned state,
the protocol range, metrics, Ed25519 identity and token minting; the Rust lander and a local test
runner; minimal advisory mode; Cloudflare and TypeScript code deleted. About 10 to 12 weeks.

## Done-check for the phase

At the end of Phase 1, CI on `main` proves:

1. `cargo test --workspace` passes with the CLI and swarm suites driving the host (through
   `tessel-server` or the host's test driver), with no `worker` crate in `cargo tree`, no
   `wasm32-unknown-unknown` target in `rust-toolchain.toml`, and no `tessel-steward/` directory.
2. The simulator lane passes on every pull request with all twelve invariant checkers on, and the
   nightly seed sweep has passed on seven nights in a row.
3. The `tessel-server` adapter suite passes: kill -9 loop with client transcripts checked, the
   store lock, a slow watcher, a panicking repo task, injected store errors, WebSocket framing and
   close codes, and the CLI of the commit before the protocol range field against the new server.
4. The lander suite passes: the steward's real-git integration cases ported to Rust, the
   isolation cases of S3, and a pinned-commit test of the deterministic rebase (C7).
5. The advisory-mode end-to-end test passes: a merge commit that contains a submitted commit
   releases its claim; a squash of the same branch does not, and the claim is reported as held.
6. The per-pull-request mutation check on changed lines is required and green; the nightly
   proptest run at 10,000 cases is green.
7. Every Phase 1 row of the proposal's "Requirement to test" table names a test that exists,
   checked by the same script that checks the `runtime.rs` rules list (P1-RULES).

Outside CI, recorded in `docs/ROADMAP.md`: this repository was coordinated in advisory mode by a
hosted `tessel-server` for at least five merged pull requests before the Cloudflare and
TypeScript code was deleted.

## Lanes and file ownership

At most two lanes build or test at once, and only on disjoint files (`docs/BUILD-PROTOCOL.md`,
section 5). Heavy runs (mutation, simulator sweeps, proptest at 10,000 cases, kill -9 loops) run
on GitHub runners only, never on the Mac.

| Lane | Owns |
|---|---|
| A, host and server | `src/host/**` (new), `src/sim/**` (new), `src/coordinator.rs`, `src/coordinator/**`, `src/shell.rs`, `src/protocol.rs` (additive changes only), `docs/reference/runtime-rules.md` (new), `tests/runtime_rules.rs` (new), `scripts/ci/check-named-tests.sh` (new), `tests/simulator.rs` (new), `tessel-cli/tests/support/mod.rs`, `tessel-swarm/src/local.rs`, `tessel-server/src/{main,repo,store,ws,metrics}.rs` (new) |
| B, lander, CI and identity | `.github/workflows/**`, `.cargo/**` (new), `scripts/ci/**` (new) except the named-tests check, `tessel-lander/**` (new), `tessel-server/src/{identity,admin}.rs` (new), `tessel-cli/src/init.rs`, the deletion of `tessel-steward/`, `src/runtime.rs`, `src/store.rs`, `src/identity.rs`, `wrangler.toml` |

Shared files: the root `Cargo.toml` (workspace members, features) and `Cargo.lock`. A lane that
adds a member or a dependency does it in its own small commit, and the orchestrator does not run
two such commits at once. `.github/workflows/ci.yml` is lane B's, but lane A adds one step to it
in P1-RULES and P1-CI-SIM; whichever of two such pull requests merges second merges `main` first.
`src/runtime.rs` stays unchanged until the deletion task, so the reference for the move stays
readable and the wasm lane keeps checking it.

## Milestones

Weeks are calendar weeks with two lanes. Each milestone ends with a check CI runs; the first two
are planned to task level below, the rest in outline, re-planned at each milestone boundary.

| Milestone | Weeks | Contents | Done-check (CI) |
|---|---|---|---|
| M1. Guardrails and the host's types | 1-2 | Mutation check per pull request; nightly job; `runtime.rs` rules list with in-place tests; the host's `Input`, `Output` and `Call` types; lander git layer | Mutation job required and green; nightly green once; rules check passes; host types round-trip; lander git tests pass |
| M2. The move | 3-5 | `runtime.rs` moved step for step into `src/host/`, quirks included; the CLI's fake coordinator rebuilt on the host; lander `merge` and `trial` ported | No rules-list row left "after move"; CLI suite green on the host; ported steward integration cases green |
| M3. Simulator and intended changes | 5-7 | Simulator, I1-I12 checkers, failpoints; persist retry, exact replay, durable call deadlines and reconciliation, half-lease persistence, one batch per wake; carried-over core tasks; local test runner; lander calls | Simulator lane required and green; each intended change flips its named test |
| M4. `tessel-server` | 7-9 | axum, one task per repo, SQLite store, store lock, `state_version` and migrations, protocol range, Ed25519 identity and token minting | Adapter suite green |
| M5. Lander behind the server; advisory mode | 9-10 | Lander and runner as separate processes over a local authenticated channel; metrics, logs, trace id; swarm local target rebuilt on the server; advisory head sync; hosted dogfood server | End-to-end landing on a local git remote; advisory end-to-end test; metrics contract test |
| M6. Dogfood, then delete | 10-12 | Five merged pull requests of this repo coordinated in advisory mode; Cloudflare and TypeScript code deleted; ruleset changed in the same step | Phase done-check above |

## M1 tasks

Order: P1-CI-MUTANTS and P1-RULES start together (the first builds nothing locally, so the two
lanes rule holds); P1-CI-NIGHTLY follows in lane B; P1-HOST-TYPES follows P1-RULES in lane A;
P1-LANDER-GIT starts in lane B once the CI tasks have merged.

The order asked for, (a) CI checks, (b) the rules list, (c) the host types, is kept, with three
changes and their reasons:

- (a) and (b) run at the same time. (a) is workflow files and is checked on GitHub, so it adds no
  local load; waiting for it would idle a lane for no gain. (b)'s new tests are still held to the
  mutation check if (a) merges first, which is the likely order.
- (a) is split into the per-pull-request check (P1-CI-MUTANTS) and the nightly job
  (P1-CI-NIGHTLY). The required check should not wait on the full-mutation baseline work. The
  simulator lane is not created in M1: there is nothing for it to run until P1-SIM, and an empty
  job would be a phantom check.
- (c) follows (b), not the other way round: the rules list is the checklist the types are
  reviewed against (every rule that lives only in `runtime.rs` must have an input or output that
  carries it).

The lander port is a parallel lane from M1, on separate files (`tessel-lander/**`). It is the
longest path to the deletion step, and it shares no files with the host.

- [ ] **P1-CI-MUTANTS** — Lane B. Files: `.github/workflows/ci.yml`, `.cargo/mutants.toml`.
  A job "Slow lane, mutants on changed lines", run on `pull_request` only: checkout with
  `fetch-depth: 0` and `persist-credentials: false`; `taiki-e/install-action` (pinned SHA, as
  today) with `cargo-mutants@27.1.0` (current release, 2 June 2026) and the existing
  `cargo-nextest@0.9.148`; `git diff "origin/$BASE_REF...HEAD"` into `$RUNNER_TEMP`, with
  `BASE_REF` passed through `env:` rather than expanded in `run:` (zizmor's template-injection
  rule); then `cargo mutants --in-diff <diff> --package tessel-coordinator --test-tool nextest
  --jobs 2`. Time budget: `timeout-minutes: 30`; a pull request over budget is split, never
  skipped. Exit 0, or 3 (timeouts only, which cargo-mutants reports apart from missed mutants),
  passes; any other code fails (2 missed, 4 baseline failed, 5 and 6 a bad diff).
  `.cargo/mutants.toml` excludes `src/runtime.rs` (wasm glue with no
  tests, deleted in M6) with the reason in a comment. Packages are added to `--package` as
  `tessel-lander` and `tessel-server` appear; the CLI and swarm crates stay out, because their
  tests run the real binary and take minutes per mutant. Done: on the task's pull request, a
  planted commit that adds an untested branch to `src/merge.rs` fails the job with the mutant
  named, and the job passes after it is reverted; a pull request with no Rust change passes;
  `actionlint` and `zizmor` clean. Then Felix adds the check to the ruleset (decision 5).
- [ ] **P1-RULES** — Lane A. Files: `docs/reference/runtime-rules.md`, `tests/runtime_rules.rs`,
  `scripts/ci/check-named-tests.sh`, one step in the fast lane of `ci.yml` (coordinated with
  P1-CI-MUTANTS, which edits the same file: whichever merges second merges `main` first). Name
  every rule in `src/runtime.rs` at base `6efe835`, starting from the appendix of this plan, with:
  the lines it lives on, where its decision lives (`shell`, the core, or `runtime.rs` only), and
  its status: "now" (a test exists or is written in this task, against the public `shell` and core
  API, in `tests/runtime_rules.rs`), "after move" (the host test that will pin it, by name, written
  in M2), or "dropped" (no counterpart outside Cloudflare, with the reason, for example socket
  attachments that cannot be read). The script reads the table and fails if a "now" row names a
  test missing from `cargo test -p tessel-coordinator -- --list`. Done: the script fails on the
  base (no table) and passes after; each new "now" test fails against a mutant of the decision it
  pins (the mutation job's output on the pull request); every `fn` in `runtime.rs` appears in at
  least one row.
- [ ] **P1-CI-NIGHTLY** — Lane B. Files: `.github/workflows/nightly.yml`,
  `scripts/ci/new-missed-mutants.sh`, `scripts/ci/new-missed-mutants.test.sh`,
  `.cargo/mutants-missed-baseline.txt`. `schedule` (once a day) and `workflow_dispatch`, plus a
  `pull_request` trigger filtered to the workflow's own paths so the task's pull request runs it.
  Jobs: "Nightly, proptest at 10,000 cases" (`PROPTEST_CASES=10000 cargo nextest run -p
  tessel-coordinator --lib`); "Nightly, full mutants" as a four-shard matrix (`--shard k/4`) on
  the core, whose missed mutants are compared with the committed baseline after line and column
  numbers are stripped, failing on any missed mutant not in the baseline (the baseline only
  shrinks). Done: the comparison script's test fails on a planted new missed mutant and passes on
  an unchanged list; the workflow's run on the task's pull request is green; `actionlint`,
  `zizmor` and `shellcheck` clean.
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
  uses a fixed committer name, email and date (C7). Done: the 11 cases of
  `merge-commands.test.ts` and the 14 of `merge-coverage.test.ts`, ported one for one; a test
  that rebases a fixture repository and gets a commit id written in the test, which fails if
  any committer field or the date is taken from the environment.

## M2 tasks

Lane A moves `runtime.rs` into `src/host/`, one area per task, keeping today's behaviour,
including its quirks. Lane B ports the steward's merge and trial. `src/host/testkit.rs` (from
P1-MOVE-CALLS) runs the host against an in-memory store and a simulated clock, executes its
outputs and records each client's transcript; the CLI's test support and, in M3, the simulator are
built on it.

- [ ] **P1-MOVE-CALLS** — Lane A. Files: `src/host/{mod,batch,core_slot,testkit}.rs`. The call
  path: load (rules R14, R15), apply with the soft and hard limits (R16 to R18), one commit at a
  time with sends held until `Committed` (C1; only `Committed` builds the token that releases
  sends), a failed commit dropping the core (R30, today's quirk), bind after `Welcome` (R29),
  settle (R31, R32), expiry before a client call (R28). Done: every rules-list row R14 to R18 and
  R28 to R32 moves to "now" with a host test that fails against a mutant of the moved rule.
- [ ] **P1-MOVE-SOCKETS** — Lane A. Files: `src/host/sockets.rs`. Sessions and the socket table:
  `Opened { sock, verified }`, `Frame`, `Closed`; binary and malformed frames (R7, R8); identity
  mismatch closing with 1008 (R9); errors closing with 1011 (R10, R32); withdrawal (R11 to R13).
  Done: rows R6 to R13 "now" or "dropped" with reasons.
- [ ] **P1-MOVE-WAKE** — Lane A. Files: `src/host/{wake,calls}.rs`. The wake: its steps in
  today's order, each its own batch (R20); merge and verification dispatch as `Call` with the
  marker persisted first (R25, R27); cut-off recovery (R24); head reads and trunk pokes (R21 to
  R23); call deadlines enforced by the adapter as today's timeouts (R26, R38). Outstanding calls
  replace the `merging` and `verifying` cells. COORD-WAKE-WHILE-VERIFYING is decided here: a host
  test of I4 ("if work is due, exactly one wake is pending, and it is in the future") runs a merge
  dispatch while a verification is in flight and pins what the moved code does; if it schedules a
  wake at `now` in a loop, the test is written to fail and the fix is a separate commit. Done:
  rows R20 to R27 and R38 "now"; the I4 test exists and passes.
- [ ] **P1-MOVE-REPLAY** — Lane A. Files: `src/host/replay.rs`. `Watch` replay through
  `ReadEvents` and `ReplayPage`, with every other input queued until the replay ends, which is
  what the Durable Object's input gate gives today (R36). Done: row R36 "now"; a host test that a
  `Hello` sent after `Watch { from_seq: 0 }` is answered only after the last page.
- [ ] **P1-CLI-ON-HOST** — Lane A, after the four above. Files:
  `tessel-cli/tests/support/mod.rs`. The CLI's fake coordinator keeps its WebSocket plumbing and
  fault injection (`go_silent`, `go_deaf`) and replaces its copy of the orchestration with the
  host and the test driver. Done: the CLI suite passes with the same test count; a mutant of a
  moved rule (named in the report) fails at least one CLI test, which shows the suite
  characterises the host at the wire.
- [ ] **P1-LANDER-MERGE** — Lane B. Files: `tessel-lander/**`. `run_merge` ported from
  `merge-steps.ts`, in two calls as the proposal's C6 requires: `prepare` (fetch, verify the
  commit, pin main, deterministic rebase, coverage and gate-path checks; returns the head; runs no
  repo code) and `land` (compare-and-swap push of main from base to the journaled head on a git
  remote reached by path or URL, then a read-back by ancestry). Done: the six `runMerge` cases
  and the five coverage cases of `merge-git.integration.test.ts` and the three of
  `gate-trunk.integration.test.ts`, ported against real git, pass.
- [ ] **P1-LANDER-TRIAL** — Lane B. Files: `tessel-lander/**`. `run_trial` and the two-sided
  report (baseline, then the new main) ported, with the test step behind a trait the runner
  implements in M3. Done: the six `runTrial` and three `reportTrial` cases ported pass.
- [ ] **P1-LANDER-ISOLATION** — Lane B. Files: `tessel-lander/**`. S3: a fork that carries
  hooks, `.gitattributes` filters and drivers, a malformed pack, and a file name with control
  characters. Done: tests that each is refused or inert, each failing when the matching part of
  the fixed git environment is removed.

M2's done-check: no rules-list row is "after move"; `cargo test --workspace` green with the CLI
suite on the host; the wasm lane still green (`runtime.rs` unchanged).

## M3 to M6, in outline

**M3. Simulator and intended changes.**
- P1-SIM: `src/sim/` (SplitMix64 moved from `tessel-swarm/src/rng.rs`), the fault model of the
  proposal (a clock that jumps; a store that loses uncommitted writes on crash and can fail a
  commit without crashing; an asynchronous merge path that delays, reorders, duplicates, drops,
  times out, pushes then loses its reply, reports for stale commits, flips on re-run and returns
  rate-limit and compare-and-swap refusals); a crash after any output; the twelve checkers below.
  Each checker has a test with a planted bug in a host variant that the checker must catch.
- P1-FAILPOINTS: the six crash windows of a merge (dispatch persisted; lander running; head
  journaled; pushed, outcome not persisted; outcome persisted, not delivered; rescheduling).
- P1-CI-SIM: "Fast lane, simulator" per pull request (a fixed number of seeds tuned to about two
  minutes, seeded from the commit sha so each push explores new seeds, failing seeds printed for
  reproduction) and a nightly sweep; failing seeds are kept as regression cases.
- Intended changes, one commit each, each flipping a named host test: persist retry (C8); exact
  replay without stalling the repo (C3); durable call deadlines with `Unknown` and `Reconcile`
  (C5, C6); lease renewal persisted below half the lease (C10); expiry, recovery and dispatch
  folded into one batch per wake.
- Carried over: COORD-HEAD-FROM-AGENT (on `Loaded`, a `ReadHead` call before any dispatch; the
  first dispatch never uses a base an agent declared) and COORD-UNDELIVERED-GRANT (decision 7).
- Lane B: P1-RUNNER, the local test runner as its own process, refusing to start unless
  configured for a single trusted user; P1-LANDER-CALLS, the lander answering `Prepare`, `Land`,
  `Trial`, `ReadHead` and `Reconcile`.

**M4. `tessel-server`.** axum 0.8 (current 0.8.9) on tokio, one task per repo owning its host
(C2), rusqlite (current 0.40.2) in WAL mode with `synchronous=FULL` (C1), an exclusive lock on the
store (C2), `state_version` with a migration chain and a fixture per shape (O1; version 1 is the
first shape, since nothing was ever deployed to migrate from), the protocol range announced in a
new optional field of `Welcome` with the CLI warning outside it (O2), structured `(tenant, repo)`
ids (S5). Lane B: Ed25519 identity with `kid`, `aud`, tenant claims and reserved service names,
minted by `tessel-server admin token`, replacing the steward's HMAC minting and `tessel init
--steward` (S1); ed25519-dalek (current 3.0.0) is the candidate, chosen in the task under the
dependency rule.

**M5. Lander behind the server; advisory mode.** The lander and runner as separate processes,
accepting work only from `tessel-server` over a local authenticated channel and taking fork and
scopes from the persisted dispatch (S1, S3); `/metrics`, structured logs keyed by repo, claim and
seq, and one trace id from submission to landing (O3); `tessel-swarm/src/local.rs` rebuilt on the
server; minimal advisory mode (below); hook behaviour with the server unreachable, documented and
tested (O6); the dogfood server deployed.

**M6. Dogfood, then delete.** Five merged pull requests of this repository with claims released
by head sync; then one pull request that deletes `tessel-steward/`, `src/runtime.rs`,
`src/store.rs`, `src/identity.rs`, `wrangler.toml`, the `runtime` feature, the wasm target and the
two CI jobs, with the ruleset change below. Docs that describe the Cloudflare deployment are
rewritten in the same pull request.

## Invariants and the simulator checks that prove them

Every checker runs after every step, every crash and reload, and every migration, on what clients
received (their transcripts), not on what the host intended to send.

| Invariant | The checker compares | Faults that exercise it | Phase 1 scope |
|---|---|---|---|
| I1 | each grant and event in a client transcript against the state and log reloaded from the simulated store | crash after any output; commit failure without crash | full |
| I2 | each watcher's received events against `log[from..]`: in order, no gap, no duplicate, including a `Hello` sent mid-replay | crash mid-replay; slow and dropped sockets | full, except `Truncated`, which arrives with retention (Phase 3) |
| I3 | `Merged` count per claim (at most one), each submission on the simulated main's first-parent history (at most once), evidence records per landed commit (at most one) | duplicated and lost lander replies; push then lost reply | full |
| I4 | after each step: if the core has due work, exactly one wake is pending and it is later than `now` | clock jumps; failed wake writes | full |
| I5 | state against the real store codec and every migration: decode of encode is equal | every step | full |
| I6 | a submission whose dispatch is cut off every time is rejected after `MAX_INFRA_RETRIES`; no other claim is rejected because of it | failpoint "lander running" on one submission, always | full |
| I7 | a snapshot of state before and after an answer for any dispatch id not in flight: equal | reordered, duplicated and stale answers | full |
| I8 | every pair of granted claims after each step: no conflicting locks (`Lock::conflicts_with`) | random claim, amend, release, expiry and race traffic | full |
| I9 | every fence, claim id and seq ever seen by any client: no duplicates; a retired fence is refused | crash and reload; migrations | reloads and migrations; restores arrive with backup (Phase 3) |
| I10 | dispatches against held-for-review submissions (none); `Merged` against uncovered submissions (none); each claim's expiry against heartbeat inputs (no extension without one) | review traffic; uncovered submits; clock jumps | full |
| I11 | the host's incremental counters against `Summary::from_events` over the log; each landing's evidence head against the head the lander journaled and tested | lander reports for stale commits; flipped re-runs | full, over the live log (no archive until retention) |
| I12 | a ready verification is dispatched within N merges under continuous merge load; `rank_entries` is unchanged by any permutation of its input | continuous submissions; races | full; the `rank_entries` half stays a proptest, as it is pure |

## CI: enforcing rule 9

- Per pull request, required: the mutation check on changed lines (P1-CI-MUTANTS); from M3, the
  simulator lane (P1-CI-SIM). The pull request template's field for the new test's failing output
  stays.
- Nightly: proptest at 10,000 cases and full mutants against a shrinking baseline (P1-CI-NIGHTLY);
  from M3, the simulator seed sweep. A nightly failure is fixed as a task, never retried away.
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

The release is logged as `Merged` with a new optional field marking it advisory (decision 4), so
the summary never counts it as a landing Tessel made or as evidence that required checks passed
(`CLAUDE.md`, rule 6). Verifications of an advisory merge have no runner on the hosted server and
are recorded with the `unverified` list from P0-UNVERIFIABLE-LANDING.

In advisory mode the hooks fail open when the server is unreachable: they warn and allow the
commit, and the CLI reconciles claims on reconnect (O6). A dogfood server outage then slows lanes
down instead of stopping them.

Where the dogfood server runs. It needs to be always on, with a persistent disk for SQLite, an
inbound WebSocket with TLS, and outbound HTTPS to GitHub. The repository is public, so head sync
reads it without a token, and the server holds one secret, its Ed25519 signing key. Felix's Mac
is not an option.

| Option | Cost a month (checked 18 October 2026) | Notes |
|---|---|---|
| Fly.io, `shared-cpu-1x` with 512 MB, 1 GB volume | about $3.84 ($3.69 + $0.15), in `iad` | builds the image remotely (no Mac load); restarts on crash; TLS on the shared IPv4 at no charge; daily volume snapshots kept 5 days, free under 10 GB; a volume lives on one host and is not replicated |
| Hetzner Cloud CX23 (2 vCPU, 4 GB), systemd | €5.49 plus €0.50 for IPv4, excluding VAT | a plain VM, closest to how a team would run the binary; Hetzner's cost-optimised plans were reported out of stock through October 2026, and its other shared plans cost more |
| Oracle Cloud Always Free (Ampere A1) | $0 | Oracle reclaims idle Always Free instances, and this server idles most of the time |

Recommended: **Fly.io** (decision 2): the lowest cost that is orderable today, nothing to
administer, and remote builds keep image builds off the Mac. Nothing in the code is Fly-specific:
the deployment is the server binary in a container with one volume, and moving to a VM later is a
copy of the SQLite file. Opening the account and its billing are Felix's.

## The deletion step and the ruleset change

The Cloudflare and TypeScript code is deleted only after the Rust lander suite passes (M2, M3)
and advisory mode has released claims on five merged pull requests of this repository (M6). The
deletion removes two of the six required checks, "Fast lane, wasm clippy" and "Slow lane,
steward". A required check that never reports blocks every pull request, including the deletion
itself, so Felix changes the ruleset right before merging the deletion pull request, with no other
pull request open:

```
gh api repos/devfep/tessel/rulesets/25345043 \
  | jq '{name, target, enforcement, conditions, bypass_actors,
         rules: [.rules[] | if .type == "required_status_checks"
           then .parameters.required_status_checks |= map(select(
             .context != "Fast lane, wasm clippy" and .context != "Slow lane, steward"))
           else . end]}' \
  | gh api -X PUT repos/devfep/tessel/rulesets/25345043 --input -
```

The same shape adds a check (`+= [{"context": "...", "integration_id": 15368}]`) and, for
decision 3, sets `.parameters.allowed_merge_methods = ["merge"]` on the `pull_request` rule
(today it allows `merge`, `squash` and `rebase`).

## Carried-over tasks

| Task | Slot | How |
|---|---|---|
| COORD-WAKE-WHILE-VERIFYING | M2, P1-MOVE-WAKE | an I4 host test pins the moved behaviour; a fix, if needed, is its own commit |
| COORD-HEAD-FROM-AGENT | M3 | a `ReadHead` call on `Loaded`, before any dispatch; simulator check that no dispatch uses an agent-declared base |
| COORD-UNDELIVERED-GRANT | M3 | decision 7; a host test that the scope is free again within one lease and the fence is refused after it |

## Risks

| Risk | Mitigation |
|---|---|
| The move drops a rule nobody wrote down | every `fn` of `runtime.rs` in the rules list; CLI suite on the host; mutants of moved rules must fail a test |
| A simulator checker passes without checking anything | each checker has a planted-bug test that must fail |
| The lander port drifts from the steward's behaviour | the steward's real-git cases are ported first and are the lander's done-checks; the TypeScript stays as reference until M6 |
| The lander runs git on objects an agent wrote | fixed git environment; S3 tests with hooks, filters and malformed packs |
| SQLite loses a commit on a crash | `synchronous=FULL`; kill -9 loop on CI with transcripts checked against I1 |
| Advisory mode holds a claim forever (squash, force push) | ruleset allows merge commits only; lanes never rebase pushed branches; proposed change A |
| The dogfood server is down | hooks fail open in advisory mode; claims reconcile on reconnect; daily volume snapshots |
| The mutation check is slow or flaky | changed lines only, core crates only, 30-minute budget; flakes are fixed, never retried |
| The shared Mac is overloaded | heavy runs on CI only; at most two lanes; no local mutation or sweep runs |
| Clash with the P0-UNVERIFIABLE-LANDING lane | M1 tasks touch none of its files; the move starts after it merges |
| Phase 1 runs past 12 weeks | re-plan at each milestone boundary; Phase 2's GitHub work depends on the host and the lander, not on the deletion, so M6 can overlap Phase 2 design |

## Decisions for Felix

1. **`wrangler dev` captures: drop.** They need a stub steward and Node on the shared Mac, compare
   one interleaving where the risky rules are about interleavings, and nothing deployed depends
   on the quirks; the rules list, host tests and CLI suite check the move.
2. **Dogfood server on Fly.io**, `shared-cpu-1x` 512 MB with a 1 GB volume, about $3.84 a month.
   The cheapest orderable option, with nothing to administer and builds off the Mac. Needs an
   account and billing.
3. **Merge commits only on `main`**, set through the ruleset (`allowed_merge_methods: ["merge"]`)
   before advisory mode starts. No code, no token; matching by pull request number is Phase 3.
4. **Advisory releases are `Merged` with a new optional `advisory` field** and their own
   `Summary` counter, never counted as a Tessel landing. Additive under rule 1; a new event kind
   would bump `PROTOCOL_VERSION` for no gain.
5. **Required checks:** add "Slow lane, mutants on changed lines" once P1-CI-MUTANTS merges and
   "Fast lane, simulator" once P1-CI-SIM merges; remove the wasm and steward checks right before
   the deletion merges (command above). Each change is a ruleset edit only Felix can make.
6. **Five merged pull requests** with claims released by head sync, none released by hand, before
   the deletion. Enough to see merge commits from different lanes and a server restart; fewer
   would not show a restart.
7. **COORD-UNDELIVERED-GRANT: keep the claim until its lease lapses** (30 s). The server cannot
   tell a grant that was never received from one whose write failed after the bytes left; keeping
   it lets an agent that did receive it heartbeat and carry on, and the cost of the other case is
   at most one lease of a blocked scope.
8. **The lander lane runs in parallel from M1.** Scheduling within the phase, not a scope change:
   the lander is the longest path to the deletion and shares no files with the host.

## Proposed changes for Felix

These change `PLAN.md` scope or order, so they wait for Felix (rule 10):

- **A. A force-release command in Phase 1.** `tessel-server admin release <claim>`, which retires
  the claim's fence, moved forward from Phase 3's `tessel admin`. Without it, a claim that
  advisory mode cannot release (a squash that slipped through, a force-pushed branch) is held
  until someone edits the database by hand.
- **B. A deployment file for the dogfood server in Phase 1** (a container build and the host's
  config), ahead of Phase 3's signed image. It is not a release and is not signed.
- **C. I2's `Truncated` clause and I9's restore clause are checked when retention and backup
  arrive (Phase 3).** `PLAN.md` says "the simulator with invariants I1 to I12"; the rest of each
  is checked in Phase 1 (table above).
- **D. The native test hosts are rebuilt earlier.** The proposal lists them with the deletion
  (Phase 1, item 7); this plan rebuilds the CLI's in M2, as the wire-level check of the move, and
  the swarm's in M5.

## Appendix: starting list for P1-RULES

Lines are `src/runtime.rs` at `6efe835`. "Lives in" says where the decision is made today; "Then"
says how the rule is checked: "now" (a test of `shell` or the core), a host test in M2, an
adapter test in `tessel-server` (M4), or dropped. P1-RULES checks and completes this list.

| Rule | Lines | Statement | Lives in | Then |
|---|---|---|---|---|
| R1 | 128-131, 149-171 | Routes and token checks decide 404, 405 with `Allow`, 403, 401 with a fixed body | `shell::authorize`; status mapping in runtime | now; status mapping in the adapter |
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
| R19 | 439-446 | After a steward call, the core is reloaded before an answer is applied | runtime | host |
| R20 | 454-463 | A wake runs expiry, merge recovery, verification recovery, one merge, one verification, the head read, then sets the wake; each step is its own write | runtime | host (quirk; one batch per wake in M3) |
| R21 | 469-486 | A trunk poke for a repo with no state answers 204 and stores nothing; otherwise it is counted, stored, and answered 202 | `shell::poke_reply`, core; runtime | now, then host and adapter |
| R22 | 491-507 | A head read runs only when the core offers one; its answer is stored before anyone is told | core; runtime | host |
| R23 | 511-533 | A failed, timed-out or headless head answer is `None`, and logged | `parse_head_response`; runtime | now, then host |
| R24 | 537-551, 626-640 | A stored in-flight marker with no call waiting here is a cut-off and counts an attempt | `shell::merge_cut_off`, core | now, then host |
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
| R36 | 978-1012 | Replay pages from `from_seq`, then marks the watcher; no other input runs until the replay ends | `shell::after_page`; input gate | now, then host (quirk; exact replay in M3) |
| R37 | 1014-1042 | The summary is a plain read with an incremental tally; no core, event, wake or socket; `no-store`; 405 for any method but `GET` | `SummaryTally`; runtime | now, then adapter |
| R38 | 87-91 | A head call waits 30 s and a steward call 13 minutes | runtime | host (deadlines sized for CI in M3) |
| R39 | 227-229 | Hibernation keeps sockets, with sessions in their attachments | runtime | dropped: `tessel-server` does not hibernate |
| R40 | 235-239, 264-273 | Accept, close and error lines carry the agent | `shell` log helpers | now, then adapter |

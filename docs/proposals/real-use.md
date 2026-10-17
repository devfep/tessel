# Proposal: Tessel for real use

Status: plan and order approved by Felix on 17 October 2026, after four adversarial reviews
(correctness, security, testing, real use and operations). Revised the same day after a second
round of four (fixes and consistency, Phase 0 and 1 feasibility, the GitHub adapter, the test
plan); the phase contents changed and await Felix's confirmation on the pull request. `PLAN.md`
summarises this document and records the decisions.

The challenge is over. Tessel is now being built for teams who would trust it with their main
branch, so it has to be robust, well tested, cloud agnostic, and fit how teams already work.

## What the reviews changed

Round 1:

- **Teams keep their code on GitHub and run their own CI.** Tessel lands work there before it
  hosts git or runs tests itself.
- **Today's merge path has correctness bugs** a port would carry over (see "Fixes to the merge
  path").
- **Nothing is enforced automatically**: no CI, and `src/runtime.rs` has no tests.
- **The parallel merge queue is premature** and is deferred until queue metrics show the need.
- **Upgrades, observability and recovery** come before outside users.

Round 2:

- **Phase 1 deleted the only merge path** while the GitHub adapter arrived in Phase 2, so nothing
  could land in between and submitted claims (which never expire) would lock their files. Phase 1
  now ships a Rust lander, a local test runner, identity minting and a minimal advisory mode.
- **The first GitHub design ran agent code with the repository's secrets** (a candidate ref pushed
  to the base repository triggers its push workflows) and needed an App with `administration`
  rights over the whole organisation. It is replaced: candidates are pull requests from forks, CI
  runs without secrets, and no App administers repositories.
- **GitHub's merge queue** creates its own commits and never checks a fence, so it is not a
  landing mode for Phase 2.
- **Several round-1 fixes were wrong or underspecified**: keeping lease renewals in memory broke
  protocol invariant 4; the restore drill could reissue fences; "landing by ancestry" did not say
  who journals the commit; the verification-slot fix could stall merges.
- **Phase 0 did work on code Phase 1 deletes** and asked for tests that cannot run (`runtime.rs`
  builds only for wasm). Phase 0 is now CI, a GitHub spike and fixes in the portable core.
- **The test plan checked host plumbing, not the protocol's safety**, modelled the steward as a
  synchronous call, and had no tests for nine requirements. Invariants, the fault model and a
  requirement-to-test table were added.

## Where Tessel stands

The core is portable. `protocol`, `coordinator` (`merging`, `racing`, `verifying`, `trunk`),
`shell` and `merge` build natively without the `runtime` feature. Nothing is deployed: the
Cloudflare instance and its stored state were deleted on 17 October 2026, so there is no data to
migrate from it.

| Area | Today | Why it matters |
|---|---|---|
| Hosting | Durable Object per repo (`runtime.rs`, 1,078 lines, 0 tests, wasm only) | Its rules depend on Durable Object input gates, documented only in comments |
| Native hosts | `tessel-swarm/src/local.rs` and `tessel-cli/tests/support/mod.rs` each copy the orchestration, with no persistence or recovery; they cherry-pick instead of rebasing and skip the coverage check | Neither tests the real merge path |
| Merge path | TypeScript steward on Cloudflare (Artifacts, Containers); the gate allows only `npm test`, `pnpm test` or fixed `cargo test` flags | Gone with the deployment; the code is the reference for the Rust lander |
| Identity | The steward mints tokens; one symmetric HMAC key shared by both services; 24 h tokens; no revocation; `steward` is not a reserved name | A compromised service can mint any identity |
| CI | None | "Tests pass before every commit" is not enforced |
| Stored state | One JSON blob, rewritten on every call (heartbeats included), no version field | An upgrade that changes its shape breaks every repo |
| Protocol | `PROTOCOL_VERSION` must match exactly (`coordinator.rs`, `hello`) | CLI and server must upgrade in lockstep |
| Observability | Two `console_log!` calls | No queue depth, durations or failure rates |
| Event log | Grows forever; `Watch {from_seq: 0}` replays all of it | Disk and replay time grow without bound |

## Requirements

Each requirement has a test, listed in "Requirement to test".

**Correctness**

1. **Persist before send** (rule 5). Only the store can build a `Persisted`, after a durable
   commit (SQLite in WAL mode with `synchronous=FULL`). Sends of a batch are released only after
   its commit.
2. **One writer per repo**: one task owns each repo's core, with at most one outstanding commit (the
   Durable Object's input gate, made explicit). The server takes an exclusive lock on its store; a
   second process on the same store refuses to start.
3. **Replay is exact and does not stall the repo.** A watcher is replayed up to the repo's
   `head_seq` at the moment it asked, while its live events queue in a bounded per-socket buffer;
   the repo keeps serving others. Overflow closes that socket.
4. **Sends never block the repo.** Slow sockets are closed, not waited for.
5. **Merge-path work is asynchronous, durable and fenced.** Every dispatch carries a persisted,
   monotonic dispatch id and a persisted deadline. Outcomes are applied only for the dispatch id in
   flight. A restart does not count a failure: recovery reconciles the dispatch (requirement 6),
   and only an expired deadline with no landing counts as an infrastructure failure.
6. **Landing is decided by ancestry of a journaled commit.** The lander computes the rebased head
   before any repo code runs and returns it; the coordinator persists it in the in-flight record
   before authorising the push. "Landed" means that commit is an ancestor of main, checked by the
   lander through git or the host's API, never inside a sandbox where repo code ran. A crash before
   the push means not landed; after it, landed exactly once.
7. **Rebases are deterministic.** The lander rebases with a fixed committer name, email and date,
   so the same inputs give the same commit IDs; a test pins that.
8. **A failed persist is retried** with the answer still in memory, and never turned into a
   cut-off.
9. **Wakes make progress.** At most one wake per repo is pending, always in the future; a failed
   wake is retried with a bounded policy; if work is due, a wake is scheduled.
10. **Leases keep fencing intact** (protocol invariant 4). A renewal is persisted when less than
    half the lease remains, so heartbeats write at most twice per lease instead of every time. A
    restart never extends a lease; a claim whose lease ran out stays expired.
11. **Fences, claim ids and sequence numbers are never reissued,** including after a restore from
    backup: on restore, the counters move past a high-water mark recorded outside the database.

**Security**

1. **Every boundary authenticates.** Agents present asymmetric identity tokens (Ed25519, `kid`
   with overlapping rotation, `aud` and tenant claims, reserved service names, a deny-list that
   takes effect when revocation returns). The lander and test runner run as separate processes and
   accept work only from `tessel-server` over a local authenticated channel; they take fork and
   scopes from the coordinator's persisted dispatch, never from the request. Webhooks are verified
   by signature.
2. **Agent code never runs where secrets live.** On GitHub, candidates are pull requests from
   forks, so the repository's CI runs them as `pull_request` from a fork: no secrets, a read-only
   token. `tessel admin doctor` refuses a repository whose settings send secrets or write tokens to
   fork pull requests. Agent push tokens have no `workflows` permission, so GitHub itself refuses
   workflow-file edits.
3. **The lander runs git, never repo code**, in its own process: `transfer.fsckObjects`, no hooks,
   no filters, no attributes-driven drivers, no checkout of agent trees outside a sandbox.
4. **Free text stays data**: webhook payloads, CI output and attacker-named file paths are bounded
   and quoted, as `merge::tests_reason` does; token-shaped strings are redacted before free text
   is broadcast.
5. **Tenant isolation**: structured `(tenant, repo)` ids instead of `<repo>--<agent>` string joins,
   lowercase-only names, exact source equality, per-repo reviewers, admin scope and quotas.
6. **Stated non-guarantees.** On GitHub, a fork network shares objects: anyone with read access to
   one fork can fetch commits pushed to another, so agents' unsubmitted work is not secret from
   other agents of the same repository. Tests and evidence are only as trustworthy as the test
   code an agent may change, which is why test and build paths are review-flagged (see Evidence).

**Operations**

1. **Versioned state**: a `state_version` with a migration chain; a fixture of each shape that
   ever shipped must load; a newer version than the server knows is refused.
2. **Protocol range**: the server accepts a range and announces it in a new optional field of
   `Welcome`; the CLI warns outside it.
3. **Observability**: `/metrics` (queue depth and wait, merge and CI duration by outcome, lease
   expiries, denials, state size), structured logs keyed by repo, claim and seq, and one trace id
   from submission to landing.
4. **Bounded growth**: retention with an archive export; a watcher asking below the retained floor
   gets a new message `Truncated { oldest_seq }` (additive); summary counters are kept
   incrementally and checked against a recomputation over archive and live log.
5. **Recovery tooling**: `tessel admin` to pause and drain the queue, force-release a claim (which
   retires its fence), revoke an agent or token, rotate keys, export and import a repo; a restore
   drill.
6. **Documented failure behaviour**: what agents' hooks do when the server is unreachable, and
   who can change it.

## Architecture

**A sans-IO host.** The orchestration in `runtime.rs` moves into a step machine in the core crate.
Adapters do only I/O.

- Inputs, each with `now_ms`: `Opened { sock, verified }`, `Frame { sock, text }`,
  `Closed { sock }`, `Wake`, `TrunkPoked`, `Loaded { stored }`,
  `ReplayPage { sock, events, done }`, `Committed { batch }`, `CommitFailed { batch }`,
  `Replied { call, answer }`, `CallLost { call }`, `Resumed { sockets }`.
- Outputs: `Load`, `Commit { batch, entries }`, `Send { sock, msg }` (held until its batch
  commits), `SetSession`, `Close`, `ReadEvents`, `SetWake { at }`,
  `Call { call, dispatch_id, request, deadline }`, `Log`, `Metric`.
- State: the core (with inputs queued while it loads), the last durable snapshot and its size, the
  socket table, outstanding calls (replacing the `merging` and `verifying` cells), the one
  in-flight commit, per-socket replay cursors and buffers, and the wake agenda.
- Today's alarm runs six steps that each persist; the host folds expiry, recovery and dispatch into
  one batch per wake.

**Components of `tessel-server`** (one binary, Rust):

- The host, one task per repo, on SQLite.
- Identity: mints and verifies agent tokens, replacing the steward's minting and
  `tessel init --steward`.
- The lander, a separate process: deterministic rebase, coverage check against the claim, gate
  checks that need only git (protected paths such as `tessel.toml` and CI configuration), and the
  compare-and-swap push. A port of `merge-steps.ts`, `merge-coverage.ts` and `rebaseCommand`.
- A local test runner, for one trusted user on their own machine only, which runs the repository's
  test command against the rebased commit. Teams use their CI instead (Phase 2).
- The git host adapter: a git remote reachable by path or URL first, GitHub in Phase 2.

**The smallest deployment a team would accept:** `tessel-server` with SQLite, a GitHub App, the
repository's existing CI, and the `tessel` CLI. No sandbox and no git hosting.

**Advisory mode.** Tessel coordinates claims while humans and CI still merge. The minimal version
(Phase 1): head sync releases a submitted claim once its commit is an ancestor of main, which works
with merge commits. The GitHub version (Phase 3): a required `tessel/claims` check on each pull
request, and landings learned from the pull request's `merge_commit_sha`, so squash and rebase
merges work too.

## The GitHub adapter

1. **Least privilege.** The lander App has contents and pull requests write, checks and actions
   read, and metadata, installed on the target repository only. No App has `administration`.
2. **Forks are provisioned by a human** once, as a pool of private organisation forks, with
   `tessel admin forks provision N` under that person's credentials: an App can create forks only
   with access to every repository in the organisation. Leases reset fork branches rather than
   create repositories.
3. **Agents push** to their own fork with a one-hour token scoped to that fork: contents write
   only, no `workflows`.
4. **The lander** checks coverage and that CI, test and build paths are untouched or flagged,
   rebases deterministically, pushes the candidate to the fork's pull request branch, and opens or
   updates one pull request per claim.
5. **CI runs as `pull_request` from the fork**: no secrets, a read-only token.
6. **Two rulesets on main.** Ruleset A restricts updates and blocks force pushes, with only the
   lander App allowed to bypass it. Ruleset B requires the checks, pinned to the GitHub Actions
   app, and linear history, with no bypass, so GitHub itself enforces that checks passed on
   exactly the landed commit.
7. **Landing** is the GraphQL `updateRefs` mutation with `beforeOid` set to the base and `afterOid`
   to the tested head: an atomic compare-and-swap (the REST ref update only enforces
   fast-forward). The head is journaled first (correctness requirement 6) and the outcome is read
   back.
8. **Reconciliation polls** check suites and refs with conditional requests and is authoritative.
   Webhooks are signed hints to poll sooner; GitHub does not redeliver failed webhooks on its own,
   so lost ones are recovered by the poll.
9. **One serialized mutation queue per installation**, under GitHub's secondary limits (80
   content-creating requests a minute, 500 an hour).
10. **Prerequisites**, checked by `tessel admin doctor`: rulesets on private repositories need a
    paid plan; CI must trigger on `pull_request`; fork pull request settings must withhold secrets
    and write tokens; whether the team requires signed commits (a rebased commit is unsigned).
11. **No GitHub merge queue in Phase 2**: it creates its own commits, never checks a fence, and
    gives no per-item result. It can come later as its own adapter with its own reconciliation.

**The spike** (Phase 0) answers, with a working script against a test organisation: does a check
run on a fork pull request's head satisfy ruleset B for a direct `updateRefs` of that commit; does
`updateRefs` refuse a stale `beforeOid` with the App as ruleset A's bypass actor; how long checks
and webhooks take; and so whether landing is one request or a durable job finished by
reconciliation. The host's `Call` design is frozen only after it.

## Evidence

Only verified outcomes count, and the dashboard says exactly what was verified. On GitHub a landing
records conditional facts: check name, app id, event (`pull_request`), the workflow file's blob
(equal to main's), the head commit and the conclusion. The dashboard says "required checks
passed", never "verified safe". Test, build and dependency-manifest paths are review-flagged,
because an agent can weaken the tests that judge it. Verifications dropped because their claim
ended first are logged as `VerificationDropped` and counted, not silently discarded.

## Fixes to the merge path

| Problem | Where | Fix |
|---|---|---|
| A stale answer is applied to a later dispatch | `merge_outcome` matches the claim only (`merging.rs`); `request_body` does not send the attempt (`merge.rs`) | Phase 0, core: a persisted dispatch id sent with every call and matched on every outcome |
| `already_merged` becomes `Landed { head: base }`, so later verifications compare main with itself | `merge.rs` | Phase 0, core: the outcome carries the landed commit; no verification or trial is queued before it is reconciled |
| Verifications starve under continuous merges, and dropped ones vanish | `begin_verification` refuses while a merge is due (`verifying.rs`) | Phase 0, core: after N merges (a setting, default 4), the next merge waits if the head verification is ready and not in backoff; `begin_merge` checks for a verification in flight; `VerificationDropped` events |
| An orphaned call and its retry both report `merged`, doubling the evidence | `pushToMain` reports `merged` whenever main equals the head, whoever pushed it (`merge-steps.ts`) | Removed by design: the Rust lander knows its own push result, and dispatch fencing covers the rest |
| A push just before an admin merge reads as `main_moved` | `merge-steps.ts` | Removed by design: landing is decided by ancestry of the journaled commit |

The TypeScript steward itself is not fixed: nothing runs it, and Phase 1 replaces it.

## How it is verified

**CI lanes** (GitHub Actions pinned to SHAs, checked with zizmor and actionlint):

- Fast and required, under three minutes: fmt; clippy with `-D warnings`, native and wasm until
  the Cloudflare code is removed; core library tests; a time-boxed simulator run;
  `cargo mutants --in-diff` on the core library with no missed mutants on changed lines.
- Slow and required: CLI and swarm suites through cargo-nextest partitions; the steward's tests
  until it is removed; `cargo deny check advisories bans`.
- Nightly: proptest at 10,000 cases, a time-boxed simulator seed sweep that keeps failing seeds as
  regression cases, and full mutation runs on the core.

**Rule 9 is enforced** by the per-PR mutation check above, and by a pull request template field
for the output of the new test failing before the change.

**Characterisation by a behaviour-preserving move.** `runtime.rs` builds only for wasm and nothing
is deployed, so it is not tested in place. Phase 1 starts by moving it into the host step for step,
including today's quirks (a failed persist drops the core); its documented rules become named host
tests; the CLI end-to-end suite runs against the host as wire-level characterisation. Each intended
behaviour change is then its own commit that flips a named test. Optionally, the same CLI scripts
run against `wrangler dev` with a stub steward and the transcripts are compared before the
Cloudflare code is deleted.

**Deterministic simulation of the host.** A seeded simulator (in the core crate, with SplitMix64
moved there from `tessel-swarm`) drives the host with:

- a simulated clock that can jump;
- a store that loses uncommitted writes on crash and can fail a commit without crashing (busy,
  disk full);
- a merge path that is asynchronous like CI: it delays, reorders, duplicates and drops answers;
  times out; pushes then loses its reply; reports checks for stale commits; flips a result on
  re-run; never reports; and returns rate-limit and compare-and-swap refusals.

It crashes after any output and reloads. These invariants are checked on what clients actually
received, after every step, every reload and every migration:

- I1: every grant or event a client received is in durable state after reload.
- I2: each watcher receives exactly `log[from..]` in order, with no gap or duplicate, or
  `Truncated` when `from` is below the retained floor; a `Hello` mid-replay does not change this.
- I3: at most one `Merged` per claim; each submission at most once on main's first-parent history;
  at most one evidence record per landed commit.
- I4: if work is due, exactly one wake is pending, and it is in the future.
- I5: state round-trips through the real store codec and every migration.
- I6: a commit that cuts off the merge path every time is rejected after `MAX_INFRA_RETRIES`, and no
  other claim is rejected because of it.
- I7: an outcome is applied only for the dispatch id in flight.
- I8: no two conflicting claims are ever granted at once.
- I9: a fence, claim id or sequence number is never issued twice, and a stale fence is never
  accepted, across reloads, migrations and restores.
- I10: a submission held for review is never dispatched; an uncovered submission never gets
  `Merged`; a lease never extends without a heartbeat.
- I11: incremental counters equal `Summary::from_events` over archive and live log, and evidence
  cites checks on exactly the tested commit.
- I12: a ready verification is dispatched within N merges; `rank_entries` gives the same result
  whatever the order of its input.

**Failpoints, not timing**, for each crash window of a merge: dispatch persisted; lander running;
head journaled; pushed but outcome not persisted; outcome persisted but not delivered; during
rescheduling.

**The `tessel-server` adapter suite**: a kill -9 loop with clients' transcripts checked; the store
lock; a slow watcher; a panicking task; injected store errors; WebSocket framing and close codes;
an old-release CLI against the new server.

**Property tests of the merge queue** interleave every outcome (`Merged`, `Conflict`,
`TestsFailed`, `ServiceUnavailable`, `MainMoved`, `AlreadyMerged`, cut-off, watchdog) with claims,
leases, reviews and races, which today's tests do not generate.

**Model checking** with stateright over the real `Coordinator`, used only with a canonical
fingerprint (ids renumbered, time abstracted to ordered expiries, the log left out), symmetry over
agents, bounds of three agents, two files and three submissions, and retry bounds lowered through
configuration. Without these it is a slow proptest, and it is dropped.

**Conformance suites per port**, against the local adapters and a GitHub test organisation on a
paid plan: two racing compare-and-swap landings, exactly one wins; a stale `beforeOid` is refused;
a spoofed status is refused by ruleset B; a workflow-file push is refused for a token without
`workflows`; a redelivered or out-of-order webhook changes nothing; a token is refused for another
repository and after revocation; no secret appears in any output; rebasing the same inputs on two
machines gives identical commits.

**Flake control**: real sleeps and wall-clock deadlines in the CLI, swarm and support tests are
replaced with a clock seam in the daemon (an injected time source) and explicit signals.

Swarm A/B runs remain measurements, never correctness evidence.

## Requirement to test

| Requirement | Test |
|---|---|
| C1 persist before send | I1; kill -9 loop |
| C2 one writer | Adapter suite: second process refused; one outstanding commit in the host |
| C3 replay | I2 in the simulator; paged replay in the adapter suite |
| C4 sends never block | Adapter suite: slow watcher |
| C5 asynchronous fenced calls | I7; failpoints; adapter suite: panicking task |
| C6 landing by ancestry | I3; failpoints before and after the push |
| C7 deterministic rebase | Conformance: identical commits on two machines; a pinned-commit unit test |
| C8 persist retry | Simulator: commit failure without a crash |
| C9 wakes | I4 |
| C10 leases | I10; a crash loop shorter than the lease still expires dead claims |
| C11 no reissue | I9, including restores |
| S1 authenticated boundaries | Conformance: unauthenticated, spoofed, replayed, wrong `aud`, rotated `kid`, reserved names |
| S2 no secrets for agent code | Conformance on GitHub; `doctor` tests |
| S3 lander isolation | Lander tests with hooks, filters and malformed packs present |
| S4 free text | Redaction property test; webhook signature tests |
| S5 tenant isolation | Name-collision property test |
| O1 versioned state | I5; fixtures; a newer version refused |
| O2 protocol range | An old-release CLI against the new server |
| O3 observability | Metrics contract test; trace id carried through |
| O4 bounded growth | I2 with retention; I11; heartbeats within a lease cause at most two writes |
| O5 recovery | Admin command tests; force-release retires the fence; restore drill with I9 |
| O6 failure behaviour | Hook tests with the server unreachable |

## Phases

Sizes are rough person-weeks for one maintainer working with agents.

**Phase 0: foundations (about 3 weeks).**
1. CI lanes, required on main.
2. The GitHub spike. Done: a written answer and a working script against a test organisation.
3. Merge-queue property tests over every outcome. Done: green at 10,000 cases nightly.
4. Core fixes: dispatch id, landed commit on `already_merged`, verification slot and
   `VerificationDropped`. Done: property tests showing that a stale answer changes nothing, that no
   verification compares main with itself, and that verifications run under continuous merges.

**Phase 1: portable host, `tessel-server` and the Rust lander (about 10 to 12 weeks).**
1. Move `runtime.rs` into the host step for step; its rules become named tests; the CLI end-to-end
   suite passes against it.
2. Intended changes, one commit each: persist retry, exact replay, durable call deadlines with
   reconciliation, lease persistence at half the lease.
3. The simulator with I1 to I12, and the failpoints. Done: a seed sweep passes in CI.
4. `tessel-server`: axum, a task per repo, SQLite, the store lock, `state_version` and migrations,
   the protocol range, metrics and logs, and native identity with Ed25519 keys and token minting.
   Done: the adapter suite passes.
5. The Rust lander and the local test runner. Done: the steward's real-git integration cases,
   ported, pass.
6. Minimal advisory mode. Done: this repository is coordinated by `tessel-server`, with claims
   released when their pull requests merge.
7. Both native test hosts rebuilt on the host; the Cloudflare and TypeScript code deleted.

**Phase 2: GitHub landing (about 5 to 7 weeks).** The adapter above, `tessel admin doctor` and
`tessel admin forks provision`. Done: the conformance suite passes against the test organisation.

**Phase 3: adoptable.**
- Trials on GitHub: shadow and assumption verification as two-sided CI runs on fork pull requests.
- The GitHub advisory mode: the `tessel/claims` check, and landings from `merge_commit_sha`.
- The review page and dashboard served by `tessel-server` behind OIDC; the server verifies iss,
  aud, exp and `email_verified` itself.
- Per-repo review configuration read from trunk.
- The rest of `tessel admin`; backup, restore drill and retention.
- Signed release binaries and an image.

**Phase 4: reach.** Symbol claims for Python and Go; hooks for a second agent harness; Linux and
macOS packages.

**Phase 5: design partners.** Real teams; queue wait, CI duration and merge outcomes measured.

**Phase 6, only if the data or a user calls for it:** a native parallel merge queue; a self-hosted
git host and sandbox; several nodes.

## Deferred designs

**Parallel merge queue (K > 1)**, for self-hosted use:

- Select up to K submissions in order, skipping held-for-review ones and stopping at one in
  backoff. Pin main once per round.
- Test main+A, main+A+B and main+A+B+C in K sandboxes. Each sandbox reports its item-boundary
  commits before any repo code runs; a disagreement is an infrastructure failure.
- Land items 1 to j, where stacks 1 to j all passed, with one compare-and-swap push from a fresh,
  code-free environment that redoes the rebase and checks it gets the tested commit.
- The first item that did not pass gets its stack's outcome; later items return to the queue in
  order, attempt counts unchanged. A failure of the whole round is charged to item 1, and the next
  round runs with K = 1. `main_moved` counts against item 1 only.
- Each landed item gets its own base, head, coverage check and evidence record; an item empty on
  its stack gets none. Round state is persisted before dispatch and each stack's head journaled.
- Fetch only submitted commits, never whole fork branches. Per-agent and per-tenant concurrency
  limits.
- Verified by a differential property (K = 4 and K = 1 give identical landings, attributions and
  baselines when outcomes depend only on stack contents) and by stateright at K in {1, 2, 3}.

**Self-hosted sandbox and git host:**

- The sandbox's only route is its own proxy instance, default-deny including DNS, link-local, the
  metadata address and the host; the proxy runs the gateway policies.
- gVisor required. Per run: fresh, all capabilities dropped, no-new-privileges, read-only root,
  limits on pids, memory, CPU and disk, no shared writable caches. Orphans are reaped. Sandboxes
  run on a separate host or VM from secrets and repositories, driven by a narrow runner daemon
  instead of the Docker socket.
- The git host runs as its own sandboxed process: `transfer.fsckObjects`, no hooks, quotas, no
  shared object stores across tenants, tokens stored hashed, revocation immediate.
- Images pinned by digest and signed; no network or builder secrets while building toolchain
  images; package registries never on the egress allowlist.

**Several nodes**: the ownership epoch is checked in the same transaction as every commit, carried
in every dispatch (the lander refuses lower epochs), and modelled in stateright with two nodes,
lease expiry and bounded clock skew before it is built.

## Decisions (17 October 2026)

Recorded in `PLAN.md`, section 9: the plan and order are approved; Cloudflare has no special status
and its code is removed as soon as the portable host replaces it; the GitHub adapter is Rust inside
`tessel-server`; the protocol version range is a new optional field; the contest-era licence
whitelist no longer applies.

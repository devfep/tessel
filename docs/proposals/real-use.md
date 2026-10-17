# Proposal: Tessel for real use

Status: approved by Felix on 17 October 2026, after four adversarial reviews (correctness,
security, testing, real use and operations). `PLAN.md` summarises it and records the decisions.

The challenge is over. Tessel is now being built for teams who would trust it with their main
branch, so it has to be robust, well tested, and fit how teams already work. This proposal says
what that takes, in what order, and how each step is verified.

## What the reviews changed

The first draft put portability first: a self-hosted git server and sandbox, then a parallel
merge queue. The reviews found that order wrong for real use, and found bugs in today's code that
any plan has to fix first:

- **Teams keep their code on GitHub (or GitLab) and run their own CI.** Tessel should land work
  there, gated by the repo's own required checks, before it hosts git or runs tests itself.
- **Today's merge path has correctness bugs** that a port or a parallel queue would make worse
  (see "Fixes to today's merge path").
- **Nothing is enforced automatically.** There is no CI, `src/runtime.rs` has no tests, and the
  orchestration it holds is about to be moved.
- **The parallel merge queue is premature.** GitHub and GitLab already test combined changes in
  parallel. A native one waits until real queue metrics show the need.
- **Upgrades, observability and recovery** are missing, and a team needs them before anything else.

## Where Tessel stands

The core is portable. `protocol`, `coordinator` (`merging`, `racing`, `verifying`, `trunk`),
`shell` and `merge` build natively without the `runtime` feature. Two native hosts drive the real
core over tokio: `tessel-swarm/src/local.rs` and the CLI's `tests/support/mod.rs`.

What holds it back:

| Area | Today | Why it matters |
|---|---|---|
| Hosting | Durable Object per repo (`runtime.rs`, 1,078 lines, 0 tests) | Its rules depend on Durable Object input gates, documented only in comments |
| Native hosts | Two copies of the orchestration, no persistence, no recovery; cherry-pick instead of rebase; no coverage check | Neither tests the real merge path; neither calls `recover_merge`, head sync, `merge_cut_off` or `load_core` |
| Git and tests | Cloudflare Artifacts and Containers; the gate allows only `npm test`, `pnpm test` or fixed `cargo test` flags, with dependencies baked into an image | A team cannot rebuild an image for every lockfile change |
| CI | None (no `.github/`, no hooks config) | "Tests pass before every commit" is not enforced |
| Stored state | One JSON blob under `core`, rewritten on every call, no version field | An upgrade that changes the shape breaks every repo at once |
| Protocol | `PROTOCOL_VERSION` must match exactly | Every CLI and server must upgrade in lockstep |
| Observability | Two `console_log!` calls | No queue depth, merge durations, or failure rates |
| Event log | Grows forever; `Watch {from_seq: 0}` replays all of it; heartbeats rewrite the core | Disk and write load grow without bound |
| Identity | One symmetric HMAC key shared by both services, 24 h tokens, no revocation, `steward` not reserved | A compromised service can mint any identity |
| Steward entry | `MergeService` trusts `scopes` from the caller | Safe only while it is a private binding |

## Requirements

Every item here becomes a test (see "How it is verified"). They were properties of the Durable
Object or of Cloudflare; a portable host has to provide each one explicitly.

**Correctness**

1. **Persist before send** (rule 6). Only `store::write` can build a `Persisted`, after a durable
   commit (SQLite `synchronous=FULL` in WAL mode, or a Postgres commit).
2. **One writer per repo,** enforced: one task owns each repo's `Core`, and the server takes an
   exclusive lock on its store so a second process on the same file refuses to start.
3. **Replay is not interleaved.** No other message is applied to a repo while a watcher's replay
   streams.
4. **Sends never block the repo.** A slow watcher gets a bounded buffer; overflow closes its
   socket. A steward call never holds a lock, and a long call does not stall agents.
5. **Each outstanding steward call is tied to a live task.** The in-flight marker is held by a
   guard owned by the call's task, so a panicked or dropped call is detected as cut off rather
   than wedging the queue. A repo task is never evicted while a call is outstanding.
6. **Steward calls are fenced.** Every dispatch carries a persisted, monotonic dispatch id. The
   steward holds a per-repo lease keyed by it, refuses a second concurrent merge for the repo, and
   has its own push deadline shorter than the coordinator's timeout. Outcomes are matched on the
   dispatch id, not only the claim.
7. **Landing is decided by ancestry, never by merging again.** Whether a commit landed is
   `merge-base --is-ancestor <landed commit> main`, using the commit the steward journaled before
   pushing. Until reconciled, no verification or shadow trial is queued for it.
8. **Rebases are deterministic, and checked.** The steward rebases with a fixed committer
   (`rebaseCommand`). The same inputs must give the same commit IDs on every supported image; a
   test pins that.
9. **A failed persist is retried** with the answer still in memory, not turned into a cut-off.
10. **Wakes are explicit.** No two wakes for a repo overlap; a failed wake is retried with a
    bounded policy; after every step, if work is due, a wake is scheduled.

**Security**

1. **No endpoint acts on an unauthenticated request.** Service-to-service calls use mTLS or
   signed requests (HMAC over method, path, body hash and timestamp, with a nonce cache) under a
   key separate from agent identity. The steward takes fork and scopes from the coordinator's
   persisted dispatch, never from the request body.
2. **Asymmetric identity keys.** Ed25519, signed by one service, verified with the public key
   elsewhere; `kid` with overlapping rotation; `aud` and tenant claims; reserved service names
   (`steward` and others); a deny-list so a token, key or route is unusable the moment revocation
   returns.
3. **Agent code never runs where secrets live.** Forks on GitHub are separate repositories with
   Actions disabled, and CI configuration paths are gate-protected the way `tessel.toml` is, so a
   fork push cannot run workflows with repo secrets.
4. **Free text stays data.** New surfaces (round results, webhook payloads, test output from other
   agents' code, attacker-named file paths) stay bounded and quoted, as `merge::tests_reason`
   does; webhooks are signature-verified and used only as triggers; token-shaped strings are
   redacted before free text is broadcast.
5. **Tenant isolation**: structured `(tenant, repo)` ids instead of `<repo>--<agent>` string
   joins, lowercase-only names, exact source equality, and per-repo reviewers, admin scope and
   quotas.
6. **Self-hosted sandbox and git** (only if built, see Phase 6) carry their own list, under
   "Deferred designs".

**Operations**

1. **Versioned state.** A `state_version` with a migration chain, and fixtures of every released
   state shape that each release must load.
2. **Protocol range.** The server accepts a range of protocol versions and reports its own; the
   CLI warns when it falls outside.
3. **Observability.** `/metrics` (queue depth and wait, merge duration by outcome, gate outcomes,
   lease expiries, denials, state size against its limit), structured logs keyed by repo, claim
   and seq, and one trace id from coordinator through steward to the gate.
4. **Bounded growth.** Retention with an archive export; summary counters kept incrementally;
   lease renewals held in memory (every claim gets a fresh lease after a restart) so heartbeats
   stop rewriting the core; size limits derived from the store, not inherited from Cloudflare.
5. **Recovery tooling.** A `tessel admin` CLI: pause and drain the queue, force-release a claim,
   revoke an agent or token, rotate keys, export and import a repo. A restore drill: an older
   database against a newer main, reconciled by head sync.
6. **Documented failure behaviour**: what agents' hooks do when the coordinator is unreachable,
   and who can change it.

## Architecture

**A sans-IO host.** The orchestration in `runtime.rs` moves into a portable step machine in the
core crate: `Host::step(Input) -> Vec<Output>`. Inputs: a client message, a socket closed, a wake,
a steward reply, a commit completed. Outputs: commit these entries, send to these sockets, set
the next wake, call the steward. Adapters do only I/O. This is what makes the simulation tests
below possible, and it replaces the three copies of the orchestration with one.

**Adapters, in the order a team needs them:**

| Port | First | Later, only on demand |
|---|---|---|
| Host | `tessel-server`: axum, one task per repo, SQLite | Postgres with an ownership epoch, several nodes |
| Git host and gate | GitHub App: candidate ref, the repo's required checks (or GitHub's merge queue), compare-and-swap landing | Generic git remote; GitLab merge trains; git on disk |
| Tests | The repo's own CI | Tessel's own sandbox |
| Push events | GitHub webhooks (signed, trigger-only) | Outbox table, NATS JetStream, SQS FIFO |
| Viewer auth | OIDC verified by the server itself (iss, aud, exp, `email_verified`), behind oauth2-proxy | Cloud identity providers |

**The smallest deployment a team would accept:** one `tessel-server` binary with SQLite, a
GitHub App, the repo's existing CI, and the `tessel` CLI. No sandbox and no git hosting.

**Advisory mode first.** A repo can adopt claims while humans and CI still merge pull requests as
today. Tessel only lands work once the team turns that on, with branch protection so only the
Tessel App writes main, compare-and-swap pushes only (never a force push), a documented uninstall,
and evidence taken from CI check results instead of the agent's self-reported `--evidence`.

## Fixes to today's merge path

These exist now, in the serial queue, and are fixed before anything is built on it:

- **A timed-out or cut-off steward call keeps running** while a retry starts (each merge runs on
  a fresh randomly named runner, `merge-request.ts`). If both land the same deterministic commit,
  `pushToMain` reports `merged` twice and two evidence records are written, which overcounts
  (rule 7). Fixed by requirement 6 (dispatch fencing).
- **Recovery re-merges to find out whether work landed.** `rebaseCommand` replays onto main
  without cherry-pick detection, so work already on main can conflict with later commits and be
  rejected, and `already_merged` reports `head = base`, which makes the following verifications
  and shadow trials meaningless. Fixed by requirement 7 (ancestry, journaled commit).
- **A push that lands just before an admin merge** is read back as `main_moved`. Also fixed by
  requirement 7.
- **Verifications starve** while merges are continuously due (`verifying.rs`), and claims that end
  meanwhile drop theirs silently. A verification slot is reserved every N merges.

## How it is verified

**CI before any refactor.** GitHub Actions, pinned to SHAs and checked with zizmor and
actionlint: fmt; clippy with `-D warnings`, native and `wasm32-unknown-unknown --features runtime`;
cargo-nextest for the workspace; `cargo deny check advisories bans`; the steward's typecheck,
oxlint and vitest, including the real-git integration tests. Nightly: proptest at 10,000 cases,
a simulator seed sweep, and cargo-mutants on changed files with a minimum kill rate.

**Characterisation tests first.** Before moving `runtime.rs`, one test per behavioural rule it
documents, as the list the new host must pass.

**Deterministic simulation of the host.** A seeded simulator (in-crate, on proptest and the
existing SplitMix64, no runtime dependency) drives `Host::step` with a simulated clock that can
jump, a store that loses uncommitted writes on crash, and a steward that delays, reorders,
duplicates, drops, times out, or pushes and then loses its reply. It crashes after any output and
reloads. 10,000 seeds in CI, a million nightly. After every step and every reload:

- I1: every grant or event a socket received is in durable state after reload.
- I2: each watcher receives exactly `log[from..]` in order, with no gap or duplicate, even with a
  `Hello` arriving mid-replay.
- I3: at most one `Merged` per claim, and each submission appears at most once on main's
  first-parent history; at most one evidence record per landed commit.
- I4: if work is due, a wake is scheduled.
- I5: state round-trips through save and load at every step.
- I6: a commit that cuts off the steward every time is rejected after `MAX_INFRA_RETRIES`, and no
  other claim is rejected because of it.
- I7: an outcome is applied only for the dispatch id that is in flight.

**Failpoints, not timing,** for each crash window of a merge: marker persisted; steward running;
pushed but outcome not persisted; outcome persisted but not delivered; during rescheduling.

**Property tests of the merge queue.** Interleave every outcome (`Merged`, `Conflict`,
`TestsFailed`, `ServiceUnavailable`, `MainMoved`, `AlreadyMerged`, cut-off, watchdog) with
claims, leases, reviews and races. Today none of them is generated.

**Model checking** with stateright (MIT), driving the real `Coordinator` with three submissions
and a nondeterministic steward and compare-and-swap push.

**Conformance suites per port,** the same cases against every adapter: two racing
compare-and-swap pushes, exactly one wins; a redelivered or out-of-order event changes nothing; a
token is refused for another repo and after revocation; unauthenticated or spoofed calls are
refused; no secret appears in any output; rebasing the same inputs on two images gives identical
SHAs. Toxiproxy (MIT) partitions between server and steward (pushed, reply lost) end reconciled,
never double-landed.

**Mutation testing**: cargo-mutants on `coordinator/merging.rs`, `shell.rs` and the host; Stryker
on `merge-steps.ts`, `merge-coverage.ts` and the gateway policies.

**Flake control**: replace real sleeps and wall-clock deadlines in the CLI, swarm and support
tests with the simulated clock or explicit signals; track flakes in nextest.

Swarm A/B runs remain, reported as measurements, never as correctness evidence.

## Phases

Each phase is done when its checks pass in CI.

**Phase 0: foundations.** CI and nightly jobs; characterisation tests for `runtime.rs`; property
tests for the merge queue; the four fixes to today's merge path; `state_version` with migrations
and fixtures; the protocol version range; metrics and structured logs.
Done when: CI is green and required on main, the property and characterisation suites pass, and
the dispatch-fencing and ancestry fixes are covered by failpoint tests.

**Phase 1: portable host and `tessel-server`.** The sans-IO host, the simulator (I1 to I7), the
Durable Object and both native hosts rebuilt on it, `tessel-server` with SQLite and the store lock.
The native test hosts use the real rebase and coverage path.
Done when: the simulator passes its seed sweep, every failpoint test passes against
`tessel-server`, and the swarm runs against it.

**Phase 2: GitHub.** The GitHub App adapter (forks as separate repos with Actions disabled,
candidate ref, required checks or the merge queue, compare-and-swap landing, check results as
evidence), service authentication and the new identity keys (security requirements 1 to 5).
Done when: the conformance suite passes against GitHub (a test organisation) and the local fake.

**Phase 3: adoptable.** Advisory mode; per-repo review configuration read from trunk
(`REVIEWERS`, `SENSITIVE` move out of environment variables); the admin CLI; backup, restore
drill, retention and archive; signed release binaries for the CLI and server, and an image.

**Phase 4: reach.** Symbol claims for Python and Go; hooks for a second agent harness; Linux and
macOS packages.

**Phase 5: design partners.** Run with real teams; collect queue wait and merge duration.

**Phase 6, only if the data or a user calls for it:** a native parallel merge queue; the
self-hosted git host and sandbox; several nodes on Postgres. Their designs are below so the
earlier phases do not close them off.

## Deferred designs

**Parallel merge queue (K > 1).** On GitHub, Tessel hands its ordered, claim-checked submissions
to the host's merge queue instead. A native one, for self-hosted use, follows these rules, each
from a review finding:

- Select up to K submissions in order, skipping held-for-review ones and stopping at one in
  backoff. Pin main once per round.
- Test main+A, main+A+B, main+A+B+C in K sandboxes. Each sandbox reports its item-boundary SHAs
  before any repo code runs; a disagreement is an infrastructure failure.
- Land items 1 to j, where stacks 1 to j all passed, with one compare-and-swap push from a fresh,
  code-free environment that redoes the deterministic rebase and checks it gets the tested SHA.
- The first item that did not pass gets its stack's outcome; later items return to the queue in
  order, attempt counts unchanged. A failure of the whole round (steward error, timeout) is
  charged to item 1, and the next round runs with K = 1. `main_moved` counts against item 1 only.
- Each landed item gets its own base and head, coverage checked against the item before it, and
  its own evidence record; an item empty on its stack gets none.
- Round state `{dispatch id, base, items}` is persisted before dispatch and each stack's head is
  journaled, so recovery reconciles by ancestry.
- Fetch only submitted SHAs, never whole fork branches, so tests cannot read other agents'
  unsubmitted work. Per-agent and per-tenant concurrency limits bound sandbox cost.
- Verified by a differential property: with outcomes a deterministic function of stack contents,
  K = 4 and K = 1 produce identical landed sequences, `Merged` pairs, rejection attribution and
  verification baselines; and by stateright at K in {1, 2, 3}.

**Self-hosted git and sandbox.** Required before it ships:

- The sandbox's only route is its own proxy instance (one network namespace and proxy per run),
  default-deny including DNS, link-local, the metadata address and the host; the proxy runs the
  existing gateway policies.
- gVisor required (refuse to start without `runsc`); per run: fresh, `--rm`, all capabilities
  dropped, no-new-privileges, read-only root, limits on pids, memory, CPU and disk, no shared
  writable caches; orphans reaped on restart; sandboxes on a separate host or VM from secrets and
  repositories, driven by a narrow runner daemon instead of the Docker socket.
- The git host runs as its own sandboxed process: `transfer.fsckObjects`, no hooks, quotas, no
  shared object stores across tenants, tokens stored hashed, revocation immediate.
- Images pinned by digest and signed; no network or builder secrets while building toolchain
  images for arbitrary repos; package registries never on the egress allowlist.

**Several nodes.** The ownership epoch is checked in the same transaction as every commit, carried
in every steward dispatch (the steward refuses lower epochs), and modelled in stateright with two
nodes, lease expiry and clock skew before it is built.

## Decisions (17 October 2026)

Felix approved the plan and its order. The decisions are recorded in `PLAN.md`, section 9:
Cloudflare has no special status and its code is removed as soon as the portable host replaces it;
the GitHub adapter is Rust inside `tessel-server`; the protocol version range is a new optional
field; the contest-era licence whitelist no longer applies; `CLAUDE.md` and `PLAN.md` are rewritten
for real use.

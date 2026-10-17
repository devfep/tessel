# Proposal: run Tessel anywhere, and merge in parallel

Status: draft for Felix's decision, 17 October 2026. Nothing here changes `PLAN.md` until approved
(CLAUDE.md rule 9).

Tessel was built for Cloudflare's challenge and runs only on Cloudflare. This proposal makes
Cloudflare one way to deploy it among several, and removes the limit swarms hit first: the merge
queue lands one submission at a time.

## What is asked

1. Approve a post-challenge plan with four phases (below), replacing the contest schedule in
   `PLAN.md`.
2. Decide the open questions at the end.

## Where Tessel stands

The core is already portable. `protocol`, `coordinator` (with `merging`, `racing`, `verifying`,
`trunk`), `shell` and `merge` build natively without the `runtime` feature, and two native hosts
already drive the real core over tokio: `tessel-swarm/src/local.rs` (a WebSocket server on
127.0.0.1 with a git steward) and the CLI's test support. Neither persists state or recovers from a
restart.

What is tied to Cloudflare:

| Part | Cloudflare primitive | What Tessel relies on |
|---|---|---|
| `src/runtime.rs` | Durable Object per repo, alarms, hibernating WebSockets | One live instance per repo, one thread; alarms never overlap and retry on error; sockets survive eviction |
| `src/store.rs` | Durable Object storage transaction | Atomic multi-key commit; keys listed in `seq` order |
| `runtime.rs` → steward | Service binding to `MergeService` | A private channel, so `MergeService` has no authentication |
| `tessel-steward` | Artifacts | Create and fork repos, read files, repo-scoped short-lived tokens, a push accepted only if main is still where the merge started |
| `tessel-steward` | Containers | A fresh sandbox per run, no Internet, tokens added outside the sandbox |
| `tessel-steward` | Queues | Artifacts push events; a trunk move pokes the coordinator to re-read its head (`coordinator/trunk.rs`) |
| Dashboard | Cloudflare Access | Viewer identity for `/dashboard` and `/review` |

Smaller assumptions sit in portable code: size limits in `shell.rs` match Durable Object storage and
socket attachments, and `merge::STEWARD_CALL_TIMEOUT_MS` (13 minutes) is sized to the 15-minute
alarm limit. `identity.rs` has no Cloudflare imports; it is feature-gated only because its hashing
crates are optional dependencies.

`runtime.rs` (1,078 lines) is the weak seam. It mixes portable orchestration (persist-then-send,
merge and verification dispatch, cut-off recovery, the watchdog, head sync) with Cloudflare calls.
`local.rs` and the CLI's `tests/support/mod.rs` each reimplement that orchestration again, without
persistence, and their stewards cherry-pick instead of rebasing and skip the coverage check on the
merged diff (invariant 11), so neither merges the way the real steward does.

## Guarantees a port must keep

These hold today partly because of Durable Object semantics, so each needs an explicit answer:

1. **Fences persisted before grants are sent** (rule 6). Already enforced by types: only
   `store::write` can build a `Persisted`, after the commit. A native store must commit durably
   (SQLite `synchronous=FULL`, or a Postgres commit) before returning it.
2. **One writer per repo.** All messages, alarms and steward answers for a repo are applied one at a
   time. A native host gives each repo one task that owns its `Core`.
3. **Replay is not interleaved.** The CLI treats the first event after `Welcome` as the end of
   replay. The repo task must not apply other messages while it streams the log.
4. **A long steward call does not block the repo.** A merge call can take up to 13 minutes. The task
   must keep handling agents while it waits, and must not hold a lock across the call.
5. **"Is a merge still running?"** is answered today by memory: a stored in-flight marker with no
   local call means the instance restarted (`shell::merge_cut_off`). That is only true with one
   instance per repo. Several nodes need an ownership epoch instead.
6. **Pushes to main are compare-and-swap.** Merge idempotency (`already_merged` after a timeout)
   depends on it. Any git host must offer an atomic conditional ref update.
7. **Rebases are deterministic.** The steward rebases with a fixed committer name, email and date
   (`merge-commands.ts`, `rebaseCommand`), so the same inputs give the same commit IDs in any
   sandbox. The parallel merge queue below depends on this; any port of the git steps must keep it.
8. **No token inside the sandbox.** No network from the sandbox, and an egress proxy that adds
   credentials, as the steward's gateways do now.
9. **Private endpoints become authenticated.** The verified-agent header between the Worker and the
   Durable Object, and `MergeService`, are safe only because nothing outside can reach them.

## Target shape

Ports in the core crate, one adapter per platform:

- **Coordinator host** (Rust): `EventStore` (load, atomic commit returning `Persisted`, read events
  in order), `Scheduler` (set and clear the next wake), `Clock`, `Connections` (list, send, close,
  per-socket session), `StewardClient` (merge, trial, head). Authentication stays
  `identity::verify_bearer` with an injected key.
- **Steward** (TypeScript): `GitHost` (create, fork, read, mint and revoke repo tokens,
  compare-and-swap push), `Sandbox` (start, exec, destroy, egress handler), `PushEvents`. The merge
  logic is already written against `MergeDeps` and `TrialDeps` (`merge-steps.ts`) and tested with
  real git, so this is mostly moving Cloudflare calls behind those interfaces.

Adapters, in the order they are needed:

| Port | Cloudflare (today) | Self-hosted | Later, on demand |
|---|---|---|---|
| Coordinator host | Durable Object | `tessel-server`: axum, one tokio task per repo, SQLite | Postgres with an ownership epoch, several nodes |
| Git host | Artifacts | Bare repos on disk behind the steward's own gateway | Gitea (MIT), GitHub App tokens |
| Sandbox | Containers | Docker with gVisor (`runsc`), no network, egress proxy | Kata Containers, Firecracker, Fly Machines, Modal, E2B |
| Push events | Queues | Outbox table in the same database | NATS JetStream, SQS FIFO |
| Viewer auth | Access | oauth2-proxy (MIT) with any OIDC provider, or Dex (Apache-2.0) | Cloud identity providers |

Licences checked against rule 3. Excluded on licence: Forgejo (GPL-3.0+ since v9), Redis 8
(RSAL/SSPL/AGPL), Zitadel v3+ core (AGPL), Restate (BUSL-1.1), libgit2 (GPL-2.0 with linking
exception; use `gix`).

## The merge queue: from one at a time to many

Today `Coordinator.merge_in_flight` is an `Option<InFlight>`: one submission per repo is merged
and tested at a time, in submission order. On the live steward (build log, `docs/ROADMAP.md` on
`sprint/build`), a clean change merged in 18 s and admin merges of Tessel itself took 96 s and
140 s; the steward call may take up to 13 minutes. That caps a repo at roughly 25 to 200 merges an
hour for changes like these, however many agents run. Claims moved the cost of collisions out
of rework and into this queue.

This is a property of the coordinator, not of Cloudflare, so porting alone does not change it.

**Proposed: test stacked prefixes of the queue in parallel.**

A round takes up to K submissions and tests them as stacks, each on top of the ones before it:

- **Selection.** Take submissions in order. Held-for-review ones are skipped, as now. A submission
  in backoff is waited for, as now, so the round stops in front of it.
- **Testing.** With A, B and C selected, the steward tests main+A, main+A+B and main+A+B+C in K
  sandboxes at once. Each stack is built by the same deterministic rebases, so the commit for A is
  identical in every sandbox.
- **Landing.** Land items 1 to j, where j is the largest number such that stacks 1 to j all passed.
  A longer stack that passed after a shorter one failed does not count: a flaky test, or C fixing
  what B broke, must not land a commit that was never tested as the tip of main. The steward
  pushes from the sandbox of stack j, kept alive for it, with one compare-and-swap push from the
  main the round started on, through the existing `withPushAccess`.
- **The first item that did not pass** gets the outcome its stack gave, as a single merge would
  today: a test failure or conflict rejects it; an infrastructure outcome sends it into the usual
  backoff. Items after it return to the queue in their places, not rejected.
- **Per-item results.** The steward reports each landed item's own base and head (the intermediate
  commits), checks each item's diff against its claim on top of the item before it, and writes one
  evidence record per item once main reads at the round's head. `Merged`, `BaseMoved` and the
  verification baselines (`before`) are then exactly what K sequential merges would produce.

The guarantee is unchanged: every landed item was tested as the tip of main, exactly as it
landed. A conflict or failure is attributed to the same item a serial queue would blame, because
everything before it passed.

How often stacks fail is the open question. A submitted claim keeps its scopes until it merges
(invariant 5), and write claims on overlapping scopes conflict, so queued submissions rarely edit
the same function. But claims do not make them disjoint: `Depend` does not conflict with `EditBody`
or `Create`, symbol claims in one file put only intention locks on the file, and the steward's
coverage check works on whole files. Several `Create` claims in one file (appends, module lists,
imports) can be queued together and conflict textually when stacked. That costs a wasted parallel
run, not a wrong result: with frequent failures, a round lands what a serial queue would, a little
later.

What changes, and the rules each change needs:

- `merge_in_flight` becomes an ordered round. K is a setting; K = 1 is today's behaviour.
- **Cut-off recovery.** Today a merge cut off by a restart counts as an infrastructure failure for
  its claim, which then waits out the backoff (`recover_merge`). For a round, the retry runs with
  K = 1, so the existing per-claim bound applies and a commit that hangs the steward cannot get
  others rejected. `already_merged` must then report each item's own base and head; today it maps
  to `head = base` (`merge.rs`), which would make recovered items' verifications meaningless.
- **`main_moved`.** It counts against the first item of the round only, as it counts against the
  one in-flight claim today, so one admin merge cannot push K claims towards `MAX_MAIN_MOVED`.
- **Time.** A round waits for its slowest stack, and the longest stack does K − 1 extra rebases.
  On Cloudflare the round must fit `STEWARD_CALL_TIMEOUT_MS`, so the step-budget test gains a round
  case and bounds K. The native host has no alarm limit.
- **Head sync** stays paused while a merge is in flight (`trunk.rs`). Longer rounds delay noticing
  an admin merge until the round ends; admin merges are rare.
- **Contract.** The steward contract in `src/merge.rs` and `tessel-steward/src/merge-types.ts`
  gains a round form. Agents still see `Merged` and `SubmitRejected`, so `protocol.rs` and
  `PROTOCOL_VERSION` do not change. Verification keeps its rule that merges go first.

Throughput is not claimed until measured (rule 7): the `tessel-swarm` A/B harness runs the same
tasks with K = 1 and K = 4 and reports merges per hour, rejections, wasted sandbox runs and wall
time.

## Phases

Each phase ends with `cargo test --workspace` and the steward tests passing, and with the stated
check.

**Phase 0: one host, two adapters.** Move the orchestration out of `runtime.rs` into a portable
`host` module over the coordinator ports. The Durable Object becomes a thin adapter. Add
`tessel-server` (axum, one task per repo, SQLite). Rebase `tessel-swarm`'s local target and the CLI
test support on the same host, removing their copies of the orchestration.
Check: the swarm A/B runs against `tessel-server`, and a test kills the server mid-merge and shows
the merge recovered once, with no duplicate landing.

**Phase 1: parallel merge queue,** in the portable host so every adapter gets it.
Check: the K = 1 versus K = 4 swarm comparison, plus recovery tests for a cut-off round.

**Phase 2: self-hosted steward.** Steward ports, then the self-hosted adapters (git on disk with the
existing gateway policies, Docker with gVisor and an egress proxy, outbox events, oauth2-proxy).
One `docker compose up` brings up Tessel on a Linux machine.
Check: the swarm runs end to end on compose, and a conformance suite per port passes against each
adapter: persist before send, a redelivered event is harmless, no write token during tests, a token
cannot reach another repo, a push is refused when main moved.

**Phase 3, only when someone needs it:** several coordinator nodes (Postgres, ownership epoch,
routing a repo's sockets to its owner), and further adapters.

## Open questions for Felix

1. **Scope.** Approve this as the post-challenge plan? If yes, `PLAN.md` gets a new section and the
   contest schedule moves to history.
2. **The Cloudflare adapter.** Workers Paid ends on 12 November, after which Artifacts and
   Containers cannot be exercised live. Keep the adapter compiling and unit-tested only, or keep a
   paid plan for live checks? Recommendation: compile and unit-test only, until someone wants to
   deploy on Cloudflare again.
3. **Steward language.** Rule 8 kept TypeScript because Cloudflare's SDKs are JavaScript-first.
   Off Cloudflare that reason is gone. Recommendation: keep TypeScript on Node for Phase 2, since
   the merge logic is written and tested; revisit a Rust steward (a single binary) afterwards.
4. **Protocol freeze.** Keep rule 1 (additive changes only, or bump `PROTOCOL_VERSION`)? It still
   protects CLIs and coordinators of different versions. Recommendation: keep it. Nothing above
   needs a wire change.
5. **Order.** Recommendation: Phase 0, then 1, then 2. Phase 1 rewrites code that Phase 0 moves, so
   it is cheaper after it, and it is the change users of a swarm would notice first.

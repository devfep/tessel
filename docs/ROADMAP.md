# Roadmap

Task list for the build, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope and schedule;
this file tracks the tasks that deliver it. Only the orchestrator edits this file.

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-05 03:39 EDT.
**Orchestrator:** Claude Code session in `repos/tessel` (Claude Fable 5.1), role taken 2026-10-05.
**Tip:** `sprint/build` at the FIX-STEWARD merge `7de5ff3` (plus this docs commit), pushed. Pull request 1 to `main` is open.
**Milestone:** coordinator core, then the protocol API freeze (PLAN §9, Oct 4–5 row). Stop and report
to Felix at FREEZE.

**Agents:**

| Agent | Task | Worktree / branch | Stage → next |
|---|---|---|---|
| impl-fix-rust | FIX-RUST | `.claude/worktrees/fix-rust` / `task-fix-rust` | fix pass 1 (7 items) → re-check |
| cq-fix-rust | FIX-RUST review | same worktree; live probes on port 8796 | "With fixes" on `222e499` (1 Important, 6 Minor; 24 of 25 mutants killed) → re-check |

**Rulings carried into COORD-2..4** (from the COORD-1 reviews):
1. The `agent` argument of `handle` is the only identity the core trusts and logs. A `Hello` naming
   another agent is `Malformed`. The shell binds identity per connection.
2. `handle` routes by message family to two functions, each naming every variant with no wildcard.
3. The current fence never appears in an error message.
4. Claims live in a `BTreeMap`; the lock table is derived state, rebuilt on load, never persisted;
   serialized state is byte-identical across identical replays; conflicts are sorted explicitly.
5. Exact-duplicate scopes in a request are dropped once, up front.
6. `Watch` replay belongs to the shell, which owns the event store (COORD-4).
7. COORD-2: an agent with a queued `Wait` request may make no other claim; waiters are served FIFO
   and never jump an earlier waiter they conflict with; queueing logs nothing because the protocol
   has no event for it (proposed below).
8. COORD-2 fix pass: time never goes backwards inside the core (a persisted clock clamps `now_ms`);
   `Coordinator::new` rejects `lease_ms == 0`.
9. COORD-3 and COORD-4 run as two parallel lanes after COORD-2 merges (disjoint files). A submitted
   claim needs a status that expiry, heartbeat and `next_expiry_ms` skip; shadow claims place no locks.
10. COORD-3: an uncovered submission leaves the claim active; a submitted claim rejects `Release` and
   `Amend` with `AlreadySubmitted`; assumption challenges are sent at `Submit`; shadow claims hold no
   locks, block no one, and a shadow `Submit` is answered `Accepted` with position 0 (recorded for
   verification, never queued). The merge-outcome path waits for the steward work on Oct 7.
11. COORD-4: one atomic write of state plus events before any send; identity bound per socket on
   `Welcome`; `Notify` to an agent with no open socket is dropped; `Watch` is served by the shell.
   COORD-4 merges after COORD-3 and sets `shadow_enabled` from a `SHADOW_ENABLED` variable.
12. The 75-character subject on COORD-3 commit `2e760b1` is reworded by the orchestrator at merge
   time, with a tree-equality check and the note carried over.
13. COORD-4 fix pass: sending requires a `Persisted` token that only a completed write returns, so
   persist-before-send is enforced by the types; delivery order is a pure, tested function; a failed
   send to another socket closes that socket only; a second `Watch` on a watching socket is refused.
14. FIX-RUST: the 1 MiB state limit refuses only growth caused by a client message; expiry-only work
   (the alarm, a disconnect, and the expiry run after a refused message) may store up to the storage
   cap, so a repo near the limit keeps making progress. A disconnect withdraws the agent's queued
   request before expiry runs. The only deployed coordinator versions that ran the core (6a493efd,
   f661b66f) had `SHADOW_ENABLED = "false"`, so no stored state holds a shadow claim.

**Merge queue:** empty.
**Background jobs:** none. Docker Desktop is running for the steward lane and the redeploy; stop it with `docker desktop stop` when FREEZE closes (quitting the window leaves the backend running).

**Deployed:** `tessel-coordinator` (the full coordinator, version `f661b66f`) and `tessel-steward`
(with the `TestRunner` container, version `64e07633`) on `devfep.workers.dev`. Queue `tessel-artifacts-events` with
subscriptions `tessel-repo-lifecycle` and `tessel-push-demo--agent-1`. Artifacts repos `demo` and
`demo--agent-1` in namespace `tessel`.

**Pending from Felix:**
1. Push-event design. Recommendation: the CLI's existing `Submit` is the merge signal, the steward
   verifies the commit by reading the fork through the binding, and one push subscription stays on
   the main repo. No protocol change. It changes the diagram in PLAN §4, so it needs his yes.
   Research and probes: 60 per-fork subscriptions on one queue also work.
2. `SUBMISSION_CHECKLIST.md` says Artifacts billing starts Oct 15; the pricing page says Oct 14.
3. An untracked `AGENTS.md` (a copy of `CLAUDE.md`) appeared in the repo root on Oct 3. Not created
   by this build; left untracked.
4. Protocol gaps found while building the core, all additive, for a decision at the freeze: no
   event for a queued `Wait` request (invariant 10 says every state change is logged); `Release`
   errors carry no `req`; `Conflict` carries no claim id; `ClaimId` and `Scope` lack `Ord`.
5. `cargo clippy -D warnings` fails on `src/protocol.rs` itself (14 doc-list lints and unused items).
   New code is clean; fixing the protocol file means editing it.
6. Agents are not authenticated, and it is exploitable, confirmed live on the deployed coordinator on
   2026-10-05: a socket that never sent `hello` read claim 5's fence from the `watch` stream; a second
   socket said `hello` under the holder's name, released the claim with that fence, and took the
   scope. Fix in two steps, neither needing a protocol change:
   - SEC-1 (in the FREEZE gate, decided by the orchestrator, Felix may flip): the coordinator
     refuses the WebSocket upgrade without a deployment secret, so nothing anonymous can connect.
   - Recommended next, needs Felix's yes because the CLI and steward must issue and carry it: a
     per-agent token signed by the steward and verified at the upgrade, binding the socket's
     identity. With that, a fence seen on `watch` is useless to another agent (`NotOwner`).
7. A shadow claim's `Submit` has no dedicated reply in the protocol; the core answers `Accepted`
   with `queue_position: 0`. An additive `ServerMsg` variant would be clearer.

**Standing rules:** Sonnet implementers, Opus reviewers. At most two lanes building at once. No
attribution trailer on commits. `src/protocol.rs` frozen. Deploys of the two Workers are approved.

**Next actions:**
1. On each report: dispatch the Opus reviewer, run fix passes, merge on "Yes", gate, close.
2. FREEZE: steward and Rust gates on the tip, `/code-review` and `/security-review` on the milestone
   diff, pull request `sprint/build` → `main`, merge, report to Felix.

## Tasks

- [x] **SPIKE-1** — Rust Durable Object deployed.
  CLOSED 2026-10-03: `tessel-coordinator` live, hello/welcome verified over `wss`.
- [x] **SPIKE-2** — Artifacts fork + repo-scoped token + push event via Queue.
  CLOSED 2026-10-05 at `ddaeb44`: create, fork, 1h write token, git push and the `pushed` event
  verified on the deployed steward.
- [x] **SPIKE-3** — Sandbox runs `npm test` on an Artifacts repo.
  CLOSED 2026-10-05 at `a4b411c` (merge of `task-spike-3`; review "Yes" after two fix passes; 67
  steward tests). Live on version `7be0d0c7`: `demo` passed in 18 s, a deliberately failing test on
  `demo--agent-1` returned exit 1 and `passed: false`, then passed after the revert. The token stays
  in the Worker and is revoked straight after the clone.
- [x] **COORD-1** — Pure coordinator core: hello, claims with `Fail`, release, lock table, fences,
  event log, at-risk assumptions.
  CLOSED 2026-10-05 at `e3e2847` (merge of `task-coord-1`; review "Yes" after two fix passes, 15
  reviewer mutants killed). `cargo test` 50 passed on the merged tree, including a property test
  against a brute-force oracle with save-and-reload before every operation.
- [x] **COORD-2** — Leases, heartbeat, expiry, `Amend`, and the `Wait` queue (invariants 2, 3, 4).
  CLOSED 2026-10-05 at `8b50045` (merge of `task-coord-2`; review "Yes" after two fix passes; one
  equivalent mutant survived out of 41). `cargo test` 78 passed on the merged tree, including a
  model-based property test over claims, waits, amends, releases, heartbeats and time.
- [x] **COORD-3** — `Submit` with fence and coverage checks, assumption challenges, and shadow
  claims (invariants 5, 8, 10, 11).
  CLOSED 2026-10-05 at `a10a680` (merge of `task-coord-3`; review "Yes" after two fix passes).
  `cargo test` 104 passed on the merged tree. Left for the steward task: the merge-outcome path, and
  a test that queue positions follow the submission ordinal once merged claims are removed.
- [x] **COORD-4** — Durable Object shell: persist state and events before sending, alarms for lease
  expiry, WebSocket sessions, `Watch` replay from the event store, deploy.
  CLOSED 2026-10-05 at `f0868ed` (merge of `task-coord-4`; review "Yes" after three fix passes).
  `cargo test` 158 passed on the merged tree; sending without a completed write does not compile.
  Live on version `f661b66f`: grant, denial with the holder's intent, queue then grant on release,
  watcher replay, expiry through the alarm, and head, counters and the full log intact after a
  redeploy. Not exercised live: close-on-failure for a send to another socket, and hibernation.
- [ ] **FIX-RUST** — Coordinator fixes from the milestone code review and security review: SEC-1 (the
  upgrade requires the deployment secret), bounded frames, scope counts and state size, waiters
  withdrawn on disconnect, canonical scope paths only, shared intent, large `from_seq`, state
  enums, cheaper delivery, `StaleFence` for claims that are gone.
  Files: `src/lib.rs`, `src/shell.rs`, `src/store.rs`, `src/coordinator.rs`, `wrangler.toml`, `README.md`.
  Verify: on the deployed Worker an upgrade without the secret gets 401, and the scripted two-agent
  run passes with it.
- [x] **FIX-STEWARD** — Steward fixes from the milestone code review: refuse repos with
  dependencies as step `install` instead of reporting failed tests, never run repo code when the
  token revoke failed, cap captured output, mint write tokens for forks only.
  CLOSED 2026-10-05 at `7de5ff3` (merge of `task-fix-steward`; review "Yes" after two fix passes;
  119 steward tests). Live on version `64e07633`: `demo` passes, a fork with a declared dependency
  returns step `install`, a token for `demo` is refused with 403 and one for the fork is issued.
- [ ] **FREEZE** — Milestone gate: full gate on the tip, `/code-review` and `/security-review` on
  the milestone diff, pull request `sprint/build` → `main`, report to Felix, protocol frozen.

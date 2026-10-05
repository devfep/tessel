# Roadmap

Task list for the build, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope and schedule;
this file tracks the tasks that deliver it. Only the orchestrator edits this file.

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-05 03:02 EDT.
**Orchestrator:** Claude Code session in `repos/tessel` (Claude Fable 5.1), role taken 2026-10-05.
**Tip:** `sprint/build` at the COORD-2 merge `8b50045` (plus this docs commit), pushed. `main` at `9211b67`.
**Milestone:** coordinator core, then the protocol API freeze (PLAN §9, Oct 4–5 row). Stop and report
to Felix at FREEZE.

**Agents:**

| Agent | Task | Worktree / branch | Stage → next |
|---|---|---|---|
| impl-coord-3 | COORD-3 | `.claude/worktrees/coord-3` / `task-coord-3` | reported at `2e760b1` (101 tests); holding → fix pass |
| cq-coord-3 | COORD-3 review | same worktree, read-only | reviewing `8b50045..HEAD` → verdict |
| impl-coord-4 | COORD-4 | `.claude/worktrees/coord-4` / `task-coord-4` | reported at `b2cc7c7` (107 tests, local run a–g); holding → fix pass |
| cq-coord-4 | COORD-4 review | same worktree; live probes on port 8796 | reviewing `8b50045..HEAD` → verdict |

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

**Merge queue:** empty.
**Background jobs:** none. Docker Desktop is quit; start it only to rebuild the sandbox image.

**Deployed:** `tessel-coordinator` (toolchain check, version `7aeaebc0`) and `tessel-steward`
(spikes 2 and 3 with the `TestRunner` container, version `7be0d0c7`) on `devfep.workers.dev`. Queue `tessel-artifacts-events` with
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
6. Agents are not authenticated: any client can connect and claim under any agent name. The
   protocol has no credential in `Hello`.
7. A shadow claim's `Submit` has no dedicated reply in the protocol; the core answers `Accepted`
   with `queue_position: 0`. An additive `ServerMsg` variant would be clearer.

**Standing rules:** Sonnet implementers, Opus reviewers. At most two lanes building at once. No
attribution trailer on commits. `src/protocol.rs` frozen. Deploys of the two Workers are approved.

**Next actions:**
1. On each report: dispatch the Opus reviewer, run fix passes, merge on "Yes", gate, close.
2. Merge COORD-3 first, then COORD-4 (it adapts to the new `Config` field), deploy the coordinator
   and run the two-agent live check, then FREEZE.

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
- [ ] **COORD-3** — `Submit` with fence and coverage checks, and shadow claims
  (invariants 5, 10, 11).
  Files: `src/coordinator.rs`.
  Verify: uncovered submissions are rejected with the list; shadow claims place no locks and
  cannot merge.
- [ ] **COORD-4** — Durable Object shell: persist state and events before sending, alarms for lease
  expiry, WebSocket sessions, `Watch` replay from the event store, deploy.
  Files: `src/lib.rs`, `wrangler.toml`, `README.md`.
  Verify: two agents on the deployed coordinator, a conflicting claim denied with the other's
  intent, state intact after the object is evicted.
- [ ] **FREEZE** — Milestone gate: full gate on the tip, `/code-review` and `/security-review` on
  the milestone diff, pull request `sprint/build` → `main`, report to Felix, protocol frozen.

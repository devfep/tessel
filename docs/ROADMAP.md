# Roadmap

Task list for the build, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope and schedule;
this file tracks the tasks that deliver it. Only the orchestrator edits this file.

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-05 01:08 EDT.
**Orchestrator:** Claude Code session in `repos/tessel` (Claude Fable 5.1), role taken 2026-10-05.
**Tip:** `sprint/build` code at `ddaeb44` (docs commits on top), pushed. `main` at `9211b67`.
**Milestone:** coordinator core, then the protocol API freeze (PLAN §9, Oct 4–5 row). Stop and report
to Felix at FREEZE.

**Agents:**

| Agent | Task | Worktree / branch | Stage → next |
|---|---|---|---|
| impl-spike-3 | SPIKE-3 | `.claude/worktrees/spike-3` / `task-spike-3` | fix pass 1 (7 items) → re-check |
| cq-spike-3 | SPIKE-3 review | same worktree, read-only | verdict "With fixes" on `80b8ae1` (1 Important, 6 Minor) → re-check after fix pass 1 |
| impl-coord-1 | COORD-1 | `.claude/worktrees/coord-1` / `task-coord-1` | reported at `4862f59` → fix pass 1 (rulings below) |
| cq-coord-1 | COORD-1 review | same worktree, read-only | reviewing `ddaeb44..4862f59` → verdict |

**Rulings for COORD-1 fix pass 1** (sent with the reviewer's findings as one brief, once the reviewer
has finished mutating the worktree):
1. `Hello` identity: the core uses the `agent` argument of `handle` for the reply and the log. A
   `Hello` whose message agent differs from the argument is `Malformed` and logs nothing. The shell
   passes the connection's agent, which for the first `Hello` is the one the message names.
2. Dispatch complexity: `handle` routes by message family to two functions (claim lifecycle; races,
   review and watch), each an exhaustive match with every variant named and no wildcard, so all three
   stay at or under complexity 8 as later tasks fill in the arms.

**Merge queue:** empty.
**Background jobs:** none. Docker Desktop is running for SPIKE-3; quit it when SPIKE-3 closes.

**Deployed:** `tessel-coordinator` (toolchain check, version `7aeaebc0`) and `tessel-steward`
(spike 2, version `94904597`) on `devfep.workers.dev`. Queue `tessel-artifacts-events` with
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

**Standing rules:** Sonnet implementers, Opus reviewers. At most two lanes building at once. No
attribution trailer on commits. `src/protocol.rs` frozen. Deploys of the two Workers are approved.

**Next actions:**
1. On each report: dispatch the Opus reviewer, run fix passes, merge on "Yes", gate, close.
2. SPIKE-3 gate includes a deploy of `tessel-steward` and a live test run against `demo`.
3. Dispatch COORD-2 when COORD-1 merges; COORD-3 and COORD-4 follow in order.

## Tasks

- [x] **SPIKE-1** — Rust Durable Object deployed.
  CLOSED 2026-10-03: `tessel-coordinator` live, hello/welcome verified over `wss`.
- [x] **SPIKE-2** — Artifacts fork + repo-scoped token + push event via Queue.
  CLOSED 2026-10-05 at `ddaeb44`: create, fork, 1h write token, git push and the `pushed` event
  verified on the deployed steward.
- [ ] **SPIKE-3** — Sandbox runs `npm test` on an Artifacts repo.
  Files: `tessel-steward/**`.
  Verify: `POST /repos/demo/test-runs` on the deployed steward returns `passed: true`, and the
  sandbox never holds a token.
- [ ] **COORD-1** — Pure coordinator core: hello, claims with `Fail`, release, lock table, fences,
  event log, at-risk assumptions.
  Files: `src/coordinator.rs`, `src/lib.rs` (module line), `Cargo.toml`, `Cargo.lock`.
  Verify: `cargo test`, including a property test against a brute-force conflict oracle.
- [ ] **COORD-2** — Leases, heartbeat, expiry, `Amend`, and the `Wait` queue (invariants 2, 3, 4).
  Files: `src/coordinator.rs`.
  Verify: an expired lease retires its fence; a stale fence is rejected; no hold-and-wait.
- [ ] **COORD-3** — `Submit` with fence and coverage checks, shadow claims, and `Watch`
  (invariants 5, 10, 11).
  Files: `src/coordinator.rs`.
  Verify: uncovered submissions are rejected with the list; shadow claims place no locks and
  cannot merge; `Watch` replays the log from any `seq`.
- [ ] **COORD-4** — Durable Object shell: persist state and events before sending, alarms for lease
  expiry, WebSocket sessions, deploy.
  Files: `src/lib.rs`, `wrangler.toml`, `README.md`.
  Verify: two agents on the deployed coordinator, a conflicting claim denied with the other's
  intent, state intact after the object is evicted.
- [ ] **FREEZE** — Milestone gate: full gate on the tip, `/code-review` and `/security-review` on
  the milestone diff, pull request `sprint/build` → `main`, report to Felix, protocol frozen.

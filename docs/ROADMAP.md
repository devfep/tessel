# Roadmap

Task list for the build, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope and schedule;
this file tracks the tasks that deliver it. Only the orchestrator edits this file.

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-05 04:17 EDT.
**Orchestrator:** none (stopped at FREEZE to report to Felix; last held by the Claude Code session in
`repos/tessel`, Claude Fable 5.1).
**Tip:** `sprint/build` at the FIX-RUST merge `597f625` plus this docs commit, pushed, and merged to
`main` by pull request 1.
**Milestone:** reached. Spikes 1 to 3 and the coordinator (PLAN §9 rows for Oct 3 and Oct 4–5) are
done. The protocol in `src/protocol.rs` is unchanged since the first commit and is ready to freeze,
subject to item 1 below.

**Agents:** none live. Every worktree and task branch is reclaimed.

**Merge queue:** empty.
**Background jobs:** none. Docker Desktop is stopped (`docker desktop stop`).

**Deployed** on `devfep.workers.dev`:
- `tessel-coordinator` version `7ff796a0`: the full coordinator. Every WebSocket upgrade needs
  `Authorization: Bearer <COORDINATOR_TOKEN>`. The value is in the gitignored `.dev.vars` in the
  repo root and set as a Worker secret.
- `tessel-steward` version `64e07633`, with the `TestRunner` container. Admin routes need
  `STEWARD_ADMIN_TOKEN` (in `tessel-steward/.dev.vars`).
- Queue `tessel-artifacts-events` with subscriptions `tessel-repo-lifecycle` and
  `tessel-push-demo--agent-1`. Artifacts repos `demo` and `demo--agent-1` in namespace `tessel`.
- Scratch coordinator repos from the gates (`gate-*`, `gate2-*`, `gate3-*`) hold test state only.

**Pending from Felix:**
1. Protocol freeze sign-off. Additive changes are still allowed today (CLAUDE.md rule 1). Gaps found
   while building, none blocking: no event for a queued or withdrawn `Wait` request (invariant 10
   says every state change is logged); `Release` errors and `Uncovered` carry no `req`; `Conflict`
   carries no claim id; a shadow claim's `Submit` and `Amend` have no dedicated replies (the core
   answers `Accepted` with `queue_position: 0`, and `Granted`); no message cancels a queued `Wait`
   (closing the socket does); `ClaimId` and `Scope` lack `Ord`. Recommendation: add the queued and
   withdrawn events and the two `req` fields now, leave the rest.
2. Per-agent identity. The coordinator now refuses anything without the deployment secret, but any
   holder of that secret can still say `hello` under another agent's name and read fences on
   `watch`. Recommendation: a per-agent token signed by the steward and verified at the upgrade,
   binding the socket's identity; no protocol change. It touches the CLI and steward work of
   Oct 6–7, so it needs his yes.
3. Push-event design. Recommendation: the CLI's existing `Submit` is the merge signal, the steward
   verifies the commit by reading the fork through the binding, and one push subscription stays on
   the main repo. No protocol change, but it changes the diagram in PLAN §4. Probes showed 60
   per-fork subscriptions on one queue also work.
4. `cargo clippy -D warnings` and `rustfmt` report findings in `src/protocol.rs` itself (doc-list
   lints, unused items, formatting). All new code is clean. Fixing it means editing the protocol
   file, with no wire change.
5. `SUBMISSION_CHECKLIST.md` says Artifacts billing starts Oct 15; the pricing page says Oct 14.
6. An untracked `AGENTS.md` (a copy of `CLAUDE.md`) appeared in the repo root on Oct 3. Not created
   by this build; left untracked.

**Known limits, recorded so nobody rediscovers them:**
- A submitted claim is held until a merge outcome is reported; that path arrives with the steward
  merge work (Oct 7). Add then: a test that queue positions follow the submission ordinal once
  merged claims are removed.
- The test runner refuses repos with dependencies (step `install`) until install is built.
- A `Granted` whose send fails after the write is not re-sent; the claim then lives until its lease
  lapses, or longer if that agent keeps sending heartbeats. A resync on `hello` would close it.
- New claims are checked against active claims only, so a steady stream of compatible claims can
  delay a waiter.
- Worker glue in `src/lib.rs` has no native tests (no fake storage layer); it is covered by the
  `Persisted` type, the pure functions it calls, and the live runs in the merge notes.
- Not exercised live: closing another socket after a failed send to it.

**Standing rules:** Sonnet implementers, Opus reviewers. At most two lanes building at once. No
attribution trailer on commits. `src/protocol.rs` is not edited without Felix. Deploys of the two
Workers are approved. Design rulings from the reviews are in the git notes on the merge commits
and in the doc comments of the code they govern.

**Next actions on resume:**
1. Take Felix's answers to items 1 to 3.
2. PLAN §9, Oct 6: CLI levels 1–2 and the skill file; then the steward merge path (Oct 7).

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
- [x] **FIX-RUST** — Coordinator fixes from the milestone code review and security review: the
  upgrade requires the deployment secret, bounded frames, scope counts and state size, waiters
  withdrawn on disconnect, canonical scope paths only, `StaleFence` for claims that are gone.
  CLOSED 2026-10-05 at `597f625` (merge of `task-fix-rust`; review "Yes" after four fix passes).
  `cargo test` 220 passed on the merged tree. Live on version `7ff796a0`: an upgrade without the
  secret gets 401, the two-agent run passes with it, and a waiter that closes after hibernation is
  withdrawn.
- [x] **FIX-STEWARD** — Steward fixes from the milestone code review: refuse repos with
  dependencies as step `install` instead of reporting failed tests, never run repo code when the
  token revoke failed, cap captured output, mint write tokens for forks only.
  CLOSED 2026-10-05 at `7de5ff3` (merge of `task-fix-steward`; review "Yes" after two fix passes;
  119 steward tests). Live on version `64e07633`: `demo` passes, a fork with a declared dependency
  returns step `install`, a token for `demo` is refused with 403 and one for the fork is issued.
- [x] **FREEZE** — Milestone gate: full gate on the tip, `/code-review` and `/security-review` on
  the milestone diff, pull request `sprint/build` → `main`, report to Felix.
  CLOSED 2026-10-05 with pull request 1. The two reviews produced FIX-RUST and FIX-STEWARD, both
  merged, deployed and checked live before the merge to `main`.

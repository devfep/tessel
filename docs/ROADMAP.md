# Roadmap

Task list for the build, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope and schedule;
this file tracks the tasks that deliver it. Only the orchestrator edits this file.

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-05 09:32 EDT.
**Orchestrator:** the Claude Code session in `repos/tessel` (Claude Opus 5.5).
**Tip:** `sprint/build` at the CLI-FIX merge `36abdb3` plus this STATE commit; `main` at `29aa8fe`
(pull request 1).
**Milestone:** PLAN §9 Oct 6 delivered. Dogfood v0 has started: CLI-2 is the first lane that
claims its files through the live coordinator (repo `tessel-dogfood`, agent `cli-2`). Limit, stated
honestly: subagents share the parent session's hook settings, so the pre-edit hook does not enforce
claims for lanes; they claim by hand following the skill. Oct 7 work in progress (SUBMIT-1).

**Felix's rulings, 2026-10-05 07:00 EDT** (the four items pending at FREEZE), all now delivered
except ruling 3, which lands with the steward merge path:
1. Protocol freeze additions: queued and withdrawn wait events, `req` on `Release` and `Uncovered`.
2. Identity: per-agent tokens signed by the steward (IDENTITY).
3. Push events: `Submit` is the merge signal; the steward verifies the commit by reading the fork;
   one push subscription on the main repo. PLAN §4 diagram updated in `8feb196`.
4. `src/protocol.rs` clippy and rustfmt clean (PROTO-FREEZE).

`tools/merge-one.sh` refuses any change to `src/protocol.rs`; a change there is merged by Felix by
hand (his ruling, 07:19), with the orchestrator handing him the command and the predicted tree.

**Agents:**
- SUBMIT-1 (Sonnet, `.claude/worktrees/submit-1`): review "With fixes" (one merge per alarm, a
  timeout on the steward call, steward 4xx as agent errors, sha checked at Submit); fix pass 1.
- CLI-2 (Sonnet, `.claude/worktrees/cli-2`): part a, `tessel submit`; dogfooded on
  `tessel-dogfood`.
**Merge queue:** empty.
**Background jobs:** none.

**Deployed** on `devfep.workers.dev`:
- `tessel-coordinator` version `b3681796`: every upgrade needs `Authorization: Bearer <token>` minted
  by the steward for that repo and agent. `IDENTITY_SIGNING_KEY` is set on both Workers and kept in
  both gitignored `.dev.vars` files. The old `COORDINATOR_TOKEN` secret is unused (refused live) and
  still set on the Worker; delete it with `wrangler secret delete COORDINATOR_TOKEN` when convenient.
- `tessel-steward` version `0b6da38d`, with the `TestRunner` container; admin routes need
  `STEWARD_ADMIN_TOKEN` (in `tessel-steward/.dev.vars`). `POST /repos/<repo>/merges` runs the merge
  executor (STEWARD-1). `POST /repos/<repo>/agents/<agent>/identity`
  mints a 24 h agent token.
- Queue `tessel-artifacts-events` with subscriptions `tessel-repo-lifecycle` and
  `tessel-push-demo--agent-1` (the fork subscription goes once the Submit path lands). Artifacts
  repos `demo` and `demo--agent-1` in namespace `tessel`.

**Pending from Felix:**
1. At the next hand merge of `src/protocol.rs`: delete the six `cfg_attr(not(test), expect(dead_code))`
   lines (no wire change; the items are public now that the CLI uses the crate), so CLI-1's
   `#[allow(unfulfilled_lint_expectations)]` on `pub mod protocol` can go.
2. `SUBMISSION_CHECKLIST.md` says Artifacts billing starts Oct 15; the pricing page says Oct 14.
3. An untracked `AGENTS.md` (a copy of `CLAUDE.md`) sits in the repo root; left untracked.

**Known limits, recorded so nobody rediscovers them:**
- OPEN BUG, fix in progress in CLI-2 (found by the CLI-FIX reviewer after the merge at `36abdb3`):
  when the hook's stdin JSON has no `cwd`, a relative `file_path` is resolved against the hook
  process's cwd and can exit 0 unclaimed. Claude Code always sends `cwd`; no lane runs the hook yet.
- A submitted claim is held until a merge outcome is reported; that path arrives with the steward
  merge work (Oct 7). Add then: a test that queue positions follow the submission ordinal once
  merged claims are removed.
- The test runner refuses repos with dependencies (step `install`) until install is built.
- A `Granted` whose send fails after the write is not re-sent; a resync on `hello` would close it.
- New claims are checked against active claims only, so a steady stream of compatible claims can
  delay a waiter.
- Worker glue in `src/lib.rs` has no native tests; it is covered by `Persisted`, the pure
  functions it calls, and the live runs in the merge notes.
- Not exercised live: closing another socket after a failed send to it.
- Merge executor: a process the repo's tests detach (setsid, nohup) can outlive the test timeout
  and still run while the push token is live. Bounded: the push gateway forwards only the pinned
  update, and the outcome comes from a Worker-side read of the trunk. Isolating the tests under
  their own uid would close it.
- Merge executor: pushes over 1 MiB (git's probe request) are not exercised live.
- Scratch repos `gate6-*` in Artifacts hold test state only.

**Standing rules:** Sonnet implementers, Opus reviewers. At most two lanes building at once. No
attribution trailer on commits. `src/protocol.rs` changes only under PROTO-FREEZE. Deploys of the
two Workers are approved.

**Next actions:**
1. Review SUBMIT-1 and CLI-2 as they report; deploy SUBMIT-1 (steward first) and check a clean
   merge, a conflict and a failing test end to end through `Submit`.
2. REVIEW-1 after SUBMIT-1 merges: `Review` approve/reject for held submissions; note that
   `Summary.reviews_requested` now counts held submissions too; a held submission keeps its locks.
3. CLI-2b: tree-sitter symbol claims in the hook and symbol-level `touched`.

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
- [x] **PROTO-FREEZE** — Felix's rulings 1 and 4: queued and withdrawn wait events, `req` on
  `Release` and `Uncovered`, clippy and rustfmt clean on `src/protocol.rs`; the coordinator emits
  the events.
  CLOSED 2026-10-05 at `e402440` (merged by Felix; review "Yes" after one fix pass, which also made
  `Mode::permits` exhaustive). `cargo test` 241, clippy `-D warnings` clean on the whole crate. Live
  on `57e82531`: `wait_queued` then `wait_withdrawn` on close, and a bad `release` echoes its `req`.
- [x] **IDENTITY** — Felix's ruling 2: steward-signed per-agent tokens replace `COORDINATOR_TOKEN`;
  the coordinator binds the socket to the token's agent.
  CLOSED 2026-10-05 at `d6f127f` (review "Yes" after one fix pass; adds `deny.toml`). Merged tree:
  `cargo test` 232, steward 163. Live on coordinator `4426eafc` and steward `dfed7cbe`: a minted
  token connects, a hello under another agent gets `not_owner`, and the wrong repo, no auth, a
  garbage token, the old secret and a forged agent header all get 401.
- [x] **CLI-1** — PLAN §9 Oct 6: protocol as a library, `tessel-cli` with a per-worktree daemon,
  `start`/`claim`/`status`/`inbox`/`release`/`stop`, and the Claude Code pre-edit hook that
  auto-claims files and blocks on denial.
  CLOSED 2026-10-05 at `1e07da7` (review "Yes" after three fix passes: escaped server text, grant
  expiry, reconcile after reconnect with an end marker, daemon lock, symlinks). Workspace tests 324.
  Live on coordinator `9b1646ff`: grant, denial with quoted intent, hook auto-claim and block, and a
  coordinator redeploy survived with the claim intact.
- [x] **STEWARD-1** — PLAN §9 Oct 7, early: steward merge executor. Verify a fork commit, rebase it
  onto the trunk in the Sandbox, test, push only if the trunk has not moved; report a typed outcome.
  CLOSED 2026-10-05 at `663c65c` (review "Yes" after two fix passes; `merged` is decided by a read
  of the trunk, never by the sandbox's exit code). Steward tests 307. Live on `0b6da38d`: merged,
  already_merged, commit_not_in_fork, tests_failed, conflict, and main_moved from two concurrent
  merges, each with the trunk checked afterwards.
- [x] **SKILL-1** — skill file and the `AGENTS.md` carried into forks, for shipped commands only.
  CLOSED 2026-10-05 at `d282935` (review "Yes" after one fix pass; `skills/tessel/`).
- [x] **CLI-FIX** — found live: a socket path over 100 bytes makes `start` fail and the pre-edit
  hook exit 0 (fails open). Short socket path independent of the worktree depth; the hook fails
  closed on every error.
  CLOSED 2026-10-05 at `36abdb3` (review "Yes" after one fix pass, which also pinned the hook to its
  worktree root: a cwd outside the worktree could otherwise skip the claim). Workspace tests 348.
  Live from a 130-byte-deep worktree: start works; the installed hook auto-claims, blocks a held
  file and allows a path outside.
- [ ] **CLI-2** — part a: `tessel submit` (file-level `touched`, local coverage check, evidence
  required). Part b (CLI-2b): tree-sitter symbol claims and mode escalation in the hook.
- [ ] **REVIEW-1** — `Review` approve/reject for submissions held under invariant 12.
- [ ] **SUBMIT-1** — Felix's ruling 3: on `Submit`, the coordinator sends the claim's fork
  (`<repo>--<agent>`) and commit to the steward merge executor through a service binding (not
  public), and applies the outcome (`Merged` / `SubmitRejected`, `BaseMoved`,
  `AssumptionChallenged`). The CLI command is in CLI-2.
- [x] **COORD-HARDEN** — from the CLI-1 review: the coordinator refuses control characters (C0, DEL,
  C1) in scope paths and qualified names, as defence in depth behind the CLI's escaping. Touches
  `src/coordinator.rs` only.
  CLOSED 2026-10-05 at `a5a5f96` (review "Yes" after one fix pass, which added the Unicode
  Bidi_Control characters and U+2028/9). `cargo test` 244. Live on `ff1dd858`: newline, ESC, U+202E
  and a padded name are refused as malformed; a clean scope is granted.

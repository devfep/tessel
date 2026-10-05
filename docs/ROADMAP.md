# Roadmap

Task list for the build, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope and schedule;
this file tracks the tasks that deliver it. Only the orchestrator edits this file.

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-05 11:34 EDT.
**Orchestrator:** the Claude Code session in `repos/tessel` (Claude Opus 5.5).
**Tip:** `sprint/build` at the COVER-1 merge `191dc0d` plus this STATE commit; `main` at `29aa8fe`
(pull request 1).
**Milestone:** PLAN §9 Oct 6 and Oct 7 delivered: an agent claims through the CLI, submits, and the
steward merges in order behind the review gate; checked live end to end with the real CLI. Dogfood
v0 ran on `tessel-dogfood` (agent `cli-2`, claims 1–19, every edit claimed first; record in the
CLI-2 merge note). Limit, stated honestly: subagents share the parent session's hook settings, so
the pre-edit hook does not enforce claims for lanes; they claim by hand following the skill.
Next per PLAN §9 Oct 8: assumptions end to end and races.

**Felix's rulings, 2026-10-05 07:00 EDT** (the four items pending at FREEZE), all now delivered
except ruling 3, which lands with the steward merge path:
1. Protocol freeze additions: queued and withdrawn wait events, `req` on `Release` and `Uncovered`.
2. Identity: per-agent tokens signed by the steward (IDENTITY).
3. Push events: `Submit` is the merge signal; the steward verifies the commit by reading the fork;
   one push subscription on the main repo. PLAN §4 diagram updated in `8feb196`.
4. `src/protocol.rs` clippy and rustfmt clean (PROTO-FREEZE).

**Felix's ruling, 2026-10-05 11:34 EDT:** go ahead with dogfood v1 through the steward (DOGFOOD-1), once CLI-2b
and ASSUME-1 merge. Condition he set: only if it is tried, tested and robust. So `merge-one.sh`
stays the fallback until DOGFOOD-1 passes review and live checks on scratch repos, and the switch
is recorded with the first Tessel commit merged by the steward.

`tools/merge-one.sh` refuses any change to `src/protocol.rs`; a change there is merged by Felix by
hand (his ruling, 07:19), with the orchestrator handing him the command and the predicted tree.

**Agents:** CLI-2b (Sonnet, `.claude/worktrees/cli-2b`): building; dogfooded as agent `cli-2b`.
COVER-1 closed and reclaimed.
**Merge queue:** empty.
**Background jobs:** none.

**Deployed** on `devfep.workers.dev`:
- `tessel-coordinator` version `493e4466`: every upgrade needs `Authorization: Bearer <token>` minted
  by the steward for that repo and agent. `IDENTITY_SIGNING_KEY` is set on both Workers and kept in
  both gitignored `.dev.vars` files. The old `COORDINATOR_TOKEN` secret is unused (refused live) and
  still set on the Worker; delete it with `wrangler secret delete COORDINATOR_TOKEN` when convenient.
- `tessel-steward` version `c43ad948` (merge executor checks coverage), with the `TestRunner` container; admin routes need
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
   In the same merge: the `Summary` doc says "merges - reviews_requested = merged without review",
   which stopped being true with REVIEW-1 (a rejected and resubmitted change is requested twice and
   a rejected one never merges). Correct the doc; optionally add `reviews_approved` with
   `#[serde(default)]` if the dashboard needs "merged after approval".
2. `SUBMISSION_CHECKLIST.md` says Artifacts billing starts Oct 15; the pricing page says Oct 14.
3. An untracked `AGENTS.md` (a copy of `CLAUDE.md`) sits in the repo root; left untracked.

**Known limits, recorded so nobody rediscovers them:**
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
- Scratch repos `gate6-*`, `gate7-*`, `gate8-*` and `gate9-*` hold test state only.
- A `watch` connection does not reschedule the alarm; a repo whose queue stalled (only possible
  before `8632d73`) resumes on the next agent message or lease alarm.
- `Summary.reviews_requested` counts review requests, not reviewed merges (see Pending item 1).
- A submission held for review keeps its locks until a reviewer in `REVIEWERS` decides it; it never
  expires. The CLI has no `review` command yet (raw `review` message only).
- Five lines over 100 characters predate REVIEW-1: `src/coordinator.rs:13`,
  `src/coordinator/merging.rs:629` and `:825`, `src/identity.rs:153`, `src/shell.rs:1`.
- `tessel submit` computes `touched` from a base pinned at the first start and advanced only by
  this agent's `Merged`; deleting `.tessel/state.json` resets it to HEAD (documented). The steward
  also checks the rebased commit's files against the claim (COVER-1), at file level only.

**Standing rules:** Sonnet implementers, Opus reviewers. At most two lanes building at once. No
attribution trailer on commits. `src/protocol.rs` changes only under PROTO-FREEZE. Deploys of the
two Workers are approved.

**Next actions:**
1. Review CLI-2b when it reports.
2. PLAN §9 Oct 8: assumptions end to end (verified), races. Dashboard and review screen (agents'
   lane per PLAN §7).
3. Milestone PR `sprint/build` -> `main` after Oct 8 (full gate, `/code-review`, `/security-review`).

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
- [x] **COVER-1** — invariant 11 enforced at merge, not only from the agent's `touched` list: the
  coordinator passes the claim's scopes to the steward, which checks the rebased commit's changed
  files against them (file level) before testing or pushing, and returns `uncovered {files}` as a
  verified rejection. Filed from the CLI-2 reviews (a client-computed diff base kept failing open).
  CLOSED 2026-10-05 at `191dc0d` (review "Yes" after two fix passes). Steward tests 375. Live: a
  commit touching an unclaimed file was rejected before any test ran and the trunk did not move; a
  covered change merged.
- [x] **CLI-2** — part a: `tessel submit` (file-level `touched`, local coverage check, evidence
  required). Part b (CLI-2b): tree-sitter symbol claims and mode escalation in the hook.
  Part a CLOSED 2026-10-05 at `5700165` (review "Yes" after four fix passes: the diff base, one
  claim per agent through `Amend`, a deterministic submit reply, `stop` confirming releases, and
  the hook's missing-cwd fail-open). Workspace tests 468. Live with the real CLI: claim, commit,
  push, `tessel submit`, `merged` in the inbox, trunk at the agent's commit.
- [ ] **DOGFOOD-1** — dogfood v1 (PLAN §7, §9 Oct 8; Felix approved): the steward runs Tessel's
  own gate. Test image with the Rust toolchain (wasm target) and pnpm; an install step for repos
  with dependencies (lockfile only, scripts disabled); per-repo test command; Tessel imported into
  Artifacts as `tessel`, one fork per lane, lanes push and `tessel submit`; the Artifacts trunk
  mirrored to GitHub `sprint/build`. After CLI-2b and ASSUME-1 merge.
- [ ] **SYM-SIG** — from the CLI-2b review: attributes, derives, doc comments, decorators and
  `impl` bounds count as file `edit_body`, so `review_reasons` never flags them as signature
  changes. Put leading attribute and decorator siblings in the signature range.
- [x] **REVIEW-1** — `Review` approve/reject for submissions held under invariant 12.
  CLOSED 2026-10-05 at `38e5ff8` (review "Yes" after one fix pass; flagged submissions now get
  `ReviewRequired` before any `Accepted`). Workspace tests 404. Live on `9f75140a`: held submit,
  non-reviewer refused, approval merged, rejection with a fixed reason that never carries the note.
- [x] **SUBMIT-1** — Felix's ruling 3: on `Submit`, the coordinator sends the claim's fork
  (`<repo>--<agent>`) and commit to the steward merge executor through a service binding (not
  public), and applies the outcome (`Merged` / `SubmitRejected`, `BaseMoved`,
  `AssumptionChallenged`). The CLI command is in CLI-2.
  CLOSED 2026-10-05 at `8632d73` (review "Yes" after three fix passes; the third came from the live
  check: an alarm time of 0 meant no merge was ever dispatched). Workspace tests 386, steward 317.
  Live on coordinator `e9d00c31`: through `Submit`, a clean change merged in 18 s, a failing test
  and a conflict were rejected with fixed-form reasons, and the trunk moved only for the merge.
- [x] **COORD-HARDEN** — from the CLI-1 review: the coordinator refuses control characters (C0, DEL,
  C1) in scope paths and qualified names, as defence in depth behind the CLI's escaping. Touches
  `src/coordinator.rs` only.
  CLOSED 2026-10-05 at `a5a5f96` (review "Yes" after one fix pass, which added the Unicode
  Bidi_Control characters and U+2028/9). `cargo test` 244. Live on `ff1dd858`: newline, ESC, U+202E
  and a padded name are refused as malformed; a clean scope is granted.

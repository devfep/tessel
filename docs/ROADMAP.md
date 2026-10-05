# Roadmap

Task list for the build, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope and schedule;
this file tracks the tasks that deliver it. Only the orchestrator edits this file.

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-05 18:05 EDT.
**Orchestrator:** the Claude Code session in `repos/tessel` (resumed 15:30 after the context clear).
The session's permission classifier refuses production deploys, secret writes and Artifacts
deletes, so Felix runs those from a command the orchestrator hands him (Pending 0).
**First live swarm run** (16:03, seed 1, 10 tasks, 6 agents, overlap 0.5, scratch repo
`swarm-s1-tmgae3`, results in the session scratchpad `orch/swarm1/`): `on` landed 9 of 10 with 0
rejections, 5 queued waits and 4 script approvals over 60 events in 172 s; task 6 timed out after
120 s with no grant, because merges through the live steward take tens of seconds each and the
queue ahead did not clear. `off` (local replay, labelled local) landed 7 with 3 rejected (2
textual conflicts, 1 broke the tests) in 5 s. For the Oct 10 A/B runs, set the wait timeout to fit
live steward latency, and report this run's timeout as it happened.
**Tip:** `sprint/build` at the DOGFOOD-4 merge `4793bca`, pushed. The GitHub trunk is at `29aa8fe`
(pull request 1). The Artifacts trunk `tessel-dogfood` is at `6f78128`, the same tree as `b6b8bfb`
The Artifacts trunk `tessel-dogfood` is at `8432f8c`, one catch-up commit on `6f78128` whose tree
equals `sprint/build` `aadba8b` (HARNESS-1 and DOGFOOD-4); admin-merged with the trunk gate in
136 s and mirrored fast-forward to GitHub `artifacts-trunk`. Lessons from the catch-up (in its
note): replaying `sprint/build` commits onto the trunk conflicts with their rebased copies, so
catch-ups are one commit on the trunk head; added files need `create` scopes, not `edit_body`.
From here new work starts from `artifacts-trunk`, not `sprint/build`.
**Milestone:** PLAN §9 Oct 6–8 delivered and checked live: CLI and dogfood v0, the steward merge
path with review gate and coverage check, assumptions verified end to end, races (one scripted race
live end to end). Dogfood v1 is technically ready: the first Tessel commit merged through the
steward (admin merge, 96 s) and probe 3 passed. Lanes have NOT switched yet; DOGFOOD-4 finishes
the switch. Lanes claim through the coordinator (`tessel-dogfood`) by hand (subagents share the
parent session's hook settings), and merge notes record each late claim.

**Felix's rulings** (newest first; older ones are in the git notes and earlier STATE commits):
- 15:2x EDT: (1) agent `orchestrator` joins `REVIEWERS`; it approves held lane work only after the
  Opus review says Yes and the orchestrator's gate passes, and every approval is in the event log.
  (2) The mirror pushes the Artifacts trunk fast-forward-only to a NEW GitHub branch
  `artifacts-trunk`; `sprint/build` keeps the pre-dogfood history and its git-notes evidence; no
  history is rewritten; milestone pull requests go from `artifacts-trunk`. (3) Delete the stray
  Artifacts repo `tessel` (unused; holds an old copy at `ea9bd43`).
- 13:42 EDT: a third Worker `tessel-coordinator-swarm` (same code, own Durable Objects,
  `REVIEWERS = "swarm-reviewer"`) may be deployed for live swarm and A/B runs.
- 12:40 EDT: parallelise with subagents when the Mac has headroom (see Standing rules).
- 11:34 EDT: go ahead with dogfood v1 through the steward, only if tried, tested and robust;
  `merge-one.sh` stays the fallback until the probe passes the go/no-go list (in DOGFOOD-1 below).
- 07:19 EDT: `tools/merge-one.sh` refuses changes to `src/protocol.rs`; Felix merges those by hand
  from a command the orchestrator hands him.

**Agents:** `impl-shadow-1` (Sonnet) in `.claude/worktrees/shadow-1` on `task-shadow-1` (cut from
the trunk `8432f8c`), fork `tessel-dogfood--lane-shadow-1`, agent `lane-shadow-1`. `rev-shadow-1`
(Opus) said "Yes" at `401d8d0` after one fix pass (761 tests). Waiting to push: Felix's hook
guard blocks any push naming `main`, and every Artifacts fork's branch is `main`. Felix chose to
exempt Artifacts URLs (hook change done by Felix 18:3x); the session's classifier also needs a
project allow rule `Bash(git push https://1e40d7b5aed4b7049e5b83bc07a5264c.artifacts.cloudflare.net/git/tessel/tessel-dogfood--*)`
(Pending 0). Then: lane claims all four touched files again (claim 35 expired; 36 covers only two),
pushes with the literal fork URL, `tessel submit`, approval as `orchestrator` if held.
**Merge queue:** empty.
**Background jobs:** none. Docker Desktop stopped.

**Deployed** on `devfep.workers.dev`:
- `tessel-coordinator` version `66fbe44a` (`REVIEWERS = "felix,orchestrator"`, deployed by Felix
  about 16:04). Live: a review as `orchestrator` passes the reviewer check (`unknown_claim` for a
  missing claim); a non-reviewer gets `not_owner`.
  Every upgrade needs a steward-minted agent token. The old `COORDINATOR_TOKEN` secret is unused
  and still set; delete with `wrangler secret delete COORDINATOR_TOKEN` when convenient.
- `tessel-steward` version `f5f3d912` (deployed by Felix about 16:15; the first attempt failed in
  wrangler's image push, untagged mid-push, and the retry succeeded): toolchain image (Rust, Node 22, pnpm, GNU time, tini) on
  `standard-4` for repos with `tessel.toml`; the `lite` image otherwise. Admin routes need
  `STEWARD_ADMIN_TOKEN` (`tessel-steward/.dev.vars`): `POST /repos/<repo>` (create),
  `/forks/<fork>`, `/tokens` (fork write tokens), `/read-tokens`, `/agents/<agent>/identity`,
  `/test-runs`, `/merges` (admin merge; the only path that may change `tessel.toml`).
- Artifacts (namespace `tessel`): `tessel-dogfood` (the dogfood v1 trunk, `8432f8c`) and forks
  `tessel-dogfood--orchestrator` (at `aadba8b`, sprint/build lineage: do not submit from it),
  `tessel-dogfood--admin` (catch-ups), `tessel-dogfood--lane-shadow-1`; `demo`, `demo--agent-1`; scratch `gate*-*` and `swarm-*` repos.
  The stray `tessel` was deleted by Felix about 16:05 (confirmed gone from the listing). Queue `tessel-artifacts-events` with its subscriptions as before.
- `tessel-coordinator-swarm` version `39c3a7de` (secret change on `44538ffb`;
  `REVIEWERS = "swarm-reviewer"`, `swarm-*` repos only). Live: a steward-minted token gets
  `welcome`, `tessel-dogfood` gets 403, no token gets 401.

**Pending from Felix:**
0. Add the fork-push allow rule above to `.claude/settings.local.json` (command handed to Felix).
   Note: the existing `Bash(npx wrangler deploy *)` rule does not match a deploy piped through
   other commands; the orchestrator runs deploys bare. Production deploys, secret writes, Artifacts deletes and forced pushes are
   refused by the session's permission classifier; the orchestrator hands Felix a command for
   each (done today: swarm key, coordinator deploy, stray repo delete, steward image, the
   catch-up admin merge).
1. At the next hand merge of `src/protocol.rs` (additive only): delete the six stale
   `cfg_attr(not(test), expect(dead_code))` lines (then drop CLI-1's
   `#[allow(unfulfilled_lint_expectations)]` on `pub mod protocol`); correct the `Summary` doc on
   `reviews_requested`; add `#[serde(default)] commit: Option<CommitId>` to
   `EventKind::AssumptionVerified`; add `#[serde(default)] entries: Vec<RaceEntry>` to
   `EventKind::RaceDecided`. New (DOGFOOD-4 review): `ReviewDecided` does not name the reviewer, so
   with `felix` and `orchestrator` both listed the log cannot show who approved. Proposed: add
   `#[serde(default)] reviewer: Option<AgentId>` to it. Until then the orchestrator puts
   "orchestrator: Opus Yes, gate <sha>" in every approval's note.
2. `SUBMISSION_CHECKLIST.md` says Artifacts billing starts Oct 15; the pricing page says Oct 14.
3. An untracked `AGENTS.md` (a copy of `CLAUDE.md`) sits in the repo root; left untracked.

**Known limits, recorded so nobody rediscovers them:**
- STATE commits still go to `sprint/build` (pushing each through the steward costs a gate run),
  so the trunk's copy of this file lags. Before the milestone pull request from `artifacts-trunk`,
  land the current `docs/ROADMAP.md` through the steward as one commit.
- Probe 3 left two go/no-go items unmeasured: `id -u` inside the gate was never printed (the
  numeric `1000:1000` was accepted), and `readFile` with a bogus sha was never tried.
- The peak memory figure is written by code inside the gate and is untrusted (clamped to the
  instance's memory); it is a measurement, never a pass/fail input.
- A process the repo's tests detach can outlive the test timeout while the push token is live;
  bounded by the push gateway (pinned update only) and the Worker-side trunk read.
- Merges over 1 MiB (git's probe request) are not exercised live.
- A `Granted` whose send fails after the write is not re-sent; a resync on `hello` would close it.
- New claims are checked against active claims only, so compatible claims can delay a waiter.
- A `watch` connection does not reschedule the alarm.
- A submission held for review keeps its locks until a reviewer decides it.
- `tessel submit` computes `touched` from a base pinned at the first start and advanced only by
  this agent's `Merged`; deleting `.tessel/state.json` resets it. The steward re-checks coverage on
  the rebased commit (COVER-1, file level).
- `unsafe impl` on an empty impl is file `edit_body` (SYM-SIG).
- Lines over 100 characters predate their tasks: `skills/tessel/SKILL.md` (3, 73, 74, 126),
  `src/coordinator.rs:13`, `src/coordinator/merging.rs` (two), `src/identity.rs:153`,
  `src/shell.rs` (1, 387).

**Standing rules:** Sonnet implementers, Opus reviewers. Up to four lanes at once when the Mac has
headroom (load under about 10; Felix's iOS simulator is often the main load), disjoint files, cargo
as `CARGO_BUILD_JOBS=3 nice -n 10`, one Docker image build at a time (start and stop Docker Desktop
around it). Hand a worktree to a reviewer only after the implementer has committed everything,
and keep the implementer out until the verdict (two overlaps happened on Oct 5). Every lane claims
each file through `tessel` before editing it. No attribution trailer. Deploys of
`tessel-coordinator`, `tessel-steward` and (once HARNESS-1 merges) `tessel-coordinator-swarm` are
approved. In Bash, never put `git push` and the bare word "main" in one command (the push hook
blocks it); write `HEAD:refs/heads/main` or push in a separate command.

**Next actions on resume:**
1. (Done 16:03: swarm deployed and checked, first live run above.)
2. (Done about 16:20: image rebuilt, trunk caught up to `8432f8c`, mirrored.)
3. SHADOW-1 (in progress) is DOGFOOD-4's last step: review, then `tessel submit` from the lane,
   approval as `orchestrator` if held, `merged` in the inbox, `tools/mirror.sh`. Then SHADOW-2:
   `SHADOW_ENABLED = "true"` for the swarm env and a shadow policy in `tessel-swarm`, so the A/B
   table's "Conflicts prevented, verified by shadow runs" cell is measured.
4. Shadow verification (PLAN §9 Oct 9) reusing the trial primitive; swarm and A/B runs (Oct 10);
   milestone pull request from `artifacts-trunk` with `/code-review` and `/security-review`.

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
  Part b CLOSED 2026-10-05 at `3fb687b` (review "Yes" after two fix passes). Workspace tests 551.
  Live: the hook claimed `src/lib.rs::greet` for a body edit and amended `edit-signature` for a
  signature edit; the submit was held for review; `tessel review` approved it and it merged.
- [x] **REVIEW-CLI-FIX** — found live: `tessel review` fails in a directory whose git repo has no
  commit, because it reads HEAD for its hello. A reviewer needs no checkout; send a fixed base.
  CLOSED 2026-10-05 at `125cb3a` (review "Yes" after one fix pass; the coordinator now ignores an
  all-zeros base). Live: review from outside any repo is refused cleanly; head stays real.
- [x] **ASSUME-1** — PLAN §9 Oct 8: challenged assumptions verified after the challenging merge
  by a steward trial (no push, no write token) of the assuming agent's work on the pre-merge and
  post-merge trunk; a break counts only when the baseline was clean.
  CLOSED 2026-10-05 at `4a4644e` (review "Yes" after three fix passes). Workspace tests 595, steward
  445. Live, with the real CLI: a1 assumed `greet() returns 1`; a2's body edit was flagged at risk,
  held for review, approved, merged; `assumption_verified` = `tests_failed` for a1's claim.
- [x] **DOGFOOD-1** — dogfood v1 (PLAN §7, §9 Oct 8; Felix approved): the steward runs Tessel's
  own gate. Test image with the Rust toolchain (wasm target) and pnpm; an install step for repos
  with dependencies (lockfile only, scripts disabled); per-repo test command; Tessel imported into
  Artifacts as `tessel`, one fork per lane, lanes push and `tessel submit`; the Artifacts trunk
  mirrored to GitHub `sprint/build`. After CLI-2b and ASSUME-1 merge.
  Research (Opus, 2026-10-05): run on `standard-4` (4 vCPU, 12 GiB, 20 GB; today every run is on
  `lite`, 256 MiB) with the Internet off; bake the toolchain and dependencies into the image
  (cargo-chef, `pnpm fetch`); `cargo test --workspace --locked --offline` and
  `pnpm install --offline --frozen-lockfile --ignore-scripts`; a changed lockfile fails at step
  `install`; skip wasm and clippy in v1; `tessel.toml` with argv-array gate commands read from the
  trunk commit, never the fork; import with the existing create route plus a push from the Mac;
  mirror with a local fast-forward-only script (no GitHub credential in Cloudflare). Probe first:
  build time and memory, the Docker build context, a 5-minute merge over the service binding, and
  image storage (50 GB per account).
  MERGED 2026-10-05 at `72a2959` (review "Yes" after one fix pass; a semantic conflict with RACE-1
  was caught at compile time by the exhaustive-match rule and fixed on the branch). Not closed as
  dogfood v1: the first live probe failed. See DOGFOOD-2.
- [x] **DOGFOOD-2** — diagnose and fix the failed probe, then rerun it against the go/no-go list;
  switch lanes to merging through the steward only if every threshold is met.
  CLOSED 2026-10-05 at `87f99d9`: `exec` rejected the user name `node` (needs numeric `uid:gid`);
  the mode-000 test skips where it cannot hide a file. Probe 2 then ran the gate (no-go: peak memory
  null, three CLI tests failing only in the container, cargo stopping at the first failure).
- [x] **DOGFOOD-3** — tini reaps children, GNU time measures peak memory (clamped, untrusted), the
  gate runs every test binary.
  CLOSED 2026-10-05 at `b6b8bfb` (review "Yes" after one follow-up). First Tessel merge through the
  steward (admin merge, 96 s, Artifacts trunk `6f78128`); probe 3 passed (58 s, 625 MiB).
- [ ] **DOGFOOD-4** — finish the switch: `orchestrator` in `REVIEWERS`, mirror to
  `artifacts-trunk`, delete the stray `tessel` repo, `docs/BUILD-PROTOCOL.md` for the steward flow,
  first lane merge through the steward.
  MERGED 2026-10-05 at `4793bca` (review "Yes" after one fix pass; 9 of 9 mirror mutants killed).
  Live: `tools/mirror.sh` created GitHub `artifacts-trunk` at `6f78128`; coordinator `66fbe44a`
  accepts `orchestrator` as reviewer; stray `tessel` repo deleted. Open: the first lane merge
  through the steward (after the image rebuild and the trunk catch-up).
  Time budget (from the ASSUME-1 review): one merge's worst case is clone 240 s + fetch 240 s +
  rebase 120 s + dependency check 30 s + tests 600 s, about 20 minutes, above the 13-minute steward
  call timeout and the Durable Object alarm's 15-minute wall limit. DOGFOOD-1 must fit inside it:
  measure the real numbers in the probe, then set the step timeouts so their sum stays under the
  call timeout with margin, or move the merge off the alarm's wall clock.
  Go/no-go for the live probe on `standard-4` (from the DOGFOOD-1 review; any miss is a no-go and
  `merge-one.sh` stays): `exec` runs the gate as uid 1000; `memory.peak` is non-null; `readFile`
  with a 40-hex ref returns the blob and a bogus sha returns null without throwing; the deployed
  image build with `build_context ..` succeeds; `cargo test` compiles no dependency; container start
  under 10 s cold; clone plus fetch under 60 s; install under 45 s; test under 200 s warm and 240 s
  cold; peak memory under 9 GiB; a full merge over the service binding under 600 s; and the gate
  self-protection (submissions touching `tessel.toml` refused, admin-only escape hatch) has landed.
- [ ] **SHADOW-1** — PLAN §8 item 2, §9 Oct 9: when a blocking claim's work merges, the
  coordinator has the steward trial each shadow submission it blocked against the pre- and
  post-merge trunk (the ASSUME-1 primitive) and appends `DenialVerified`; a red baseline or a
  trial without a result counts nothing. The first lane to land through the steward.
- [ ] **CLI-LIVENESS** — found while dogfooding SHADOW-1: claim 35 expired while the lane's daemon
  was running. Heartbeats get no reply (`src/coordinator.rs:824`); `send_heartbeat`
  (`tessel-cli/src/daemon.rs:426-436`) pushes the local expiry forward once the frame is queued,
  and the socket task never pings (`daemon.rs:1855` ignores ping/pong), so a half-open connection
  heartbeats into nothing while the coordinator expires the claim and the daemon never notices.
  Fix without a protocol change: send a WebSocket ping each tick; no inbound frame within about
  lease/2 means a dead link: close, reconnect, and move the local expiry only on proof of delivery.
  Check the lane's daemon log for a connected/closed gap around the expiry first.
- [ ] **SHADOW-GC** — from the SHADOW-1 review: submitted shadow claims are never removed (true
  before SHADOW-1). They hold no locks, leases or queue positions, but state grows by one claim per
  shadow submit in experiment runs, and a shadow blocked only by a race can never be verified.
  Drop a submitted shadow claim once none of its `blocked_by` claims is live and none of its trials
  is queued. Before the swarm runs that use shadows at scale.
- [x] **RACE-1** — PLAN §9 Oct 8: races (invariant 7) in the coordinator: open by a reviewer,
  join, outsiders denied with `Conflict.race`, entries ranked with `rank_entries` after a steward
  trial each, winner merged, losers rejected, `HumanPick` waits for `PickWinner`.
  CLOSED 2026-10-05 at `fe57323` (review "Yes" after one fix pass; HumanPick bounded, unmeasured
  criteria refused). Workspace tests 677. Live: race opened, outsider denied with the race named,
  entry trial `tests_passed: true`, winner merged. Found live: judged as soon as the only entrant
  submitted, so a second join got `race_closed`.
- [x] **RACE-FIX** — judge a race early only when it is full and every entry has submitted;
  otherwise at the deadline.
  CLOSED 2026-10-05 at `ea9bd43`. Live on `6411b1db`: two entrants joined and submitted, the
  failing entry was filtered by its trial, the winner merged, the loser got "lost the race".
- [x] **HARNESS-1** — PLAN §8, §9 Oct 9–10: `tessel-swarm`, a seeded workload generator and
  scripted agents in two modes: coordinated (real protocol, numbers from `Summary::from_events`)
  and uncoordinated local replay (labelled local); JSON and a Markdown A/B table. Targets only
  `swarm-*` repos.
  CLOSED 2026-10-05 at `0c7a776` (review "Yes" after two fix passes). Workspace tests 745 on the
  merged tree, as predicted. Not live yet: the swarm Worker awaits its secret.
- [x] **SYM-SIG** — from the CLI-2b review: attributes, derives, doc comments, decorators and
  `impl` bounds count as file `edit_body`, so `review_reasons` never flags them as signature
  changes. Put leading attribute and decorator siblings in the signature range.
  CLOSED 2026-10-05 at `05793e3` (review "Yes" after one fix pass). Doc-only changes count as body
  (Felix-delegated ruling, review by exception). Dogfood record includes two late claims, stated in
  the merge note.
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

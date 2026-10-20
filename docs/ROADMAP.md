# Roadmap

Task list for the work in `PLAN.md`, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope
and order; `docs/proposals/real-use.md` holds the requirements, invariants and done-checks this
file's tasks refer to. Only the orchestrator edits this file. The contest-era roadmap and build log
are in git history (`78bcb9a` and earlier).

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-20 10:30 EDT. Merged since the last rewrite: the previous STATE (`53d799e`,
#60) and P1-FLAKE-UNDER-LOAD (`fe7ac16`, #62). The orchestrator's gate on `fe7ac16` is green
(1686 tests, all seven steps). M2's done-check waits on P1-CLI-ON-HOST.
**The nightly is broken on main:** since 20 October, shard 0 of the full mutants run ends with
"The runner has received a shutdown signal" (scheduled run 39393491358; last green 39225162697
on 19 October). The cause is the mutant `src/protocol.rs:168` `parent_dir -> "xyzzy"`, which makes
`Scope::ancestors` grow until the runner runs out of memory. P1-MUTANTS-PROPTEST-ONLY (#61)
caps each test process's memory so the shard completes; until it merges, expect the scheduled
nightly to fail.
**In flight:**
- P1-MUTANTS-PROPTEST-ONLY (lane A, `.claude/worktrees/p1-mutants-proptest-only`, #61), fix
  pass 1: one proptest the filter missed, and the nightly's memory cap. The filtered run on
  `a162b31` missed only the baseline; it is repeated on the final head with every proptest out.
- CLI-GITHOOK-FIRST-COMMIT and SWARM-AGENT-CHECKS-UNDER-LOAD (lane B, in turn).
**Next:** lane A takes P1-CLI-ON-HOST once decision 14 is made. P1-FAKE-HELLO-LOST is decided
when P1-CLI-ON-HOST is dispatched, since that task replaces the fake's orchestration.
**Decided by Felix (19 October), M2's done-check:** it covers the host rows of the rules list.
The adapter rows R2-R5 and R49 stay "after move" until M4, whose done-check makes each "now" with
its named adapter test; with `runtime.rs` deleted in M3 they have no code to protect meanwhile.
**Pending from Felix:** the plan's decisions 1-11, 13 and 14 and proposed changes A-D
(`docs/proposals/phase-1.md`). Decisions 1 (no `wrangler dev` captures) and 14 (the CLI's fake
rebuilt on the host in M2) apply to M2: the move proceeds on them as recommended, and
P1-CLI-ON-HOST needs decision 14 before it is dispatched. Tearing down the spike sandbox needs
`gh auth refresh -s delete_repo`; it costs nothing and can stay until Phase 2.
**Stacked pull requests:** GitHub retargets a stacked pull request to `main` only when its base
branch is deleted, so #26 merged into #25's branch and was reopened as #28. Stack no more pull
requests; open the second after the first merges.
**Rule from 18 October:** no synthetic CPU load on this Mac (it broke another project's builds);
load and flake checks run on CI runners.

## Phase 0: foundations

- [x] **P0-CI** (`348c1ff`, #19) — GitHub Actions, every action pinned to a full SHA with its
  version in a comment, `persist-credentials: false`, clean under `actionlint` and `zizmor`. Fast
  lane (target under three minutes): `cargo fmt --check`; clippy `-D warnings` native and for
  `wasm32-unknown-unknown` with `--features runtime`; the core library's tests. Slow lane: the
  workspace through cargo-nextest; the steward's `pnpm install --frozen-lockfile`, typecheck, lint
  and tests; `cargo deny check advisories bans`. A pull request template with a field for the new
  test's failing output (CLAUDE.md rule 9). Known flakes the slow lane will meet are fixed or
  quarantined with a named task, never retried silently (SWARM-ONLOCAL-LOAD-2 below). Done: both
  lanes green on the task's pull request, with measured durations in the report.
- [x] **P0-MQ-PROPTEST** (`f00d10b`, #20) — property tests for the merge queue
  (`src/coordinator/merging.rs`) interleaving every `MergeOutcome` (`Merged`, `Conflict`,
  `TestsFailed`, `ServiceUnavailable`, `MainMoved`, `AlreadyMerged`), cut-off recovery and the
  watchdog with claims, leases, review holds and races. Checks: at most one `Merged` per claim; a
  held submission is never dispatched; attempt counts and bounds (`MAX_MAIN_MOVED`,
  `MAX_INFRA_RETRIES`) hold; state round-trips through serde at every step; the queue never wedges
  (if a submission is eligible, `begin_merge` or the next wake dispatches it). Tests only, no
  behaviour change; a property that exposes a bug is recorded as a new task, not fixed in this lane.
  Done: green at 10,000 cases, with the run time reported.
- [x] **P0-DISPATCH-ID** (`1d29bca`, #22) — a persisted, monotonic dispatch id on every merge and
  verification dispatch, carried by the shell's call, and matched in `merge_outcome` and the
  verification outcome; a stale answer changes nothing (proposal, correctness requirement 5 and I7).
  Done: a property test showing a stale answer is ignored, failing before the change.
- [x] **P0-LANDED-COMMIT** (`4a219f7`, #28) — `already_merged` no longer becomes `Landed { head:
  base }` (`src/merge.rs`); the outcome carries the landed commit when known, and no verification or
  trial is queued until it is (proposal, fixes table). Done: a test that no verification compares
  main with itself, failing before the change. Built: the steward's `already_merged` base is main as
  it is now, not the landed commit, so the coordinator records main at each dispatch
  (`InFlight::main`) and keeps the first one whose answer was lost (`unanswered_from`); a retry
  answered `already_merged` lands with that as `before`. With none, or with main unchanged, the
  landing is recorded but nothing is queued to verify it. No steward change.
- [x] **P0-CLI-REPLAY-UNKNOWN-EVENT** (`5400daf`, #23) — carried over: `tessel-cli/src/daemon.rs`
  aborts replay or drops the socket on one event it cannot parse, so any new event kind breaks older
  daemons. Decode the envelope tolerantly (seq plus a possibly unknown kind) and keep replaying.
  Must land before P0-VERIFY-SLOT adds an event kind. Done: a test replaying a log with an unknown
  kind.
- [x] **P0-VERIFY-SLOT** (`3695300`, #25) — after N merges (setting, default 4) the next merge waits
  if the head verification is ready and not in backoff; `begin_merge` respects a verification in
  flight; dropped verifications are logged as a new `VerificationDropped` event and counted in
  `Summary` (proposal, fixes table, I12). Needs P0-CLI-REPLAY-UNKNOWN-EVENT. Done: a property test
  that a ready verification runs within N merges under continuous merge load.
- [x] **P0-DOC-MAIN-MOVED** (`4a219f7`, #28) — from the P0-MQ-PROPTEST review: `src/merge.rs`
  documents `MAX_MAIN_MOVED` as answers "in a row", but the code counts them per submission (an
  infra failure does not reset the count, which is the safer rule), and `merging.rs` says "up to
  MAX_MAIN_MOVED times" where there are MAX_MAIN_MOVED - 1 re-dispatches. Fix the docs to match the
  code. Small.
- [x] **P0-FLAKE-WALLCLOCK** (`1a458b1`, #27) — at a local load average near 240, two tests fail on
  wall-clock checks in code no Phase 0 change touched: `tessel-swarm`
  `off::tests::agents_work_at_the_same_time` ("branches were built one after another") and
  `tessel-cli` `heartbeats_keep_a_claim_alive_past_its_lease` (once; passed on rerun). Both pass on
  GitHub. Replace the timing assertions with ordering or explicit signals, or move them behind the
  daemon clock seam planned for Phase 1 (proposal, "Flake control"); never add retries.
- [x] **P0-UNVERIFIABLE-LANDING** (this pull request) — from the P0-LANDED-COMMIT review: when a
  recovered merge lands with no known baseline, its assumption verifications are not queued and
  nothing records that they were skipped. Make it visible (rule 7, invariant 10). Decided after
  adversarial critique, at Felix's delegation: a new `VerificationDropped` reason would add an
  enum value to a logged event, which invariant 13 says bumps `PROTOCOL_VERSION`, and its
  `ReleaseReason` would be false (the assuming claim is still live). Built instead: `Merged`
  gains the optional lists `unverified` and `untried_shadows`, and `Summary` the separate counter
  `verifications_unverifiable`; additive, no version bump.
- [ ] **COORD-HEAD-FROM-AGENT** — from the same review: until the first landing or head read,
  the coordinator's `head` is the base the first agent declared in its `Hello`
  (`src/coordinator.rs`), and it becomes the baseline of the first dispatch unchecked. Phase 1:
  read the trunk head from the git host when the repo loads, before any dispatch.
- [x] **P0-SPIKE** (this pull request) — the GitHub spike in the proposal ("The GitHub adapter",
  spike paragraph): does a fork pull request's check satisfy a required-checks ruleset for a direct
  `updateRefs` of that commit; is a stale `beforeOid` refused with the App as bypass actor; check
  and webhook latencies. Done: a written answer in `docs/proposals/` and a script that reproduces
  it. Felix created the organisation `tessel-sandbox` and the private App `tessel-lander-spike` (ID
  5368982; contents, pull requests and administration write; checks, statuses and actions read; no
  webhook) on 18 October. Its private key is kept outside the repository and is never committed.
  Answered in `docs/proposals/github-spike.md`: a fork pull request's check satisfies the
  required-checks ruleset for a direct `updateRefs`, also for the App as bypass actor of the
  update-restriction ruleset; a stale `beforeOid` is refused, but with a generic "Something went
  wrong" error, so the lander reads the ref back; checks took 9-14 s on a trivial workflow; a
  contents-write token is enough to land. Landing is a durable job finished by reconciliation.

## Phase 1: portable host, `tessel-server` and the Rust lander

The plan, its milestones M1-M6, lanes and file ownership are in `docs/proposals/phase-1.md`. Each
task's full text (files, what, done-check) is there; this list tracks status. M1 and M2 are
planned to task level; M3-M6 are re-planned at each milestone boundary.

### M1. Guardrails and the host's types (done 19 October)

Done when: the mutation job is required and green; the nightly run is green once; the rules check
passes; the host types round-trip; the lander git tests pass.

- [x] **P1-CI-MUTANTS** (`1e48ff4`, #35) — lane B. Per-pull-request mutation check on changed
  lines, sharded, with one aggregating required job; a `workflow_dispatch` run for unchanged files.
- [x] **P1-RULES** (`0b6ff41`, #36) — lane A. Every rule in `src/runtime.rs` named, with its
  status ("now", "after move", "dropped") and a script that fails if a "now" row's test is missing.
- [x] **P1-CI-NIGHTLY** (`b3b6c52`, #39) — lane B, after P1-CI-MUTANTS. Proptest at 10,000
  cases and full mutants nightly, compared with a baseline that only shrinks. The first full run
  (39176270373: 904 mutants, shards 26-38 minutes) gave the 33-line baseline.
- [x] **P1-HOST-TYPES** (`9fd80cb`, #40) — lane A, after P1-RULES. The host's `Input`,
  `Output`, `Call` and `CallAnswer` types, no behaviour. Where they differ from the frozen `Call`
  text (decision 12) is listed in #40's description; Felix merged it on 18 October.
- [x] **P1-LANDER-GIT** (`e520463`, #45) — lane B, after the CI tasks. The lander's git layer
  and coverage check ported from the steward, with a deterministic rebase. Git runs with a cleared
  environment and fixed config; repository config is not neutralised, so a test shows a filter
  and merge driver run in an in-place rebase and not after a clone, which led to the clone-only
  `Workspace` (#46).
- [x] **P1-FLAKE-REPLAY-HELLO** (`e283369`, #41) — found on `main`: the slow lane failed on
  #34's merge (run 39165527090, a docs-only change) at `tessel-cli/tests/replay_unknown.rs:146`,
  "the socket was dropped and redialled". Cause: the daemon's event-log read opens a second
  socket whose `Hello` can reach the coordinator after `start` returns. Built: `start_settled`
  waits for the complete-read line; `liveness.rs` had the same race with a wider window and is
  fixed too; a test pins the two hellos.
- [x] **P1-MUTANTS-SHRINK** (`9a875fc`, #42) — from P1-CI-NIGHTLY's first full run
  (39176270373): of the 21 missed mutants outside `src/store.rs`, 18 are killed by behaviour tests
  and 3 are equivalent (`next_to_merge` `<` → `<=`; `recover_merge` and `merge_outcome` `||` →
  `&&`, which differ only in a state nothing can reach). The baseline keeps those 3 and the 12 in
  `src/store.rs` (15 lines). The pull request's nightly also caught `waiters_conflict`, which
  only a proptest had killed, for some seeds; an example test now kills it.
- [x] **P1-HELLO-FALSE-PASS** (`98cb8e3`, #43) — from the P1-FLAKE-REPLAY-HELLO review: every
  hello check across a dropped connection in `tessel-cli/tests/cli.rs` and `submit.rs` now starts
  from a settled daemon, and the replay-frame loop waits for each failed read and its inbox notice
  before the next round. Each check fails when the redial it claims is removed.
- [ ] **P1-FAKE-HELLO-LOST** — from the P1-HELLO-FALSE-PASS review: with a 1.5 s delay at the
  top of `snapshot_task`, the fake coordinator (`tessel-cli/tests/support/mod.rs`) sometimes does
  not count a log read's `Hello` (5 hellos before a third drop in some runs, 6 in others). Find
  whether the fake drops it or the daemon never sends it; `replay_unknown`'s
  `a_settled_daemon_has_sent_a_hello_on_each_of_its_two_sockets` (exactly two) could flake on it.
  Done: the cause named, and a test that pins it.
- [ ] **P1-MUTANTS-PROPTEST-ONLY** — from #42's nightly (39189028810): `waiters_conflict`
  (`src/coordinator.rs`, `||` → `&&`) was missed in one full run and caught in two, because its
  only killer was `coordinator_matches_brute_force_model`, which finds it for some seeds. Any
  mutant killed only by a proptest makes the nightly comparison fail at random. List the mutants
  each proptest kills that no example test kills (a full mutants run with the proptests filtered
  out, on CI), and give each an example test. Done: that run misses nothing outside the baseline.

### M2. The move

Done when: no host row of the rules list is "after move" (the adapter rows R2-R5 and R49 are
M4's, decided by Felix on 19 October); `cargo test --workspace` is green with the CLI suite on the
host; the wasm lane is still green (`runtime.rs` unchanged); the ported steward tests are green.

- [x] **P1-MOVE-CALLS** (`febea36`, #49) — lane A. The call path moved into `src/host/` with its
  test driver (`testkit.rs`): rows R14-R19, R28-R32 and R42. Outstanding calls are a table of
  `Call` values; applying an answer's effects to the core is left to P1-MOVE-WAKE (R21-R27). The
  plant `2ba1227` failed 15 new host tests on CI (run 39272015573); the tests it does not reach
  were shown red locally. R43 is unreachable (`Work::Plain` is never refused): whichever task
  moves it marks it "dropped" with that reason.
- [x] **P1-MOVE-SOCKETS** (`90a4fac`, #52) — lane A. Sessions and the socket table: rows R6-R13,
  and the halves of R31 and R32 P1-MOVE-CALLS left. New host inputs `Input::Binary` and
  `Input::SendFailed` (the proposal's input list had neither; only `Call` is frozen). A failed send
  to the sender closes with reason "send failed", not "coordinator error", and dead sockets are
  withdrawn on `SendFailed`, after queued inputs: both stated on the R31 and R32 rows. CI evidence
  in two plants, so no rule's mutant stops another rule's test first (runs 39301356658 and
  39301559065, each test checked at its own assertion line).
- [x] **P1-MOVE-WAKE** (`35202e7`, #56) — lane A. The wake, its calls and the trunk poke:
  rows R20-R27, R38, R41 and R47 are "now"; R43 is "dropped" (every step with no sender is
  `Work::Plain`, never refused). A wake issues its call and returns; `Replied` or `CallLost`
  resumes it at the next step, and answers are applied through a match on the call's `Request`.
  Decides COORD-WAKE-WHILE-VERIFYING: while any call is out, a `Wake` only re-arms the watchdog,
  as today. The test-only `begin_call`, `StartJob`, `MarkerStep` and `Job::Start` are gone. A
  merge is one `Prepare` call standing for the steward's `/merge` until M3 (HOST-MERGE-STANDIN).
  CI evidence in six plants, each rule's test checked at its own assertion line.
- [x] **P1-MOVE-REPLAY** (`148b187`, #58) — lane A. `Watch` replay (R36) with every other input
  queued until it ends, R46 (errors name the repo and the failed operation) and R50 (the stored
  size set on the first load only). A failed send to the replayed socket ends the replay at once,
  unmarked, before the socket is closed and withdrawn. Two declared differences from
  `runtime.rs`, in R36's row: one replay can log up to a page of failed-send lines, and a failing
  last page marks the watcher before its socket is closed. The adapter must always answer
  `ReadEvents`, or the gate stays shut. CI evidence in nine plants, each test checked at its own
  assertion line.
- [ ] **P1-CLI-ON-HOST** — lane A, after the four above; needs decision 14. The CLI's fake
  coordinator rebuilt on the host, same test count.
- [x] **P1-LANDER-MERGE** (`70526c9`, #48) — lane B. `prepare` and `land` ported from
  `merge-steps.ts`, with the clone-only `Workspace` type; 53 unit and 14 real-git tests ported.
  `clone_main` refuses a destination that is in use, a symlink or contains `..`, and makes it
  absolute; every `&Workspace` command pins `GIT_DIR` and `GIT_WORK_TREE`; the fixed environment
  also pins `core.sshCommand` and resets `credential.helper`.
- [x] **P1-LANDER-TRIAL** (`034f521`, #51) — lane B. `run_trial` and the two-sided report; 33
  steward cases ported. A trial clones its own `Workspace`, and read access is revoked before any
  repo code runs on every path. `TrialPorts` has no push and no read-back (compile-fail
  doctests). The sandbox half of `merge-trial-types.test.ts` is left to the M4 executor.
- [x] **P1-LANDER-ISOLATION** (`bda5a49`, #54) — lane B. 19 real-git tests that hooks, filters,
  drivers, diff programs, malformed objects and control characters in a fork are refused or inert.
  The tests found that a fork could name git's built-in `union` driver and land a conflict as
  merged: `clone_main` now writes `* merge=text` to `info/attributes`. Declared difference: a
  repository's own built-in driver on main is overridden too, which only adds conflicts. If a
  repository needs its own `merge=union`, `GIT_ATTR_SOURCE=<base>` (git 2.42, above the 2.31
  floor) would honour main's attributes and ignore the fork's.
- [x] **P1-LANDER-AFTER-TESTS** (`716aca5`, #59) — lane B. The package step runs in a
  `TestTree`, a second clone of the rebased head that the port cannot turn into a `Workspace`;
  `land` pushes from the untouched `Workspace`. A hostile step's `url.*.pushInsteadOf`,
  `include.path`, `remote.<URL>.pushurl` and `.receivepack` in its tree change nothing `land`
  does, and each subverts `land` when planted in the workspace (controls). It does not stop
  hostile code run as the same OS user, which can find the workspace by path: that is
  M4-SANDBOX-WORKSPACE.
- [x] **P1-FLAKE-UNDER-LOAD** (`fe7ac16`, #62) — lane B. The lander runner's timeout tests,
  the CLI lease and heartbeat tests and the swarm reconnect tests no longer depend on the
  scheduler: the fake coordinator's clock can be frozen and advanced, a `pre-receive` hook holds
  the shadow agent's push instead of a 250 ms delay, and waits end at once on the cause (a release
  or a lapse of the waiter's own claim) instead of running to their bound. One real-time bound
  stays, explicit: `a_healthy_link_is_kept_and_the_local_expiry_follows_the_heartbeats` keeps its
  3 s lease, because the daemon's pong silence limit runs on the daemon's own clock. Two recorded
  failures have no cause found; both tests now fail fast with the cause printed, filed below.
- [ ] **CLI-GITHOOK-FIRST-COMMIT** — lane B. From P1-FLAKE-UNDER-LOAD: in
  `tessel-cli/tests/githook.rs`, `a_submitted_claim_no_longer_covers_the_work_it_was_submitted_for`
  once had the hook refuse `src/c.rs (create)` right after `tessel claim` exited 0, under heavy
  load. Not reproduced with a 2.5 s delay in the replay. Candidates: the hook's 5 s
  `STATUS_TIMEOUT` (`tessel-cli/src/githook.rs`), or a claim made right after `tessel start` (the
  test now waits for the daemon to settle, which may hide the race: reproduce with plain
  `start`). Done: the cause named, and a test that fails on it and passes with the fix.
- [ ] **SWARM-AGENT-CHECKS-UNDER-LOAD** — lane B. From P1-FLAKE-UNDER-LOAD: in
  `tessel-swarm/tests/reconnect.rs` an agent released its claim without submitting, under heavy
  load; the same log comes from the agent's own checks (`node --test`) failing. Why they fail under
  load is unknown, and it may be a product bug. The checks note carries no output of the failing
  run. Also: the `UNHEARD_LEASE_MS` comment still says the wait could match the holder's lapse,
  which is no longer true. Done: the failing run's output kept in the note, the cause named, and a
  test for it.
- [ ] **HOST-MERGE-STANDIN** — M3, with P1-CORE-STAGED-LANDING; its adapter half is M4's. From
  the P1-MOVE-WAKE review: until the staged landing, a merge is one `Prepare` call that stands for
  the steward's whole `/merge`, and the host reads `Prepared { base, head }` as merged
  (`src/host/calls.rs`). An adapter that gave `Prepare` its frozen meaning (rebase, no landing)
  would make the core report merges that never landed (rule 6). The frozen `Prepare` also has no
  field for the merge record `runtime.rs` sends today (`merge::request_body`: the claim's intent,
  decision record and review), so the steward's evidence record would lack them. Done: the
  staged landing removes the stand-in, or M4's adapter maps `Prepare` to `/merge` with a test;
  and the record reaches the lander, by a route Felix approves if it changes `Call`.
- [ ] **HOST-CALL-DEADLINE-BACKSTOP** — M4, with the adapter's call deadlines. From the
  P1-MOVE-WAKE review: while a call is out, each `Wake` re-arms the watchdog, so a merge stalls
  if the adapter never answers or sends `CallLost`. The host knows each call's `deadline_ms`.
  Done: a host test that a call past its deadline with no answer is treated as lost, or a
  written reason that the adapter's deadline is enough.
- [ ] **M4-SANDBOX-WORKSPACE** — M4, with the local test runner's sandbox. From the
  P1-LANDER-AFTER-TESTS review: the `TestTree` split stops the port API and accidental writes, not
  hostile repo code run as the same OS user. The tree names the workspace path (`remote.origin.url`,
  `.git/logs/HEAD`), so such code can write the workspace's `.git/config` directly. Done: the
  sandbox denies the package step the workspace directory and kills its leftover processes
  before `land`, with a test of each; `prepare` and `land` being public is weighed there too.
- [ ] **SERVER-CONFLICT-NAMES** — M4, with `tessel-server`'s merge response. From the
  P1-LANDER-ISOLATION review: `Conflict.files` (merge and trial) carries fork-chosen file names
  raw, with no cap or total. The steward does not cap them either (`merge-steps.ts` 144-146), and
  the core reads only their count (`src/merge.rs` 264-267), so nothing reaches an agent today.
  Done: before any agent or log sees them, the names are capped at 50 with a total and quoted
  (rule 3), with a test using control characters.

Carried-over tasks with slots in the plan: COORD-HEAD-FROM-AGENT and COORD-UNDELIVERED-GRANT
(see "Carried-over tasks" in the plan). COORD-WAKE-WHILE-VERIFYING was decided by P1-MOVE-WAKE.

## Carried over from the contest build

Kept, and scheduled where they fit:

- [x] **SWARM-ONLOCAL-LOAD-2** — already done before Phase 0 (the test asserts the order of the two
  `Submitted` events, `tessel-swarm/tests/on_local.rs`); the contest roadmap had left the box open.
- [x] **CLI-GITHOOK-REINSTALL** — already done before Phase 0 (`cdb445c`: a different hook moves the
  old backup to a dated name and is chained; tests in `tessel-cli/tests/githook.rs`); the contest
  roadmap had left the box open.
- [x] **SWARM-PUSH-PHRASES** — already done before Phase 0 (all four phrases are in the transient
  set with tests, `tessel-swarm/src/git.rs`); the contest roadmap had left the box open.
- [ ] **COORD-UNDELIVERED-GRANT** — a `Granted` that fails to send closes the socket but leaves the
  claim held until its lease lapses. Phase 1, in the host (send failure handling).
- [x] **COORD-APPLY-LOADED** — already done before Phase 0 (`apply_loaded` reloads the core before
  applying a steward outcome, `src/runtime.rs`); the contest roadmap had left the box open.
- [x] **COORD-WAKE-WHILE-VERIFYING** (`35202e7`, #56) — from the P0-MQ-PROPTEST review:
  `runtime.rs` (~661) calls `next_wake_ms(true, false, ..)` even while this instance is
  verifying, which looks like an immediate-alarm loop during a verification. Decided by
  P1-MOVE-WAKE: while any call is out, a `Wake` only re-arms the watchdog, pinned by I4 host
  tests that the wake is in the future.
- [x] **REVIEW-SENSITIVE** — already done before Phase 0 (`ae62618`: submissions touching paths in
  the `SENSITIVE` variable are held); the contest roadmap had left the box open.
- [ ] **REVIEW-NOTE** — a reviewer's note never reaches the agent. Needs a design (untrusted text,
  rule 3) and Felix's ruling. Phase 3.

Dropped, because the code they concern is deleted in Phase 1 and nothing runs it:
STEWARD-ACCESS-STALE-KEYS, GATE-PATH, GATE-OUTPUT.

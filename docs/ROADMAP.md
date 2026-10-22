# Roadmap

Task list for the work in `PLAN.md`, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope
and order; `docs/proposals/real-use.md` holds the requirements, invariants and done-checks this
file's tasks refer to. Only the orchestrator edits this file. The contest-era roadmap and build log
are in git history (`78bcb9a` and earlier).

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-22 12:20 EDT. Main at `86c93ca` (#90, #91, #92 and #89 merged in that order;
the orchestrator's gate on `86c93ca` is green on all seven steps, 1923 tests; main's CI green). M4
is three tasks in: the server lands over HTTPS with a token file read through its own askpass hook
(#92), `docs/dogfood.md` and its script set the dogfood server up (#91; the orchestrator walked
the page once more on `main`'s own config with a placeholder token: the script, the server start,
a held submission kept over a restart, the rejection by `orchestrator`, zero token hits in the
logs), and the build protocol's section 2 describes landings through the server (#90), in force
from the first landing this STATE records. Nothing is dispatched.
**Waiting on Felix:** the GitHub steps of `docs/dogfood.md` (the fine-grained token saved 0600,
the three private fork repositories, ruleset step 4) and a word on D1-D4 (the page states them as
proposals; merging it is not that word); for the sandbox landing that closes M4-GITHUB-REMOTE's
done-check, a token with contents write on two new private repositories in `tessel-sandbox`
(`m4-trunk`, `m4-fork-lane-b`), saved 0600 anywhere he names, or the word to skip it since the
dogfood landing proves the same path; this STATE.
**Blocked:** CLI-GITHOOK-FIRST-COMMIT, not reproduced (see its entry).
SWARM-AGENT-CHECKS-UNDER-LOAD waits for its next failure's note. P1-FAKE-HELLO-LOST: the test
cannot see the flake (see its entry).
**Next:** dispatch CI-ACTIONLINT-ZIZMOR (lane A) and SERVER-FORK-CASE (lane B) from `86c93ca`,
both due before the first landing and the second lane name; then, on Felix's GitHub steps, the
orchestrator starts the server from the page and M4-FIRST-LANDINGS begins with those two tasks'
own landings.
**Found at dispatch (22 October):** `tessel.toml` allows only `cargo test`, `pnpm test` and `npm
test`, so clippy, fmt and the named-tests check stay on CI after a landing, and `actionlint` and
`zizmor` run nowhere but the lanes' gates until CI-ACTIONLINT-ZIZMOR; the runner inherits no
`CARGO_TARGET_DIR`, so each landing builds the workspace cold (237 s measured). `devfep/tessel`'s
ruleset "main: CI required" (`pull_request`, `required_status_checks`, `non_fast_forward`,
`deletion`, no bypass actors) refuses the lander's push until step 4; the repository admin role
(id 5) as bypass actor can then also force-push or delete `main`, which M6 removes.
**Decided by Felix (20 October), decision 15, local first:** M3 the local server, M4 its dogfood
on this repository, M5 hardening, M6 team mode and the deletion; the first local release lands
through the host's single `Prepare` call (`docs/proposals/phase-1.md`, decision 15; `PLAN.md`
decision 6). Ruleset step 4 (Felix as bypass actor) is his, before M4's first landing.
**Decided by Felix (20 October), decision 14:** the CLI's fake coordinator is rebuilt on the host
in M2 (P1-CLI-ON-HOST), and the swarm's test host in M5, as recommended in
`docs/proposals/phase-1.md`.
**Decided by Felix (19 October), M2's done-check:** it covers the host rows of the rules list.
The adapter rows R2-R5 and R49 became "now" with P1-SERVER's named adapter tests, R1 with
P1-IDENTITY.
**Pending from Felix:** the plan's decisions 1-11 and proposed changes A-D (B now M6)
(`docs/proposals/phase-1.md`); decision 13 was carried out as recommended (P1-DROP-WORKER merged).
Tearing down the spike sandbox needs `gh auth refresh -s delete_repo`; it costs nothing and can
stay until Phase 2.
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
- [ ] **COORD-HEAD-FROM-AGENT** — in #92 (M4-GITHUB-REMOTE, under review): a repository with
  no head reads main's head from the remote at server start, before the first `Hello` can adopt
  an agent's declared base. Left open by that change: a `Hello` that arrives before the read
  still sets the head (the read's answer is then dropped), and a failed start read is not
  rescheduled. From the P0 review: until the first landing or head read, the coordinator's
  `head` is the base the first agent declared in its `Hello` (`src/coordinator.rs`), and it
  becomes the baseline of the first dispatch unchecked.
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
planned to task level; M3-M6 are re-planned at each milestone boundary (re-ordered on 20 October,
decision 15: local server, dogfood, hardening, team mode).

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
  Checked in P1-CLI-ON-HOST (21 October): with the delay, the test passes 20 of 20 on the host
  fake and 10 of 10 on the old one, so it cannot see the flake, which was a 5-versus-6 count before
  a third drop; the cause is not found and `daemon.rs` is untouched. Reopen from the next failure.
- [x] **P1-MUTANTS-PROPTEST-ONLY** (`4e68ed9`, #61) — lane A. A dispatched nightly takes a
  nextest filter (`test_filter`); with all ten proptests filtered out, the full mutants run misses
  nothing outside the baseline (run 39444984593). Two coordinator tests now pin the order of a
  denial's conflicts, which only a proptest caught before (`Blocked::key`). The nightly's shard 0
  had died on every run since 20 October: the mutant `parent_dir -> "xyzzy"` made
  `Scope::ancestors` grow until the runner ran out of memory. Each test process is now capped at
  3 GiB, so the runner survives and the mutant counts as a timeout; two runaways at once could
  still exceed 16 GB, as the comment beside the cap says.

### M2. The move (done 21 October)

Done when: no host row of the rules list is "after move" (the adapter rows R2-R5 and R49 are
M3's after decision 15, decided by Felix on 19 October); `cargo test --workspace` is green with
the CLI suite on the
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
  merge is one `Prepare` call standing for the steward's `/merge` until M5 (HOST-MERGE-STANDIN).
  CI evidence in six plants, each rule's test checked at its own assertion line.
- [x] **P1-MOVE-REPLAY** (`148b187`, #58) — lane A. `Watch` replay (R36) with every other input
  queued until it ends, R46 (errors name the repo and the failed operation) and R50 (the stored
  size set on the first load only). A failed send to the replayed socket ends the replay at once,
  unmarked, before the socket is closed and withdrawn. Two declared differences from
  `runtime.rs`, in R36's row: one replay can log up to a page of failed-send lines, and a failing
  last page marks the watcher before its socket is closed. The adapter must always answer
  `ReadEvents`, or the gate stays shut. CI evidence in nine plants, each test checked at its own
  assertion line.
- [x] **P1-CLI-ON-HOST** (`cf1ed21`, #70) — lane A; decision 14 made 20 October. The CLI's
  fake coordinator (`tessel-cli/tests/support/mod.rs`, the only file changed) drives `Host::step`
  directly under one lock, feeding storage and replay answers at once; every seam keeps its name
  and meaning and the suite is 537 tests before and after. Seams: the clock drives a wake timer;
  `merge_next` and `verify_next` feed a `Wake` and answer `Prepare` or `Trial`; `act` is a
  synthetic socket (one extra `AgentConnected` event, counted by no test); the lease is set by
  seeding a patched stored state, since `HostConfig` has no lease field; a call the fake cannot
  answer fails naming it. Plant `7fa59b8` (R29) was red on CI (run 39525193786: the core's R29 test
  and the first four `cli` tests) and its revert green (39525475588). Review mutants pinned twelve
  seams; six are pinned by no CLI test (CLI-FAKE-SEAM-GAPS below).
- [x] **CLI-FAKE-SEAM-GAPS** (`adcc7b6`, #77) — lane A, with the `cli.rs` lease tests. From
  the P1-CLI-ON-HOST review: mutants of the fake survive the whole CLI suite for the wake timer
  (`advance_clock` not waking it), `act`'s close, a socket's close reaching the host (the old
  fake with its disconnect removed also passed 537 of 537), replay page truncation, the review
  reorder (the core replies `ReviewRequired` alone, so nothing is reordered) and the head-read
  answer (the fake never pokes the trunk). Also: `merge_next`'s outstanding-trial check uses
  `matches!`, and a failure inside the timer task would surface as a timeout. Done: a CLI test
  that a queued wait is withdrawn on the waiter's last close, one per seam a CLI behaviour
  depends on, and the rest written down as fake-only with a reason. #77: the wake timer (its test
  holds a 120 s lease so a heartbeat cannot close the window), `act`'s close and a socket's close
  are pinned; the old truncated-read test pinned nothing (the fake dropped the cut socket before
  any event), and `cut_replay` now keeps every event before the agent's release; the review
  reorder, the head-read answer and the replay page size are documented fake-only; `merge_next`
  is exhaustive; a timer-task panic fails the next call naming it. Fourteen plants each red.
- [x] **CLI-REVIEW-ORDER-TEST** (`01e3ce3`, #82) — lane A. From CLI-LEASE-TESTS: the CLI
  review-order test passed with its seam removed (the core replies `ReviewRequired` alone), so it
  pinned nothing. Reworked against the real core: a held submission replies `ReviewRequired` and
  the daemon expects an approval; the fake's `review_before_accepted` seam is deleted. The daemon
  still tolerates `Accepted` before `ReviewRequired`, an order the core never sends (a dead-code
  candidate for the next CLI task).
- [x] **P1-LANDER-MERGE** (`70526c9`, #48) — lane B. `prepare` and `land` ported from
  `merge-steps.ts`, with the clone-only `Workspace` type; 53 unit and 14 real-git tests ported.
  `clone_main` refuses a destination that is in use, a symlink or contains `..`, and makes it
  absolute; every `&Workspace` command pins `GIT_DIR` and `GIT_WORK_TREE`; the fixed environment
  also pins `core.sshCommand` and resets `credential.helper`.
- [x] **P1-LANDER-TRIAL** (`034f521`, #51) — lane B. `run_trial` and the two-sided report; 33
  steward cases ported. A trial clones its own `Workspace`, and read access is revoked before any
  repo code runs on every path. `TrialPorts` has no push and no read-back (compile-fail
  doctests). The sandbox half of `merge-trial-types.test.ts` is left to the M3 runner.
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
  Not reproduced (20 October). Ruled out: an acknowledgement before the state write or a stale
  status (the state is written before the reply, and `Status` runs on the same daemon loop); the
  5 s `STATUS_TIMEOUT` (it fails with "daemon does not answer", not an uncovered refusal);
  reconcile forgetting the claim (that needs the claim ended in the log and not fresh); a
  housekeeping lapse (only while the link is down, and a fresh claim expires 30 s out); a grant
  racing the first log read (it held the claim twice, CLI-DUPLICATE-ADOPTED-CLAIM, which adds
  coverage and cannot refuse). Unproven: a reconnect clearing `fresh`. Blocked on a reproduction.
- [x] **CLI-DUPLICATE-ADOPTED-CLAIM** (`813e231`, #64) — lane B. Found by
  CLI-GITHOOK-FIRST-COMMIT: a claim granted while the daemon's first log read is in flight was
  adopted from the log, then pushed again by `on_granted`, so it was held twice. Agents saw a
  second claim instead of an amend, `tessel submit` failing with "several claims (1, 1, 2)" and a
  release sent twice. `on_granted` now replaces an entry with the same claim id.
- [x] **CLI-RECONCILE-INFLIGHT-NOTICE** (`fd91df2`, #67) — lane A. A claim the first log read
  finds granted while its request still waits for the reply is kept on that request and left for
  the reply, not reported as unexplained with `tessel release` offered. If the reply is a grant of
  another id, a denial or an error, the matched claim was an older one of this agent's with the
  same scopes, and it is adopted with the usual notice.
- [x] **CLI-RELEASE-BEFORE-GRANT** (`fd91df2`, #67) — lane A. A claim released while its request
  waits is not held when the grant arrives. For an amend, whose grant carries a newer fence than
  the release did, the daemon sends the release again with the grant's fence; before, the agent was
  told the claim was released while the coordinator still held it.
- [ ] **SWARM-AGENT-CHECKS-UNDER-LOAD** — lane B; part 1 merged (`345b722`, #65). From
  P1-FLAKE-UNDER-LOAD: in `tessel-swarm/tests/reconnect.rs` an agent released its claim without
  submitting, under heavy load. The same log fits the agent's own checks failing, any other
  `commit_and_push` error, a denied amend and a lapsed lease. #65 makes the checks note carry the
  first failing test and the reporter's pass/fail counts (quoted, at most 1,500 bytes), and the
  shadow test print the agents' results. Ruled out: a timeout on the checks, fixture timing, a
  shared directory or port. Not ruled out: memory or process pressure from many `node --test` runs,
  and a second writer in the checkout (two cuts within one heartbeat). Done: the cause named from
  the next failure's note, and a test for it.
- [x] **SWARM-SHORT-LEASE-TESTS** (`adcc7b6`, #77) — lane B for the swarm half (`e9df98c`, #71),
  lane A for the `cli.rs` half (CLI-LEASE-TESTS, #77, with CLI-FAKE-SEAM-GAPS). Two more tests
  failed once under load and passed on rerun, both after P1-FLAKE-UNDER-LOAD:
  `tessel-swarm/tests/on_local.rs` `a_claim_granted_after_a_queue_wait_of_many_leases_is_kept_alive`
  gave `[Lapsed, Merged]` at a load average near 90 (a 400 ms lease with an 80 ms heartbeat: a 400
  ms stall lapses it), and two `tessel-cli/tests/cli.rs` lease tests at a load average near 120 (not
  yet named). Done: each bound removed or made explicit as in P1-FLAKE-UNDER-LOAD, never raised to
  pass, shown on CI. #71: `tessel-swarm/src/local.rs` has one `Clock` the test freezes and steps;
  the named test and the works and checks tests step it after observed heartbeats and assert the
  coordinator's timestamps with `==` (a no-op freeze fails all four); the two cut-waiter tests hold
  the holder's push with a `pre-receive` hook until the cut has returned; `verification_outcome`
  reads the same clock. Left on real time, filed as SWARM-REAL-LEASE-CUT-TESTS:
  `the_tasks_a_cut_off_agent_never_took` (3 s lease; the waiter's claim must lapse) and
  `an_agent_cut_off_while_it_works` (600 ms lease, 1.2 s cut margin). #77: the two `cli.rs` tests
  were never named, so the whole file was audited; every CLI world ran the fake's clock on real
  time with 30 s leases, now frozen (only `advance_clock` moves it); sleeps replaced by waits on
  the cause; two real-time bounds stay with their reason in the test; no bound raised. The first
  push was red on Linux: the fake closed a cut replay socket with the reader's `Hello` unread, the
  OS reset the connection and events already written were lost; the fake now drains and completes
  the close handshake (the reviewer reproduced the symptom on a Mac with the drain removed).
- [ ] **SWARM-REAL-LEASE-CUT-TESTS** — lane B, with the next swarm task. From the
  SWARM-SHORT-LEASE-TESTS review: the two cut tests above keep real leases because the cut
  waiter's claim has to lapse for the run to end; the hook and node wait loops count iterations,
  not time, so at a load near 50 the hook gave up after 256 s instead of 150 s. Done: both tests
  end on the cause under the frozen clock or state their real-time bound, and the loops wait by
  the clock.
- [ ] **HOST-MERGE-STANDIN** — M5, with P1-CORE-STAGED-LANDING; its adapter half is done
  (P1-SERVER-LANDING, `9b2533f`: `Prepare` is the whole merge, with tests); the merge record
  reaching the lander is open. From
  the P1-MOVE-WAKE review: until the staged landing, a merge is one `Prepare` call that stands for
  the steward's whole `/merge`, and the host reads `Prepared { base, head }` as merged
  (`src/host/calls.rs`). An adapter that gave `Prepare` its frozen meaning (rebase, no landing)
  would make the core report merges that never landed (rule 6). The frozen `Prepare` also has no
  field for the merge record `runtime.rs` sends today (`merge::request_body`: the claim's intent,
  decision record and review), so the steward's evidence record would lack them. Done: the
  staged landing removes the stand-in, or M3's adapter maps `Prepare` to the whole merge with a
  test;
  and the record reaches the lander, by a route Felix approves if it changes `Call`.
- [x] **HOST-CALL-DEADLINE-BACKSTOP** (`9b2533f`, #84) — the adapter bounds each call by its
  `deadline_ms` and answers `Unknown` past it; `src/host/calls.rs` carries the written reason (a
  crash takes the call with the host; R24 recovers on restart). From the
  P1-MOVE-WAKE review: while a call is out, each `Wake` re-arms the watchdog, so a merge stalls
  if the adapter never answers or sends `CallLost`. The host knows each call's `deadline_ms`.
  Done: a host test that a call past its deadline with no answer is treated as lost, or a
  written reason that the adapter's deadline is enough.
- [ ] **M4-SANDBOX-WORKSPACE** — M5 (named before decision 15), with the local test runner's
  sandbox. From the
  P1-LANDER-AFTER-TESTS review: the `TestTree` split stops the port API and accidental writes, not
  hostile repo code run as the same OS user. The tree names the workspace path (`remote.origin.url`,
  `.git/logs/HEAD`), so such code can write the workspace's `.git/config` directly. Done: the
  sandbox denies the package step the workspace directory and kills its leftover processes
  before `land`, with a test of each; `prepare` and `land` being public is weighed there too.
- [x] **SERVER-CONFLICT-NAMES** (`9b2533f`, #84) — capped at 50, quoted, the real total carried
  and read by the agent's reason. From the
  P1-LANDER-ISOLATION review: `Conflict.files` (merge and trial) carries fork-chosen file names
  raw, with no cap or total. The steward does not cap them either (`merge-steps.ts` 144-146), and
  the core reads only their count (`src/merge.rs` 264-267), so nothing reaches an agent today.
  Done: before any agent or log sees them, the names are capped at 50 with a total and quoted
  (rule 3), with a test using control characters.

Carried-over tasks with slots in the plan: COORD-HEAD-FROM-AGENT and COORD-UNDELIVERED-GRANT
(see "Carried-over tasks" in the plan). COORD-WAKE-WHILE-VERIFYING was decided by P1-MOVE-WAKE.

### M3. The local server (done 21 October)

Done when (decision 15): no rules-list row is "after move"; an end-to-end landing on CI (a local
`tessel-server`, a bare git remote, two CLI agents, one commit landed by compare-and-swap after
the runner's command ran on it); `tessel review` approves through the server; a kill -9 loop with
claims reconciled.

- [x] **P1-DROP-WORKER** (`38045c2`, #73) — lane B, first (decision 13). Deleted `src/runtime.rs`,
  `src/store.rs`, `src/identity.rs`, `wrangler.toml`, `.cargo/mutants.toml`, the `runtime` feature
  and the dependencies only it pulled (`worker`, `console_error_panic_hook`, `hmac`, `sha2`,
  `base64`), the `cdylib` type, the wasm target and job and the no-default-features step, the
  `#[cfg(feature = "runtime")]` items of `src/shell.rs` and the dead `repo_allowed` and
  `needs_sessions` with them, the swarm's live target (`Remote` is a plain struct); 47 tests gone
  (1719 to 1672). Rewrote `README.md`, `docs/how-tessel-works.md`, `docs/reference/coordinator.md`,
  `docs/configuration.md` and three `CLAUDE.md` lines; R1's row became an after-move adapter row.
  Ruleset step 2 done by Felix 21 October; the build protocol's Rust gate row lost its wasm clause.
  Stale Worker comments outside the lane's files are CORE-WORKER-COMMENT-SWEEP.
- [x] **P1-SERVER** (`ade2d60`, #74) — lane A. `tessel-server serve`: axum 0.8, one task per repo
  owning its host with every input through one channel, rusqlite WAL with `synchronous=FULL` on
  every connection, grants sent only after the batch's transaction commits (pinned by a test that
  holds the store locked from a second connection), an advisory `flock` with the pid (a dead
  holder's lock is taken over, a live one names itself), `state_version` 1 with a fixture and a
  test that a fresh store's schema equals it, `(tenant, repo)` ids with a collision property test,
  routes `/tenant/<tenant>/repo/<repo>/{ws,summary,trunk-moved}` reusing `parse_route`, the
  protocol range in an optional `Welcome` field with the CLI's one warning. Two ports:
  `Verifier` (none wired: 401 everywhere, logged once) and `CallPort` (`NoLander` refuses every
  call). R2-R5 and R49 are "now" with their adapter tests; 1672 to 1730 tests. Review mutants:
  eleven, each red under its named test. Not verified: the CLI against a real server, the kill -9
  loop, an old-release CLI binary, Linux beyond CI.
- [x] **SERVER-NOLANDER-MESSAGE** (`9b2533f`, #84) — lane B, design A. From the P1-SERVER review:
  `NoLander` answers `Refused { Rule { message } }`, and the core collapses every `Rule` refusal
  into `MergeOutcome::Refused` with the fixed text "the steward refused the request (fork missing
  or not a fork of this repo)", so the agent and the `SubmitRejected` event never see the real
  reason (rule 6). Done: `Refusal::Rule` carries a fixed kind (`NoLander`, `ProtectedRef`,
  `RequiredCheck`, `ForkMissing`, `Other`); the agent sees one fixed reason per kind (rule 3, the
  fixed-form test extended); the host's message goes bounded and quoted to the server log only;
  `NoLander`'s reason names the missing lander, with an adapter test.
- [x] **SERVER-RESTART-TEST-FORK-RACE** (`d4d3728`, #86) — lane B. The store lock is a POSIX
  record lock (`fcntl`, per process, not inherited by a fork child) with a process-wide registry
  so a second open in one process is still refused, the file opened once and closed before leaving
  the registry; a reproducing test with a `pre_exec` sleep, red at `9b2533f`; all-threads stress 5
  of 5 against 4 of 5 before. From the orchestrator's gate on `9b2533f`:
  `tessel-server/tests/adapter.rs` `a_restart_over_the_same_directory_keeps_the_claims` and then
  `a_second_server_on_the_same_directory_is_refused_at_start` each failed once, reopening the
  directory right after a stop, with the store `Locked` by the test's own pid (five reruns green
  alone). Since #84 the same binary spawns git and the runner, and a forked child holds the lock
  file's open description until it execs, so a restart reopening the directory in that window is
  refused; `flock` is per description, not per process. A test artefact, not a server bug (a real
  restart follows a process exit). Done: the restart tests run in a binary that spawns no
  children, or the spawn and the reopen are serialised, with the reason written where the lock is
  taken; preferred: a per-process `fcntl` record lock that a fork child does not inherit, with a
  reproducing test.
- [ ] **CLI-WORKTREE-LOCK-FORK-RACE** — with the next CLI task. From the #86 review: the CLI
  daemon's worktree lock (`tessel-cli/src/daemon.rs`, `flock`) has the same fork-window
  exposure, and the daemon spawns git and hooks. Done: the same record lock, or a written reason
  the daemon cannot race itself, with a test.
- [ ] **SERVER-FRAME-LIMIT-TESTS** — M5 hardening, with the next server task. From the P1-SERVER
  review: a frame over 64 KiB is refused by the host with a fixed `Malformed` error before parsing
  (`src/shell.rs`) and the socket stays open; one over 256 KiB fails inside axum and the socket
  drops with no close frame. Neither is tested end to end. Done: an adapter test for each, and a
  decision whether the axum limit should answer with a close frame.
- [x] **CORE-WORKER-COMMENT-SWEEP** (`01e3ce3`, #82) — lane A. Comments and docs that still
  described the Worker or the Durable Object after P1-DROP-WORKER now describe the host and the
  server (`src/protocol.rs`, `src/coordinator.rs`, `src/merge.rs`, `src/trunk.rs`, `src/host/*`,
  `src/shell.rs`, the swarm's local target, the CLI daemon, `docs/reference/coordinator.md`, the
  README, the skills); one error string changed ("watch is served by the host and never reaches
  the core") with its assertion; no other behaviour change, 1837 tests one for one. Kept on
  measurement: the release profile's `opt-level = "s"` and `lto` (69 s and 4.36 MB against 79 s
  and 7.15 MB without). Left: `src/shell.rs` test names `the_worker_serves_*` (history), the
  swarm's `tests/reconnect.rs` and README (the next swarm task), the CLI strings and the skill
  lines that call the steward the merger (P1-LOCAL-SETUP's second half); `keep_first`,
  `agents_to_withdraw`, `SummaryTally` and the log-line helpers still have no production caller
  (pinned by R13, R37, R40; the server should use them).
- [x] **P1-IDENTITY** (`c2f00d2`, #76) — lane B. Ed25519 tokens `<payload>.<signature>` with
  `v`, `kid`, `aud`, `tenant`, `repo`, `agent`, `role` and `exp_ms`, the signature over the payload
  text as sent and checked with `verify_strict` (pinned by an identity-point key test; weak keys
  are refused at load); refusals in a fixed order with fixed text, a token over 1 KiB refused
  before decoding. `[identity]` config (keys directory, audience; every `.pub` present verifies,
  so rotation overlaps; no deny-list yet, said in the docs); `tessel-server admin keygen` (0600,
  no overwrite) and `admin token` (stdout only). Reserved service names `lander`, `runner`,
  `server`, matched case-insensitively: `ws` and `summary` admit agents only, `trunk-moved`
  admits `lander` only; the `Verifier` trait states that contract. R1 "now" with the ten deleted
  `routes` cases ported one for one in `tessel-server/tests/routes.rs`. 1730 to 1787 tests; ten
  review mutants each red. Not verified: the real CLI against a server with `[identity]` (owed
  by P1-LOCAL-SETUP); the flip-any-byte proptest runs 24 of 256 cases. Key-file modes are Unix
  APIs, as the CLI and lander already use.
- [x] **P1-RUNNER** (`39feb6b`, #79) — lane B. `tessel-runner`, its own crate and binary: refuses
  to start unless `tessel-runner.toml` (a regular file opened without following links, owned by
  the effective uid, mode 0600) names the effective user, resolved through `getpwuid_r`; takes one
  JSON request on stdin (an absolute tree the user owns, the trunk's `[[test]]` commands re-checked
  through the lander's `tessel.toml` parser, a timeout capped at a day) and answers one JSON
  result convertible to the lander's `StepOutcome`: exit code, pass, timed out (only for the
  runner's own kill), 256 KiB tails cut on a character boundary, the first failing test's block
  from real cargo output bounded to 1,500 bytes. One deadline across the commands; at the timeout
  the process group and then the leader are killed; a `setsid` grandchild can outlive it (the
  no-sandbox limit, stated in the docs). Exit 0 ran, 2 refused, 3 not supervised. No git, no
  network, no new dependencies. 1789 to 1843 tests; sixteen review mutants each red. Not verified:
  root or another uid. The sandbox is RUNNER-SANDBOX (M5, with M4-SANDBOX-WORKSPACE).
- [ ] **RUNNER-SANDBOX** — M5, with M4-SANDBOX-WORKSPACE. From the P1-RUNNER review: a command
  that calls `setsid` escapes the runner's process-group kill and the output drain gives up on it
  after 500 ms; the owner check of the config is reachable only as root. Done: the sandbox the
  M4-SANDBOX-WORKSPACE row describes also bounds the runner's children, with a test.
- [x] **P1-SERVER-LANDING** (`9b2533f`, #84) — lane B. The adapter half of HOST-MERGE-STANDIN,
  with HOST-CALL-DEADLINE-BACKSTOP, SERVER-CONFLICT-NAMES and SERVER-NOLANDER-MESSAGE. Built:
  `[[repository]] remote` and `fork` (a path or URL with `{agent}` once; the name re-checked with
  the protocol's agent-name rule before substitution, else refused as a missing fork), `[lander]
  work_dir` and `runner_config`; `Prepare` runs the whole merge through the lander with the
  runner's library as the package step against a path remote, main read back with the lander's
  fixed git environment, `Prepared` only when the head is on main; `Test` and `Land` refused;
  `Trial`, `ReadHead` and `Reconcile` through the lander and git on a blocking thread; each call
  bounded by its deadline (the written reason in `src/host/calls.rs`: a crash takes the call with
  the host and R24 recovers on restart, which needed a wake at task start for a repository with
  stored state); `Conflict.files` capped at 50, quoted, the real total carried (`total`, additive)
  and read by the agent's reason; a `Prepared` whose head is its base read as already merged so a
  retry after a lost answer keeps the dispatch main as its baseline; the landing feeds the
  trunk-moved poke; one adapter test per outcome over a bare remote, a SIGKILL mid-merge with a
  restart that lands the claim once, and the end-to-end landing job on CI (75 s). 1837 to 1887
  tests; the review's mutants each red, the one CI missed pinned by a boundary test. Left for rows
  below: case-folded fork names, the unused `trunk-moved` route, a kill after the push.
- [ ] **SERVER-FORK-CASE** — M4, with the GitHub fork adapter. From the P1-SERVER-LANDING
  review: on a case-insensitive filesystem two agent names differing only by case share one
  fork (on this Mac a call as `A1` landed through `a1`'s fork), and GitHub logins fold case the
  same way, so URL forks collide too. `docs/configuration.md` says to mint tokens for lowercase
  names. Done: a name whose case-folded form equals another live agent's is refused at the
  verifier or the fork template, with a test on both a path and a URL fork.
- [ ] **SERVER-TRUNK-MOVED-ROUTE** — M6, with team mode. From the same review: nothing in the
  tree calls `trunk-moved` since the adapter feeds the host its own poke after a landing; the
  route stays for main moves the adapter did not make. Done: wired to the git host's push
  event in team mode, or deleted with its `lander` service name.
- [ ] **CI-ACTIONLINT-ZIZMOR** — M4, before the orchestrator's gate on the merged tree stops (the
  protocol rewrite, #90). From the M4-DOGFOOD-SETUP review: `.github/workflows/ci.yml` runs fmt,
  clippy and the named-tests check but neither `actionlint` nor `zizmor`, which only the lanes'
  gates run; once landings bypass the orchestrator's gate nothing on `main` checks the workflows.
  Done: both in the fast lane, pinned by SHA, red on a planted workflow fault.
- [ ] **MERGE-ADMIN-MERGE-TEXT** — with the next core task. From the same review:
  `src/merge.rs:327` still says a change to `tessel.toml` needs "an admin merge", a steward route
  that no longer exists; the lander refuses the change and it goes through a pull request. Done:
  the text says that, with its test.
- [ ] **SERVER-KILL-AFTER-PUSH-TEST** — M5 hardening, with the next server task. From the same
  review: `restart.rs` kills the server during the runner's command; a kill between the push
  and the answer is untested (it should end as already merged, counted once). Done: that test.
- [x] **P1-LOCAL-SETUP** (`725834c`, #87; first half `af595c5`, #80) — lane A. Second half:
  `docs/local-setup.md` (86 lines) walks to a commit landed on the developer's remote, the CI job
  "Slow lane, quickstart" runs the page's own fenced blocks from a clean checkout (5 s after the
  build, a drift in the page fails it), the CLI's text says the server's lander lands after the
  runner ran the gate and names no fork, the transcript is prepared for the evidence record and
  nothing copies it yet, and the skills, README, configuration, how-tessel-works and coordinator
  docs say the same. First half merged (`af595c5`, #80): `tessel init` against a local server
  (`--steward`, the `curl` transport and `STEWARD_ADMIN_TOKEN` deleted; the token comes from the
  file or `TESSEL_TOKEN`, and the no-token error names the `tessel-server admin token` command),
  `[[repository]]` `reviewers` and `sensitive` validated at load and passed into `HostConfig` (a
  configured reviewer approves a held submission through the server; a name not configured is
  refused), the real CLI against an in-process server with `[identity]`
  (`tessel-cli/tests/local_server.rs`; the token in no file but the 0600 config), the skills,
  README and configuration docs rewritten, and `docs/local-setup.md` (59 lines, the server's keys
  outside any checkout) walked by the lane and the reviewer; the page says what this build does
  after a reviewer approves: the merge is refused and the claim reopens. 1789 to 1783 tests (15
  steward minting tests deleted, 9 added). Second half, after P1-SERVER-LANDING: the quickstart
  followed on CI from a clean checkout to a landed commit, the steward-as-merger wording in the
  CLI's strings (`inbox_hook.rs`, `mcp.rs`, `moves.rs`, `transcript.rs`) and the skills'
  merge-path lines rewritten, and `skills/tessel/SKILL.md`'s "before the merge's evidence step
  finishes" sentence (no such step exists) corrected.


### M4. Dogfood on the local server

Plan (`docs/proposals/phase-1.md`, M4): this repository coordinated and landed by a
`tessel-server` Felix starts on his machine; lanes claim before editing; the runner runs the
workspace gate on the rebased commit; the lander lands `main` on GitHub by compare-and-swap push
over HTTPS with Felix's token as the ruleset bypass actor (step 4); `docs/BUILD-PROTOCOL.md`
section 2 rewritten, with Felix's approval of the landing shape before the first landing. Done
when: five landings of this repository with their notes and claims released by `Merged`, none by
hand; one server restart while a submitted claim is held, that claim landed afterwards.

**Decisions for Felix (block the first landing, not the first two tasks):**
- D1, the landing shape: the lander pushes `main` over HTTPS with a token read from a 0600 file
  named in `[lander]` (never an environment variable, never in a URL or a log); the bypass actor
  is Felix (ruleset step 4). Approve or change.
- D2, the fork shape for lanes: (a) a real GitHub fork per lane name (`fork` a URL with
  `{agent}`; lanes are few and fixed), or (b) branches in this repository with the template
  gaining a ref, which needs a lander change since it reads only a fork's `main`. Recommended:
  (a), no lander change, forks named for `lane-a` and `lane-b`.
- D3, the runner's load on the Mac: each landing runs `cargo test --workspace` cold on Felix's
  machine (237 s measured at load 9 on 22 October; the merge call's deadline is 13 minutes and
  the runner's `timeout_secs` is 720). Accept, or ask for a way to limit the load: the gate's
  parser accepts none of `-j`, `--` or an environment setting, so limiting it is a runner or
  gate change, not a `tessel.toml` line.
- D4, identities: tokens per lane name from the server's keys; the orchestrator holds the
  reviewer token (`reviewers = ["orchestrator"]`); `sensitive` covers `src/protocol.rs`,
  `src/host/`, `.github/` and `docs/BUILD-PROTOCOL.md`.

- [ ] **M4-GITHUB-REMOTE** (`6fcf36b`, #92) — lane B. Merged; its done-check's sandbox landing
  is pending on a token from Felix (STATE). As built: `[lander] token_file` (0600, checked at
  start and before every call, no line break inside); the server binary as git's askpass hook,
  answering only a prompt whose host equals the configured `remote` or `fork` host; `{agent}`
  refused in a URL fork's host; the head read at server start for a repository with no head.
  As planned: `with_push_access` for an HTTPS remote: the credential from
  the `[lander]` token file through git's askpass or credential-helper environment, never in
  argv, a URL or a log (test: a fake askpass records what git asked; argv and the log scanned);
  the read-back by `ls-remote` with the same credential; `fork` as a URL; the trunk head read
  from the git host at load (COORD-HEAD-FROM-AGENT). Done: one landing against the P0-SPIKE
  sandbox organisation, recorded in the report, plus the tests.
- [x] **M4-DOGFOOD-SETUP** (`bf3e6bc`, #91) — lane A. The page (120 lines), the script, the
  trunk's `tessel.toml` without the steward's pnpm step; a cold `cargo test --workspace` measured
  at 237 s (D3). Walked by the reviewer and, on `main`'s own config, by the orchestrator. As
  planned: `docs/dogfood.md` and a script Felix runs: the server
  directory outside the checkout, keys, tokens for the two lanes and the orchestrator, the
  repository entry for this repository with `remote`, `fork`, `reviewers` and `sensitive`,
  `tessel-runner.toml`, and the trunk's `tessel.toml` with the gate's commands. Done: Felix
  starts the server from the page; a restart by hand keeps a held submission.
- [x] **M4-PROTOCOL-REWRITE** (`de2ec7f`, #90) — the orchestrator; Felix merged the text. In
  force from the first landing. As planned: `docs/BUILD-PROTOCOL.md` section 2
  and `CLAUDE.md`'s dogfooding section: lanes `tessel start` with the brief's intent and claim
  before editing; `tessel submit --evidence` replaces push and pull request; the orchestrator
  reviews held submissions with `tessel review`; the adversarial reviewer reads the submitted
  commit on the fork; the server's landing replaces Felix's merge; the runner's run replaces the
  orchestrator's gate. Done: Felix approves the text before the first landing.
- [ ] **M4-FIRST-LANDINGS** — both lanes, after step 4, D1 and the protocol rewrite. The five
  landings of the done-check, each counted here with its note. SERVER-FORK-CASE lands before the
  second lane name is minted.
- [ ] **M4-RESTART-HELD** — lane B. The done-check's restart with a submitted claim held, on the
  real server, that claim landed afterwards.
- [ ] **M5-PROOF-CANDIDATE** — M5, to weigh at its re-plan. GitHub Next and Microsoft Research's
  Proof Driven Development Kit (21 October) proves Rust against a Verus spec and falls back to
  tests. The core's invariants that proptests pin (one `Merged` per claim, fences persisted before
  grants, a held submission never dispatched) are candidates for a proof. Done: a written answer
  whether Verus fits the core, with one invariant tried.

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

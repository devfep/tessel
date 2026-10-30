# Roadmap

Task list for the work in `PLAN.md`, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope
and order; `docs/proposals/real-use.md` holds the requirements, invariants and done-checks this
file's tasks refer to. Only the orchestrator edits this file. The contest-era roadmap and build log
are in git history (`78bcb9a` and earlier).

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-30 19:18 UTC. Main at `ca864b2`, where RESOLVE-FOLLOWUPS landed through the
server at 19:08 UTC as `68ea3e6`..`ca864b2` (four commits on `e23690e`; claim 134, held as merge
path and approved by the orchestrator on a re-vote). CI (41112834681) and the Adapter suite
(41112834446) passed on `ca864b2`; `tessel-server/tests/restart.rs` now runs the `sqlite3` CLI, and
CI's runner has it (CI 41112834681 ran
`restart::a_server_killed_after_the_push_and_restarted_counts_the_landing_once`, which passed). The
submission held a merge of main (`3e73289`), so the daemon copied no notes; the orchestrator copied
all four by subject and author (the fourth landing to hit that gap). RACE-UNKNOWN-REASON's six
commits were linear, and their notes were in place after the landing. Its first vote on `3e73289`
(`~/tessel-orch-notes/2026-10-30/vote-resolve-followups/`): VOTE-1 "No" (after the move, step 1 ran
a bare `sqlite3` under `sudo -u _tessel`, which a lane-writable `PATH` entry such as
`/opt/homebrew/bin` could hijack as `_tessel`, the account that owns the keys, the GitHub token and
the stores; fix: `/usr/bin/sqlite3` in full, before and after the move), VOTE-2 "No" (the runbook's
"`--landed` copies no notes" was false: the submitter's daemon queues the copy from the logged
`Merged` on reconnect), VOTE-3 "Yes with should-fixes". Fix pass 1 (`67f8094`, then `ca864b2` for
the upgrade read) closed both and most should-fixes; the re-vote (`vote-resolve-followups-2/`) went
3-0 "Yes with should-fixes", no must-fix; its should-fixes are RESOLVE-RUNBOOK-NITS (below). At
18:49 UTC RACE-UNKNOWN-REASON landed as `513adf3`..`e23690e` (six commits on `d8199ca`; claim 132,
held as merge path, approved on a 3-0 vote, `vote-race-unknown/`, "Yes with should-fixes"; VOTE-2
and VOTE-3 allowed folding the wording, a `NOT_PICKED` pin and a control run in before landing, done
in `2d0ee67`..`e23690e` and checked by the lane's gate and CI, not re-voted); CI (41110192280) and
the Adapter suite (41110193072) passed on `e23690e`. An entry of a `TestsPass` race that ends with
no winner and that no trial judged (`tests_passed: None`) is now rejected with "the tests gave no
result, so no trial judged this entry" (`TESTS_GAVE_NO_RESULT`; no wire change); a failed entry
still hears "lost the race", and an unpicked `HumanPick` race still says `NOT_PICKED`. The STATE
commits `37658de`..`d8199ca` landed at 13:49 UTC (claim 133). Before, at 13:04 UTC, P1-SIM-MERGE
landed through the server as `5d68ed7`..`7423616` (eight commits on `3b21d4d`; claim 130, held for a
signature change in `src/sim`, not as merge path, and approved by the orchestrator on Opus
REVIEW-3's "Yes"). Its daemon stopped with the note copy still pending (its log, 13:04 UTC: the
commits were not fetched yet), and the submission's merge of main (`db16825`) means it would have
refused anyway, so the orchestrator copied the notes by subject and author; all eight match the
submitted ones. CI (41060256861), which sweeps 1,000 simulator seeds, and the Adapter suite
(41060256883) passed on `7423616`. Its history: REVIEW-1 "No", fix pass 1, REVIEW-2 "With changes",
fix pass 2 (`db16825` merge of `9188032`, `724e73f`, `7eb1ee9`; gate green, 2375 tests; by the
lane's report the agent-base check passes on seeds 64, 196, 229, 0..300 and 1,000 seeds from first
seed 442872731007), REVIEW-3 "Yes", no must-fix
(`~/tessel-orch-notes/2026-10-28/lane-a/sim-merge/REVIEW-3.md`). REVIEW-3's should-fix: the lane's
reason for FIX-2 item 4 was wrong (the simulated lander does refuse `ReadHead`; the probe saw 46 of
280 reads refused or never run). The checkers accept a race judged with the tests unknown only
because none reads a race's verdict, and no seed of 0..100 (REVIEW-3's probe) reached four failed
reads with a race waiting, so nothing in the simulator drives that path: a scripted run is in
RACE-UNKNOWN-REASON. At 12:43 UTC the STATE commits `5e2e7ec`..`3b21d4d` landed (claim 131). Before,
at 11:28 UTC, CORE-HEAD-ADOPTED landed through the server as `b4e1348`..`9188032` (twelve commits on
`77d5db5`; claim 125, held as merge path and approved by the orchestrator on a re-vote). The
submission holds a merge of main (`109da3d`), and the daemon's note copy refuses such a submission
(`tessel-cli/src/notes.rs:229-231`; its log, 11:31 UTC), so the orchestrator copied all twelve notes
by subject and author; this is the second landing to hit that gap, P1-SIM-MERGE's was the third (by
the merge rule; its daemon stopped first), and RESOLVE-FOLLOWUPS' the fourth. CI (41048353740) and
the Adapter suite (41048353696) passed on `9188032`. Its first vote on `4fdd79b`
(`~/tessel-orch-notes/2026-10-30/vote-core-head-adopted/`): VOTE-1 "No (must-fix)" (a head adopted
from a `Hello` with no read owed stranded a closing race in `Judging` for good: a port that never
reads heads with an empty store, or a `Landing` port whose fresh store's first commit failed; a
regression from `df4b39a`), VOTE-3 "Yes with should-fixes", VOTE-2 never finished (no VOTE-2.md).
The orchestrator rejected the hold; fix pass 3 (`109da3d` merge of main, `7ab34ae` red, `389a54d`;
gate green, 2330) owes a read whenever a closed race waits on an adopted head, counts a refused read
as a failure, and judges the race with the tests unknown after four failed reads (about 130 s). A
re-vote of three fresh voters on `389a54d` (`vote-core-head-adopted-2/`) went 3-0 "Yes with
should-fixes", no must-fix; the should-fixes are HEAD-ADOPTED-FOLLOWUPS (below). At 07:32 UTC the
STATE commits `663b304`..`77d5db5` landed (claim 129). Before, at 06:50 UTC, RESOLVE-NAMES-HEAD
landed through the server as `97ca04c`..`3b708cd` (eight commits on `d5bf531`, fast-forward, so
every commit kept its sha and note; claim 128, held as merge path and approved by the orchestrator
on a vote). CI (41016436513), the Adapter suite (41016436480) and Own users (41016436479) passed on
`3b708cd`. Its vote, under Felix's standing order of 30 October (below): three Opus devil's
advocates (safety of the one path to `Merged` outside the lander; test honesty; operations and the
runbook) each voted "Yes with should-fixes", with no must-fix
(`~/tessel-orch-notes/2026-10-30/vote-resolve-names-head/`); their should-fixes are
RESOLVE-FOLLOWUPS (below). The voters were told it would land after claim 125, rebased; it landed
first, alone on `d5bf531`, after claim 125's hold was rejected. The vote still covers the landed
tree: VOTE-2 ran its baselines on `3b708cd` alone, VOTE-1 and VOTE-3 found no conflict in either
order, and CI is green on it. Before the vote: Opus REVIEW-1 "With changes", fix pass 1
(`6394810`..`a81e0da`), REVIEW-1's re-check "With changes" (the commands after the move), and fix
pass 2 (`3b708cd`), which only the voters read. Lane B ran it ahead of CORE-HEAD-ADOPTED's landing:
the row's "after CORE-HEAD-ADOPTED lands or leaves the core files" was about two lanes in one file.
It edited `merging/doubt.rs` and `merging/stages.rs` in lane A's core (the cross edit its row names)
while claim 125 held other core files; the coordinator granted every claim (no denial), which is
what the claims enforcing it file by file rests on. Before, at 29 October 00:31 UTC,
P1-CORE-RECONCILE landed as `8f15d61`..`df4b39a` (eleven commits, base `93a2f6a`; claim 122, held as
merge path and approved by the orchestrator). Felix delegated the merge-path yes for it to a vote of
Opus voters on its final commit (as for `dab6a22`); three voters (honesty and safety, test strength,
operational risk) read `7897dc7` and each voted "Yes with should-fixes", with no must-fix
(`~/tessel-orch-notes/2026-10-28/vote-core-reconcile/`). The landed tree is `7897dc7`'s plus main's
docs-only commits since. CI (40794648923) and the Adapter suite (40794649049) passed on `df4b39a`.
The votes' should-fixes are RESOLVE-NAMES-HEAD (below), which the orchestrator gates the server's
upgrade on. The submission held a merge of main, so the daemon copied no notes; the orchestrator
copied all eleven by subject. Before this, main was `93a2f6a` (STATE). P1-CORE-STAGED-LANDING
(`05b6ec1`..`55528c9`, 28 October 18:23 UTC) was voted the same way (`vote-dab6a22/`). The daemon
copying no notes for a submission with a merge commit is a gap CLAUDE.md's dogfooding paragraph does
not mention.
Agent tokens are valid to 31 October (set when Felix issued them); Felix renews them. Lanes cannot
push (Felix's `~/.claude/hooks/block-lane-push.sh`), so the orchestrator pushes each lane commit to
a new branch of its fork; the reviewer reads it there; the orchestrator runs `tessel submit` in the
lane's worktree and decides holds as `orchestrator`. Two lanes ran at once four times by 25 October;
the coordinator denied lane A `src/merge.rs` and `docs/configuration.md` (twice) and lane B
`src/coordinator/verifying.rs` (three times), each while the other lane held it, and the denied lane
claimed the file after the holder landed or left it to a follow-up.
In flight (each item gives its brief's path; votes in `~/tessel-orch-notes/2026-10-30/`):
- P1-HOST-HARDENING (lane B, sonnet, dispatched about 19:11 UTC from `ca864b2`,
  `.claude/worktrees/host-hardening`; brief
  `~/tessel-orch-notes/2026-10-30/lane-b/host-hardening/BRIEF.md`): P1-PERSIST-RETRY (flips R30),
  exact replay without stalling (flips R36), COORD-UNDELIVERED-GRANT's host test. `src/host/` is
  sensitive, so it goes to a vote. A lane A row (phase-1.md's order) run by lane B, an orchestrator
  choice that frees lane A for P1-FAILPOINTS: a cross edit into lane A's `src/host/**`,
  `src/shell.rs`, `tests/runtime_rules.rs` and `docs/reference/runtime-rules.md`.
- P1-FAILPOINTS (lane A, sonnet, dispatched about 19:11 UTC from `ca864b2`,
  `.claude/worktrees/failpoints`; brief
  `~/tessel-orch-notes/2026-10-30/lane-a/failpoints/BRIEF.md`): a scripted crash in each landing
  window, in the simulator where it reaches and in tessel-server's tests where it does not; nothing
  in `src/host/**`, and a bug found in the core is reported, not fixed. It runs ahead of
  HEAD-ADOPTED-FOLLOWUPS, which shares `src/host/tests.rs` and `src/host/calls.rs` with
  P1-HOST-HARDENING (an orchestrator ordering, not the plan's).
- P1-CORE-RECONCILE's history, for the record: Opus REVIEW-1 "With changes" (a new top-level
  `ServerMsg::MergeDoubt` with no version bump, against invariant 13); Felix put that to a vote,
  three Opus voters all chose to keep it with changes, and the orchestrator merged their
  conditions (`vote-invariant-13/DECISION.md`; Felix can still overrule): invariant 13 rewritten
  on whether an older daemon can decode and safely skip a frame (`f1ea1f6`), checked by an
  exhaustive `skippable()`, and the doubt rebuilt from an optional `Welcome` field on every
  connection. Five fix passes followed; REVIEW-2 "With changes", no must-fix. Pass 4 added tests
  for the nine missed mutants Nightly 40764265692 found in P1-CORE-STAGED-LANDING's code (killed
  by hand, four of the eight `merge.rs` mutants applied, the rest by argument; cargo-mutants not
  run). The scheduled Nightly 40853018831 on `001606a` was the first cargo-mutants check of
  those tests: none of the nine is missed any more.
P1-HOST-HARDENING follows P1-CORE-RECONCILE; P1-ONE-BATCH follows CORE-HEAD-ADOPTED;
P1-ADAPTER-SUITE's rest follows P1-HOST-HARDENING; P1-FAILPOINTS follows P1-SIM-MERGE. The dogfood
server runs `455c889` from `~/tessel-dogfood/bin` (restarted 27 October 22:25 UTC). Its
upgrade's "When" (`docs/dogfood.md`) is met: P1-SERVER-STAGED, P1-CORE-STAGED-LANDING,
P1-CORE-RECONCILE and RESOLVE-NAMES-HEAD (which `docs/dogfood.md` names since `3b708cd`) have
landed. The upgrade waits on the trial landing, which waits on Felix's token (below), and on the
page's "nothing in flight" check. Before an upgrade that includes CORE-HEAD-ADOPTED, the
orchestrator confirms no served repository lacks a `remote` (its VOTE-1).
`.claude/worktrees/orchestrator` holds this
commit, `gate-main` (at `455c889`) is the gate worktree.
**Standing order from Felix (24 October, morning):** each recommendation goes to adversarial Opus
reviewers first; on their buy-in the orchestrator proceeds without asking.
**Standing order from Felix (30 October):** "always vote via adversarial, devils advocate
subagents": every merge-path yes ("Felix reviews") is a vote of Opus devil's advocates (three, on
the final commit: the orchestrator's convention, not his words), standing in for his written yes
(BUILD-PROTOCOL section 6), and "you have my standing order to merge your PRs": the orchestrator
merges its own pull requests (`tessel.toml`, the server's set-up, or anything the server cannot
land) after CI and review. If the auto-mode classifier refuses such a merge, the orchestrator gives
Felix the command. Everything else lands through the server.
**Decided by Felix (24 October):** D1 as written, with the ruleset split below; D2 (a), private
repos `devfep/tessel-lane-a`, `-lane-b` and `-orchestrator` (Actions off), lanes pushing a new
branch per submission since #114; D3 accepted with landing builds capped at 6 jobs
(`<server dir>/.cargo/config.toml`; tests still use every core: load reached 55 on the first
landing); D4 accepted as a process control, not a security boundary, until M5 runs the server as
its own macOS user. The GitHub token is fine-grained ("tessel-dogfood-lander": the four repos,
Contents read/write, Metadata read; expires 23 November 2026). `delete_repo` was removed from gh's
token. `PUSH_ALLOW_REMOTES` in `~/.claude/overlay.env` exempts the forks and the sandbox repos
from Felix's push hook. Claude Code deny rules on the server directory were not added: lanes are
the orchestrator's subagents and share its permissions; the orchestrator runs `tessel init` in
each worktree before handing it over instead.
**Ruleset split (24 October):** "main: no rewrite" (25786281: deletion, non-fast-forward, no
bypass; `current_user_can_bypass` never) and "main: CI required" (25345043: pull request and the 7
required checks, the repository admin role as bypass; always). Proven first on
`tessel-sandbox/m4-trunk` (public): the trial server landed `8c878d3` through the bypass, and
Felix's own force-push was refused. A `pre-push` hook in the checkout's shared hooks directory
(`pre-push.pre-tessel` since `tessel init`) refuses pushing `main` of `devfep/tessel`.
**Waiting on Felix:** whether CLAUDE.md rule 1 should also name a never-logged live
notice an older daemon can skip and stay correct as additive (invariant 13 since `f1ea1f6`); the
plan's rule (d) (`docs/proposals/phase-1.md`), which says a build bumps
`state_version` "only if the old binary can still read the new state", a case the store's
`refuse_unknown` rules out (#125 follows the code); a GitHub token that can push
`tessel-sandbox/m4-trunk` for the upgrade's trial landing, which the dogfood token cannot; where
P1-DENIAL-BOUND goes in the order; narrowing `PUSH_ALLOW_REMOTES` to the sandbox trunk now that
lanes push branches; the suggested patch to his push hook that exempts only a URL git resolves to
itself (`~/tessel-dogfood/push-hook-resolved-url.patch`); the Cloudflare API token in
`tessel-steward/.dev.vars` revoked at M6's deletion at the latest; the head branches of merged pull
requests on `devfep/tessel` (the orchestrator deletes nothing on the remote without his word).
Note: commits on `devfep/tessel` carry the author address of Felix's global git config; the
sandbox commits use his GitHub noreply address.
**Blocked:** CLI-GITHOOK-FIRST-COMMIT, not reproduced (see its entry).
SWARM-AGENT-CHECKS-UNDER-LOAD waits for its next failure's note. P1-FAKE-HELLO-LOST: the test
cannot see the flake (see its entry).
**Next:** P1-FAILPOINTS, its review and landing (lane A); P1-HOST-HARDENING, its vote and landing
(lane B); then HEAD-ADOPTED-FOLLOWUPS (lane A) and RACE-LOSER-TEXT (lane B); then
RESOLVE-RUNBOOK-NITS (lane B); then P1-ONE-BATCH. Felix makes ruleset step 3 ("Fast lane, simulator"
required). Felix's Homebrew postgres trusts local connections on a world-writable socket; he closes
it before the accounts move. LANDER-ADMIN-POLICY-DEAD waits for M6.
**Decided by Felix (25 October), OWN-USERS-DESIGN:** the note as written
(`docs/proposals/own-users.md`).
**Decided by Felix (25 October), M5-7:** the CANDIDATE-REF shape as the note describes, with
`CandidateMissing` as a `Call` change (decision 12); the plan's lander fork and its account steps
are dropped.
**Decided by Felix (25 October), M5's re-plan:** M5-1 to M5-8 as recommended
(`docs/proposals/phase-1.md`, "Decided at M5's re-plan"), after an adversarial Opus review of
the re-plan; decision 7 and proposed change A's `resolve` are decided with them.
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
**Pending from Felix:** the plan's decisions 1-6 and 8-11, proposed change A's `release`, and
B-D (B now M6)
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
- [x] **COORD-HEAD-FROM-AGENT** (`36b17f2`, #106) — lane B. As built: the core records the head a
  `Hello` adopted (`head_from_hello`); a head read replaces it while `head` still equals it and logs
  `BaseMoved`; a head from a landing or an earlier read is kept; an answer that confirms the Hello
  head ends its replaceability; a failed read while the head is missing or Hello-adopted is offered
  again on `infra_backoff_ms` (10 s, 30 s, 90 s, at most `MAX_INFRA_RETRIES`), reset by a good read
  or a poke. Agents already welcomed keep the old base until their next `Welcome`; no protocol
  change. Three `#[serde(default)]` state fields. Two host tests in `src/host/tests.rs` that pinned
  the gap now pin the fix. Reviewed "Yes" after one fix pass (two tests for surviving mutants).
  Left: COORD-HEAD-RESTART-READ. Planned: In #92 (M4-GITHUB-REMOTE, under review): a repository with
  no head reads main's head from the remote at server start, before the first `Hello` can adopt an
  agent's declared base. Left open by that change: a `Hello` that arrives before the read still sets
  the head (the read's answer is then dropped), and a failed start read is not rescheduled. From the
  P0 review: until the first landing or head read, the coordinator's `head` is the base the first
  agent declared in its `Hello` (`src/coordinator.rs`), and it becomes the baseline of the first
  dispatch unchecked.
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
- [x] **SWARM-REAL-LEASE-CUT-TESTS** (`cad3703`, #110) — lane B. As built: both cut tests run under
  the frozen clock, the lapse produced by clock steps after the observed grant, and assert the lapse
  at its exact clock time, so a no-op freeze fails both; the hook (`date +%s`), node (`Date.now`)
  and Rust (`Instant`) wait loops wait by the clock with their 150 s bound; no bound raised.
  Reviewed "Yes" after one fix pass. Left: SWARM-UNBOUNDED-LOOPS. Planned: lane B, with the next
  swarm task. From the SWARM-SHORT-LEASE-TESTS review: the two cut tests above keep real leases
  because the cut waiter's claim has to lapse for the run to end; the hook and node wait loops count
  iterations, not time, so at a load near 50 the hook gave up after 256 s instead of 150 s. Done:
  both tests end on the cause under the frozen clock or state their real-time bound, and the loops
  wait by the clock.
- [ ] **HOST-MERGE-STANDIN** — the stand-in half in P1-CORE-STAGED-LANDING (M5), the record
  half with P1-EVIDENCE (M6; the route is decision M5-1); its adapter half is done
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
- [ ] **M4-SANDBOX-WORKSPACE** — M5, closed by RUNNER-OWN-UID (named before decision 15).
  From the
  P1-LANDER-AFTER-TESTS review: the `TestTree` split stops the port API and accidental writes, not
  hostile repo code run as the same OS user. The tree names the workspace path (`remote.origin.url`,
  `.git/logs/HEAD`), so such code can write the workspace's `.git/config` directly. Done: the
  sandbox denies the package step the workspace directory and kills its leftover processes
  before `land`, with a test of each; whether `prepare` and `land` stay public is CANDIDATE-REF's.
- [x] **SERVER-CONFLICT-NAMES** (`9b2533f`, #84) — capped at 50, quoted, the real total carried
  and read by the agent's reason. From the
  P1-LANDER-ISOLATION review: `Conflict.files` (merge and trial) carries fork-chosen file names
  raw, with no cap or total. The steward does not cap them either (`merge-steps.ts` 144-146), and
  the core reads only their count (`src/merge.rs` 264-267), so nothing reaches an agent today.
  Done: before any agent or log sees them, the names are capped at 50 with a total and quoted
  (rule 3), with a test using control characters.

Carried-over tasks with slots in the plan: COORD-UNDELIVERED-GRANT (P1-HOST-HARDENING) and
COORD-HEAD-FROM-AGENT's simulator check (P1-SIM-MERGE, on main since `7423616`; its `ReadHead`
half #106); see "Carried-over tasks" in the plan. COORD-WAKE-WHILE-VERIFYING was decided by
P1-MOVE-WAKE.

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
- [x] **CLI-WORKTREE-LOCK-FORK-RACE** (`3b19099`, #102) — lane A. As built: `acquire_lock` in
  `tessel-cli/src/daemon.rs` takes an `fcntl` `F_SETLK` record lock, as the server's store does
  (#86), refusing with `Ok(None)` as before; one process runs one daemon and nothing else opens
  `daemon.lock`, so the record lock's per-process hazards do not arise; the inbox `flock`
  (`tessel-cli/src/state.rs`) is blocking and nothing is spawned while it is held, so it is
  unchanged. Test `a_forked_child_before_exec_does_not_hold_the_worktree_lock`, red before. The
  review found std's `Command` uses `posix_spawn` for these spawns, so on macOS there is no
  user-space window and on Linux glibc a microsecond one; the test forces fork and exec, so it shows
  the `flock` property; reviewed "Yes", mutants killed. Upgrade overlap: CLI-LOCK-UPGRADE-WINDOW.
  Planned: With the next CLI task. From the #86 review: the CLI daemon's worktree lock
  (`tessel-cli/src/daemon.rs`, `flock`) has the same fork-window exposure, and the daemon spawns git
  and hooks. Done: the same record lock, or a written reason the daemon cannot race itself, with a
  test.
- [x] **SERVER-FRAME-LIMIT-TESTS** (`f2d1bd1`, #103) — lane B. As built: two adapter tests in
  `tessel-server/tests/adapter.rs`: a frame of `MAX_FRAME_BYTES` + 1 gets the host's fixed
  `Malformed` error and a later claim is granted on the same socket; a frame over `MAX_FRAME_BYTES *
  4` (axum's limit) ends the socket with no close frame. Mutants on each limit go red. Decision
  (lane and reviewer agree): keep the silent drop; the host's 64 KiB answer covers real clients, and
  a 1009 close would mean buffering past axum's limit in `ws.rs`. Planned: M5 hardening, with the
  next server task. From the P1-SERVER review: a frame over 64 KiB is refused by the host with a
  fixed `Malformed` error before parsing (`src/shell.rs`) and the socket stays open; one over 256
  KiB fails inside axum and the socket drops with no close frame. Neither is tested end to end.
  Done: an adapter test for each, and a decision whether the axum limit should answer with a close
  frame.
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
- [ ] **RUNNER-SANDBOX** — M5, closed by RUNNER-OWN-UID. From the P1-RUNNER review: a command
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
- [x] **SWARM-NODE24-TAP** (`d5c27b2`, #98) — lane A. As built: `tessel-swarm/src/git.rs`
  runs `node --test --test-reporter=tap` in the demo repository, a test pins the flag, the README's
  Node line names it, and the swarm's `on_local` test (a third Node 24 failure) is fixed with it;
  1929 tests; reviewed "Yes"; the orchestrator's gate on `d5c27b2` is green on this Node 24 Mac.
  From the orchestrator's gate on `04493a4`: the parser read TAP (`# pass N`, `not ok`), but from
  Node 23 the default reporter when stdout is not a terminal is `spec`, so on a Node 24 machine
  `the_starting_repository_passes_its_own_tests` saw 0 tests and the failure note lost its first
  failing test; CI, pinned to Node 22.23.3, stayed green.
- [x] **CI-LANDING-E2E-LOG-RACE** (`e7d2a7e`, #100) — lane A; CI green on all 13 checks (the
  e2e job included), reviewed "Yes" with no fixes, merged by Felix. As built: the two one-shot
  greps are one wait of at most 30 s with the server-alive check inside it and the two failure
  messages kept apart; the order is the push (`tessel-lander/src/merge.rs:487`), the read-back
  (`:488`), then the log line (`tessel-server/src/landing/answer.rs:45`) after `run_merge`
  returns; the reviewer ran the new loop against a log appended 2 s late (green; the old grep red
  with CI's message). Not reproduced: the 4 ms race itself. From main's CI on
  `d5c27b2` failed "Slow lane, local landing end to end" with "the server did not log a read-back
  landing" while the server log in the same output holds the line, 4 ms after the script's grep:
  `tessel-server/ci/landing-e2e.sh` waits for the bare trunk's `main` to move and then checks the
  log once (the read-back line, then the landed head in it), but the server pushes first and logs
  the read-back after `ls-remote` confirms it. Fix: wait for the log line naming the landed head in
  a bounded loop, same shape as the script's other waits; no server change. Done: the ordering in
  the server shown by file and line; the script green locally; the job green on the pull request;
  the failure recorded here as the job's first failure in twelve `main` runs, not retried.
- [x] **DOCS-GATE-SECTION** (`dec1c56`, #105) — lane A. As built: `docs/configuration.md`'s gate
  section and time-budget paragraph say what the server's lander and the runner do, with Rust file
  and line refs (the runner's 1800 s default at `tessel-runner/src/config.rs:23`); the admin policy
  is marked as issued by no route. Reviewed "Yes" after one fix pass (a wrong line ref). Planned:
  From lane A's CLI-LOCK-ADMIN-TEXT report (24 October): `docs/configuration.md`'s "Who may change
  the gate" (near line 104) still describes the steward: `merge-steps.ts` line refs, an admin merge
  over `POST /repos/<repo>/merges` with `STEWARD_ADMIN_TOKEN`, and the old reason strings. Now a
  submission touching `tessel.toml` is refused by the server's lander with the #102 text, the change
  goes through a pull request Felix merges (BUILD-PROTOCOL section 2), and `MergePolicy::Admin` has
  no caller outside `tessel-lander/tests/merge_steps.rs`. The step-budget paragraph above it cites
  `tessel-steward/src/step-budget.ts`: check whether the Rust runner keeps those budgets. Done: the
  section says what the server does, with Rust file and line refs, and the named-tests check green.
- [ ] **LANDER-ADMIN-POLICY-DEAD** — from the same report: `MergePolicy::Admin` and the
  `GateInvalid` outcome it alone produces are built only by tests. Decide with the M6 team-mode
  re-plan whether an admin route returns (then keep) or not (then delete both, "replace, don't
  deprecate").
- [x] **CLI-LOCK-UPGRADE-WINDOW** (`dec1c56`, #105) — lane A. Answer: `tessel start` reuses any
  daemon that answers `Status` within 2 s (`tessel-cli/src/commands.rs:219-228`) and stops none, so
  a second daemon starts only after a timeout or an unreadable reply (`:243-259`). As built:
  `acquire_lock` first tries the old binary's `flock` and gives it back at once, refusing while
  another open file description holds it; on Linux the two lock kinds are independent, on macOS they
  exclude each other (the reviewer's experiment), so the probe runs before the record lock. Linux
  red shown by a CI plant (run 40071707487 on `29a5ddc`), then reverted. Left: an old binary started
  after a new daemon still runs beside it on Linux; fixing that would bring back #102's fork window.
  Reviewed "Yes" after one fix pass. Planned: From the #102 review: on Linux a `flock` and an
  `fcntl` record lock do not exclude each other, so during an upgrade a daemon from the old binary
  (`flock`) and one from the new (record lock) could both run on one worktree. Done: a written
  answer whether an upgrade can overlap (does `tessel` stop the old daemon first?), and if it can,
  the new daemon also refuses while an old daemon's pid in `daemon.lock` is alive, with a test.
- [x] **DOCS-STEWARD-SWEEP** (`c50be59`, #107) — lane A. As built: `docs/configuration.md` (36
  mentions to 2), `docs/how-tessel-works.md` (7 to 1), `docs/reference/runtime-rules.md` (wording
  only; the four left are test names) and `docs/reference/coordinator.md` (1 to 0) describe the Rust
  server, lander and runner with Rust refs checked at `36b17f2`; one legacy sentence per page;
  steward-only features removed. Reviewed "Yes" after one fix pass. Planned: from the #105 review:
  `docs/configuration.md`'s images section and the gate's fallback text still cite the steward.
  Done: every page under `docs/` (except history and proposals) says what the Rust server does, with
  Rust file refs; a `rg -n -i steward docs/*.md docs/reference` pass shows only deliberate
  historical mentions.
- [x] **CLI-START-REFUSED-MESSAGE** (`c50be59`, #107) — lane A. As built: when the spawned daemon
  exited and a daemon answered `Status`, `tessel start` names that daemon's pid, connection and last
  error and points to `tessel status` and `tessel stop`; only when nothing answered does it say the
  worktree lock is held and no daemon answers. Tests in `tessel-cli/tests/start_lock.rs`. The
  review's two-starts probe found the first version wrong; fixed. Left: CLI-START-RACE-TEST-VACUOUS.
  Planned: from the #105 review: when a new daemon refuses the lock (an older daemon's `flock`, or a
  fork child in its exec window), `tessel start` waits 10 s and says "running (pid) but not
  connected", naming the exited child's pid. Done: the message says the worktree lock is held and by
  what it knows, with a test.
- [x] **COORD-HEAD-RESTART-READ** (`ff56bd5`, #108) — lane B. As built (`src/coordinator/trunk.rs`
  near 118): a state loaded from storage whose head still equals the `Hello`'s base, with no failed
  read since, owes one read at the next wake that runs anyway (the server wakes once at start,
  `tessel-server/src/repo.rs:171`); it sets no wake of its own, retries as #106's backoff does, and
  a head from a landing or a read owes nothing. A non-stored `loaded` flag marks a deserialized
  state; the host's in-process reload after a failed commit sets it too, which the review found
  harmless. Reviewed "Yes", no fixes; every mutant red. Planned: from the #106 review: a server
  restarted while the start head read is out keeps the head a `Hello` adopted unchecked until the
  next trunk poke (a scratch test showed it). Old behaviour, not a regression of #106; the one-line
  fix (offer the read again on load when `head_from_hello` is set) breaks five tests, so it needs a
  design: which wakes a restored state owes. Done: the design in the row, the restart case tested,
  the five tests changed only where they pinned the gap.
- [x] **CLI-START-RACE-TEST-VACUOUS** (`f9f92d6`, #109) — lane A. As built: the two-starts test
  forces a loser (the first daemon holds the lock offline and its socket is hidden until the second
  daemon logs that it gave way) and asserts the loser names the winner's pid; a non-zero exit at the
  deadline keeps "the daemon exited (status) before connecting" (`deadline_message`,
  `exited_message` in `tessel-cli/src/commands.rs`, two unit tests). Reviewed "Yes". Left:
  CLI-START-DEADLINE-SEAM. Planned: from the #107 re-check: the two-starts race test in
  `tessel-cli/tests/start_lock.rs` passes without checking anything when neither start loses, and
  the deadline's `try_wait` treats any exit as giving way, so a daemon that fails in the last second
  gets the held-lock text. Done: the test asserts exactly one start lost (or forces it), and a
  non-zero exit at the deadline keeps the "exited before connecting" message, with a test.
- [x] **DOCS-R22-HOST-TEST** (`f9f92d6`, #109) — lane A. R22 names
  `host_a_restart_with_a_hello_adopted_head_reads_main_at_its_first_wake_and_not_before`; the
  named-tests check matches it. Planned: from the #108 review: `docs/reference/runtime-rules.md` R22
  does not name the host test #108 added for the restart read. Done: R22's row names it, and the
  named-tests check stays green.
- [x] **CLI-START-DEADLINE-SEAM** (`3ae90c8`, landed by the server) — lane A. The wait loop is
  `await_online(worktree, child, timeout)`; `start` passes START_TIMEOUT; a test with a silent
  socket and a late child pins the deadline branch's exit status; the race test kills its winner
  daemon on every path. Opus "Yes". Planned: from the #109 review: the start loop's deadline branch
  (`tessel-cli/src/commands.rs:280`) passing the child's status to `deadline_message` is not
  pinned (a mutant passing `ExitStatus::default()` survives); a real-binary test needs the exit to
  land inside one probe before the fixed 10 s deadline, so it needs a seam (an injectable deadline
  or clock for `start`). Also: if `start_lock.rs:135` returns early, the winner daemon (:118) is
  not killed. Done: the seam, a test that the late failure reaches the user through `start`, and
  the race test's cleanup on every path.
- [x] **SWARM-UNBOUNDED-LOOPS** (`9701abf`, landed by the server) — lane A. Planned: from the
  #110 review: `tessel-swarm/tests/on_local.rs` has two
  wait loops with no bound (near :115 and :1389) outside the two cut tests. Done: each waits by
  the clock with a stated bound and a message naming what it waited for.
- [x] **CLI-TEST-STDIN-EPIPE** (`de24d41`, #112) — lane A. As built: `tessel_in_deleted_cwd` in
  `tessel-cli/tests/support/mod.rs` spawns, then `send_stdin` treats a broken pipe on the write as
  an early exit; a test in `tessel-cli/tests/cli.rs` waits for the exit, then writes, red with the
  old helper; the runner test's wait loop sleeps 1 ms between polls. Reviewed "Yes". Minor left: the
  two deleted-cwd tests assert only exit code 2, not the cause in stderr. Planned: From the #111
  review: `tessel-cli/tests/cli.rs:1302` runs `tessel` in a deleted working directory;
  `tessel-cli/src/commands.rs:90` fails before stdin is read, so the write at
  `tessel-cli/tests/support/mod.rs:1474` can meet a closed pipe, the same race RUNNER-BINARY-EPIPE
  fixed in the runner's tests. Done: the support helper treats a broken pipe on the write as an
  early exit (as `tessel-runner/tests/runner/binary.rs` does), with a test that forces the exit
  before the write; the runner helper's busy wait sleeps between polls.
- [x] **RUNNER-BINARY-EPIPE** (`35d1124`, #111) — lane A. As built: the runner binary tests'
  `send_request` treats a broken pipe on the write as the runner having exited early (it refuses a
  missing config at `tessel-runner/src/main.rs:57`, before reading stdin at :58); a test waits for
  the exit, then writes, red with the old helper. #110's failed job was re-run once with this row
  cited (attempt 2 green). Reviewed "Yes". The same race in the CLI's test support:
  CLI-TEST-STDIN-EPIPE. Found: flake seen on #110's CI (run 40083221329, 24 October, Linux):
  `tessel-runner/tests/runner/binary.rs:34`
  `the_binary_exits_2_with_a_fixed_message_when_the_config_is_missing` panicked with "send the
  request: Broken pipe": the runner exits 2 on the missing config before it reads stdin, so the
  test's `write_all` can meet a closed pipe. Failed on both nextest attempts in that run; #110 does
  not touch the runner. Done: the helper treats a broken pipe on the write as the runner having
  exited early and still checks the exit code and message; a test that forces the early exit before
  the write.
- [x] **SERVER-FORK-CASE** (`9334aaf`, #95) — lane B. As
  built: the server accepts only lowercase agent names (the verifier, `admin token`, `reviewers`
  and the fork template each refuse any other; the protocol's rule is unchanged); a store that
  already holds claims for an uppercase agent keeps them until they expire. From the
  P1-SERVER-LANDING review: on a case-insensitive filesystem two agent names differing only by
  case share one fork (on this Mac a call as `A1` landed through `a1`'s fork), and GitHub logins
  fold case the same way, so URL forks collide too. `docs/configuration.md` says to mint tokens
  for lowercase names. Done: a name whose case-folded form equals another live agent's is refused
  at the verifier or the fork template, with a test on both a path and a URL fork.
- [ ] **SERVER-TRUNK-MOVED-ROUTE** — M6, with team mode. From the same review: nothing in the
  tree calls `trunk-moved` since the adapter feeds the host its own poke after a landing; the
  route stays for main moves the adapter did not make. Done: wired to the git host's push
  event in team mode, or deleted with its `lander` service name.
- [x] **CI-ACTIONLINT-ZIZMOR** (`8c49be8`, #94) — lane A. As built: "Fast lane, workflow lint"
  (actionlint 1.7.12 by checksum-verified binary, zizmor 1.31.1 through its action pinned by SHA);
  Felix adds it to the ruleset's required checks. M4, before the orchestrator's gate on the merged
  tree stops (the protocol rewrite, #90). From the M4-DOGFOOD-SETUP review:
  `.github/workflows/ci.yml` runs fmt, clippy and the named-tests check but neither `actionlint`
  nor `zizmor`, which only the lanes' gates run; once landings bypass the orchestrator's gate
  nothing on `main` checks the workflows. Done: both in the fast lane, pinned by SHA, red on a
  planted workflow fault.
- [x] **MERGE-ADMIN-MERGE-TEXT** (`3b19099`, #102) — lane A. As built: a submission touching
  `tessel.toml` is refused with "changes the gate (tessel.toml); the server's lander refuses it, so
  change it in a pull request that the repository's owner merges", and `GateInvalid` names the
  lander; two tests pin the texts, red on the old strings; no protocol type changed.
  `MergePolicy::Admin` has no caller outside `tessel-lander/tests/merge_steps.rs`
  (LANDER-ADMIN-POLICY-DEAD); `docs/configuration.md` still quotes the old texts
  (DOCS-GATE-SECTION). Planned: With the next core task. From the same review: `src/merge.rs:327`
  still says a change to `tessel.toml` needs "an admin merge", a steward route that no longer
  exists; the lander refuses the change and it goes through a pull request. Done: the text says
  that, with its test.
- [x] **SERVER-KILL-AFTER-PUSH-TEST** (`f2d1bd1`, #103) — lane B. As built: a `post-receive` hook on
  the trunk holds the lander's push open, the real server is SIGKILLed there (main moved, no
  `Merged` in the store), restarted on the same directories, and the claim lands once as already
  merged: `recover_merge` (`src/coordinator/merging.rs:173`) keeps `unanswered_from`, the
  re-dispatch reads main fresh and the rebase gives head equal to base, answered `Prepared` and read
  as `AlreadyMerged`. About 12 s a run, stable over the lane's and reviewer's runs; one fix pass
  made the hook's wait end when the test's directory is gone. A second `Merged` mutant goes red.
  Planned: M5 hardening, with the next server task. From the same review: `restart.rs` kills the
  server during the runner's command; a kill between the push and the answer is untested (it should
  end as already merged, counted once). Done: that test.
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

**Decided by Felix (24 October):** D1-D4, as the STATE block records, after two adversarial
reviews (security, operations) and the sandbox trial.

- [x] **M4-GITHUB-REMOTE** (`6fcf36b`, #92) — lane B. Done-check met 24 October: the sandbox
  landing on `tessel-sandbox/m4-trunk` (`8c878d3`, parent `661a420`), lane B's commit fetched from
  the private fork `m4-fork-lane-b` and pushed over HTTPS with a fine-grained token through the
  askpass hook, through the admin-role bypass. As built: `[lander] token_file` (0600, checked at
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
- [x] **M4-FIRST-LANDINGS** (24 October) — both lanes and the orchestrator. Landings 4 and 5 lost
  their notes on the rebase and were copied by hand; after LANDER-REBASE-DROPS-NOTES the rebased
  landing `905b6b1`..`d540b0f` (SERVER-CWD) kept its six notes with no hand step. Landed by the
  server on 24 October:
  1. `91785b2` DOCS-REFLOW-LONG-LINES (lane A; held for `docs/BUILD-PROTOCOL.md`, approved).
  2. `97b0316`..`3f4278b` this file's STATE (orchestrator; not held).
  3. `9701abf` SWARM-UNBOUNDED-LOOPS (lane A; not held).
  4. `c41051a`..`0c0f119` GATE-INVALID-RETRIED (lane B; held, approved after a restart).
  5. `2a7c560`..`768383e` STEWARD-LOG-WORDING (lane A; held, approved).
  Each had an Opus "Yes" on the commit read from the fork before its submission; four needed one
  fix pass. While lane B held `src/merge.rs` with `edit-signature`, the coordinator denied lane A's
  claim on it, and lane A narrowed its scope (STEWARD-WORDING-REST): the first real denial
  between two lanes.
- [x] **DOGFOOD-HARDENING** (`5864fe2`, #113) — lane A. From the two adversarial reviews of
  D1-D4: `sensitive` grew to 20 gate and infrastructure prefixes; `setup.sh` copies the binaries
  to `<server dir>/bin` and writes the 6-job cap; `docs/dogfood.md` gained the ruleset split, the
  token's permissions, Actions off on forks and the rules while landings run. Opus "Yes" after one
  fix pass.
- [x] **LANDER-FORK-ANY-BRANCH** (`4382641`, #114) — lane A. Lanes could not `git push --force
  <fork> HEAD:main` (denied to their sessions). The lander fetches `+refs/heads/*` of the fork
  (`--no-tags --quiet`) and lands a commit only if `for-each-ref --contains` finds it on a fork
  branch; an empty fork answers `CommitNotInFork`. Never fetched by sha: protocol v2 serves any
  object the fork holds. Design chosen by an adversarial review; Opus "Yes" after one fix pass.
- [x] **DOCS-REFLOW-LONG-LINES** (`91785b2`, landed by the server) — lane A. Two prose lines #114
  left over 100 characters. Others remain at `docs/BUILD-PROTOCOL.md:16,119,128` and
  `docs/how-tessel-works.md:181,182` (DOCS-LONG-PROSE-LINES).
- [x] **GATE-INVALID-RETRIED** (`0c0f119`, landed by the server) — lane B. A missing, invalid or
  non-UTF-8 trunk `tessel.toml` is refused once as `RuleKind::GateInvalid`; a failed read stays
  infrastructure. The agent sees a fixed reason (rule messages are untrusted and never sent to
  agents); the parser error, quoted and cut at 1200 characters, is in the server log. Opus "Yes"
  after one fix pass.
- [x] **GATE-INSTANCE-REQUIRED** (`799c5bc`, landed by the server) — lane B. Nothing outside
  tests read `GateConfig.instance`; it is optional for the lander (absent parses, an unknown value
  is still refused); `tessel.toml` keeps it for the steward. Opus "Yes" after one fix pass.
- [x] **STEWARD-LOG-WORDING** (`768383e`, landed by the server) — lane A. The host's calls are
  "head call", "lander merge call" and "lander trial call", with test names and runtime-rules rows
  to match. Opus "Yes" after one fix pass.
- [x] **STEWARD-WORDING-REST** (`87ed743`, landed by the server) — lane A. Renamed the core's
  steward wording where the Rust lander is meant; kept `STEWARD_CALL_TIMEOUT_MS` (the steward's
  step-budget test reads it), the historical runtime-rules Functions cells and the steward-only
  budget lines. Opus "No", then "Yes" after one fix pass.
- [x] **STEWARD-PROSE-REST** (`cd68555`, landed by the server) — lane B. The remaining comment and
  test-name prose; what describes the TypeScript steward's wire or Worker stays "steward". Opus
  "Yes" after one fix pass. Planned for STEWARD-WORDING-REST: the constant
  `STEWARD_CALL_TIMEOUT_MS` (`src/merge.rs`), doc lines in `src/coordinator.rs`,
  `src/coordinator/merging.rs` (two asserts and a test name), `src/coordinator/verifying.rs:37-40`,
  `src/merge.rs:32-37`, `tessel-server/src/repo.rs:597`, `tests/runtime_rules.rs:417` and
  `docs/reference/runtime-rules.md:61` (`ask_steward_for_head`, a name not in code).
- [x] **LANDER-REBASE-DROPS-NOTES** (`d43aeeb`, landed by the server) — lane A. `Merged` gains an
  additive `base` (`#[serde(default)]`); the CLI daemon queues {claim, submitted, floor, base,
  head} and, off its message loop, copies each submitted commit's note to the landed commit with
  the same author line and raw message (never patch id: a clean rebase changes it), unique matches
  only, never `-f`, shas and counts only in its log. Design chosen by an adversarial review; Opus
  "Yes" after two fix passes. Its first landing attempt failed on the server's removed working
  directory (SERVER-CWD); the merge retried after a restart and landed.
- [x] **CI-NOTES-TEST-IDENTITY** (`1c9497f`, landed by the server) — lane A. The notes tests'
  repositories get a local git identity; main's CI was red on `d43aeeb` without it.
- [x] **SERVER-CWD** (`d540b0f`, landed by the server) — lane A. Every lander git command runs in
  an explicit directory; relative config paths (`work_dir`, `token_file`, path `remote`/`fork`,
  `store_dir`, `runner_config`) are made absolute once at start, absolute ones are kept as given;
  tests start from a directory that is then removed. Opus "Yes" after two fix passes.
- [x] **TRIAL-UNSUBMITTED-READS-FORK-MAIN** (`54b292b`, landed by the server) — lane B. A trial
  with no submitted commit answers `CommitNotInFork` after the fork-name check and runs nothing;
  the core makes it Inconclusive. The dead `trial_request_body` is gone. Design chosen by an
  adversarial review; Opus "Yes". Planned: from the #114 design review: a trial with no commit
  reads the fork's `main` (`tessel-server/src/landing/trial.rs:39`, `src/host/call.rs:53`), which
  since #114 is never updated by lanes. Sensitive path.
- [x] **DOCS-LONG-PROSE-LINES** (`2928159`, landed by the server) — lane B. The two prose lines
  over 100 characters in `docs/BUILD-PROTOCOL.md`; the rest are tables and quoted output.
- [x] **M4-RESTART-HELD** (24 October) — lane B's claim 4 (GATE-INVALID-RETRIED) held for review;
  the server stopped and started at 16:54 UTC (12:54 EDT) with nothing running; the claim stayed
  submitted, was approved after the restart and landed (`0c0f119`).

### M5. Hardening: simulator, staged landing, reconciliation

Plan (`docs/proposals/phase-1.md`, M5, re-planned 25 October): the simulator with I1-I12 (their
evidence and advisory clauses in M6) and failpoints; the core's landing split into `Prepare`, a
journaled head on a candidate ref, `Test` and `Land`; `Unknown` and `Reconcile`; the host
hardening and the adapter suite; repo code under test as its own macOS user; the swarm on the
server; the dogfood server upgraded to the staged landing. Done when: the simulator lane is green
with all twelve checkers on and its short seed set runs in every landing; R20, R24, R30 and R36
flipped; P1-ADAPTER-SUITE green on CI; repo code under test runs as `_tessel-runner` on the
dogfood server, with a CI test; M5-DOGFOOD-STAGED.

**Decided by Felix (25 October):** M5-1 to M5-8 as recommended, after an adversarial Opus review
of the re-plan (bought in on its sixth draft). Moved to M6: P1-LANDER-OUT (the lander out of the
server's link graph, the phase's done-check 8), the simulator's advisory and evidence clauses,
the failpoint "landed, evidence note not written" and the merge record's route. Moved to Phase 3:
M5-PROOF-CANDIDATE. Merge-path tasks ("Felix reviews") are approved only after Felix's written yes
naming the fork commit's sha (MERGE-PATH-REVIEW).

Lane A:
- [x] **P1-SIM-CORE** (`cc59a08` stack, landed by the server; #115 first) — lane A, in two
  halves. `src/sim/`: the driver, a store that loses staged writes or fails a commit, a jumping
  clock, crashes after any output, and traffic with claims, amends, releases, heartbeats, races,
  reviews and watchers; checkers I1, I2, I4, I5, I8, I9, I10, each catching its own planted bug
  alone and pinned by hand-built cases; each seed run twice with transcripts compared; clock calls
  banned in `src/host` and `src/sim`. 20 seeds by default (about 10 s in `cargo test`),
  `TESSEL_SIM_SEEDS` 2 in the mutants jobs (#115). Opus "Yes" after three fix passes per half.
- [x] **P1-CI-SIM** (#120, merged) — lane A. Code landed with the stack (`cc59a08`): seeds from
  `TESSEL_SIM_FIRST_SEED`, every failing seed printed with its rerun line (at most 20), kept seeds
  in `tests/simulator-regressions.txt` replayed first. The workflow, #120: 1,000 seeds per pull
  request and push (2 min 4 s on a GitHub runner), 20,000 nightly. Ruleset step 3 is Felix's next.
  Its first nightly full mutants found 61 missed mutants beyond the baseline, all in `src/sim/`
  (SIM-MUTANTS).
- [x] **SIM-MUTANTS** (`dfceb8a`, landed by the server) — from #120's nightly. `.cargo/mutants.toml`
  leaves out 24 named simulator mutants, all missed on CI at two seeds and none killed today; new
  tests (driver wiring, a golden RNG sequence, traffic and crash-kind coverage) kill the rest. The
  baseline did not grow. The 26 October nightly on main is the first full check.
- [x] **P1-SWARM-ON-SERVER** (`12d77b9`, landed by the server) — lane A. The swarm's local
  target drives a real `tessel-server`; no swarm code calls the core's merge functions. Additive
  `lease_ms` (1 s to 10 min) and `shadow_enabled`, applied on every load; a test clock only in the
  server's test support. The workspace test takes about 305 s on this Mac against the runner's
  720 s. Its first landing failed in the runner (a keep-alive test lapsed under load); the
  keep-alive tests are now driven by the clock. Opus "Yes" after two fix passes.
- [ ] **P1-ADAPTER-SUITE** — M. First part done (`9a2a044`, landed by the server; workflow #122,
  merged): an `#[ignore]` kill -9 loop run by the "Adapter suite" job, checking every client
  transcript against I1, I5 and I9 after each reload (1 min 11 s on its first GitHub run), and the
  CLI of `e9df98c` landing through the current server. The rest after P1-HOST-HARDENING: a slow
  watcher, a panicking repo task, injected store errors.
- [ ] **P1-HOST-HARDENING** (dispatched 30 October, run by lane B: see STATE) — M, after
  P1-CORE-RECONCILE. P1-PERSIST-RETRY (C8, flips R30), exact replay without stalling (C3, flips
  R36), COORD-UNDELIVERED-GRANT (decision 7).
- [x] **P1-SIM-MERGE** (`5d68ed7`..`7423616`, landed by the server 30 October; held for a signature
  change, approved on Opus REVIEW-3's "Yes") — L, after P1-SIM-CORE and P1-CORE-STAGED-LANDING;
  lands after P1-CORE-RECONCILE and CORE-HEAD-ADOPTED (its review). The asynchronous merge path;
  checkers I3, I6, I7, I11, I12; COORD-HEAD-FROM-AGENT's check that no dispatch uses an
  agent-declared base. Also the `dab6a22` reviews' simulator findings, moved here from
  P1-CORE-RECONCILE by the orchestrator because this task replaces that lander and keeps to
  `src/sim/**`: the simulator's `Reconcile` reports any prepared head as landed
  (`src/sim/driver.rs:691` on `dab6a22`), and its lander does not model C7. I12 is unproven at the
  default seeds (at most two merges per seed); its doc says so. Done also carries
  CORE-HEAD-ADOPTED's simulator check: the agent-base check green on seeds 64, 196 and 229 and on
  CI's sweeps of the merged tree. Met: REVIEW-3 ran seeds 64, 196 and 229, 50 seeds from 1944327623
  and 150..300, all green; CI 41060256861's 1,000-seed sweep on `7423616` is green; by the lane's
  report (its commit notes), 0..300 and 1,000 seeds from 442872731007 also pass. Known gap
  (REVIEW-3): a lax `resend_test` is caught only by the scripted restart run, never by the sweeps
  (I6 keeps its slack).
- [ ] **P1-FAILPOINTS** — M, after P1-SIM-MERGE and P1-CORE-RECONCILE. The crash windows of a
  landing, the candidate ref's included.
- [x] **CORE-HEAD-ADOPTED** (`b4e1348`..`9188032`, landed by the server 30 October; held as merge
  path, approved on a 3-0 re-vote after a first vote's must-fix) — S, lane A, Felix reviews, after
  P1-CORE-RECONCILE (when the core is lane A's again) and before P1-ONE-BATCH, which changes the
  same wake; found by P1-SIM-MERGE's agent-base check (seeds 64, 196, 229). A dispatch names the
  `Hello`-declared head while it is only adopted: a race trial records `state.head` as
  `before`/`main` (`src/coordinator/racing.rs:563`, `verifying.rs:336-337` on `7d671d3`), and so
  does a `Prepare`'s `main` (`merging.rs:230` on `7d671d3`). By P1-SIM-MERGE REVIEW-1's reading,
  after the start read fails `MAX_INFRA_RETRIES` + 1 times `start_retry_owed` (`trunk.rs:118-123` on
  `7d671d3`) stops owing a read, so the head can stay adopted; nobody has traced it on those seeds.
  Keep owing the read with capped backoff and dispatch nothing that names a head while the head is
  only adopted. Done in part: the core and server tests (racing.rs
  `a_race_on_a_head_adopted_with_no_start_read_is_decided_by_the_reads_it_owes`, tessel-server
  `tests/clock.rs` `a_race_is_decided_on_a_port_that_reads_no_heads_with_an_empty_store`) green on
  CI 41048353740. The simulator's agent-base check is on main since `7423616`; see P1-SIM-MERGE's
  Done. The row, its lane and its place before P1-SIM-MERGE's landing (which delays P1-FAILPOINTS)
  rest on P1-SIM-MERGE REVIEW-1 and the standing order.
- [ ] **HEAD-ADOPTED-FOLLOWUPS** — S, lane A, Felix reviews (`src/coordinator/` and the host: merge
  path, voted), after P1-HOST-HARDENING (it shares `src/host/calls.rs` and `src/host/tests.rs`) and
  before or with P1-ONE-BATCH (the same wake), and before RESOLVE-RUNBOOK-NITS (it shares
  `docs/dogfood.md` and `src/host/calls.rs`); from CORE-HEAD-ADOPTED's re-vote
  (`~/tessel-orch-notes/2026-10-30/vote-core-head-adopted-2/VOTE-1..3.md`), resting on the votes and
  the standing order. A port that never reads heads (`NoLander`, every repository without a
  `remote`) polls a refused head read every 90 s for ever once one is owed (on `df4b39a` it stopped
  after four): give the core the port's `reads_trunk_head()`, so such a port owes no reads and its
  races do not wait, or stop owing once the waiting races are judged; at the least, say in
  docs/configuration.md that a repository without a `remote` logs a refused head read every 90 s
  after its first race or restart. The ReadHead path drops the refusal's kind, so the log says "no
  head in the answer", not "no lander". A race that closes after the retries are spent is judged at
  once with the tests unknown, although the next owed read is at most 90 s away: flag it and judge
  it on that read. A poke resets `head_start_failures` (`trunk.rs:102` on `9188032`), so pokes that
  keep coming keep a closed race waiting without bound: a per-race wait start, or document it. Log
  wording: the log-once line goes through `failure(..)` ("head call failed: races wait ..."), and
  `log_waiting_races` is computed before the apply (`host/calls.rs:377` on `9188032`), so the read
  that releases the races can still log the wait. docs/configuration.md: "the race stays undecided
  until a read answers" should add "or four reads have failed". The docs/dogfood.md rollback note:
  give the store query for `"trials_await_head":true` and the remedy (wait for the read, or the 130
  s), a query for the rollback's `state_version` (`select value from meta where key =
  'state_version'`), and the "no served repository lacks a `remote`" check before an upgrade (from
  RESOLVE-FOLLOWUPS' re-vote, VOTE-3 should-fix 4). Tests: a host test where a poke resets the
  failures while the head is adopted and a second wait is logged (mutant M5 survives); the landing
  test checks that a race waiting on the adopted head is released by a landing whose head is the
  adopted base; tessel-server's clock test asserts `tests_passed: None` on the `RaceResult`. From
  RACE-UNKNOWN-REASON (its item 2 and its vote's VOTE-3 should-fixes 3-4): an operator log line when
  a race is judged with the tests unknown (the core has no channel for it; it belongs in
  `src/host/**`); the opener's `RaceResult` line is RACE-LOSER-TEXT's. From P1-SIM-MERGE's REVIEW-3
  nits: the poison in `a_restart_during_every_poisoned_test_is_rejected_at_exactly_the_bound` is
  inert (the test passes with `poison_fork` deleted): drop it or rename the test; say in
  `Sim::crash_after_every_test_dispatch`'s doc why it is `pub`, or use it from `tests/simulator.rs`.
  Done: those tests, each failing without its change, and the docs.
- [ ] **P1-ONE-BATCH** — M, after P1-CORE-RECONCILE. One batch per wake (flips R20) and lease
  renewal persisted below half the lease (C10). Evidence from the kill loop on CI: the server
  keeps one commit outstanding and commits each step as its own batch (`src/host/batch.rs:1`),
  each with an fsync (`synchronous=FULL`, `tessel-server/src/store.rs:357`). In PR run
  40428101997, round 0 stored 16 events at about 70 ms each, and the two rounds with no new grant
  stored 4 and 7 (about 90 ms each for the 7); other CI rounds ran at about 13-20 ms, as on this
  Mac. Inference, not proof. P1-SIM-MERGE weakens the simulator's I4 while a call is out (a
  suspended wake owes only the watchdog, R20/R28), so with calls out for up to 400 s it misses
  most lease-expiry bugs. This task restores it if its wake runs expiry while a call is out;
  otherwise the gap needs its own row.
- [x] **NIGHTLY-TIME** (kill-loop half landed as `1dd12a9`; workflows #124, merged) — lane A, from
  the 26 October scheduled runs cancelled at their limits. Every kill-loop wait is bounded and names
  its round, the check runs on a thread, the child is reaped with a bound. #124: the nightly's full
  mutants in 8 round-robin shards (32-55 min each over its two full runs), the kill-loop job 60 min
  for 200 rounds, iterations capped at 1-300. Its first full run found one new missed mutant beyond
  the baseline (SIM-APPLY-CRASH-MUTANT). Done 28 October on `fe5a898`: the scheduled nightly passed
  in 56.5 min (shards 42-56 min) and the scheduled adapter suite's 200-round kill loop in 457 s.
- [x] **SIM-APPLY-CRASH-MUTANT** (`e2e473d`, landed by the server) — lane B. The scripted crash
  test pins `crashes_after_apply` at 0 disarmed and 1 armed, so a constant, skipped or doubled
  count fails. Opus "Yes" after one fix pass.
- [x] **KILL-LOOP-PROGRESS** (`0e63247`, landed by the server) — lane A, from the 27 October
  200-round run: no hang (it reached round 156 at the 30-minute limit), but the per-round check grew
  quadratically (1.08 s at 1,486 events, 27.4 s at 7,331). The checkers are linear; events grew
  because each denial lists every conflicting claim, and the test's agents released too few while
  heartbeats renewed their leases. Each agent now holds at most 6 claims and amends a claim at most
  4 times, and releases a probed claim once its stale-release probe is refused; the I9 check counts
  a stale release as accepted (a violation) only when no refusal came for that probe and the agent
  released the claim. Each round waits, bounded, for its first grant. 25 rounds: check 162 ms at
  1,836 events; 200 rounds on #124's branch (dispatched run 40612885606): passed in 470 s, 13,095
  events, last check 3.26 s. Opus "Yes" after two fix passes.
- [x] **KILL-LOOP-I9-ACCEPT-TEST** (`f217e0c`, landed by the server) — lane A, S, from
  KILL-LOOP-PROGRESS's review: no unit test pinned the kill loop's "retired fence accepted" verdict,
  so a mutant that never counts a stale release as accepted survived every test, the CI kill loop
  included. Three `violations()` tests now cover a stale release with no refusal and an agent
  release (flagged), a refused one, and no refusal without an agent release of that claim (not
  flagged); five mutants of the rule each fail at least one. Opus "Yes" after one fix pass.

Lane B:
- [x] **CANDIDATE-REF** (`a451766`, landed by the server) — lane B.
  `docs/proposals/candidate-ref.md`: `Prepare` pushes the head to `refs/tessel/candidate/<head sha>`
  on `remote` itself (fallback `refs/heads/tessel/candidate/*`), `Test` fetches it, `Land` reads the
  ref first, pushes main by compare-and-swap, reads back by ancestry and deletes the ref; a missing
  ref answers the new `CandidateMissing` (a `Call` change), which sends the submission back to
  `Prepare`. No new repository or token step. Opus "Yes" after two fix passes; Felix approved the
  shape (M5-7) on 25 October.
- [x] **P1-SERVER-STAGED** (`cfb468b`, landed by the server; Felix's written yes for `4ea4014`) —
  lane B. The server answers `Test` and `Land` from `refs/tessel/candidate/<head>` (the probe passed
  on `tessel-sandbox/m4-trunk`), refuses to push a head not built on its base, reads back by
  ancestry (P1-LANDER-ANCESTRY folded in), and answers `CandidateMissing`. `Prepare` still stands
  for the whole merge, but a lost lease whose head is already on main now answers `Prepared`. Opus
  "Yes" after one fix pass.
- [x] **OWN-USERS-DESIGN** (`b067c58`, landed by the server) — lane B.
  `docs/proposals/own-users.md`: hidden accounts `_tessel` (server and lander; alone reads the
  token, key and store) and `_tessel-runner` (repo code), UIDs 481 and 482; `/opt/tessel`
  root-owned, `/var/tessel`; the runner through a two-command sudoers rule, everything the runner
  account owns killed after each run; toolchain and dependency cache read-only to it; cron and at
  denied; dependency fetches and toolchain installs are Felix's password steps; probes before any
  secret moves, rotation last. The claim is that the secrets are unreadable to repo code under test;
  a lane can still reach `_tessel` at the next upgrade until binaries are built on CI (M6), and
  egress is not limited. Opus security review "Yes" after three fix passes; Felix approved it on 25
  October.
- [x] **P1-CORE-STAGED-LANDING** (`05b6ec1`..`55528c9`, landed by the server) — L, lane B, after
  P1-SERVER-STAGED, P1-SWARM-ON-SERVER and SENSITIVE-MERGE-PATH. `Prepare` → head journaled →
  `Test` → `Land` in the core, with the host and the CLI's fake in the same commit. Closes
  HOST-MERGE-STANDIN's stand-in half. A `Land` off its base goes back to `Prepare`, counted;
  `run_merge` and `land` stay public for the ported lander tests. Opus merge-path review "Yes" after
  one fix pass (`dab6a22`); the orchestrator ran `landing-e2e.sh` and
  `tessel-server/tests/restart.rs` on `dab6a22` (both pass), and CI's local landing end to end
  passed on `55528c9`. Felix delegated
  his yes to a vote of three Opus reviewers, 3-0 Yes. What the reviews and votes left is in
  P1-CORE-RECONCILE's and P1-SIM-MERGE's rows.
- [x] **P1-CORE-RECONCILE** (`8f15d61`..`df4b39a`, landed by the server; merge-path yes by a
  vote Felix delegated, 3-0 "Yes with should-fixes" on `7897dc7`) — M, lane B, after
  P1-CORE-STAGED-LANDING. `Unknown` and `Reconcile` (flips R24), a `Land` in doubt shown with its
  age, offline `tessel-server admin resolve` (M5-3), and DOGFOOD-UPGRADE's fixture test. Also, from
  the `dab6a22` reviews and votes: a store written before the staged landing with a marker in
  flight decodes as `Prepare`, not in doubt (make the old marker distinguishable and treat it as in
  doubt, or refuse to start with it); a `Land` that loses its lease answers `Unknown`
  (`tessel-server/src/landing/answer.rs:122` on `dab6a22`), so `Reconcile` must go back to
  `Prepare` when main is not the base, or it loops; `Unknown` at `Land` is still counted; dropping
  the `!work.in_doubt` first-baseline guard survives the coordinator's library tests (and, by
  VOTE-2's reading, the queue property tests cannot catch it), so a stages test needs two lost
  `Land`s on different bases; the three `_` arms on `MergeStage` in `merge_outcome`
  (`src/coordinator/merging.rs:363, 366, 373` on `dab6a22`) are spelled out. Smaller:
  `docs/configuration.md:277` says a cut-off `Test` waits 10 s, but `resend_test` sends it at once;
  `docs/dogfood.md` and `docs/configuration.md` describe the staged landing the live `455c889` does
  not run yet; the upgrade's in-flight check should read the stored `merge_in_flight`;
  `restart.rs` should assert the candidate ref is gone after a restart; the CLI fake's `merge_next`
  can land two merges in one call when a base == head `Prepare` reads as `AlreadyMerged`;
  `InFlight.main` feeds only a discarded `dispatched_from`; nothing pins that `moves_main`
  (`tessel-server/src/repo.rs:693`) leaves `Prepared` out. The fake's fix and the `moves_main`
  test edit lane A's `tessel-cli/tests/support/mod.rs` and `tessel-server/src/repo.rs`, which the
  plan's cross-edit list for this task does not name; lane A (on `src/sim/**`) stays out of them
  while this runs.
- [x] **RESOLVE-NAMES-HEAD** (`97ca04c`..`3b708cd`, landed by the server 30 October; held as merge
  path, approved on a 3-0 vote of Opus devil's advocates) — S, lane B, Felix reviews (it changes how
  `admin resolve` records `Merged`, the one path to `Merged` outside the lander: merge path, PLAN.md
  section 10), after CORE-HEAD-ADOPTED lands or leaves the core files; before the dogfood server's
  upgrade, a gate the orchestrator adds (the live `455c889` has no `admin resolve`), resting on
  VOTE-1 and VOTE-3 should-fix 1, STATE-1029-1's review and the standing order; it adds itself to
  `docs/dogfood.md`'s "When". The should-fixes of P1-CORE-RECONCILE's merge-path vote
  (`~/tessel-orch-notes/2026-10-28/vote-core-reconcile/VOTE-1..3.md`). `--landed` is the one path to
  `Merged` the lander does not check, and the operator is never shown the head to check (the lander
  rebases, so the claim's own commit is not on main by its sha): log `base` and `head` in the doubt
  line, print them in `admin resolve`, and give the runbook's read (`merge_in_flight.stage.head`,
  then `git merge-base --is-ancestor`) and that `--not-landed` is safe when unsure. Requiring
  `--landed <head>` equal to the journaled head (VOTE-1's "preferably") is the orchestrator's
  choice, an operator CLI change; the vote's VOTE-3 found it acceptable. Tests the votes found
  missing (VOTE-2: missing tests, not wrong behaviour): the `--landed` / `--not-landed` mapping in
  `run_admin` (a swap survives; a binary test against a stored `Land` in doubt), `resolve_doubt`
  refusing another claim (dropping its claim filter survives), `restart.rs` keeping the candidate
  ref, `!skippable()` for every v1 type, a waiter granted by an offline `--landed` getting its grant
  on reconnect (every notice is dropped as unsent), and the vacuous `merge_doubt` skip test dropped
  or made to check the count. Also: the "core retries" wording after a `Land` that ends `Unknown`;
  an `Unstaged` doubt does end in an infrastructure rejection at the retry bound
  (`retry_after_infrastructure`, reachable only by breaking "nothing in flight"): fix or document; a
  `Land` whose push fails for good resends forever (document what the operator fixes); the log's
  `--config`; run resolve as the store's owner; `docs/dogfood.md:154-155` splits the fixture test's
  name in a code span; after `--landed` the lane sees "claim N is gone from the coordinator", not
  `merged`, which the upgrade's "nothing in flight" check must accept. VOTE-3's optional `admin
  resolve --reject` is not taken. Also the seven missed mutants the scheduled Nightly 40853018831
  found on `001606a` in P1-CORE-RECONCILE's code, which its baseline allows none of:
  `src/coordinator/merging/doubt.rs` (`enter_doubt`: a deleted `doubts` field and `+=` to `*=`;
  `resend_land`: a deleted `retry_at_ms`; `&&` to `||` in `reconcile_outcome` and in
  `resolve_doubt`'s `submitted`/`work` guard, not VOTE-2's M2) and `src/merge.rs:51` (`*` to `+` and
  `/`); until then the Nightly's baseline check fails on them. The core tests are a cross edit into
  lane A's core, named here.
- [x] **RESOLVE-FOLLOWUPS** (`68ea3e6`..`ca864b2`, landed by the server 30 October; held as merge
  path, approved on a 3-0 re-vote after a first vote's two docs must-fixes) — S, lane B, Felix
  reviews (`admin resolve`: merge path, voted), after CORE-HEAD-ADOPTED lands, before
  P1-HOST-HARDENING; a cross edit into lane A's files, named here: `src/host/tests.rs`,
  `tessel-cli/tests/support/mod.rs` (the CLI waiter test), `src/coordinator/merging/stages.rs` (M3's
  assert) and `docs/dogfood.md`; from RESOLVE-NAMES-HEAD's REVIEW-1 and vote
  (`~/tessel-orch-notes/2026-10-30/vote-resolve-names-head/VOTE-1..3.md`), resting on the votes and
  the standing order. A host test that the real base and head reach the doubt log line (a swap
  survives today; `host_a_land_in_doubt_logs_the_claim_for_the_operator`). The runbook's step 1
  read: `-readonly` fails after a clean close ("unable to open database file"); run it as the
  store's owner without it, or add a read-only `admin` command printing stage, claim, base and head;
  make the runbook say what "unable to open database file" means. `tessel-server/tests/restart.rs`'s
  helper runs the old two-column query, not the runbook's four-column one. The log line's command
  lacks `--tenant`/`--repo` when the config lists several. After the move: the log path, the full
  `launchctl bootstrap` command, the pid kill's quoting, a command that waits for the stop. Say that
  Say what happens to the notes after `--landed` (as written here, "copies no notes", it was false:
  the daemon queues the copy from the logged `Merged`; the runbook now says so). Reword M3's assert
  to what it checks or record it as equivalent in `.cargo/mutants.toml`. After CORE-HEAD-ADOPTED: an
  offline `--landed` also releases races waiting on the head and drops their `RaceResult` notices; a
  test that state and log stay right and the entrants learn on reconnect, or make the runbook say
  those results are not sent as notices. A CLI test that a waiter's daemon adopts the claim granted
  by an offline `--landed`. For later (design, not a regression): `Merged` settled by the operator
  is not told apart from one the lander verified (rule 6; an additive marker), and the head check
  stops typos, not a skipped ancestry read (`admin resolve --landed` could run it through the
  lander's `git_read`).
- [x] **RACE-UNKNOWN-REASON** (`513adf3`..`e23690e`, landed by the server 30 October; held as merge
  path, approved on a 3-0 vote; the text shipped as `TESTS_GAVE_NO_RESULT`, "the tests gave no
  result, so no trial judged this entry"; its log line moved to HEAD-ADOPTED-FOLLOWUPS; the votes'
  wording fix, a `NOT_PICKED` pin and a control run were folded in after the vote, checked by the
  gate and CI, not re-voted; dropped on purpose: VOTE-2's note that `tests_decide` is redundant, an
  equivalent mutant) — S, lane A, after CORE-HEAD-ADOPTED (it changes `racing.rs`), from that task's
  VOTE-1 should-fix 2, resting on the vote and the standing order. A `TestsPass` race judged with
  the tests unknown rejects every entrant with `LOST_RACE` and logs `ReleaseReason::LostRace`
  (`racing.rs:746`; `protocol.rs:472` filters out `None`): honest about the outcome, but an agent is
  told it lost a race no trial judged. Keep such races waiting, or give them a rejection text saying
  the tests did not run. The text is a free `String` in `SubmitRejected` (`NOT_PICKED` is a second
  fixed text already), so a new text keeps the version; a new `ReleaseReason` value bumps it
  (invariant 13), so ask Felix only then. Also from CORE-HEAD-ADOPTED's re-vote (VOTE-1 should-fix
  2, VOTE-3 should-fix 4): no log line marks a race judged with the tests unknown (the `RaceDecided`
  event is the only trace); add one, or, if that needs `src/host/**`, move it to
  HEAD-ADOPTED-FOLLOWUPS. From P1-SIM-MERGE's REVIEW-3 (should-fix 1): a scripted simulator run
  where the lander refuses `ReadHead` four times while a race closes on the `Hello`-adopted head;
  the race is decided, no `Trial` names the adopted head, and every checker stays clean; and seeds
  64, 196 and 229 added to `tests/simulator-regressions.txt`. Done: each change with a test that
  fails without it, and that scripted run.
- [ ] **RESOLVE-RUNBOOK-NITS** — S, lane B, after P1-HOST-HARDENING, HEAD-ADOPTED-FOLLOWUPS and
  P1-FAILPOINTS (they share `src/host/calls.rs`, `docs/dogfood.md` and
  `tessel-server/tests/restart.rs`); a cross edit into lane A's `docs/dogfood.md` and
  `tessel-cli/tests/support/mod.rs`, named here; from RESOLVE-FOLLOWUPS' votes
  (`~/tessel-orch-notes/2026-10-30/vote-resolve-followups/` and `-2/`), resting on them and the
  standing order. docs/dogfood.md: step 3's commands in full after the move (`/usr/bin/sudo -u
  _tessel /opt/tessel/bin/tessel-server admin resolve ... --config /var/tessel/tessel-server.toml`),
  with "never as root" there too; give "never as root" a reason that is checked: the voters split
  (re-vote VOTE-1: the bundled rusqlite build has no `HAVE_FCHOWN`, so a root `admin resolve` leaves
  root-owned side files; VOTE-3: upstream SQLite chowns them to the store's owner), and the doc's
  reason is about Apple's `/usr/bin/sqlite3` in step 1, which nobody has checked; or give the plain
  reason (the store is `_tessel`'s); "unable to open database file" also means the wrong user; "any
  later `tessel-server admin` command" narrowed to one that opens the store; "both stops" names
  where each is; the bare-path empty file only for a wrong file name; "once the landed commits are
  fetched into that worktree" (the daemon does not fetch); "each landed commit" for `git notes
  show`. The doubt log line names a bare `tessel-server`: say in the runbook it means the server's
  own binary, or print the path. The upgrade section after the move: the lander path and log path in
  its checks (its rollback queries and the `remote` check are HEAD-ADOPTED-FOLLOWUPS'). Tests:
  `assert_the_runbook_reads_a_cleanly_closed_store` checkpoints the killed WAL with rusqlite before
  the restart (run it on a copy); `Fake::resolve_offline` copies `admin::resolve` and passes no
  sensitive list (call it, or pin it); `offline_resolve.rs` could assert the job's claim, base and
  head; `restart.rs`'s `_claim` is read but never asserted, and its query is a copy of the runbook's
  with nothing tying the two; `journaled_land` runs a bare `sqlite3` (Homebrew's on the Mac, not the
  Apple build the runbook names). For later: a read-only `admin` command printing the doubt (first
  vote's VOTE-3 should-fix 8). Done: the doc edits, and each test change failing without it.
- [ ] **RACE-LOSER-TEXT** — S, lane B, after P1-HOST-HARDENING; from RACE-UNKNOWN-REASON's vote
  (VOTE-1 should-fix 4, VOTE-3 should-fix 2), resting on it and the standing order. The CLI's
  `rejected_text` (`tessel-cli/src/render.rs` ~509) tells every rejected entrant "claim N is active
  again with the same fence; fix the cause, push, and `tessel submit` again"; for a race entrant
  that is false (`reject_loser` removes the claim). So do the daemon's `on_submit_rejected` notice
  (`tessel-cli/src/daemon.rs:1712-1721` on `ca864b2`, "the claim is active again with the same
  fence") and the Stop hook (`tessel-cli/src/inbox_hook.rs:354-359`, "Do not end your turn: fix the
  cause, push, and run `tessel submit` again", which pushes a race entrant toward a resubmit that
  fails with `UnknownClaim`). Say that the entry's claim ended and its fork is untouched, and how to
  ship the work (claim the freed scopes and submit, or ask the opener to reopen the race);
  `skills/tessel/SKILL.md` ~204 says the same for every `submit_rejected` and has no race-loser
  paragraph. Also the opener's `RaceResult` line, which renders "no entry survived"
  (`tessel-cli/src/render.rs:723` on `ca864b2`), should say when the tests gave no result
  (RACE-UNKNOWN-REASON's VOTE-3 should-fix 3). A cross edit into lane A's
  `tessel-cli/src/daemon.rs`, named here. Done: tests of the race-loser text in the CLI, the notice
  and the hook, and of the opener's line.
- [x] **GATE-PARITY-FMT** (`455c889`, landed by the server; held for review, approved by the
  orchestrator) — lane B, run ahead of its slot. The lander's gate and the steward's parser accept
  exactly `cargo fmt --all --check`; the runner quotes rustfmt's first `Diff in` line as the
  failure. A repository config alias can still make `cargo fmt` run repo code, as `cargo test`
  does, so the check holds for cooperating agents. Opus "Yes" after one fix pass. The
  `tessel.toml` entry is #117, merged; the dogfood server was redeployed from `455c889` first.
- [ ] **RUNNER-OWN-UID** — lane B. Part 1, the scripts and the dogfood page's steps: #118, merged
  (security review: six, five, then no root-safety findings). Part 2, the code (`20cd5fd`, landed by
  the server): `SudoRunner` behind `[lander.sudo_runner]`, the in-process runner still the default;
  the runner's config rules, sweep, limits, one-line reply and `rustc -V` check. It broke main's
  Linux build (glibc's `RLIMIT_*` are `u32`) and two dash-only tests; `e8fbaa8` and `bf07498` fixed
  both, and main was red from `20cd5fd` until `bf07498`. The Linux job is #119, merged (all checks
  pass on real accounts). Left: the move of the dogfood server, Felix's sudo steps in
  `docs/dogfood.md`; that closes RUNNER-SANDBOX and M4-SANDBOX-WORKSPACE.

Orchestrator:
- [x] **MERGE-PATH-REVIEW** (#116, merged) — `docs/BUILD-PROTOCOL.md` section 6: a "Felix
  reviews" submission is approved only after his own written yes naming the fork commit's sha; any
  new sha needs a new yes; an unheld submission of such a task is made only after that yes.
- [x] **SENSITIVE-MERGE-PATH** (#123, merged) — `src/merge.rs`, `src/coordinator.rs` and
  `src/coordinator/` joined `sensitive` in `setup.sh` and `install.sh`; the live server config was
  edited by hand and the server restarted at 23:13 UTC with nothing in flight.
- [x] **DOGFOOD-UPGRADE** (#125, merged) — a section of `docs/dogfood.md` (a pull request) before
  the first upgrade: rules (a) to (f) of the plan. As written: "nothing in flight" is checked by
  outcomes, since a staged merge or a timeout's backoff can be in flight with no clone on disk;
  the build goes to `bin.next` and a trial lands on `tessel-sandbox/m4-trunk` before the swap;
  rollback to `bin.prev` only when the store's `state_version` is one the old build knows.
- [ ] **M5-DOGFOOD-STAGED** — after the upgrade, three landings through the staged path, with
  notes and claims released by `Merged`.

Proposed, not yet placed (Felix decides where it goes):
- [ ] **P1-DENIAL-BOUND** — from KILL-LOOP-PROGRESS's review. A denial carries one `Conflict` per
  requested scope, lock node and conflicting holder (`find_conflicts`, `src/coordinator.rs:1102`),
  each with a clone of the holder's whole intent (`blocked_by`, `:1648`), with no deduplication by
  claim (unlike `blocking_claims`, `:1120`). The only bound is 256 scopes times the holders, each
  intent up to the 64 KiB frame, and it goes into `ClaimDenied`, the `Denied` reply and
  `ClaimShadowed`, persisted and sent to every watcher. Proposed: deduplicate per blocking claim and
  cap the list with an additive `more` count (rule 1); capping changes what a `Denied` reply
  carries, so Felix approves the wire change.

Moved to M6:
- [ ] **P1-LANDER-OUT** — M6, lane B, XL, after RUNNER-OWN-UID and P1-CORE-RECONCILE (decision
  M5-6). The lander's calls and the askpass hook move to a `tessel-lander` binary behind a local
  authenticated channel; `cargo tree -p tessel-server` has no lander crate, checked by a fast-lane
  step in a workflow pull request (the phase's done-check 8).

Moved to Phase 3:
- [ ] **M5-PROOF-CANDIDATE** — Phase 3 (decision M5-5, 25 October). GitHub Next and Microsoft
  Research's Proof Driven Development Kit (21 October) proves Rust against a Verus spec and falls
  back to tests. The core's invariants that proptests pin (one `Merged` per claim, fences persisted
  before grants, a held submission never dispatched) are candidates for a proof. Done: a written
  answer whether Verus fits the core, with one invariant tried.

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
  claim held until its lease lapses. M5, P1-HOST-HARDENING (decision 7).
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

# Roadmap

Task list for the build, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope and schedule;
this file tracks the tasks that deliver it. Only the orchestrator edits this file.

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-06 22:33 EDT.
**Orchestrator:** this session (resumed 19:43 EDT Oct 6 after Felix's context clear).
**In flight:** REVIEW-UI (`lane-review-ui`, Sonnet, worktree `.claude/worktrees/review-ui`, fork
`tessel-dogfood--lane-review-ui`): Opus "Yes" at `35989a9` after one fix pass (stale page could
approve an unseen commit; false "refused"); GO 22:32: re-claiming, pushing, submitting. After it
merges: Felix deploys the steward (`/review` routes, `REVIEWER_EMAILS`), then a live check behind
Access. CLI-LEASE-LOAD closed at trunk `4b394c9` (mirrored).
**Deployed now** (all from trunk `3c50a5e`, by Felix at ~21:50; the classifier refuses
`cd … && npx wrangler deploy`, so Felix runs deploys from a command the orchestrator hands him
unless he adds a rule): steward `31b4be89`, `tessel-coordinator` `8d66fe32`,
`tessel-coordinator-swarm` `7b7ae508`. These supersede the versions listed under Deployed below.
**Done this session:** COORD-HEAD closed live (trunk `3c50a5e`; stale `demo` head fixed by a
steward poke; see its entry). Filed CLI-LEASE-LOAD. LINT-RULE2 + CLI-CLIPPY-810 closed at trunk `7d7f4c9` (mirrored; no
deploy needed: lints and tests only). Felix minted that lane's fork token himself (the classifier
refused the lane's own mint); later lanes use the allowed mint script below. Shadow swarm run (next action 1): seed 2, 10 tasks, 4 agents, 3 verified
preventions, 0 false alarms, first live `Settled` releases (events 40, 48, 50); evidence in
`docs/evidence/2026-10-06/` (`sprint/build` `5a08ad3`). COORD-HEAD design done (Opus, read-only):
recommendation C, below under COORD-HEAD; its lane waits for the lint lane, which edits
`src/shell.rs` and `src/coordinator/*`.
**Permissions (Felix, 20:4x):** `.claude/settings.local.json` allows `Edit(docs/ROADMAP.md)` and
`bash <session scratchpad>/orch/mint-lane-token.sh <lane>` (mints a lane fork's 1 h write token
into a 0600 git include file, unprinted). Both rules name this session's scratchpad; move the
script to `tools/` if they must outlive it.
The session's permission classifier refuses secret writes, Artifacts deletes, forced pushes and
settings edits, so Felix runs those from a command the orchestrator hands him. Deploys are allowed
by `Bash(npx wrangler deploy *)` when run bare (no pipe); lane pushes to `tessel-dogfood--*` forks
by a project allow rule plus Felix's hook exemption for Artifacts URLs.
**Lanes land through the steward** since 19:13: CLI-BASE was the first lane change merged that way
(trunk `0118731`). The flow that works (BUILD-PROTOCOL §2, with these specifics): worktree cut
from `origin/artifacts-trunk`; the orchestrator creates the fork with
`POST /repos/tessel-dogfood/forks/tessel-dogfood--<agent>`; the lane mints its identity, claims
through `tessel` (including `create` for added functions and files), pushes with the token in a
0600 git include file and a bare `git push https://<acct>.artifacts.cloudflare.net/git/tessel/<fork>.git HEAD:refs/heads/main`,
submits after the Opus Yes; the orchestrator gates an archived copy, approves held work over the
coordinator socket as `orchestrator`, checks the trunk and mirrors.
**First live swarm run** (16:03, seed 1, 10 tasks, 6 agents, overlap 0.5, scratch repo
`swarm-s1-tmgae3`, results in the session scratchpad `orch/swarm1/`): `on` landed 9 of 10 with 0
rejections, 5 queued waits and 4 script approvals over 60 events in 172 s; task 6 timed out after
120 s with no grant, because merges through the live steward take tens of seconds each and the
queue ahead did not clear. `off` (local replay, labelled local) landed 7 with 3 rejected (2
textual conflicts, 1 broke the tests) in 5 s. For the Oct 10 A/B runs, set the wait timeout to fit
live steward latency, and report this run's timeout as it happened.
**Tip:** the Artifacts trunk `tessel-dogfood` is at `7d00ad6` (SHADOW-GC by Felix's admin merge;
tree checked equal to the gated `b143194`), mirrored to GitHub
`artifacts-trunk`; new work starts there. `sprint/build` (`4793bca` plus STATE commits) keeps the
pre-steward history and notes. The GitHub `main` is at `29aa8fe` (pull request 1). `8432f8c` was a
one-commit catch-up of the trunk to `sprint/build` `aadba8b` (replaying `sprint/build` commits
conflicts with their rebased copies on the trunk, so catch-ups are one commit on the trunk head).
**Milestone:** PLAN §9 Oct 6–8 delivered and checked live; dogfood v1 is in use (lanes land
through the steward). Oct 9 shadow verification is on the trunk and deployed (SHADOW-1), and the
swarm's shadow policy (SHADOW-2) is merged, with shadows on for the swarm Worker only. FIRST LIVE
VERIFIED PREVENTIONS (20:49 run, seed 1, 6 tasks, 3 agents): 2 shadow claims, both verified by
steward trials (`tests_failed` with a clean baseline, `textual_conflict`), 0 false alarms, 0
inconclusive, 0 never verified; raw output in `docs/evidence/2026-10-05/` (with the 16:03 wait
run). A small sample: it shows the mechanism live, not a rate.

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

**Agents:** none live. `.claude/worktrees/deploy` is a detached checkout of the trunk `7d00ad6`
used for deploys (holds `tessel-steward/node_modules`); move it with `git -C .claude/worktrees/deploy
checkout --detach origin/artifacts-trunk` before each deploy.
**Coordinator head is stale** (`e894fbe`, the last merge it dispatched): the SHADOW-GC admin merge
went around it (COORD-HEAD). CLI-BASE handles a stale head for lanes; the next coordinator-
dispatched merge corrects it.
**THE DASHBOARD IS LIVE behind Access** (Oct 6, 18:41 EDT; Felix signed in and saw it):
https://tessel-steward.devfep.workers.dev/dashboard/tessel-dogfood shows 54 claims granted, 0
denials, 0 verified, 0 false alarms, precision n/a, 7 merges, 6 reviews requested (equal to the
coordinator's `/summary`), the seven steward merges with trunk shas, the event feed through #323
with no `agent_connected dashboard` events, and the "a denial is not a prevented conflict" note.
Access app `tessel-dashboard` (team `fepdev`, policy `homelab-allowed-emails`) covers exactly
`/dashboard`, `/dashboard/*`, `/review`, `/review/*`; the AUD tag
`81ae313c892cf190f69330a71a9b0f600aa31e340d43a8dcd54eb629dfd33b08` was read from the Access login
redirect's `kid` (wrangler's OAuth token has no Access scope). Unsigned and forged-assertion
requests get 302 to the login; admin routes still answer the steward's own 401. The two values are
committed in `tessel-steward/wrangler.jsonc` (ACCESS-VARS, trunk `e894fbe`); the steward was
redeployed from the trunk with no `--var` (`fce132ca`) and stays behind Access.
**Decided (Felix delegated it, 23:05):** the dashboard and review screen sign in through
Cloudflare Access (Zero Trust) in front of the steward's `/dashboard` and `/review` paths; the
steward verifies the `Cf-Access-Jwt-Assertion` JWT (team certs, the application's AUD tag) and
maps Felix's email to reviewer `felix`. Until Access is on, those paths answer "sign-in not
configured" (fail closed). SHADOW-GC's commits do not touch `src/runtime.rs`, so DASH-1 started
at 23:41 in two lanes (DASH-1a coordinator route, DASH-1b steward page); REVIEW-UI after.
**Access, for next time:** done in Zero Trust → Access controls → Applications → Create new
application → Self-hosted and private → **Public DNS** (never the Workers tab, which protects the
whole Worker, admin routes included), one destination per path in custom-input form
(`tessel-steward.devfep.workers.dev/dashboard` etc.; a path wildcard does not cover its parent).
Wrangler cannot manage Access; the dashboard or an API token with Access edit rights can.
**Load note:** the load reached about 34 at 20:01 and about 73 at 20:41, mostly Spotlight
(`mds_stores`) on gate copies' `target/` dirs plus the iOS simulators; gate copies are trashed
right after each run.
**Merge queue:** empty.
**Background jobs:** none. Docker Desktop running (restarted by Felix; needed for steward deploys).

**Deployed** on `devfep.workers.dev`:
- `tessel-coordinator` version `923f39fb` (trunk `7d00ad6`, deployed Oct 6 ~19:25 EDT;
  `REVIEWERS = "felix,orchestrator"`, shadows off): `ReleaseReason::Settled`, the filled
  `reviewer`, `GET /repo/<name>/summary`. Loads its stored log: summary 56 granted, 8 merges,
  `head_seq` 340. Earlier checks of the route: no event appended on read, 401 without/with another
  repo's token, POST 405. Every upgrade needs a steward-minted agent token. The old
  `COORDINATOR_TOKEN` secret is unused and still set (a secret delete: Felix's).
- `tessel-steward` version `b9f110df` (trunk `7d00ad6`, deployed Oct 6 ~19:30 EDT from
  `.claude/worktrees/deploy`; images already in the registry): the dashboard (now shows the release
  reason and the reviewer) behind Access (team
  `fepdev`, AUD committed), service binding `COORDINATOR`; toolchain image (Rust, Node
  22, pnpm, GNU time, tini) on
  `standard-4` for repos with `tessel.toml`; the `lite` image otherwise. Admin routes need
  `STEWARD_ADMIN_TOKEN` (`tessel-steward/.dev.vars`): `POST /repos/<repo>` (create),
  `/forks/<fork>`, `/tokens` (fork write tokens), `/read-tokens`, `/agents/<agent>/identity`,
  `/test-runs`, `/merges` (admin merge; the only path that may change `tessel.toml`).
- Artifacts (namespace `tessel`): `tessel-dogfood` (the trunk, `7d00ad6`) and forks
  `tessel-dogfood--orchestrator` (at `aadba8b`, sprint/build lineage: do not submit from it),
  `tessel-dogfood--admin` (catch-ups), `tessel-dogfood--lane-<task>` per lane; `demo`,
  `demo--agent-1`; scratch `gate*-*` and `swarm-*` repos. Queue `tessel-artifacts-events` with its
  subscriptions as before.
- `tessel-coordinator-swarm` version `4e65697d` (trunk `7d00ad6`, deployed Oct 6 ~19:25 EDT;
  `REVIEWERS = "swarm-reviewer"`, `swarm-*` repos only, `SHADOW_ENABLED = "true"`, so it can emit
  `Settled`). Loads its stored log: `swarm-s1-tmgnje` (the 20:49 shadow run) still reads 2 verified
  preventions, precision 1.0, `head_seq` 32; `tessel-dogfood` gets 403.

**Pending from Felix:**
- DONE: `ReleaseReason::Settled` (ruled additive, Oct 6 ~18:50 EDT) landed with SHADOW-GC by
  Felix's admin merge (trunk `7d00ad6`). Still queued for a later hand merge of
  `src/protocol.rs`: `AssumptionVerified.commit`, `RaceDecided.entries` (only when something
  fills them).
0. Nothing blocking. The fork-push allow rule is in `.claude/settings.local.json` (Felix, 18:3x).
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
   New (REVIEW-UI review, Oct 6 22:31): add `#[serde(default)] fork_commit: Option<CommitId>` to
   `ClientMsg::Review`, and have the coordinator refuse a Review whose commit differs from the held
   submission. Closes a ~1 s race in the review screen (a stale page re-reads the log, then the
   claim is rejected, resubmitted and held again before its Review lands). Needs a small coordinator
   change beside the protocol line, so a lane prepares it and Felix merges it.
2. `SUBMISSION_CHECKLIST.md` says Artifacts billing starts Oct 15; the pricing page says Oct 14.
3. An untracked `AGENTS.md` (a copy of `CLAUDE.md`) sits in the repo root; left untracked.

**Known limits, recorded so nobody rediscovers them:**
- `wrangler deploy` of the steward (two container images) can untag the toolchain image while it
  starts pushing it ("Image does not exist remotely, pushing" then "Untagged" then "No such
  image"); it failed this way on Oct 5 and twice on Oct 6. Workaround that worked: find the built
  image id (`docker images` / the build log's "writing image sha256:…"), `docker tag <id>
  tessel-steward-testrunner-toolchain:manual-push`, `npx wrangler containers push` that tag, then
  `npx wrangler deploy` again ("Image already exists remotely, skipping push").
- The first `GET /repo/<name>/summary` after a Durable Object wake scans the whole event log
  behind the input gate (later reads cost only new events; DASH-1a). Fine at demo sizes.
- The coordinator checks an identity token only at the WebSocket upgrade, so an open socket
  outlives its token (the dashboard's tokens last 60 s).
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
blocks it); a lane's fork push is the one exception, written exactly as
`git push https://1e40d7b5aed4b7049e5b83bc07a5264c.artifacts.cloudflare.net/git/tessel/tessel-dogfood--<lane>.git HEAD:refs/heads/main`
alone in its Bash call (allowed by the project rule and Felix's hook exemption).
**How a lane lands (the recipe used for every lane on Oct 5–6):** the orchestrator cuts
`.claude/worktrees/<task>` from `origin/artifacts-trunk` and creates the fork with `POST
/repos/tessel-dogfood/forks/tessel-dogfood--lane-<task>` (admin bearer via `curl --config -` on
stdin). The brief opens with "Before your first edit: mint your identity, `tessel start`, and
claim the file. No edit before its claim.", names owned files, asks for tests first, mutants,
an exact predicted test count, and STOP before push. Opus reviews an archived copy; fix passes go
to the same implementer and re-checks to the same reviewer. After "Yes", the lane pushes (token in
a 0600 git include file, bare push as above) and runs `tessel submit`; any change after the Yes is
read by the orchestrator before the go (rule 00). If held for review, the orchestrator gates an
archived copy of the exact submitted commit (`git archive <sha> | tar -x`, `git init`,
`CARGO_TARGET_DIR` in the scratchpad, trash it after) and approves over the coordinator socket as
`orchestrator` with the note "orchestrator: Opus Yes at <sha>; gate <sha> <n> passed". After
`merged`: check the trunk tree equals the gated tree, `tools/mirror.sh`, deploy what changed from
`.claude/worktrees/deploy` (bare `npx wrangler deploy`, `--env swarm`, `--env=""`), check live,
reclaim the worktree, close the roadmap box, rewrite STATE. Changes to `src/protocol.rs` land
only by an admin merge Felix runs from a script the orchestrator writes (scopes from
`git diff --name-status`: A → create, M → edit_body).

**Next actions on resume (in order; Felix approved this order at 19:3x EDT Oct 6):**
1–3. DONE Oct 6 evening: shadow swarm run (seed 2, `Settled` live; the dashboard reads the
   production coordinator only, so swarm repos do not show there), COORD-HEAD (closed live),
   LINT-RULE2 and CLI-CLIPPY-810.
3b. CLI-LEASE-LOAD (filed 21:46): claims lapsed under load with the daemon online. Reproduce
   first; it affects every lane on a busy Mac.
4. REVIEW-UI (Access is in place; approvals from the browser as reviewer `felix`).
5. Oct 10: A/B runs at 30–50 agents with `wait` and `shadow` policies, then the milestone pull
   request from `artifacts-trunk` with `/code-review` and `/security-review`. Before it, land the
   current `docs/ROADMAP.md` (and BUILD-PROTOCOL.md) through the steward as one commit (see Known
   limits). Also open: DEMO-TS, SWARM-REVIEWER-ERR, CLI-CONNECT-TIMEOUT.

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
- [x] **DOGFOOD-4** — finish the switch: `orchestrator` in `REVIEWERS`, mirror to
  `artifacts-trunk`, delete the stray `tessel` repo, `docs/BUILD-PROTOCOL.md` for the steward flow,
  first lane merge through the steward.
  MERGED 2026-10-05 at `4793bca` (review "Yes" after one fix pass; 9 of 9 mirror mutants killed).
  Live: `tools/mirror.sh` created GitHub `artifacts-trunk` at `6f78128`; coordinator `66fbe44a`
  accepts `orchestrator` as reviewer; stray `tessel` repo deleted.
  CLOSED 2026-10-05 with CLI-BASE (trunk `0118731`), the first lane merge through the steward.
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
- [x] **SHADOW-1** — PLAN §8 item 2, §9 Oct 9: when a blocking claim's work merges, the
  coordinator has the steward trial each shadow submission it blocked against the pre- and
  post-merge trunk (the ASSUME-1 primitive) and appends `DenialVerified`; a red baseline or a
  trial without a result counts nothing (`Inconclusive`).
  CLOSED 2026-10-05 at trunk `720094e` (review "Yes" after one fix pass). Merged through the
  steward: claim 38, held for three signature changes, approved by `orchestrator` (event 218)
  after its gate on `5497ebe` (the reviewed `401d8d0` merged with the trunk; tree checked equal to
  a clean merge): 767 passed. Submitted touched = the four coordinator files. Deployed: swarm
  `9ce71fc0`, production `3c2e946f`; both load state saved before the change (production replays
  223 events, head `720094e`). Not exercised live: a `DenialVerified` (shadows are off on both
  Workers; SHADOW-2 turns them on for swarm).
- [x] **CLI-BASE** — found landing SHADOW-1: `tessel submit` diffed from the coordinator's head
  `36abdb3`, which exists locally (shared object store) but is not an ancestor of the work, and
  listed 244 unrelated scopes. `diff_base` now takes the coordinator's head when it is in the
  work's history and not older than the start commit, else its fork point with the work when that
  is at or after the start commit, else the start commit, else fails.
  CLOSED 2026-10-05 at trunk `0118731`: THE FIRST LANE CHANGE MERGED THROUGH THE STEWARD. Review
  "Yes" after two fix passes (a fork-point regression and a stale same-line head, both found by the
  reviewer's probes; no fail-open case). Workspace tests 751, re-run by the orchestrator on an
  archived copy. Live: claims through Tessel (claim 37; one `uncovered` refusal for missing
  `create` modes, then amended), push to fork `tessel-dogfood--lane-cli-base`, `tessel submit`
  sent exactly the 4 files against the stale head, held for review (signature change), approved by
  `orchestrator` (event 201, note with the Opus verdict and gate), merged by the steward, trunk
  fast-forwarded `8432f8c..0118731`, coordinator head now `0118731`, mirrored to `artifacts-trunk`.
- [x] **SHADOW-2** — PLAN §8, §9 Oct 9–10: `SHADOW_ENABLED = "true"` for `[env.swarm]` only, and a
  shadow policy in `tessel-swarm` (a denied scripted agent claims with `OnConflict::Shadow`, keeps
  working, submits), so the A/B table's "Conflicts prevented, verified by shadow runs" cell comes
  from `DenialVerified` events. (The first live run's 120 s timeout is a run setting,
  `--task-timeout`, to raise for the Oct 10 runs, not code.)
  Review 1 (Opus, "With fixes"): later shadow work force-pushed over earlier shadow commits (3 real
  conflicts → 1 prevention, 2 Inconclusive); skipped `work_ms` understated agent-minutes; red
  local baseline untested; `wait`/`skip` tables changed. Review 2: a second `Watch` on one socket
  is refused by the runtime (local server didn't model it). Review 3: per-wait polling loaded the
  measured coordinator and padded `event_count`; now one shared observer per run.
  CLOSED 2026-10-05 at trunk `78e9f2a` (review "Yes" after three fix passes). Through the
  steward: claim 43 (one `uncovered` refusal, amended), held for review, approved by
  `orchestrator` (event 263) after its gate on `2d9d257` (the reviewed `b179783` merged with trunk
  `28e246b`; tree checked equal to a clean merge): 790 passed. Swarm Worker redeployed with
  `SHADOW_ENABLED = "true"` (`f6d0e4b1`). Follow-ups: SWARM-OBSERVER; matching a shadow's blocker
  by `conflict.held` scope (only over-waits today; the swarm opens no races).
- [x] **DASH-1** — PLAN §4, §9 Oct 7 (agent column, never cut per §11; judging: "live dashboard
  with stats"): a read-only live dashboard served by the steward. It opens a `Watch` on the
  coordinator for one repo and shows active claims and their holders, denials with the holder's
  intent (quoted as untrusted data: escaped, never rendered as HTML), waits, races, merges, review
  holds, verified denials, and the headline numbers from `Summary::from_events` (the same function
  as the A/B table; a denial is never shown as a prevention). Sign-in: Cloudflare Access (decided).
  Video segment 1:15–3:00 runs on it (PLAN §10).
  MERGED in two lanes. DASH-1a (coordinator `GET /repo/<name>/summary`, paged fold with a cached
  tally, no event appended): review "Yes" after one fix pass plus one test assert read by the
  orchestrator; claim 54 approved (event 320) after the gate on `ead276f` (817); deployed to both
  coordinators and checked live. DASH-1b (steward page, SSE relay of `Watch` with no `hello`,
  30 s keepalive, bounded queue, Access JWT check failing closed, strict CSP, textContent only):
  review "Yes" after one fix pass; its last commit `715d8dd` (served-CSP test, refetch limit by
  attempt time) merged unflagged before the orchestrator read it, read right after: sound
  (BUILD-PROTOCOL rule 00 added). Trunk `d4e6335`.
  LIVE Oct 6 18:41 EDT: steward `b4a66e0f` behind Cloudflare Access; Felix signed in and the page
  showed the coordinator's own numbers.
  CLOSED 2026-10-06: ACCESS-VARS merged through the steward (claim 55, trunk `e894fbe`, diff read
  by the orchestrator under rule 00); steward redeployed from the trunk alone (`fce132ca`):
  unsigned → 302 to the fepdev login, admin route → 401.
- [ ] **REVIEW-UI** — PLAN §4, §9 Oct 8: a review screen beside the dashboard for submissions held
  under invariant 12: the reasons from `review_reasons`, the diff, the claim's intent and
  assumptions (untrusted data), and approve/reject that sends `Review` as a configured reviewer.
  `tessel review` stays the agent path (same outcome, agent-native). Needs a decision before
  building: how a human in a browser acts as a reviewer agent (a reviewer identity token, minted
  by the steward, held server side). After DASH-1.
- [ ] **DEMO-TS** — PLAN §9 Oct 7, SUBMISSION_CHECKLIST ("demo repository the agents work on is
  your own or permissively licensed"). Mostly exists: `tessel-swarm/src/demo.rs` generates a small
  TypeScript shop (twelve functions in seven modules, one `node --test` file each, cross-module
  calls) that every swarm run starts from. Left: a way to create that repo as a named Artifacts
  repo for real agents (the video's 3–5 real agents beside the scripted ones, PLAN §9 Oct 10),
  e.g. a `tessel-swarm demo-repo --repo swarm-demo` command, a LICENSE in it, and a check that the
  CLI's tree-sitter labels its symbols as expected. No second demo repo.
- [x] **SWARM-OBSERVER** — from the SHADOW-2 review (both could only lower the reported count):
  a failed shared log watcher's error was dropped at the end of a run, and the final wait trusted
  any non-empty log. Now the watcher is aborted then awaited, its error leads (the agent's or the
  final wait's kept as context), and the final wait needs every accepted shadow's `Submitted`.
  CLOSED 2026-10-05 at trunk `c79712a` (review "Yes" after one fix pass, then a tests-only
  follow-up read by the orchestrator: a deterministic shadow in the watcher-failure test). Through
  the steward: claim 44 (late claim on `on.rs` in the first commit; one `uncovered` refusal),
  approved by `orchestrator` (event 283) after its gate on `c79712a`: 802 passed.
- [x] **LINT-RULE2** — from the SHADOW-GC pass 2 review: CLAUDE.md rule 2 (no `_` wildcards on
  protocol enums) is enforced by review only; a `_ =>` arm on `ReleaseReason` passed clippy.
  Turn on clippy's `wildcard_enum_match_arm` (restriction group) for the workspace crates, with
  `#[expect(..., reason)]` only where a wildcard on a non-protocol enum is deliberate.
  CLOSED 2026-10-06 at trunk `7d7f4c9` with CLI-CLIPPY-810 (one lane; review "With fixes" twice:
  the root crate also needed `match_wildcard_for_single_variants`, since a one-variant `_` in
  `src/` passed; then a long line). 23 sites fixed, no `#[expect]`; one-variant extractors in test
  helpers became `let … else`. Mutants: one- and two-variant wildcards fail clippy in the root
  crate, tessel-cli and tessel-swarm. Workspace tests 836 (= base). Through the steward: claim 57,
  not held, merged on the first submit; trunk tree equals the reviewed tree. Not covered by any
  lint: wildcards inside `matches!` and slice patterns (test assertions only today).
- [x] **CLI-CLIPPY-810** — `cargo clippy --workspace --all-targets -- -D warnings` fails on a
  pre-existing `eprintln!` (`print_stderr`) at `tessel-cli/tests/cli.rs:810`; every review today
  had to except it. Replace it with the test harness's own reporting or an `#[expect]` with a
  reason, so the workspace clippy gate is clean again.
  CLOSED 2026-10-06 at trunk `7d7f4c9` (with LINT-RULE2): the message stays, under
  `#[expect(clippy::print_stderr, reason)]` on a `let () = eprintln!(…)` statement, the narrowest
  placement clippy honours. Workspace clippy `-D warnings` exits 0.
- [ ] **SWARM-REVIEWER-ERR** — from the SWARM-OBSERVER re-check: in `tessel-swarm/src/on.rs`
  (~227) the scripted reviewer's `??` returns before `with_watcher_error`, so a reviewer error
  hides a watcher error. Scripted reviewer only; low priority.
- [x] **COORD-HEAD** — the coordinator's head moves only on merges it dispatched, so admin merges
  (DOGFOOD-3, the `8432f8c` catch-up) leave it stale and every welcome reports an old head. Options:
  the admin merge route tells the coordinator, or the coordinator adopts the steward's reported
  trunk on every merge outcome. Design it before the next admin merge.
  DESIGNED 2026-10-06 (Opus, read-only; orchestrator chose C). Why it matters beyond welcome: race
  trials use `state.head` as both `before` and `main` (`racing.rs` ~563), so after an admin merge
  they rank entries against a trunk that no longer exists; and no `BaseMoved` reaches agents whose
  claims overlap an admin-merged change. Merges themselves are safe (the steward rebases on the real
  main). C: the steward's queue consumer (today it only logs), on a push to `refs/heads/main` of a
  non-fork repo, pokes a new coordinator route `/repo/<r>/trunk-moved` over the `COORDINATOR`
  binding. The body is never trusted: the coordinator sets a persisted `#[serde(default)]
  head_sync_due` flag (core state, not protocol) and an alarm; the alarm reads the trunk head from
  a new steward `/head` path over the binding-only merge service; the head moves only when no merge
  is in flight and `state.head` is unchanged since the read began (compare-and-set); same, empty or
  all-zero heads change nothing. Logged as the existing `BaseMoved{head, by: "steward", notified:
  []}`. No `src/protocol.rs` change. Lane owns `src/coordinator.rs`, `src/shell.rs`,
  `src/runtime.rs`, `tessel-steward/src/index.ts`, `tessel-steward/src/merge-service.ts`. Tests
  first: idle read moves head and logs one `BaseMoved`; in-flight merge, moved-meanwhile, same and
  all-zero reads change nothing; next welcome carries the new head; route auth; vitest for the
  queue poke (non-fork main only) and `/head`. Live: after the next trunk push, `welcome` carries
  the trunk sha and the dashboard feed shows `BaseMoved` by steward.
  MERGED 2026-10-06 at trunk `3c50a5e` (review "With fixes": a poke during a read was lost, and
  every steward push would have created production Durable Objects for scratch repos; fixed with
  poke counters `head_pokes`/`head_synced`/`head_tried`, a known-repo check that answers 204 with
  no write, a steward-only route (`Denied::NotSteward`), and a failed read kept pending without
  waking by itself; re-check "Yes", one survivor killed by an added assertion). Held for review
  (signatures of `CoordinatorState`, `Denied`, `Route`, `forkHead`); orchestrator gate on
  `3c50a5e`: cargo test 859, clippy 0, pnpm test 666, typecheck/oxlint/oxfmt 0, wasm 0 warnings;
  approved claim 60; trunk tree equals the gated tree. Each steward merge now costs one extra poke
  and a no-op `/head` read. CLOSED live 21:58 EDT on steward `31b4be89`, coordinator `8d66fe32`
  (swarm `7b7ae508`), deployed by Felix: on `demo` a stale welcome head (`000…001`) became the
  trunk's `58119c2` within 5 s of a steward poke, logged as `base_moved` by `steward`; no token
  401, non-steward 403, GET 405, unknown repo 204. Evidence `docs/evidence/2026-10-06/
  coord-head-live/`. The queue consumer's own poke is not yet seen live (next trunk push).
- [x] **CLI-LEASE-LOAD** — found by the COORD-HEAD lane at load 20–90: claims 58 and 59 got
  `lease_expired` ("no heartbeat reached the coordinator") about 180 s after the grant while the
  daemon (one pid throughout) reported `connection: online`; later a reconcile said the
  coordinator still held 58 after the daemon had dropped it. Daemon logs were not kept, so sent-
  and-lost and never-sent are not told apart. Reproduce under load (e.g. `nice`d CPU hog), log
  heartbeat send and pong times, then fix. Not the same as CLI-LIVENESS (half-open link).
  CLOSED 2026-10-06 at trunk `4b394c9` (review "With fixes", then "Yes"). The defect: while online,
  housekeeping dropped a claim on its local expiry and told nobody; heartbeats renew all of an
  agent's claims, so the coordinator kept holding it (blocking others) while `submit` said "no
  claim is held", until a reconnect reconciled. Now, online, only the coordinator's
  `LeaseExpired` drops a claim; offline the local lapse applies as before; a failed send to a dead
  socket task runs the close path. The likely stall source, synchronous `git` on the
  single-threaded runtime (HEAD on connect, `is_ancestor` per landed commit), now runs on blocking
  threads. The stall itself was inferred, never seen live: `daemon.log` now records each local
  lapse (expiry, now, last pong, tick lateness) and each slow pong. Held for review (five private
  `Daemon` methods became async); orchestrator gate on `4b394c9`: workspace 867, clippy 0;
  approved claim 63; trunk tree equals the gated tree. No Worker deploy (CLI only).
- [x] **CLI-LIVENESS** — found while dogfooding SHADOW-1: claim 35 expired while the lane's daemon
  was running. Heartbeats get no reply; the daemon moved the local expiry when a heartbeat was
  queued and never pinged, so a half-open link heartbeat into nothing. Now: a ping carrying the
  heartbeat's send time on each tick; any inbound frame clears the silence deadline (lease/2,
  else dead link → reconnect); the local expiry moves only on a pong echoing an outstanding send
  time, anchored at the send time (never later than the coordinator's).
  CLOSED 2026-10-05 at trunk `28e246b` (review "Yes" after one fix pass, which closed a fail-open
  of up to lease/3: any server frame had counted as proof). Through the steward: claim 42 (one
  `uncovered` refusal for missing `edit-signature`, then amended), held for review, approved by
  `orchestrator` (event 249) after its gate on an archived `28e246b`: 772 passed. Live: the lane's
  daemon on the new binary stayed online and renewed against the deployed coordinator, so the
  deployed side echoes ping payloads. The daemon logs for claim 35 were gone (worktree removed).
- [ ] **CLI-CONNECT-TIMEOUT** — from the CLI-LIVENESS review: heartbeats run only once the daemon
  is online, so a link that goes half-open after the WebSocket handshake but before `Welcome` sits
  in Connecting with no timeout (fails closed: claims lapse locally). Bound the wait for `Welcome`
  and reconnect with the existing backoff. Also (CLI-LIVENESS re-review): a link that delivers
  server frames but never gets heartbeats through is never declared dead while frames keep coming;
  claims lapse locally (no fail-open), but it never reconnects. Set the silence deadline from the
  oldest outstanding ping and clear it only on a matching pong; add a test (mutant L survives).
- [x] **SHADOW-GC** — from the SHADOW-1 review: submitted shadow claims are never removed (true
  before SHADOW-1). They hold no locks, leases or queue positions, but state grows by one claim per
  shadow submit in experiment runs, and a shadow blocked only by a race can never be verified.
  Drop a submitted shadow claim once none of its `blocked_by` claims is live and none of its trials
  is queued. Before the swarm runs that use shadows at scale.
  CLOSED 2026-10-06 at trunk `7d00ad6` by Felix's admin merge (it changes `src/protocol.rs`).
  Review 1 "With fixes" (invariant 10: a drop logged no event) → Felix ruled
  `ReleaseReason::Settled` additive; review 2 "With fixes" (CLAUDE.md wording narrowed to the one
  ruling; a redundant clock advance). Each drop now logs `ClaimReleased { reason: Settled }`;
  `ReviewDecided` carries the reviewing agent; stale dead-code expectations removed. Orchestrator
  gate on `b143194`: 836 Rust, 649 steward. Admin merge `merged` in 140 s with the trunk gate.
  Deployed: production `923f39fb`, swarm `4e65697d`, steward `b9f110df`; both coordinators load
  their stored logs. First commit `cd83a74` had no claims (late claims); every later edit was
  claimed first. Not exercised live yet: a `Settled` event (needs a shadow swarm run).
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

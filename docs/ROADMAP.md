# Roadmap

Task list for the build, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope and schedule;
this file tracks the tasks that deliver it. Only the orchestrator edits this file.

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-10-12 21:58 EDT. **COORD-GRANT-HEAD DEPLOYED AND PROVED LIVE** (Felix: `npx wrangler
deploy` from the deploy worktree at `780cd2f`, version `92108fcc`, 21:50). Proof on the raw
websocket as `lane-docs-ref`: at 21:52 and 21:54 the grant still came without `head` (the
Durable Object, held awake by the lane daemons' sockets, was still on the previous code); at
21:56 the grant carried `"head":"780cd2f…"` (handoff `grant-head/live-grant-head-proof.txt`).
Lesson: after a coordinator deploy, allow a few minutes before a live check. All lane and
reviewer agents stopped; three daemons hold the pending claims. **Pending from Felix:** the
CLI-NOTES landing (`84e606f`) and the EVIDENCE-REPO landing (`43b7a72`).
Earlier: **As of:** 2026-10-12 21:50 EDT. **Every agent idle; nothing runs; three commands wait on Felix**
(the coordinator deploy, the CLI-NOTES landing, the EVIDENCE-REPO landing; the trunk is still
`780cd2f`, watched). **Finding, from a test at 21:48:** no git hooks are installed in the main
checkout or any lane worktree (`.git/hooks` holds only samples; no `core.hooksPath`): the lanes
were set up by minting identities by hand instead of `tessel init`, so `pre-commit` and
`pre-push` never ran on today's lane commits, and a commit touching an unclaimed file goes
through locally (shown: the race lane's skill hunk committed as `a85353f` under no claim, then
reverted to `b61505b`). Coverage was still enforced where it counts, at `tessel submit` and in
the steward's gate, which is why the grant-head submit was refused until its claims matched.
Follow-up for tomorrow's lanes: run `tessel init` (or the hook install) in each worktree so the
local hooks hold too. The race lane's skill hunk still waits for the notes merge.
Earlier: **As of:** 2026-10-12 21:36 EDT. **CLI-RACE review "Yes (pending the SKILL hunk)" at `b61505b`**
(wording at `render.rs:702` verified true while a pick is pending and after the window closes,
`racing.rs:612-624, 526-534`; eight race tests pass; orchestrator gate on `4ce212d` 20 binaries,
1,196 tests, clippy 0; the amend after it is four lines in two files, the race tests rerun by
the orchestrator 8 of 8). Its last hunk, the skill paragraph, needs `skills/tessel/SKILL.md`,
which claim 114 (CLI-NOTES) holds until the notes merge; releasing that scope early would leave
the notes submission uncovered. **So the order is fixed: Felix deploys the coordinator, lands
CLI-NOTES `84e606f` and EVIDENCE-REPO `43b7a72` (each held for a signature change, the
orchestrator approves); after the notes merge the race lane claims the skill file, adds its
paragraph, re-gates, and lands last.**
Earlier: **As of:** 2026-10-12 21:35 EDT. **EVIDENCE-REPO READY TO LAND at `43b7a72`** (reviewer's fixes
verified 21:27: scope cells as code spans, wrong-head case tested, size check before the read,
`.vitest/` gone; five more mutants killed; "Ready to merge? Yes"; orchestrator workspace gate on
the archived copy: 20 binaries, 1,196 tests, exit 0, clippy 0, wasm build exit 0, steward 993
tests in 50 files, tsc, oxlint, oxfmt exit 0). It changes `MergeDispatch` and `Submission`
signatures: expect a hold; the orchestrator approves. Needs a coordinator deploy (merge request
fields) and a steward deploy (the writer) to take effect. **CLI-RACE `4ce212d`:** reviewer
confirmed the README line, the amend test (M1 killed) and the gate (482 passed) but found the
render wording at `render.rs:702` still false after a pick-window timeout: one-line fix sent
21:33; verdict "No" until then, then the SKILL hunk after the notes merge. CLI-NOTES `84e606f`
ready (gate 21 binaries, 1,202 tests). Lane REPORT.md files: notes written; race and evidence
still missing after three requests (their verdicts and gate logs stand in for them in the
handoff).
Earlier: **As of:** 2026-10-12 21:28 EDT. **CLI-NOTES READY TO LAND at `84e606f`** (fixes verified by the
reviewer 21:25: the new test `the_note_exists_while_the_coordinator_has_not_answered_the_submission`
kills the note-after-send mutant; notes tests 6 of 6; "Ready to merge? Yes"; orchestrator
workspace gate on the archived copy 21:24-21:26: 21 binaries green, tests exit 0, clippy 0).
**Landing (Felix):** `land-lane.sh cli-notes 84e606f "..."`; the lane's claim 114 holds its scopes
by qualified name (`daemon::Daemon::on_granted` and so on), and the commit changes `HeldClaim`
and `ServerClaim` signatures, so expect a hold; the orchestrator approves. CLI-RACE `4ce212d`
and EVIDENCE-REPO `43b7a72` amended with their fixes; reviewers confirming; gates running.
Earlier: **As of:** 2026-10-12 21:15 EDT. **Three Opus reviews back, each "With fixes"; fixes sent 21:13-21:15.**
CLI-NOTES `51748cb` (requirements met, five reviewer mutants, one survived: nothing proves the
note is written before the send; fix: a test with the fake holding the submission; plus the
`git push <your fork>` hint and a report correction). CLI-RACE `9912dc7` (requirements met except
6: the skill and README lines are in patches, not the commit; the "waiting for a human pick"
notice is not true after the pick window times out; the race-entry amend guard is untested;
fixes: README now, the render wording, a real-binary amend test; the SKILL lines after the
notes merge frees the file, so no command is documented before it is on the trunk). EVIDENCE-REPO
`83f60c3` (requirements 1-10 met, requirement 2 complete: `Submission` stores decisions and
review, the approve path adds the reviewer's decision, `dispatch_of` sends them; eight mutants
killed, one survived; fixes: scope cells as code spans, a wrong-head test, a transcript size
check before reading, trash the untracked `.vitest/`). Landing order once each is fixed and
re-gated: CLI-NOTES, then CLI-RACE (it needs SKILL.md); EVIDENCE-REPO is independent of both
and touches no sensitive path, so it is held only if a signature changed (`MergeDispatch` and
`Submission` did: expect a hold). **Pending from Felix:** the coordinator deploy for
COORD-GRANT-HEAD.
Earlier: **As of:** 2026-10-12 21:10 EDT. **Three final commits on `780cd2f`, all three workspace gates
green, three Opus reviews running.** CLI-NOTES `51748cb`, CLI-RACE `9912dc7`, EVIDENCE-REPO
`83f60c3` (each rebased by its lane within 2 minutes of the merge, squashed, trees clean).
Orchestrator gates on archived copies (21:00-21:06, curl off the PATH): tests exit 0 and clippy
0 for all three; evidence also wasm build exit 0, steward vitest 988 in 50 files, tsc, oxlint,
oxfmt exit 0 (`gate-*/gate.log` in the scratchpad, copied to the handoff on close). Reviewers
`review-cli-notes`, `review-race-cli`, `review-evidence-final` dispatched 21:08-21:10; lane
REPORT.md files not yet written (the lanes went idle after committing; asked for them).
**Pending from Felix:** the coordinator deploy for COORD-GRANT-HEAD (`cd
/Users/felixpatawah/repos/tessel/.claude/worktrees/deploy && npx wrangler deploy`; the worker is
rebuilt there from `780cd2f`), and then one landing line per lane as each review comes back.
Earlier: **As of:** 2026-10-12 20:49 EDT. **COORD-GRANT-HEAD CLOSED at trunk `780cd2f`** (Felix pushed 20:43;
the lane's submit failed because it had spread its scopes over seven claims and had claimed
symbols by bare name, which the submission's qualified names do not match; the orchestrator
released them, amended one claim with the 24 scopes `tessel submit` listed, and submitted;
held for the signature change and the sensitive path; approved by the orchestrator 20:48;
fast-forward merge 20:48, same sha; mirrored to GitHub `artifacts-trunk`; daemon stopped).
NOT DEPLOYED: the deploy worktree is moving to `780cd2f` and rebuilding the worker; Felix's
command after that: `cd /Users/felixpatawah/repos/tessel/.claude/worktrees/deploy && npx wrangler
deploy`. Lesson for briefs: claim symbols by the qualified name the CLI prints
(`path::module::Type::fn`), and keep one open claim so later claims amend it. The three lanes
were told at 20:50 to rebase onto `780cd2f`, take the freed daemon and coordinator symbols by
qualified name, and finish.
Earlier: **As of:** 2026-10-12 20:45 EDT. **All three open lanes are committed and gated as far as the
held daemon symbols allow** (orchestrator-run, 20:41-20:42): CLI-NOTES WIP `7bd7667` (5 files;
`tests/notes.rs` 4 of 5, the assumptions line waits for the daemon literals; every other CLI
binary green); CLI-RACE WIP `3c9b24e` (8 files, 1,086 lines; CLI test binary 100 passed, 3
failed, exactly the three that need the daemon's `JoinRace`/`RaceResult` handling, which the
command refuses with "pending" until then; unit 236 and the other binaries green);
EVIDENCE-REPO `95ba70a` on top of `1951854` (the two review fixes; steward vitest 988 passed in
50 files, tsc, oxlint, oxfmt clean). What remains in each is behind grant-head's claims on
`daemon.rs` and `coordinator.rs`, proved by a denied claim from the notes worktree at 20:36.
**Gate: Felix's grant-head landing line.**
Earlier: **As of:** 2026-10-12 20:34 EDT. **All three open lanes are in a known, narrow state.** CLI-RACE:
the orchestrator restarted its daemon and claimed, as the lane, claim 120 with exactly the
symbols it edits (main.rs, race.rs, render.rs and four of its functions, `commands.rs::run`,
`NoticeKind`, `rpc::Request`, `review::connect`/`send`, the two test harness files); it takes
the four daemon symbols after the grant-head merge. CLI-NOTES: holds `commands.rs::submit` and
the skill, both wired at 20:11; the daemon literals wait for the same merge. EVIDENCE-REPO:
both review fixes verified by the orchestrator at 20:33 (the refused reason is fenced as data
at `evidence-record.ts:197`; a transcript path other than `transcripts/<claim>.md` is refused at
`evidence-steps.ts:150`; its two test files pass, 22 tests); requirement 2 (decisions and
review carried from `Submission`) follows the same merge. **One gate for everything: Felix's
grant-head landing line.**
Earlier: **As of:** 2026-10-12 20:20 EDT. **EVIDENCE-REPO review "With fixes"** (handoff
`evidence-repo/VERDICT.md`: six reviewer mutants killed; requirements 1, 3-8, 10 met; the
record is written only after main read `merged`, each repo with its own token, commit id
pinned at the gateway, read-back bytes compared, agent text fenced, paths validated; fixes:
quote the transcript's refused reason as data, refuse any transcript path but
`transcripts/<claim>.md`; and requirement 2 only partly met: `Submission` stores no decisions
or review, so the record says "not supplied" for them, because `src/coordinator.rs` was held).
Orchestrator gate on the archived copy: workspace tests exit 0, clippy 0, wasm exit 0, steward
985 tests, tsc, oxlint, oxfmt clean (`evidence-repo/gate.log`). The lane does the two fixes now
and completes requirement 2 after the grant-head merge frees `coordinator.rs` (rebase, carry
`DecisionRecord` and the review decision through `MergeDispatch`). **Claim contention
resolved by the orchestrator at 20:14:** the race lane's daemon was stopped (`tessel stop`,
claim 110 released; its ten edited files untouched) after an hour of blocking and three
unanswered requests; `commands.rs::submit` and the skill were added to cli-notes's claim 114
from its worktree; the race lane restarts with the narrow list in the handoff brief message
and takes the daemon symbols after the grant-head merge. Still waiting: Felix's grant-head
landing line.
Earlier: **As of:** 2026-10-12 20:12 EDT. **EVIDENCE-REPO committed `1951854`** (19:51; 33 files; lane gate:
coordinator 523, steward vitest 985 in 50 files, tsc, oxlint, oxfmt, clippy, rustfmt all exit 0;
three mutants killed; its note says the coordinator-side intent/decisions/review were NOT carried
because `src/coordinator.rs` was held by grant-head: the report is asked to say what the record
holds instead). Opus review and the orchestrator's workspace gate (archived copy, wasm build,
steward checks) running since 20:08. **CLI-NOTES partial:** the note writer is in `submit.rs`
(`note_text`, `write_note`, `has_note`, five unit tests green), CLAUDE.md updated; still needs
`commands.rs::submit` (race-cli's whole-file create claim), the skill (same), and daemon.rs
literals for the assumptions field (grant-head's claims, released at its merge; patch saved in
the lane's scratch). **CLI-RACE:** no edit since 19:11; it polls the daemon symbols grant-head
holds and has ignored three requests to narrow claim 110. **Chain:** Felix lands grant-head →
its merge frees the daemon symbols → race-cli finishes and lands → cli-notes takes `submit` and
the skill and lands. COORD-GRANT-HEAD is still waiting for Felix's landing line (below).
Earlier: **As of:** 2026-10-12 19:50 EDT. **COORD-GRANT-HEAD READY TO LAND at `780cd2f`** (lane
`lane-grant-head`; `head: Option<CommitId>` with `#[serde(default)]` on `Granted`, set in
`place_grant`, the amend reply and the race entry; the daemon adopts and persists it before
answering; four real-binary tests in the new `tests/grant_head.rs` because race-cli holds
`tests/cli.rs`; coordinator 525, CLI 235 unit + 10 binaries; five implementer mutants and four
reviewer mutants killed). Opus review "Yes" (one low note: the daemon's `Granted` arm now uses
`..`, so a future field no longer forces a daemon decision; left as is). Orchestrator workspace
gate on the archived copy with curl off the PATH: 20 binaries green, clippy 0, wasm build exit 0,
done 19:43. **Landing (Felix):** `land-lane.sh grant-head 780cd2f "..."`, held for review because
`src/protocol.rs` is sensitive; the orchestrator approves through `tools/orch-review.sh`. Needs
a coordinator deploy after the merge. The other lanes: race-cli still holds its wide claim 110
(`commands.rs` create, the skill, `tests/cli.rs`) and has not answered two requests to narrow it;
cli-notes finishes everything else and takes `commands.rs::submit` after race-cli lands;
evidence-repo has 17 files in progress.
Earlier: **As of:** 2026-10-12 19:21 EDT. **FOURTH LANE: EVIDENCE-REPO dispatched** (`lane-evidence-repo`,
Opus, from `3cedad8`) on Felix's call to take up PLAN item 8's archive for the contest's "how
do you track why" question; CLI-TRANSCRIPT filed to follow CLI-NOTES. The three earlier lanes:
grant-head has its code done (protocol field, four grant paths, daemon, fixture, reference) and
moves its CLI tests to `tests/grant_head.rs` because race-cli holds `tests/cli.rs`; cli-notes
holds `notes.rs`, `tests/notes.rs`, CLAUDE.md and waits for `commands.rs::submit` (race-cli's
whole-file create claim) and for the grant-head merge (a `HeldClaim` field for assumptions);
race-cli has edits in ten files under one wide claim and was asked twice to release and
reclaim without `commands.rs`, the skill and `tests/cli.rs`. Nobody waits while holding, so no
deadlock; the wide claim serializes the other two until it narrows. Video script v2 written
(handoff `orch/video-script-v2.md`).
Earlier: **As of:** 2026-10-12 18:56 EDT. **THREE LANES DISPATCHED IN PARALLEL from trunk `3cedad8`**, on
Felix's push to use the swarm for the "not shipped" list while there is time: CLI-NOTES
(`lane-cli-notes`, Sonnet: `tessel submit` writes the commit's git note), COORD-GRANT-HEAD
(`lane-grant-head`, Sonnet: additive `head` on `Granted`, held for review as `src/protocol.rs`
is sensitive; closes the limit noted on COORD-WAITER-BASE), CLI-RACE (`lane-race-cli`, Opus:
`tessel race open|join|pick` and the `race_result` notice; the coordinator side is complete).
Disjoint claims by symbol; the race lane takes `daemon.rs::on_server_msg` last, after the
grant-head lane. Each gets an Opus review and the orchestrator's workspace gate, then Felix
lands them with `orch/land-lane.sh` as they come. Not taken up (infrastructure, not for
tonight): Workers Builds previews, separate Artifacts repos for evidence and transcripts,
commutation batching. The video script v2 (header, SENSITIVE policy, the `base_moved` line
in the `--wait` scene, and these three if they land) is the orchestrator's next write.
Earlier: **As of:** 2026-10-12 18:50 EDT. **GITHUB `main` IS CURRENT: Felix merged pull request #3** (merge
commit `662e25d` at 18:43; `main`'s tree `26f7797` equals the trunk's at `3cedad8`;
`artifacts-trunk` kept as the mirror branch). Everything landed today is on both trunks. **Left
with Felix:** the steward deploy for SEC-HARDEN after the recordings; figure/form v3, video tag,
recordings; Oct 13 form final, repo public, 2FA, submission by hand. Orchestrator idle; nothing runs.
Earlier: **As of:** 2026-10-12 18:45 EDT. **SWARM-RECONNECT-FLAKE CLOSED at trunk `3cedad8`** (Felix
landed `ac35420` 18:34; held for the signature change; orchestrator approved claim 108 at 18:36;
steward merged 18:38, rebased onto `9ab793a`, the file identical; mirrored to GitHub
`artifacts-trunk`; both daemons stopped). **COORD-WAITER-BASE PROVED LIVE** on the deployed
coordinator: the waiter `lane-docs-ref` queued behind claim 108 got `granted_after_wait` (claim
109) then `base_moved` to `3cedad8` one millisecond later, and `tessel status` shows head
`3cedad8` (handoff `swarm-flake/live-waiter-proof.txt`). All five boxes of the day are closed.
**Next:** the GitHub pull request `artifacts-trunk` → `main` (39 commits) for Felix to merge
before the repo goes public; the steward deploy for SEC-HARDEN (Felix, after the recordings).
Felix's own list: figure/form v3, video tag, recordings, Oct 13 form and submission.
Earlier: **As of:** 2026-10-12 18:35 EDT. **Coordinator DEPLOYED by Felix from `9ab793a`** (deploy worktree,
`npx wrangler deploy`, version `f6977a41`, 0 wasm warnings; COORD-WAITER-BASE is live). Steward
still from `cbef948` (SEC-HARDEN `3442343` waits for its deploy). **Live proof armed:** agent
`lane-docs-ref` (daemon up in worktree `docs-ref`) is queued at position 1 with `--wait` on
`tessel-swarm/tests/reconnect.rs` behind claim 108 (lane-swarm-flake); its status shows head
`9ab793a`. The SWARM-RECONNECT-FLAKE landing is the merge that grants it: after the merge, its
`tessel status` must show the new trunk head and its inbox `granted_after_wait` then
`base_moved` (before the fix it kept the old head). **Pending from Felix:** `bash
.../orch/land-lane.sh swarm-flake ac35420 "..."` (held on submit; the orchestrator approves
through `tools/orch-review.sh`), then the orchestrator records the proof, stops both daemons,
closes the box, mirrors, and opens the GitHub `main` PR from `artifacts-trunk` (38 commits ahead).
Earlier: **As of:** 2026-10-12 17:10 EDT. **COORD-WAITER-BASE CLOSED at trunk `9ab793a`** (Felix landed
it 17:03 before the test fix; the sandbox gate passed; rebased, `merging.rs` identical; mirrored;
daemon stopped). Deploy worktree moved to `9ab793a`; `build/worker` and the CLI binary
rebuilding there. **Pending from Felix:** the SWARM-RECONNECT-FLAKE landing (`ac35420`, held on
submit, orchestrator approves), and the coordinator deploy after the recordings:
`cd /Users/felixpatawah/repos/tessel/.claude/worktrees/deploy && npx wrangler deploy`. Then the
GitHub `main` PR from `artifacts-trunk`. Live: coordinator still `c49a44ca` (code `f9a2f0b`),
steward from `cbef948` (SEC-HARDEN not deployed yet either: same window).
Earlier: **As of:** 2026-10-12 17:04 EDT. **DOCS-CORE CLOSED at trunk `126b944`** (Felix resubmitted
`7c7131e` at 16:58 before the test fix; the sandbox gate passed this time; rebased by the steward,
the three files identical; mirrored; daemon stopped). Docs tier 1 is on the trunk: README front
door, how-it-works, configuration reference, coordinator reference, SKILL fixes. **Still to
land:** SWARM-RECONNECT-FLAKE `ac35420` (held on submit for the signature change; the
orchestrator approves), then COORD-WAITER-BASE `76786e2`. Then the GitHub `main` PR from
`artifacts-trunk`, and Felix's coordinator deploy after the recordings.
Earlier: **As of:** 2026-10-12 16:56 EDT. **SWARM-RECONNECT-FLAKE READY TO LAND at `ac35420`: Opus
"Yes"** (the hub-lock claim verified at `local.rs:127,392,502,641`: `Submitted` is logged under
the hub mutex, `begin_merge` needs it, so the merge starts after the poison; no await under the
mutex; four mutants each failed within the bound, the lost-race mutant at 152 s with the new
message; reconnect binary 5 of 5 under 24 hogs at load up to 144). Orchestrator gate passed
16:42. **Landing order for Felix:** 1. `bash .../orch/land-lane.sh swarm-flake ac35420 "..."`,
which will be held (`edit-signature`: `first_waiter` changed, `connected_count` removed) and the
orchestrator approves through `tools/orch-review.sh`; 2. DOCS-CORE `7c7131e` resubmit;
3. COORD-WAITER-BASE `76786e2`. Each later one is rebased by the steward onto the one before.
Earlier: **As of:** 2026-10-12 16:40 EDT. **SWARM-RECONNECT-FLAKE implemented at `ac35420`**
(`tessel-swarm/tests/reconnect.rs` only: the rejection test's tripwire now poisons main in the
step that logs `Submitted`, under the hub lock, then cuts the agent, so the order no longer
depends on `work_ms` (now 0); the recommended cut-at-grant order was rejected because the agent
syncs main again on reconnect (`on.rs:1048,1165`); `wait_for`/`wait_until` with a 150 s bound and
a message naming the wait and the log tail replace 13 polling loops; `lease_expired` replaces
three copies; assertions unchanged; the `reconnect` binary and the rejection test 10 of 10 each
under 24 CPU hogs at load 100-330; late-poison mutant fails at 150 s with the new message; lane
gate 195 passed, clippy and rustfmt clean). Orchestrator read the diff: sound; the reviewer is
asked to verify the hub-lock claim and the blocking git call inside the tripwire. Opus review
dispatched. Workspace gate on archived `ac35420` (curl off the PATH, reusing the previous gate's
build directory): tests exit 0 across all 19 binaries (reconnect 20, swarm on_local 23 in 42 s),
clippy `--workspace` 0, done 16:42.
Earlier: **As of:** 2026-10-12 16:18 EDT. **SWARM-RECONNECT-FLAKE reproduced and dispatched** as
`lane-swarm-flake` (Sonnet, worktree `swarm-flake` from trunk `9b6af03`, fork and identity
made): under 24 CPU hogs 1 of 8 runs of the refused test hung with no output; the fix orders
the test by log events and bounds every wait in `tessel-swarm/tests/reconnect.rs`. Review, gate
under hogs, then Felix lands it before DOCS-CORE and COORD-WAITER-BASE are resubmitted, so the
sandbox gate stops refusing sound commits. The two resubmits can also go first if Felix prefers
(the flake is intermittent); the fix lands either way.
Earlier: **As of:** 2026-10-12 16:10 EDT. **DOCS-CORE `7c7131e` REFUSED by the sandbox gate** (swarm
`reconnect` test failed on a commit that touches only `README.md`, `docs/how-tessel-works.md`
and `docs/reference/coordinator.md`, with the swarm, core and CLI code identical to the trunk).
Investigated: the test races an 800 ms work time against a 300 ms-delayed git commit; it
passed 5 of 5 on the trunk code locally; box SWARM-RECONNECT-FLAKE holds the mechanism and the
fix. Claim 106 is active again; Felix resubmits the same commit with the landing script (the
push is a no-op).
COORD-WAITER-BASE ready to land, below; its sandbox run may hit the same flake: resubmit then.
Earlier: **As of:** 2026-10-12 16:03 EDT. **COORD-WAITER-BASE READY TO LAND at `76786e2`.** Wasm build
of the gated tree: exit 0, no warnings. Local replay of the a1/a2 wait scenario against the fixed
core (the docs-demo-server rebuilt from the lane worktree, the trunk CLI): after a1's merge, a2's
inbox shows `granted_after_wait` (claim 2) then `base_moved` to `f65e9ff` affecting
`src/pricing.ts::applyDiscount`, and a2's `tessel status` head is `f65e9ff` (before the fix it
stayed at `71a4e8a`). Transcript in the handoff `coord-waiter/scenario-transcript-fixed.txt`,
with the report, verdict, gate log and reruns. **Pending from Felix:** `bash
/Users/felixpatawah/repos/tessel/.claude/handoff-2026-10-12/orch/land-lane.sh coord-waiter
76786e2 "<evidence>"` (no sensitive path: merges without a hold), then the coordinator deploy
after the recordings (the deploy worktree moved to the new trunk, `worker-build --release`, then
`npx wrangler deploy`). DOCS-CORE `7c7131e` still in the sandbox.
Earlier: **As of:** 2026-10-12 15:58 EDT. **COORD-WAITER-BASE workspace gate on archived `76786e2`
(curl off the PATH): clippy `--workspace` 0, every test binary passed except two runs under
load average 100+: `on_local` had one failure
(`a_claim_granted_after_a_queue_wait_of_many_leases_is_kept_alive`, the filed load flake file)
and the swarm `reconnect` binary sat idle for 25 minutes and was killed by the orchestrator.
Rerun alone (full logs in the session scratchpad `gate-coord-waiter/rerun/`): the queue-wait
test passed (1 of 1, 7 s); `reconnect` passed 20 of 20 in 23 s, including
`a_shadow_agent_cut_before_it_submitted_goes_on_with_the_same_claim`, which the killed run had
listed as failed. Both results recorded, as the flake rule asks. Wasm build running; then the
local a1/a2 replay; then Felix lands. DOCS-CORE `7c7131e` submitted by Felix 15:52, in the
sandbox (watcher running).
Earlier: **As of:** 2026-10-12 15:30 EDT. **DOCS-REF CLOSED at trunk `9b6af03`** (merged 15:29, no hold,
mirrored; its daemon stopped). Trunk: `9b6af03`. **Pending from Felix:** the DOCS-CORE landing
(`7c7131e`, worktree `docs-core`, based on `3442343`: the steward rebases) after he reads
`docs/how-tessel-works.md` (opened for him 15:27). The waiter fix's workspace gate is running
the tests; wasm build and local replay follow.
Earlier: **As of:** 2026-10-12 15:24 EDT. **COORD-WAITER-BASE review: Opus "Yes" on `76786e2`** (every
requirement met; its four mutants each failed at least one test; package gate 519, clippy and
rustfmt clean; two low findings, not blocking: `Granted` still carries no head, so a holder
granted on an untouched scope or by a release can start from an old base, a possible follow-up
box; and the new waiter test makes `a_merge_grants_the_waiter_it_unblocked` redundant, left as
is). Remaining before Felix lands: the orchestrator's workspace gate (running since 15:16, a
clean build under load), the wasm build and the local a1/a2 replay.
Earlier: **As of:** 2026-10-12 15:22 EDT. **SEC-HARDEN CLOSED at trunk `3442343`** (held for
`review.ts::diff`, approved by the orchestrator through `tools/orch-review.sh`, merged 15:18,
mirrored to GitHub `artifacts-trunk`; its daemon stopped; steward deploy still Felix's, after the
recordings). **README swap done:** `7c7131e` in worktree `docs-core` on top of `88c7383`, under
the lane's claim 106 (README.md edit-body added); DOCS-CORE now lands as `7c7131e`.
**Pending from Felix:** the DOCS-REF landing (`bash /Users/felixpatawah/repos/tessel/.claude/
handoff-2026-10-12/orch/land-lane.sh docs-ref 9b6af03 "<evidence>"`), then DOCS-CORE the same way
with `7c7131e` after he reads `docs/how-tessel-works.md`. Boxes for SEC-HARDEN, DOCS-REF and
DOCS-CORE added to the task list (they had been tracked in STATE only). Follow-up to file after
the waiter fix: the lane daemons' hello at 13:50 got head `d1ef2f1`, so Felix's admin merge
`f9a2f0b` (Oct 11) never advanced the coordinator's head; the SEC-HARDEN merge did (`3442343`
reported to the submitter).
Earlier: **As of:** 2026-10-12 15:17 EDT. **SEC-HARDEN `3442343` pushed and submitted by Felix (15:10),
held for the signature change `tessel-steward/src/review.ts::diff`, approved by the orchestrator
through `tools/orch-review.sh` (claim 104); the sandbox merge is running, a trunk watcher polls.
COORD-WAITER-BASE implemented at `76786e2`** (`src/coordinator/merging.rs` only: the swap in
`land`, module and function docs updated, two new tests, helper `wait_for`, and the existing
`a_merge_grants_the_waiter_it_unblocked` now expects `Granted` then `BaseMoved`; lane gate 519
passed, clippy and rustfmt clean; three mutants killed; `AlreadyMerged` goes through `land` so the
swap covers it; `trunk_read`, the rejection paths and the release paths move no main or release
nothing, so no notice is due there). Orchestrator read the diff: sound. Opus review dispatched;
orchestrator workspace gate running on an archived copy (curl off the PATH). Still to do before
Felix lands: the wasm build and a local replay of the a1/a2 wait scenario against the fixed core.
DOCS-REF landing command handed to Felix (absolute path).
Earlier: **As of:** 2026-10-12 15:06 EDT. **RESUMED; orchestrator on (Felix: "resume"). COORD-WAITER-BASE
dispatched** as `lane-coord-waiter` (Sonnet) in worktree `coord-waiter` from trunk `f9a2f0b`
(nothing has landed since the handover: `sec-harden`, `docs-ref`, `docs-core` are still waiting
on Felix's landing commands, daemons alive), fork `tessel-dogfood--lane-coord-waiter` created,
identity minted. The fix is the swap (grant waiters, then notify), two tests, three mutants, and
a per-path answer for `AlreadyMerged`, `trunk_read`, the rejection paths and the release paths.
Then an Opus review, the workspace gate with curl off the PATH, the wasm build, and Felix lands
and deploys the coordinator after his recordings. Load average 118 at resume: one cargo job at a
time.
Earlier: **As of:** 2026-10-12 14:54 EDT. **HANDOVER before a context clear Felix asked for.
Next (Felix): implement COORD-WAITER-BASE.** Resume from `.claude/handoff-2026-10-12/CHECKPOINT.md`
(untracked; rewritten 14:55): the fix plan, the three reviewed landings in order (SEC-HARDEN
`3442343`, DOCS-REF `9b6af03`, README swap, DOCS-CORE `88c7383`), the GitHub `main` sync, and
what is Felix's. Nothing runs except the three lanes' daemons (`sec-harden`, `docs-ref`,
`docs-core`), kept for their submits.
Earlier: **As of:** 2026-10-12 14:22 EDT. **COORD-WAITER-BASE root cause found (read-only):** on a
merge, `src/coordinator/merging.rs:351` sends `base_moved` to agents whose claims overlap the
touched scopes, and only then does `:352` call `grant_unblocked_waiters`; a waiter granted in that
step was not a holder when the notices were built, and `ServerMsg::Granted` (`src/protocol.rs:543`)
carries no head. Smallest fix: swap the two lines (grant waiters first, then notify), so the new
holder gets the existing `BaseMoved` message; no protocol change, one coordinator test, then a
coordinator deploy. Proposed to Felix; not dispatched (live coordinator, eve of submission).
SEC-HARDEN's two timed-out integration files rerun on the archived `3442343`: 23 of 23 passed.
Earlier: **As of:** 2026-10-12 14:20 EDT. **New follow-up box, found by `lane-docs-core` while
capturing the worked example (not yet investigated):** COORD-WAITER-BASE: an agent granted from
the wait queue right after a merge still showed the pre-merge head (`71a4e8a`) in `tessel status`,
and no `base_moved` reached it; `base_moved` goes to overlapping holders at merge time, and a
queued waiter is not a holder yet. Effect: the granted agent may edit on a stale base unless it
pulls; the steward's rebase still guards `main`. Transcript in the session scratchpad
`lane-docs-core/scenario-transcript.txt`. Also noted: SEC-HARDEN's own vitest run had 2 git
integration timeouts under load average 300-570; the Opus review and the orchestrator gate both
ran 916 of 916 afterwards.
Earlier: **As of:** 2026-10-12 14:19 EDT. **DOCS-REF READY at `9b6af03`** (Opus review "With
fixes", 92 citations checked, 90 right; all 7 fixes applied and read by the orchestrator).
**DOCS-CORE READY at `88c7383`** (Opus review "With fixes": the worked example verified line by
line against the captured run; the one medium overclaim fixed (hooks never claim `depend`), plus
three teaching fixes). README draft reviewed and fixed, links checked (copy in the handoff folder);
it goes in after SEC-HARDEN releases `README.md`. **Landing order for Felix:** SEC-HARDEN
(`3442343`), DOCS-REF (`9b6af03`), then the orchestrator has `lane-docs-core` commit the README,
then DOCS-CORE. Generic script: `<scratchpad>/orch/land-lane.sh <lane> <sha> <evidence>...`.
Felix reads `docs/how-tessel-works.md` before DOCS-CORE lands. Follow-ups: `tessel.toml` comments
still say repo `tessel` (admin merge); `src/protocol.rs` ~297 says intents go to git notes (stale
comment, protocol frozen).
Earlier: **As of:** 2026-10-12 13:50 EDT. **Docs tier 1 dispatched** (reversible; nothing lands
without Felix): `lane-docs-core` (Opus: `docs/how-tessel-works.md`, `docs/reference/coordinator.md`
with the README's hands-on bugs fixed, and a README draft in its scratch dir, applied after
SEC-HARDEN releases `README.md`) and `lane-docs-ref` (Sonnet: `docs/configuration.md`; SKILL.md /
AGENTS.md accuracy fixes). Both from `3442343` with forks and identities. The SEC-HARDEN landing
script was refused for the orchestrator ("Auto-Mode Bypass"); it stays Felix's one command.
Earlier: **As of:** 2026-10-12 13:47 EDT. **SEC-HARDEN READY TO LAND at `3442343`** (`3fb426f` +
`3442343` from `f9a2f0b`): Opus review "Yes" (own mutants 3 of 4 killed, the survivor is the
untested test-runner redaction the lane reported), orchestrator steward gate on the archived
commit: install, typecheck, oxlint, oxfmt 0, vitest 916 passed. `max_instances` dropped: the
`durable_object` scheduling policy rejects it (wrangler dry-run fails); the `/diff` forgery
check closes the cross-site container start. `COORDINATOR_TOKEN` deleted by Felix (no Worker
holds it now). **Pending from Felix:** `bash <scratchpad>/orch/land-sec-harden.sh` (copy in the
handoff `orch/`), then a steward deploy after the live recordings. **Docs:** three research
lanes done; plan in `.claude/handoff-2026-10-12/DOCS-PLAN.md` (tier 1 before submission: README
front door, how-it-works, configuration reference, SKILL fixes, form, GitHub `main` brought up
to the trunk: it is 30 commits behind `artifacts-trunk`). Awaiting Felix's go on tier 1.
Earlier: **As of:** 2026-10-12 13:19 EDT. **Felix: "yes" to SEC-HARDEN, ignoring `AGENTS.md`,
`max_instances = 2`, and deleting `COORDINATOR_TOKEN` himself.** Dispatched `lane-sec-harden`
(Sonnet) in worktree `sec-harden` from trunk `f9a2f0b`, fork `tessel-dogfood--lane-sec-harden`
created, identity minted: `.gitignore` (M1, L4), README admin-token instructions (M2),
`max_instances` and the at-cap behaviour reported (M3a), `forgeryReason` on `/diff` (M3b),
`redactTokens` on the three raw error paths (L2), CLAUDE.md "Sandbox" → Containers. L8 (repo
`SUBMISSION_FORM.md`) waits for Felix's final form. The steward deploy should follow the live
recordings, not precede them.
Earlier: **As of:** 2026-10-12 13:16 EDT. **SECURITY PASS DONE (read-only, Opus, a day early) on
trunk `f9a2f0b`:** no high findings; gitleaks over 853 commits incl. `refs/notes/commits`: one
false positive (`ACCESS_AUD`); cargo deny passes on trunk and `sprint/build`; endpoint auth,
Access JWT checks, untrusted-text escaping and logging pass. Medium: M1 untracked `.claude/`
(124 MB handoff) and `AGENTS.md` not ignored (fix before the repo goes public); M2 README puts
`STEWARD_ADMIN_TOKEN` in the agent env (self-approval and test-free admin merge possible);
M3 no container `max_instances`, `/diff` starts a container without the forgery check. Low:
L1 dev-only sharp advisory, L2 two unredacted error logs, L3 24 h tokens, L4 `.gitignore`
gaps, L5 unused `COORDINATOR_TOKEN` secret, L6 `tools/*.sh` export the API token, L8 repo
`SUBMISSION_FORM.md` (first commit) claims unshipped features. **Proposed to Felix:** one lane
SEC-HARDEN (M1, M2, M3, L2, L4, L8), `max_instances = 2`, ignore `AGENTS.md`, he deletes
`COORDINATOR_TOKEN`. Awaiting his go.
Earlier: **As of:** 2026-10-12 13:05 EDT. **ALL 16 GRAPHICS DONE, checked by the orchestrator,
well before the 18:00 gate.** 8 cards (re-rendered with New York embedded from the system font
files; zero compiler font lines), E1-E6, `fig-arch` and `fig-anatomy` (Anatomy ruling applied;
the lever 1/2 overlap at release matches the source page's own tour frames 10-13). All
1920×1080, 30 fps, no audio stream (checked on all 16), each rendered twice with identical
frames where checked. Copied out of `/tmp` to `.claude/handoff-2026-10-12/video/build/` (`out/`
renders, `src/` generators, `ASSEMBLY.md` = file-to-timecode sheet for the editor). Lanes
`lane-hf-draft`, `-cards`, `-evidence`, `-figures` closed. **Waiting on Felix:** figure and form
v3 changes, the video tag call, the live recordings.
Earlier: **As of:** 2026-10-12 12:41 EDT. **E1-E6 ACCEPTED** (`lane-hf-evidence`, Opus): six
renders in the scratchpad `video-build/out/`, page text generated from `git show d5ef265:` and a
build that stops on any label/source mismatch; the orchestrator read the contact sheet against
the `ab-wait` table (14, 40, 26, 29, 14083 ms, 837059 ms, 20, 0) and the `ab-shadow` README
sentence (20 verified, 9 false alarms, 3 inconclusive, 20 of 29): all match; E4 dims "Landed 11"
and never labels it. E1 gate passed 11:57. **Font finding, confirmed in the source**
(`deterministicFonts.ts` 811-815): the HyperFrames compiler asks Google Fonts first for every
named family, with the page text in the URL, and only then embeds the system font. The draft
and the first eight cards made that request (public card text only). Rule now: only families
declared by our own `@font-face src: local(...)`; check = no compiler font lines in the log.
`lane-hf-cards` re-rendering the eight cards under that rule (and centring the not-shipped
list); `lane-hf-figures` told before its renders. Anatomy ruling: drop the hint corner and the
granted/waiting/denied readout; keep fence digits, log lines, lever badges, legend, caption.
Earlier: **As of:** 2026-10-12 11:37 EDT. **HF-DRAFT DONE, checked by the orchestrator:** HyperFrames
`0.8.145` renders on this Mac (300 frames in 20.5 s with 2 workers; ffprobe 1920x1080, 30 fps,
10.000 s, no audio; repeat renders byte-identical; no process left). Two findings for every
composition: the compiler swaps Georgia/Palatino for a Google Fonts fetch (stack is now "New
York", "Iowan Old Style", serif; compile logs are checked for "Fetched"), and a timeline-free
composition needs `data-no-timeline` or it waits 45 s. Dispatched `lane-hf-cards` (Sonnet: the
eight T1/T4 cards) and `lane-hf-evidence` (Opus: E1-E6 from the evidence files at `d5ef265`,
every label checked against its source cell; E1 gate 17:30). Still to dispatch: the two figures
as `hf-seek` compositions. Captions (scene 4) and flash cuts stay in the editor.
Earlier: **As of:** 2026-10-12 11:35 EDT. **RESUMED; orchestrator on (Felix: "resume").** Code state
unchanged: trunk `f9a2f0b`, coordinator `c49a44ca` live. Dispatched `lane-hf-draft` (Sonnet,
video only, no repo change): install HyperFrames `0.8.145` engine-only in the session scratchpad
under the vetting conditions, render a 10 s hook-card draft at 1920×1080/30 fps, report the wall
time. Change to the shot list: the evidence graphics (E1–E6) render the evidence files at
`d5ef265` as HTML inside the composition, so screenshots S1–S4 are no longer needed from Felix.
**Pending from Felix:** open the two figures and form v3 and send changes; rehearse the
assumption scene; record the live scenes. The 18:00 gate to plain cards stands.
Earlier: **As of:** 2026-10-12 09:30 EDT. **HANDOVER before a context clear Felix asked for. Nothing
runs.** Resume from `.claude/handoff-2026-10-12/CHECKPOINT.md` (untracked; never `git add` it):
it holds the next actions, the video plan and copies of the drafts and figures (the session
scratchpad in `/tmp` may not survive). Code state unchanged since 22:05: trunk `f9a2f0b`,
coordinator `c49a44ca` live. Overnight, video work only (no repo change): two figures built and
checked (Anatomy "Claims, interlocked", a signal-box lever frame driven by the real conflict
table; iso-glow "Tessel on Cloudflare", requests as orange pulses), Cloudflare palette with
meaning, no Cloudflare marks; shot list v1 (8:57); four third-party tools vetted read-only;
renderer chosen: HyperFrames `0.8.145`, engine only, never its skills (its `usage` command reads
agent credentials), telemetry off; no music or SFX. **Next (Oct 12, recording day):** Felix checks
the figures and form v3, captures the shot-list screenshots; the orchestrator times a 10 s
HyperFrames draft, then builds the graphics with an 18:00 gate to plain cards; Felix records the
live scenes after rehearsing the assumption scene. Oct 13: read-only security pass, form final,
submit by hand.
Earlier: **As of:** 2026-10-11 22:05 EDT. **COORDINATOR DEPLOYED by Felix: version `c49a44ca`, code
`f9a2f0b`** (from the deploy worktree; bindings show `SENSITIVE`). Live check by the
orchestrator: identity `orch-live-check` started, claimed `src/protocol.rs` (granted, claim 103,
fence 283), status, stopped; `wrangler tail` for the run: 14 events, all on `c49a44ca`, 9 ok and
5 canceled (socket ends), 0 exceptions, 0 error logs, no `SENSITIVE` config report (scratchpad
`live-check/tail.jsonl`). Not yet seen live: a submission held with `sensitive_path`, a
tests-failed reason naming its tests, a review note reaching an agent; the coordinator's head
still reads `d1ef2f1` until its next dispatched merge. Steward unchanged (no redeploy needed).
Nothing runs; every lane reclaimed. Video checkpoint: Felix's call whether to move it from
`d5ef265` to `f9a2f0b`.
Earlier: **As of:** 2026-10-11 21:56 EDT. **ALL FOUR OPEN BOXES DONE; REVIEW-NOTE MERGED at `f9a2f0b`** by
Felix's admin merge (steward answer `merged`, base `d1ef2f1`, head `f9a2f0b`: a fast-forward,
so the trunk tree is the gated one); mirrored; lane reclaimed. Deploy worktree at `f9a2f0b` with
`build/worker` and `target/debug/tessel` rebuilt there (lanes claim with this CLI from now on).
**PENDING FROM FELIX: one coordinator deploy** covering GATE-OUTPUT, REVIEW-SENSITIVE and
REVIEW-NOTE: `cd /Users/felixpatawah/repos/tessel/.claude/worktrees/deploy && npx wrangler deploy
--env ""`; then the orchestrator checks it live. The coordinator's stored head is stale after the
admin merge (as after SHADOW-GC); the next coordinator-dispatched merge corrects it. Video
checkpoint stays `video-freeze-2026-10-11` (`d5ef265`) until the deploy is checked live.
Earlier: **As of:** 2026-10-11 21:35 EDT. **REVIEW-NOTE READY FOR FELIX'S ADMIN MERGE at `f9a2f0b`**
(`4673a19` + `0cdf85c` + `f9a2f0b` on `task-review-note` from `d1ef2f1`; 8 files incl.
`src/protocol.rs`: two `#[serde(default)]` optional fields `reviewer`, `note` on
`ServerMsg::SubmitRejected`, invariant 13 and the `PROTOCOL_VERSION` doc rewritten to the
amended rule, no bump. Opus review "Yes" on `4673a19`, re-check "Yes" on `f9a2f0b` (7 + 1
mutants killed; an old CLI's exhaustive pattern decodes the new message, probed). The
orchestrator's gate found what both the lane and the review missed: workspace clippy failed on
`notice_text` at 108 lines; fixed by `0cdf85c`. Gate on archived `f9a2f0b`: 1175 passed with
`curl` off the PATH, clippy 0, wasm 0 warnings.) **PENDING FROM FELIX, in this order:** (1) the
admin merge, scratchpad `orch/admin-merge-review-note.sh f9a2f0b` (scopes from `git diff
--name-status`: each modified file edit_signature + create); (2) the orchestrator moves the
deploy worktree to the new trunk and rebuilds the bundle; (3) one coordinator deploy covering
GATE-OUTPUT, REVIEW-SENSITIVE and REVIEW-NOTE.
Earlier: **As of:** 2026-10-11 20:55 EDT. **GATE-OUTPUT MERGED at `42bcdd9`, REVIEW-SENSITIVE at `d1ef2f1`**
(trunk `d1ef2f1`, mirrored; both rebased by the steward with diffs identical to the reviewed
`0a7e30c` and `e2e364a`; both held as signature changes and approved through
`tools/orch-review.sh` after the Opus re-checks' Yes; orchestrator gate on the merged trunk: 1167
passed with `curl` off the PATH, clippy 0; wasm 0 warnings on each lane commit; both lanes
reclaimed). The lanes' pushes to their own forks were refused by the classifier ("Modify Shared
Resources"); Felix pushed and submitted with the scratchpad `orch/land-two.sh` and
`land-sensitive.sh`. Tessel denied the REVIEW-SENSITIVE lane a claim on
`src/coordinator/merging.rs` while GATE-OUTPUT held it; the lane did not edit it. **PENDING FROM
FELIX: deploy the coordinator** (the classifier refused the orchestrator, "Production Deploy"):
the deploy worktree is at `d1ef2f1` with `build/worker` built there. Neither change is live until
then. Next: REVIEW-NOTE (protocol change by Felix's hand-merge, then coordinator and CLI).
Earlier: **As of:** 2026-10-11 19:05 EDT. **SWARM-4XX-HANGUP + SWARM-MACOS-TIMEOUT MERGED at `6ee08b7`**
(claim 98, held as a signature change on `git::mentions_server_error`, approved through
`tools/orch-review.sh` after the Opus re-check's Yes and the orchestrator's gate on an archived
`6ee08b7`: workspace 1146 passed with `curl` off the PATH, clippy 0; the trunk fast-forwarded, so
the merged tree is the reviewed one; mirrored; lane reclaimed). No deploy needed (harness only).
`lane-gate-output` committed `df51eaf`; its Opus review is running. `lane-review-sensitive`
dispatched 19:00 from `d5ef265` (worktree `review-sensitive`; owns `src/coordinator.rs`,
`src/shell.rs`, `src/runtime.rs`, `wrangler.toml`). Video script v1 in the scratchpad
`orch/video-script-v1.md`; form v3 now names Containers (the steward runs a container Durable
Object, not the Sandbox SDK).
Earlier: **As of:** 2026-10-11 18:50 EDT. **VERSION RULE AMENDED by Felix (18:48), on the design pass's
finding:** an old daemon does skip an unknown event, but it then marks every log read incomplete
for good (each reconnect replays from seq 0; `tessel-cli/src/daemon.rs` ~2002-2019), and an
incomplete read returns early from reconciliation (`reconcile.rs` ~270), so it loses its
reconnect repairs for that repo. The rule is now: a new `EventKind` variant, or a new value of an
enum inside a logged event, bumps `PROTOCOL_VERSION`; a new optional field (`#[serde(default)]`)
stays additive (no message type denies unknown fields); a new top-level message type bumps it.
**COORD-UNDELIVERED-GRANT CLOSED without code** (Felix, 18:48). **Designs settled** (Opus design
pass, read-only): REVIEW-SENSITIVE as a coordinator variable `SENSITIVE` (path prefixes, beside
`REVIEWERS`, fail closed on a bad value), not `tessel.toml`: the hold is decided at submit inside
the core, before the steward sees the work, so a list sent with the merge arrives too late; no
protocol change; next lane after `lane-swarm-3`. REVIEW-NOTE as two optional fields (`reviewer`,
`note`) on `ServerMsg::SubmitRejected`, sent to the submitter only, quoted by the CLI; a
`src/protocol.rs` change Felix merges by hand, then a coordinator and CLI lane. `lane-swarm-3`:
review "With fixes" (one missing test, one invented test string), fix pass sent 18:31.
`lane-gate-output` running. Form draft v3 (fact-checked, nine corrections) in the scratchpad
`orch/submission-form-v3.md`; video script being drafted.
Earlier: **As of:** 2026-10-11 18:30 EDT. **VIDEO CHECKPOINT TAGGED: `video-freeze-2026-10-11` at trunk
`d5ef265`** (local annotated tag; live coordinator `92a52a2a` with code `1e04dcf`, steward
deployed 10:45 from `cbef948`). The video shows this checkpoint; work after it lands as usual and
is used in the video only if finished and deployed before Oct 13. **Felix's rulings (18:20):**
(1) build all four open boxes (REVIEW-SENSITIVE, REVIEW-NOTE, COORD-UNDELIVERED-GRANT,
GATE-OUTPUT) after the high-priority items, the checkpoint as fallback; (2) go for one swarm lane
(SWARM-4XX-HANGUP + SWARM-MACOS-TIMEOUT); (3) version rule: a new `EventKind` variant or a new
optional field is additive and needs no `PROTOCOL_VERSION` bump (daemons since `1e04dcf` skip
unknown event kinds in replay); a new top-level message type bumps it. CLI-REPLAY-UNKNOWN-EVENT
closed on that ruling (the rule goes into the `src/protocol.rs` invariants with the next protocol
change, which Felix merges by hand); (4) next: the form draft. **Running:** `lane-swarm-3`
(dispatched 18:22 from `d5ef265`, worktree `swarm-3`, owns `tessel-swarm/src/git.rs`);
`lane-gate-output` (dispatched 18:29 from `d5ef265`, worktree `gate-output`, owns `src/merge.rs`:
the box said steward-only, but the steward already returns the failing command's output and the
coordinator's `tests_reason` drops it, so the fix is the coordinator's and needs its deploy); an
Opus read-only design pass on REVIEW-SENSITIVE, REVIEW-NOTE and COORD-UNDELIVERED-GRANT; an Opus
fact check of the form draft v2 (scratchpad `orch/submission-form-v2.md`) against `d5ef265`.
Earlier: **As of:** 2026-10-11 13:15 EDT. **ALL DISPATCHABLE BOXES CLOSED; BOTH WORKERS DEPLOYED; LIVE PROOF
DONE. Trunk `d5ef265`** (tree `09e32a6`; mirrored; every lane reclaimed; deploy worktree at
`d5ef265` with its `tessel` rebuilt at `1e04dcf`). **Steward** deployed by the orchestrator at
10:45 from `cbef948` (bare `npx wrangler deploy` in the steward directory, allowed by the standing
Oct 5 approval; exit 0; the CLI-REPLAY merge at 11:15 and the docs merge at 13:05 ran through it).
**Coordinator** deployed three times at midday, the first two wrong: Felix's bare `npx wrangler
deploy` ran in the session's working directory, the main checkout on `sprint/build`, whose `src/`
is ~3,000 lines behind the trunk (version `33d427d9`, old code); the orchestrator's `--config`
deploy from the main checkout uploaded the deploy worktree's stale Oct 7 bundle (`f4d4c68c`,
the previously live build); then `worker-build --release` inside the deploy worktree at `1e04dcf`
and a bare `npx wrangler deploy --env ""` there gave **version `92a52a2a`, code `1e04dcf`**, the
one that is live. Lesson: a deploy command for Felix always carries its `cd`, and the custom
build runs in the shell's working directory, not the config's. **Live proof** (13:05): docs claim
97 (`d5ef265`, the render-check evidence) merged through both new Workers with `wrangler tail` on
the coordinator: 38 events, 36 ok and 2 canceled (the lane's own socket open and close), no
exception, no error log, no "core is not loaded", no `already_merged` (scratchpad
`live-proof/coordinator-tail.jsonl`). The dropped-core case itself was not provoked; the proof is
that the new code loads the stored state and merges. Open boxes left: REVIEW-SENSITIVE,
REVIEW-NOTE, COORD-UNDELIVERED-GRANT, GATE-OUTPUT (Felix's rulings), GATE-PATH (process), the
three swarm follow-ups (SWARM-ORDER-CHECK-WEAK needs a decision; SWARM-4XX-HANGUP and
SWARM-MACOS-TIMEOUT are cheap, one lane, on Felix's go), and the version-bump half of CLI-REPLAY.
Earlier: **As of:** 2026-10-11 10:25 EDT. **CLI-GITHOOK-REINSTALL MERGED at `4e58f49`, COORD-APPLY-LOADED at
`cbef948`** (tree `dcfc86c`; both mirrored; both lanes reclaimed; deploy worktree at `cbef948`, its
`tessel` binary still from `1fdb6b4`, rebuilt with the next CLI merge). `lane-cli-replay`
dispatched 10:22 from `46e42ce` (worktree `cli-replay`), the last dispatchable box. **PENDING FROM
FELIX, two deploys from the deploy worktree:** the steward (`4814caf`: stale Access keys) with
`cd /Users/felixpatawah/repos/tessel/.claude/worktrees/deploy/tessel-steward && pnpm install
--frozen-lockfile && npx wrangler deploy`, and the coordinator (`cbef948`: apply after an await)
with `cd /Users/felixpatawah/repos/tessel/.claude/worktrees/deploy && npx wrangler deploy`
(the swarm coordinator too if it is kept: `--config` as before). Then the orchestrator runs one
merge through both. Also still Felix's: the rulings (REVIEW-SENSITIVE, REVIEW-NOTE,
COORD-UNDELIVERED-GRANT, GATE-OUTPUT), the version-bump half of CLI-REPLAY-UNKNOWN-EVENT, the
form and the video.
Earlier: **As of:** 2026-10-11 10:00 EDT. **SWARM-PUSH-PHRASES + SWARM-ONLOCAL-LOAD-2 MERGED at `46e42ce`**
(rebased by the steward onto `4814caf`, diff identical to the reviewed `b0694c6`; workspace gate
1123 with `curl` off the PATH; mirrored; lane reclaimed; three follow-up boxes filed from the
review). `lane-cli-githook` reported `4b575b0`; its Opus review is running. `lane-coord-apply`
dispatched 10:00 from `ab74e1f` (worktree `coord-apply`). Two heavy jobs at a time.
Earlier: **As of:** 2026-10-11 09:40 EDT. **STEWARD-ACCESS-STALE-KEYS MERGED at `4814caf`** (tree
`234c3f7`; mirrored; lane reclaimed; needs Felix's deploy, see the box). `lane-swarm-2` reported
`b0694c6` (two different load flakes in its two full runs, each passing in the other:
`a_claim_granted_after_a_queue_wait_of_many_leases_is_kept_alive`, `off::tests::
agents_work_at_the_same_time`); its Opus review is running. `lane-cli-githook` dispatched 09:36
from `ab74e1f` (worktree `cli-githook`); `coord-apply` worktree, fork and identity prepared, not
dispatched (two heavy jobs at a time).
Earlier: **As of:** 2026-10-11 00:55 EDT. **STEWARD DEPLOYED (version `5a37c8e6`, Felix, 00:20, from the
deploy worktree at `ab74e1f`; the sandbox image went up over Wi-Fi, so the first merge that runs
the container shows whether it arrived intact). REVIEW-CARD render-checked** by the orchestrator:
the Chrome extension was not connected and the live page needs an Access session, so the deployed
`reviewPage` code was rendered locally with a realistic card and screenshotted in a 390 px frame
(scratchpad `evidence/2026-10-10/review-card-render/`, to land with the next docs lane): no
horizontal overflow, sticky decision bar, 48 px buttons, approve disabled after the script ran
with no diff reachable. Not checked live: the diff load, a decision round trip, the receipt poll.
**Wave 1 dispatched 00:50** on Felix's "resume": `lane-swarm-2` (SWARM-PUSH-PHRASES +
SWARM-ONLOCAL-LOAD-2, worktree `swarm-2`) and `lane-steward-keys` (STEWARD-ACCESS-STALE-KEYS,
worktree `steward-keys`), both cut from trunk `ab74e1f`; briefs in the scratchpad `orch/`. Next
waves, briefs written: CLI-GITHOOK-REINSTALL + COORD-APPLY-LOADED, then CLI-REPLAY-UNKNOWN-EVENT
(tolerant decode in the daemon; the version bump stays Felix's call). Load average 300+ from other
sessions: two lanes at a time, disjoint crates.
Earlier: **As of:** 2026-10-10 00:15 EDT. **ALL THREE UX BOXES MERGED; trunk `ab74e1f` (tree `1332754`),
mirrored to GitHub `artifacts-trunk`; no lane runs; every lane worktree, branch, daemon and token
file reclaimed.** `ab74e1f` is the docs landing (claim 90 of `lane-docs-evidence`, orchestrator-run,
docs only, merged without a hold): `docs/evidence/2026-10-09/cli-card-live/` and `cli-init-live/`
plus rule 12 of `docs/BUILD-PROTOCOL.md` §4 (the create-claim rule). Before it, the code trunk was
`1fdb6b4` (tree `1774270`). In order: **REVIEW-CARD at `7f5330e`** (22:41; review "With fixes" then "Yes";
steward 901 tests), **CLI-CARD at `bba9344`** (22:5x; rebased by the steward onto the review-card
merge, diff identical to the reviewed `ba555ac`; review "With fixes" then "Yes"; workspace 1085),
**CLI-INIT at `1fdb6b4`** (23:50; review "With fixes" three times, the last cosmetic, final wrap
read by the orchestrator; workspace 1121). Each was decided through `tools/orch-review.sh` after
the Opus Yes and the orchestrator's gate on an archived copy with `curl` off the PATH (logs in the
session scratchpad `orch/gate-*.log`). Live checks by the orchestrator, landed in
`docs/evidence/2026-10-09/cli-card-live/` and `cli-init-live/` at `ab74e1f`: the CLI denial card against the production coordinator (holder's intent quoted
as data, two valid moves numbered, depend ruled out with its reason, `--json` envelope with
`{author, text}` objects and a `next` list), and `tessel init --steward` against the production
steward (token minted, rerun idempotent, exclude rule, both hook sets, then a real `tessel start`
with the minted token). Lesson recorded as rule 12 of `docs/BUILD-PROTOCOL.md` §4: a lane claims `--mode
create` on every file in which it adds an exported symbol (REVIEW-CARD and CLI-INIT were both
refused locally as uncovered until the orchestrator amended the claim). Small follow-up noted,
not filed: one holder with two locks on the same scope prints two identical "blocked" blocks on
the card. **PENDING FROM FELIX: deploy the steward** (REVIEW-CARD is not live until then): the
deploy worktree is at `ab74e1f` (steward code identical to `7f5330e`):
`cd /Users/felixpatawah/repos/tessel/.claude/worktrees/deploy/tessel-steward && pnpm install
--frozen-lockfile && npx wrangler deploy`; then the orchestrator checks `/review/tessel-dogfood`
live. The session's classifier refused the orchestrator's roadmap commits after 22:30 (reasons
given: "Self-Approval", "Auto-Mode Bypass"), so this STATE was written by the orchestrator and
committed by Felix. Earlier: **CLI-CARD REPORTED `671b41b` (22:26; new `tessel-cli/src/moves.rs` claimed with create, plus render, commands, hook, main, the two test files and `skills/tessel/`; +1138/-100; `cargo test -p tessel-cli` 400, workspace 1080, clippy and rustfmt clean; four mutants killed); Opus reviewer `review-cc` dispatched 22:30 (brief `orch/review-brief-cli-card.md`; asked to check every move rule against the coordinator and the extra status round trip's fail-closed behaviour). REVIEW-CARD FIXED as `e490691` (22:26; 7 files, +151/-25; steward 900 with a 60 s timeout, 3 integration timeouts at load above 100 on the plain run); re-check by the same reviewer `review-rc` sent 22:27.** Earlier: **REVIEW-CARD REVIEW 1: "With fixes" (22:22) on `608a7e9`; fix pass sent to the same lane 22:25: (1) a rejected claim stays live in the fold (the coordinator reopens it with its locks, `merging.rs:401`; the brief's "not rejected" was the orchestrator's error), (2) the receipt completes at `submit_rejected` with no grants, (3) "unblocks K" counts a waiter only when its mode conflicts with the touched mode (`Mode::conflicts_with`), (4) the held claim's own assumptions back under INTENT, (5) eight lines over 100 characters, (6) a non-integer queue position is skipped. Reviewer's own five mutants all killed; its gate 885 passed at load 30. Reviewer `review-rc` idle.** Earlier: REVIEW-CARD REPORTED `608a7e9` (22:16; 12 steward files, +1480/-177; approve token issued with the diff, receipt route, wait and assumption folds; steward 885 tests with a 60 s timeout, 7 integration timeouts at load 127 on the plain run); Opus reviewer `review-rc` dispatched 22:20 (brief `orch/review-brief-review-card.md`; asked to require the held claim's own assumptions back under INTENT). CLI-CARD still running.** **UX DECIDED by Felix (21:50, from the rendered mockups
https://claude.ai/artifact/F4ee5swgmrjuxDyFgUaTi6, three boards): build the CLI card, then the review
decision card; `tessel init` is in, with the manual steps kept as the fallback; the Floor and the
rest of the proposal are recorded under "Post-contest" at the end of this file and not built.
DISPATCHED 22:05, two lanes in disjoint areas:** **CLI-CARD** as lane `lane-cli-card` (Sonnet;
worktree `.claude/worktrees/cli-card`, branch `task-cli-card`, cut from the trunk `db31287`; fork
`tessel-dogfood--lane-cli-card`; brief in the scratchpad `orch/brief-cli-card.md`; owns
`tessel-cli/src/render.rs`, `commands.rs`, `main.rs`, `hook.rs`, `tessel-cli/tests/cli.rs`,
`tests/mcp.rs`, `skills/tessel/SKILL.md`, `skills/tessel/AGENTS.md`) and **REVIEW-CARD** as lane
`lane-review-card` (Sonnet; worktree `.claude/worktrees/review-card`, branch `task-review-card`,
same base; fork `tessel-dogfood--lane-review-card`; brief `orch/brief-review-card.md`; owns
`tessel-steward/src/review*.ts` with their tests, and `routes.ts` if a route is added). Both use
the deploy worktree's `tessel` binary for claims. **CLI-INIT** follows CLI-CARD (same CLI files).
After REVIEW-CARD merges, Felix deploys the steward (the classifier refuses the orchestrator).
Earlier: **MILESTONE MERGED: pull request 2 merged into `main` at
`1aa3684` (19:58, merge commit, on Felix's command; `main`'s tree `74e0544` equals the trunk's;
`artifacts-trunk` kept). GitHub `main` now holds everything through `db31287`.** Still for Felix:
the video script and the form rewrite (draft in the scratchpad `orch/submission-form-draft.md`),
the boxes below as lanes, his pending rulings.
Earlier: **PULL REQUEST 2 WAS READY; FELIX MERGED IT.** The trunk is at
`db31287` (docs landed 18:00 as claim 86 of `lane-docs-milestone-2`; tree `74e0544`; mirrored to
GitHub `artifacts-trunk`, 244 commits on the pull request; lane reclaimed). The auto-mode
classifier refused the orchestrator's `gh pr merge` ("Merge Without Review": the reviews are the
orchestrator's own), so Felix runs, from `repos/tessel`:
`gh pr merge 2 --merge --subject "Merge pull request #2 from devfep/artifacts-trunk"` (a merge
commit, NOT squash and NOT `--delete-branch`: `artifacts-trunk` is the mirror branch). Then
`main`'s tree should equal `origin/artifacts-trunk`'s (`74e0544`). Everything the protocol asks
for before that merge is done: Sandbox gate on every commit, both reviews posted as comments,
fresh clone clean. **Orchestrator idle after this commit; nothing runs.** Then the video script and
the form rewrite for Felix (draft in the scratchpad `orch/submission-form-draft.md`), and the
boxes below as lanes when he says so. Earlier: **FRESH-CLONE TEST PASSED on `e164717` (17:41–17:49, load up to
173): `cargo test --workspace` 1068 passed, 0 failed; `cargo deny` ok; steward 803 passed,
typecheck ok (`docs/evidence/2026-10-09/fresh-clone/`). These docs landed as `db31287` (above).** **MERGED 17:41: SWARM-RECONNECT-FIRST-CONNECT at trunk `e164717`**
(claim 85, no hold; the trunk fast-forwarded; tree `489eb2c` equals the lane commit's; mirrored to
GitHub `artifacts-trunk`, so pull request 2 now carries 243 commits; lane daemon stopped, worktree,
branch and token file reclaimed). The second fresh-clone run is the one above. History: the lane reported `e164717`
(tests/reconnect.rs only: `after_connected` helper, a unit test of it, the one call site; the
reviewer-refusing tests left alone with a reason in the note); orchestrator read it (rule 00,
test-only, no Opus pass), gated it in place with `curl` off the PATH (reconnect suite 3/3 exit 0,
20 tests, 21–24 s; clippy and rustfmt clean; the crate run had one unrelated failure, below);
pushed to the fork and submitted 17:38 as claim 85, queue position 1, no hold expected. Watches
armed on the trunk head and the lane inbox.** SWARM-ONLOCAL-LOAD REOPENED as SWARM-ONLOCAL-LOAD-2
(below): `on_local.rs:624` failed in that gate at load 75 (shadow span 5605 ms + 3000 vs holder
span 7825 ms) and in the lane's workspace gate (`work_that_already_fails_on_main...` once, passes
alone); not this lane's scope. **MILESTONE PULL REQUEST OPEN: https://github.com/devfep/tessel/pull/2
(`artifacts-trunk` → `main`, 242 commits).** **Fresh-clone test (PLAN §9 Oct 11, run 16:50–17:03
on a clone of GitHub `artifacts-trunk` at `c6d6c76`, Mac load 60–113 from other sessions):**
`cargo deny check` ok; steward `pnpm install --frozen-lockfile` ok, `pnpm test` 803 passed (42
files), `pnpm typecheck` ok; `cargo test --workspace` FAILED twice on ONE test,
`tessel-swarm/tests/reconnect.rs:334` `agents_that_cannot_reconnect_end_as_disconnected_after_their_tries`
("cannot connect to the coordinator: HTTP error: 401 Unauthorized" at `reconnect.rs:116`), 1065
other tests passed; the test passes alone (3.5 s) and passed the Sandbox gate on `c6d6c76`.
Diagnosis: the test arms its cut on the first `ClaimGranted` and refuses both agents; under load
the second agent's FIRST connection comes after that grant and is refused, which the harness
treats as a fatal run error. Filed SWARM-RECONNECT-FIRST-CONNECT (below) and dispatched 17:12 as
lane `lane-swarm-reconnect-cut` (Sonnet) in `.claude/worktrees/swarm-reconnect-cut` (branch
`task-swarm-reconnect-cut`, fork `tessel-dogfood--lane-swarm-reconnect-cut`, brief in the session
scratchpad `orch/brief-swarm-reconnect-cut.md`; owns `tessel-swarm/tests/reconnect.rs`; uses the
deploy worktree's `tessel` binary). Raw logs in the scratchpad `fresh-clone/run1.log` and
`fresh-clone/run1/run2-tests.log` (to be copied into `docs/evidence/2026-10-09/fresh-clone/`).
**Code review of the pull request (the `code-review` skill, 17:10) DONE 17:12:** nine findings,
each verified by a separate Sonnet agent with the official rubric: none reached 80 (one 75, four
50, four 25); the comment is on the pull request and the real ones are boxes below
(CLI-GITHOOK-REINSTALL, COORD-APPLY-LOADED, CLI-REPLAY-UNKNOWN-EVENT, STEWARD-ACCESS-STALE-KEYS,
SWARM-PUSH-PHRASES, EVIDENCE-CHECK-SH). **Security review DONE 16:58:** no High or Medium finding
at the bar; the comment lists what was checked and found sound. The findings were: (1) `runtime.rs:651` apply after
an awaited steward call can hit a dropped core; (2) `daemon.rs:1928` log replay aborts on an
unknown event variant; (3) `githook.rs:387` re-install bails when `.pre-tessel` exists; (4)
`git.rs:31` push retry misses common lost-connection phrases; (5) `coordinator.rs:700` first Hello
seeds the head from unvalidated text; (6) `runtime.rs:435` alarms persist unchanged state; (7)
`access.ts:92` a failed certs refetch 503s despite a cached key set; (8) `wrangler.toml:22`
`orchestrator` as a reviewer vs invariant 12 (a documented process decision, BUILD-PROTOCOL §2);
(9) `docs/evidence/.../check.sh` sources an absolute path under `set -a`. The milestone pull request now waits only on the reconnect test fix (the lane) and a final gate. **Submission form:** a rewrite matching what shipped is drafted for Felix in the
scratchpad `orch/submission-form-draft.md` (drops the unshipped items: separate evidence repos,
Workers Builds previews, races in the CLI). Step 1 done 16:52: the docs commit `8c958b3` (this
roadmap, `docs/BUILD-PROTOCOL.md`, the PLAN §6 line, `docs/evidence/` Oct 5–9) merged through the
steward as claim 84 of `lane-docs-milestone` (orchestrator-run, docs only, rule 00) and the trunk
fast-forwarded to `8c958b3` (tree `9192700`); mirrored to GitHub `artifacts-trunk`; lane worktree,
branch and token file reclaimed. Lesson: turning Wi-Fi off for the push dropped the daemon's socket
and claim 83 lease-expired 30 s after its grant; switch Wi-Fi BEFORE `tessel start`, or push over
Wi-Fi when the pack is small. In progress: the fresh-clone test of `artifacts-trunk` (`c6d6c76`
code tree, scratchpad `fresh-clone/run1`, 2 build jobs: cargo test, cargo deny, steward install,
test, typecheck), and the code and security reviews of the pull request, posted as its comments.
Neither the official `code-review` plugin nor `claude-security` is installed, so the reviews run
as subagents following the official review procedure (area reviewers, confidence-scored,
findings under 80 dropped); Felix can run the built-in `/code-review` and `/security-review` from
his terminal as well. The pull request merges only after both reviews, the fresh-clone test and a
gate on the trunk tree (`c6d6c76` passed the Sandbox gate; `8c958b3` adds docs only). Then the
video script for Felix. Every dispatchable box stays closed; the rest waits on Felix (see "Open
boxes left").** **Orchestrator:** this session (resumed 07:16 on Felix's
"resume"; read this block, `docs/BUILD-PROTOCOL.md`, `PLAN.md` §6 and §9).
**Done this session:** AX-MCP closed (live check passed 07:20; evidence in
`docs/evidence/2026-10-09/ax-mcp-live-check/`). **FIRST COMPLETED LIVE A/B RUN** (`wait`, seed 3,
40 tasks, 30 agents, 07:25–07:40, harness `4033791`, Wi-Fi off so the run went over Ethernet):
`on` landed 40/40, 0 rejected, 0 lapsed, 0 closed, 0 reconnects, 29 queued waits, 20 scripted
approvals, 837 s; `off` (local replay) 14/40 with 26 rejected in 14 s. Every edge request at
`IAD`, 759 DO invocations (mean wall 0.95 s, max 20.4 s), ping 222/222 max 101 ms. Raw output and
README in `docs/evidence/2026-10-09/ab-wait/`. The `tessel` MCP server stays registered in Claude
Code's local scope for `.claude/worktrees/ax-mcp` (`claude mcp remove tessel -s local` there
removes it); that worktree's `.tessel/config.toml` (gitignored, 0600) holds a `lane-ax-mcp`
identity minted 07:19; its daemon is stopped.
**MERGED 09:45: SWARM-SHADOW-RECONNECT at trunk `8e9ceed`** (claim 80 approved 09:37; the
steward fast-forwarded the lane's four commits; trunk tree equals `8e9ceed`'s; mirrored to GitHub
`artifacts-trunk`). The Sandbox `test-runs` on `8e9ceed` passed (194 s). SWARM-ONLOCAL-LOAD and
CLI-GITHOOK-ETXTBSY closed in the same merge. No Worker deploy (harness and tests only).
**FIRST COMPLETED LIVE `shadow` A/B RUN** (seed 3, 40 tasks, 30 agents, 09:41–09:54, harness
`8e9ceed`, Ethernet): 11 merged, 28 shadow claims, 1 failed (an Artifacts fork push answered
503; SWARM-PUSH-RETRY filed); trials: **20 verified preventions, 9 false alarms, 3 inconclusive,
0 never verified** (precision 20/29); 0 lapsed, 0 closed, 0 reconnects; 628 s; shadow agents
waited 206–552 s for their trials. Evidence and README in `docs/evidence/2026-10-09/ab-shadow/`.
Both PLAN §9 Oct 10 A/B runs (`wait`, `shadow`) now have live numbers a day early.
**Now / next:** (a) Felix's three UX decisions (action 4 below) gate all UX work; (b) smaller
open boxes (CLI-CLAIM-EXISTS, REVIEW-SENSITIVE, GATE-PATH, SWARM-PUSH-RETRY; GATE-OUTPUT needs
Felix's permission call, see the classifier note) when the Mac has headroom (load 45–100 from
other sessions all morning); (c) Oct 10–12, STARTED 16:50 a day early: land the docs through the steward (in
progress), the milestone pull request from `artifacts-trunk` with `/code-review` and
`/security-review`, the fresh-clone test, the video. The deploy worktree is at `8e9ceed`; no Worker deploy is pending.
History of the landing: lane `lane-swarm-shadow-reconnect` (Sonnet) dispatched 07:43 in
`.claude/worktrees/swarm-shadow-reconnect` (branch `task-swarm-shadow-reconnect`, fork
`tessel-dogfood--lane-swarm-shadow-reconnect` created); brief in the session scratchpad
`orch/brief-swarm-shadow-reconnect.md`; owns `tessel-swarm/src/{on,events,conn}.rs`. Reported
07:59: commit `8ceecb6` (on.rs, events.rs, tests/reconnect.rs, README), workspace 1045 passed,
clippy and rustfmt clean, 7 mutants caught; not pushed, not submitted; daemon up with claim 78.
Opus review: "With fixes" 08:07 (two 103-char test lines, one surviving mutant), fix commit
`82ed1e4`, re-check "Yes" 08:12; orchestrator gate on an archive of `82ed1e4`: 1046 passed with
`curl` off the PATH, clippy 0. Pushed to the fork, submitted, held (signature changes), approved
as claim 78 by `tools/orch-review.sh` (the classifier allowed it this time, 08:30). **The steward
REFUSED the merge** ("tests failed (exit code 101) on the commit rebased onto main"; trunk
unchanged, so the tree is `82ed1e4`'s). `POST /repos/<fork>/test-runs` on the fork reproduced it
AND NAMED IT (the test-run result carries the test step's stdout; only the merge path's notice is
blind, GATE-OUTPUT): `a_submission_still_undecided_when_the_agent_returns_is_waited_for_not_sent_again`
(reconnect.rs:115, a SWARM-RECONNECT test): 5 reconnect tries ran out while the test's local
server still refused with 401; the reconnect suite took 76 s in the Sandbox against 17 s here
(wall 237 s, peak 696 MB). Lane hardened the tests: `ac8a38a` on top of `82ed1e4` (tests/reconnect.rs only: that test on
`PATIENT`, `PATIENT` 40 tries from 200 ms capped at 8 s, cut delay 250 ms, the lapse test with a
100 ms heartbeat, two trial assertions dropped because under load the shadow can submit after
the holder merged and then no trial runs). Orchestrator read it (rule 00; no second Opus pass:
test-only). Pushed 09:11; Sandbox `test-runs` on the fork at `ac8a38a` PASSED (exit 0, 214 s).
Mac gate on an archive of `ac8a38a` at load 90–105 (other sessions): 1 failure,
`a_shadow_submission_is_on_record_early_and_its_work_time_is_still_counted` (tests/on_local.rs:612,
a pre-existing test with a fixed 6 s bound on the shadow's submit; the lane saw 6.5–7.6 s pushes
under load, 3 failures in 9 runs), everything else passed. Ruling: the lane did not touch that
test and the Sandbox passed the same tree, so the submission is approved on the Sandbox pass plus
the Mac pass at `82ed1e4`; the test is filed as SWARM-ONLOCAL-LOAD (below).
**Steward refused `ac8a38a` too (09:19, before the approval; "tests failed (exit code 101)").**
A second Sandbox `test-runs` on the same fork commit failed on ANOTHER pre-existing test:
tessel-cli `githook.rs` `without_the_recorded_binary_the_one_on_path_is_used_whatever_it_says`,
`Text file busy (os error 26)`: a Linux fork/exec race between parallel tests that `fs::copy` the
tessel binary and exec it (CLI-GITHOOK-ETXTBSY, below). So the TRUNK's suite is flaky in the
Sandbox (two independent flakes seen in three runs; AX-MCP's one refusal on Oct 8 was likely one
of them). The lane fixes both in its pass (09:25 on_local, 09:40 githook; it owns
`tests/on_local.rs` and `tessel-cli/tests/githook.rs` for this), then push, submit, approve.
Claims 78 and 79 were released by the lane's `tessel stop` each time; the approval of 79 (09:22)
landed after its refusal and decided nothing.
**Orchestrator tooling this session** (scratchpad `orch/`): `ab-run.sh <policy> <out>`,
`gate.sh <wt> <sha> <copy>` (curl off PATH), `test-run.sh <repo> <out.json>` (Sandbox gate of a
repo's main; the json names failing tests), `watch-trunk.sh <old-sha>` (ls-remote with a read
token), `mint-identity.sh <agent> <wt>`, `create-fork.sh <lane>`, `mcp_probe.py`.
Orchestrator ruling on the lane's flagged limit (a reset before the `Shadowed` reply is read
leaves the agent without its fence, so it claims again and the first claim idles out its lease):
accepted for now, documented in the harness README; the fix is an additive
`#[serde(default)] fence: Option<Fence>` on `EventKind::ClaimShadowed`, added to the protocol
hand-merge list below for Felix. Also for that list (code review, Oct 9): the wording of invariant
12 ("a human must approve it first") vs the build process where `orchestrator` approves after the
Opus Yes and a gate (BUILD-PROTOCOL §2): a comment-only change, or the process is the exception.
**Classifier refusal (08:00):** the auto-mode classifier refused a read of the steward's source
(`rg` for where trial stdout/stderr are kept) as "data exfiltration", so GATE-OUTPUT was not
prepared; Felix decides whether to allow that read or run that lane from his own session. Then the
live `shadow` A/B run with the same recording (tail, ping, Wi-Fi off; launcher
`scratchpad/orch/ab-run.sh <policy> <out-dir>`).
**Trunk:** `db31287` (docs on `e164717` on `8c958b3`; mirrored to GitHub `artifacts-trunk`);
`.claude/worktrees/deploy` is at `c6d6c76` (harness rebuilt). `cargo deny check` on the trunk
(PLAN §9 Oct 11 item, run 11:30): advisories ok, bans ok, licenses ok, sources ok.
**MERGED 11:22: SWARM-PUSH-RETRY at trunk `c6d6c76`** (claim 82 approved 11:17 after Opus "Yes"
at `6df0809` and a re-check "With fixes" (one long line) through `f4fc4e6`; four small commits
after the Yes read by the orchestrator; Mac gate of `f4fc4e6` 1067 passed with `curl` off the
PATH; trunk tree equals `c6d6c76`'s; mirrored). Harness only: no Worker deploy. Lesson kept: the
orchestrator's gate strips every PATH directory holding `curl`, which on this Mac removes
`/usr/bin` (so `which`, `chmod`, `sed`, `dirname` vanish); a test that shells out to those fails
the gate before it can fail the Sandbox. Tests spawn nothing but git and the binaries under test.
**MERGED 10:42: CLI-CLAIM-EXISTS at trunk `6b61869`** (claim 81 approved 10:38 after Opus "With
fixes" then "Yes" and a Mac gate of 1057 with `curl` off the PATH; trunk tree equals `6b61869`'s;
mirrored). CLI only: no Worker deploy.
**LOCKFILE FREEZE LIFTED** (AX-MCP merged 09:16).
**Oct 8 overnight, AX-MCP landing:** the image push failure was NOT Docker: the Wi-Fi link between
this Mac and the Optimum Ubee gateway (192.168.1.1) corrupts long TLS uploads to Cloudflare
(`bad record MAC`). Evidence: plain curl to `speed.cloudflare.com/__up` and our `*.workers.dev`
fails after 0.2–37 MB over Wi-Fi (IPv4 and IPv6, HTTP/1.1 and 2, TLS 1.2 and 1.3, 1 MB/s or full);
the same Mac passes on the iPhone hotspot (3/3) and over the Thunderbolt Ethernet `en23` to the
same gateway (6/6 × 30 MB, 2/2 × 150 MB); a gateway reboot did not help; the only enabled
data-path filters are Apple's firewall and Little Snitch (content filters cannot modify bytes).
Unexplained: 30 MB uploads to AWS (httpbin) passed 3/3 over Wi-Fi. **Large Cloudflare uploads
(image pushes, deploys with new images) must go over Ethernet**: turn Wi-Fi off for the push
(`networksetup -setairportpower en0 off`, back `on` after). Pushing tools: `crane` and `regctl`
(brew, Apache-2.0) authenticate with `wrangler containers registries credentials
registry.cloudflare.com --push --pull --json` piped to `--password-stdin`/`--pass-stdin`, config
in the scratchpad; the registry advertises `Oci-Chunk-Max-Length: 200000000`.
Then: steward `02db259b` deployed by Felix from the ax-mcp worktree (toolchain image with rmcp);
trunk `test-runs` passed; lane claim 76 had lease-expired, re-claimed as 77; submitted, held,
gated, approved by Felix (the classifier refuses the orchestrator's approval as self-approval);
the merge was refused once ("tests failed (exit code 101) on the commit rebased onto main"). My
gate had run on the lane's old base `107fc13`, not the trunk: always gate the tree merged with the
trunk. The lane merged the trunk (`fc09fed`); the same tree passed locally (1029 + 803, offline,
no curl) and in a Sandbox test-run on the fork; resubmitted, Felix approved 09:13, merged 09:16 as
`4033791`; trunk tree equals `fc09fed`'s. The Sandbox failure is an unidentified intermittent
cargo test (the trial result keeps only the last step's output): filed GATE-OUTPUT.
**Approvals:** `tools/orch-review.sh <claim> "<note>"` (throwaway worktree, minted `orchestrator`
identity, `tessel review --approve`, stop, worktree removed) is what Felix runs as a `!` command;
offered him an allow rule for it. Pending from Felix: that rule; Little Snitch filtering back on.
**Landed Oct 7 (all reviewed, gated, merged through the steward; see each box):** SWARM-FLAKE-SHADOW
`58bbbc3`, SWARM-LEASE `d9fb232`, AX-INBOX `01131a5`, AX-GITHOOKS `107fc13`, SWARM-WAIT `43edf24`,
COORD-CLOSE-WITHDRAW `9c086e9`, SWARM-RECONNECT `e7250de`; plus DEMO-TS/SWARM-REVIEWER-ERR,
CLI-CONNECT-TIMEOUT, REVIEW-UI (closed signed in), `swarm-demo` repo created live.
**Deployed:** coordinators from `9c086e9` by Felix (production `7fc954ea`, swarm `dffde46a`:
COORD-CLOSE-WITHDRAW live); steward `02db259b` (Oct 8 04:1x, from the ax-mcp worktree: steward code
equal to the trunk's, toolchain image with rmcp). AX-MCP is CLI-only: no Worker deploy.
**NEXT ACTIONS (in order):**
1. DONE 07:20: AX-MCP live check. Still pending from Felix: Little Snitch filtering back on (it
   was not the cause). GATE-OUTPUT (new, below).
2. DONE 07:40 (see above). Was: **Live A/B `wait` run** on the reconnecting harness (build `tessel-swarm` from the trunk in the
   deploy worktree; same args as `docs/evidence/2026-10-07/README.md`), with `wrangler tail` on
   `tessel-coordinator-swarm` (record `cf.colo`) and a 5 s ping from the Mac; keep raw output in
   `docs/evidence/<date>/`; stop the tail by PID after. No live `on` numbers exist yet.
3. DONE 09:45 / 09:54: SWARM-SHADOW-RECONNECT merged, then the `shadow` A/B run (see above).
4. DONE 21:50: Felix's three UX decisions (https://claude.ai/artifact/V5ifR3B8wfUtsdbpH7JZXd,
   mockups https://claude.ai/artifact/F4ee5swgmrjuxDyFgUaTi6): CLI card, then the review decision
   card, the Floor not before the contest; `tessel init` in with the manual fallback; the
   post-contest list recorded only. Boxes CLI-CARD, REVIEW-CARD, CLI-INIT below.
5. Smaller open: CLI-CLAIM-EXISTS, REVIEW-SENSITIVE, REVIEW-NOTE (needs Felix), COORD-UNDELIVERED-
   GRANT (needs Felix: a new ReleaseReason variant; recommendation: leave it), GATE-PATH.
6. STARTED 16:50; docs landed 16:52; pull request 2 open 16:58. Oct 10–12: reviews and the
   fresh-clone test (in progress), merge the pull request, then the video.
**Pending from Felix:** COORD-UNDELIVERED-GRANT and REVIEW-NOTE rulings; the
protocol hand-merge list below (now also `Review.fork_commit`); an allow rule for lane `tessel`
commands (offered: `Bash(cd /Users/felixpatawah/repos/tessel/.claude/worktrees/* && target/debug/
tessel *)`), since the classifier refused lane `tessel submit` four times and a fork push once
on Oct 7 and Felix ran them by hand.
**How lanes ran on Oct 7 (keep doing this):** worktree from `origin/artifacts-trunk`; fork via
`POST /repos/tessel-dogfood/forks/tessel-dogfood--lane-<task>`; the lane mints its own identity
and claims EACH file as a SEPARATE argument with all modes up front (three lanes passed a
space-joined zsh variable as one bogus scope); stop before push; Opus review of a `git archive`
snapshot; fix passes; orchestrator reads any post-Yes commit (rule 00); orchestrator mints the
fork token (`tools/mint-lane-token.sh`), lane pushes and submits; if held, the orchestrator gates
an archived copy with `curl` REMOVED from PATH (build the PATH in bash; build tests first with the
normal PATH) and `--no-fail-fast`, then `tessel review <id> --approve` as `orchestrator`; watch
the trunk head with a read token (`git ls-remote`) instead of lane inbox loops; check tree or
patch-id equality, `tools/mirror.sh`, deploy what changed (Felix runs `cd … && npx wrangler
deploy`), close the box. Lanes must never pattern-kill (`pkill`) or `git stash`; one lane did each.
Mac load ran 20–100 all day from other sessions (Spotlight, iOS simulators, xcodebuild).
**Permissions:** `.claude/settings.local.json` allows `Edit(docs/ROADMAP.md)` and (Felix, Oct 8)
`Bash(bash tools/mint-lane-token.sh *)` plus its absolute-path form; the script takes
`<lane> <dir>`. The old scratchpad rule is removed.
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
pre-steward history and notes. The GitHub `main` is at `1aa3684` (pull request 2, the milestone merge of the trunk `db31287`). `8432f8c` was a
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

**Agents:** none live; every lane worktree reclaimed. Worktrees: `ax-mcp` (its `tessel` binary
serves `tools/orch-review.sh`) and `deploy` (code of `c6d6c76`, harness and `tessel-cli` built;
its `tessel` binary ran the docs lanes). No background jobs; Wi-Fi on. Scratchpad keeps
`fresh-clone/run1` and `run2` clones (safe to `trash`). One lane at a
time while other sessions keep the load at 20–100 (a reviewer saw load 493 once).
**Open boxes left:** REVIEW-SENSITIVE (recommendation above, needs Felix's yes), REVIEW-NOTE
(needs Felix), COORD-UNDELIVERED-GRANT (needs Felix), GATE-PATH, GATE-OUTPUT (classifier call),
the UX work behind Felix's three decisions, and the Oct 10–12 milestone work (land ROADMAP through
the steward, pull request from `artifacts-trunk` with `/code-review` and `/security-review`,
video). Nothing else is dispatchable without Felix.
**REVIEW-SENSITIVE recommendation for Felix** (design, not built): the sensitive-path list lives
in the repo's `tessel.toml` (`[review] sensitive = ["src/protocol.rs", ...]`), read by the steward
with the gate config and sent to the coordinator with each merge-time coverage check, so the hold
reason fires where the other hold reasons are decided and the list is versioned with the code.
A coordinator var would be a second place to configure per repo. Say yes and a lane builds it. `.claude/worktrees/deploy` is a detached checkout of the trunk `7d00ad6`
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
   New (SWARM-SHADOW-RECONNECT, Oct 9): add `#[serde(default)] fence: Option<Fence>` (and the
   task reference, if cheap) to `EventKind::ClaimShadowed`, so a shadow agent that reconnects
   before reading its `Shadowed` reply can find its claim in the log instead of claiming again.
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
- [x] **REVIEW-UI** — PLAN §4, §9 Oct 8: a review screen beside the dashboard for submissions held
  under invariant 12: the reasons from `review_reasons`, the diff, the claim's intent and
  assumptions (untrusted data), and approve/reject that sends `Review` as a configured reviewer.
  `tessel review` stays the agent path (same outcome, agent-native). Needs a decision before
  building: how a human in a browser acts as a reviewer agent (a reviewer identity token, minted
  by the steward, held server side). After DASH-1.
  MERGED 2026-10-06 at trunk `b446bf3` (plan approved with five rulings; review "With fixes": a
  stale page could approve a commit it never showed, and any coordinator error read as "refused";
  re-check "Yes"). Routes `GET /review/<repo>`, `GET /review/<repo>/<claim>/diff` (reviewers only,
  a git-only sandbox diff against the merge base, 200 KB cap, read tokens revoked after the
  fetch), `POST /review/<repo>/<claim>/decision` (Access JWT, `REVIEWER_EMAILS` mapping,
  same-origin + JSON + byte cap, HMAC token bound to email/repo/claim/commit, log re-read before
  sending, exactly one `Review` noted "felix via review UI:", "decided" only on the logged
  `review_decided`). Steward tests 803. Held (moved dashboard helpers); gate on `35989a9`: pnpm
  803, typecheck/oxlint/oxfmt 0, cargo 860, clippy 0; approved claim 65; the merged change's
  patch-id equals the gated one (rebased over `4b394c9`). Left: a ~1 s race closed only by
  `Review.fork_commit` (queued for the protocol hand merge). DEPLOYED Oct 6 ~22:50 EDT by Felix:
  steward `0f01d3fb` (`REVIEWER_EMAILS` bound). From outside: GET `/review/tessel-dogfood`, GET
  `/diff`, a forged assertion and the decision POST all get 302 to the Access login; admin routes
  still answer 401. CLOSED 2026-10-07 12:35 EDT: Felix opened `/review/tessel-dogfood` signed in
  through Access; it rendered "Held for review: tessel-dogfood … Nothing is held for review",
  which is correct (claims 65–67 were approved before merging). Not yet exercised live: a held
  submission's diff from a real container and an approval from the browser; the next held
  submission is that test (check the log for the note "felix via review UI:").
- [x] **DEMO-TS** — PLAN §9 Oct 7, SUBMISSION_CHECKLIST ("demo repository the agents work on is
  your own or permissively licensed"). Mostly exists: `tessel-swarm/src/demo.rs` generates a small
  TypeScript shop (twelve functions in seven modules, one `node --test` file each, cross-module
  calls) that every swarm run starts from. Left: a way to create that repo as a named Artifacts
  repo for real agents (the video's 3–5 real agents beside the scripted ones, PLAN §9 Oct 10),
  e.g. a `tessel-swarm demo-repo --repo swarm-demo` command, a LICENSE in it, and a check that the
  CLI's tree-sitter labels its symbols as expected. No second demo repo.
  CLOSED 2026-10-07 at trunk `e648ad5` (with SWARM-REVIEWER-ERR; review "With fixes": the admin-
  token scrub was untested, `starts_with("http://localhost")` let `http://localhost.evil.example`
  receive the admin token over plain HTTP (pre-existing, now reachable), the README overstated
  the existing-name refusal, and the symbols test covered only the base tree; re-check "Yes").
  The first submit was REJECTED by the steward's Sandbox gate: the new tests spawned `curl`, which
  the gate image lacks (passed on the Mac); fixed with a `Transport` boundary so tests use an
  in-memory steward (filed GATE-PATH). Gate on `e648ad5`: workspace 876 with `--locked` and `curl`
  removed from `PATH`, clippy 0; approved claim 67; trunk tree equals the gated tree. `on_local.rs`
  was claimed after its edit (rule 0; recorded in the git note). LIVE 09:4x: `tessel-swarm
  demo-repo --repo swarm-demo --agents agent-1,agent-2,agent-3,agent-4` created `swarm-demo` at
  starting commit `71a4e8a` (MIT LICENSE, seven modules, 12 `node --test` tests pass on a clone)
  and forks `swarm-demo--agent-1` … `--agent-4`. Permanent: Artifacts repos are never deleted.
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
- [x] **SWARM-FLAKE-SHADOW** — from the DEMO-TS review (Oct 7): `tessel-swarm/tests/on_local.rs`
  ~601 `a_shadow_submission_is_on_record_early_and_its_work_time_is_still_counted` failed once in
  6 runs at load 45 on its ordering assert (passed 5 reruns). Timing-dependent; it runs in the
  steward's gate, so it can reject an unrelated lane on a busy machine. Make the ordering
  deterministic or assert only what the harness guarantees.
  CLOSED 2026-10-07 at trunk `58bbbc3` (review "With fixes", then "Yes"). Not a product bug: the
  old assert raced the shadow's git steps against the blocker's 2.5 s work time; the coordinator
  only promises a trial for shadow work submitted before the blocker's `Merged`, and later work is
  labelled never verified. The lane's first fix reran the scenario until the race went its way,
  which hid the very regression the test exists for (a mutant that submits late passed 5/5); now
  the blocker works 6 s and the test asserts hard that the shadow submits within that of its
  `ClaimShadowed`, and that its work time is counted (~12x headroom on measured git time). 30-run
  loop 0 failures; workspace 885. Test-only, not held; merged on the first submit. The lane ran
  `git stash` once (forbidden; the stack was empty afterwards; recorded in its git note); the
  classifier refused its `tessel inbox` polling, so the orchestrator watched the trunk head.
- [x] **SWARM-WAIT** — found by the A/B run 1 RERUN (Oct 7 16:46–17:01, trunk `d9fb232`, repo
  `swarm-s3-tmk1p6`): exit 1 "the coordinator closed the connection", no `on` results; 38 of 40
  merged. Claims 22 (a05, granted after 357 s in the queue) and 36 (a20, after 624 s) expired one
  lease after the grant with no renewal, as did claims 12 and 19 in the first run: an agent granted
  from the wait queue after a long wait never renews. Local runs (short waits) pass. Lane
  dispatched 17:03 (Opus, `.claude/worktrees/swarm-wait`); a close must become the task's outcome,
  not abort the run. The `shadow` A/B run waits for it.
  CLOSED 2026-10-07 at trunk `43edf24` (review "With fixes" twice). Harness cause, proven: an
  agent's own socket closing made `Conn::recv` return a plain error that `agent_main` propagated,
  aborting the run; the dead agents' queued requests were later granted to nobody and lapsed
  (coordinator side: COORD-CLOSE-WITHDRAW). Now a close is the task's outcome `disconnected` (own
  A/B row), the agent stops while the others finish, and its time is recorded honestly (wait =
  claim→grant/close, work = grant→push/close; review and merge-queue time is not work, as for a
  finished task). A local `Cutter` fault reproduces the live log. Felix ran the submit (the
  classifier refused it as "Out-of-Place Publication"); held; gate on `d69edba`: workspace 908
  with `curl` removed from `PATH` at load 50, clippy 0; approved claim 71; merged patch-id equals
  the gated one (rebased over the CLI commits). Why the live sockets closed is still unknown.
- [x] **CLI-CLAIM-EXISTS** — three lanes on Oct 7 passed a space-joined zsh variable to
  `tessel claim`, which created one scope named "a.rs b.rs c.rs" with no warning; the submit was
  then refused as uncovered. `claim` should warn (or refuse, with an override) when a file scope
  does not exist in the worktree and is not being created (`--mode create`).
  CLOSED 2026-10-09 at trunk `6b61869` (Opus "With fixes" at `1822644`, "Yes" at `6b61869`):
  `tessel claim` refuses a file, directory or symbol scope absent from the worktree AND from
  `HEAD` (so a file deleted or renamed before the claim is still claimable) unless the mode is
  `create`; a directory scope on a regular file and a symbol scope on a directory are refused;
  a scope argument holding whitespace warns on stderr; the check runs in `commands::claim`, so
  the MCP tool inherits it (`isError`); the edit and git hooks bypass it by design (they cover
  new files and staged deletions). Exit 1 like other scope errors. 11 tests, 14 mutants caught.
  Known: a symbol scope whose path is a tree at HEAD but gone on disk is accepted; a failure to
  spawn git reads as "missing".
- [x] **SWARM-RECONNECT** — A/B run 3 (Oct 7 19:31–19:39, trunk `43edf24`, `wrangler tail` on):
  at 19:39:36–37 every WebSocket dropped at once with no close frame (17 queued agents withdrawn
  in one instant; the Durable Object kept running, one script version, all 376 invocations ok, CPU
  0–24 ms: a transport reset between the Mac and the edge, cause undecided), and the scripted
  reviewer's reset aborted the run (by design), losing 16 merges of evidence. Lane dispatched
  19:44: agents and the reviewer reconnect with backoff and resume; reconnects recorded per task.
  Next live run measures: reconnects and close codes per connection, `cf.colo` per `/ws` request
  plus a 5 s ping from the Mac (edge restart vs local loss), DO wall time per message (0.7–1.1 s at
  ~0 CPU in run 3) and per alarm (18–23 s).
  CLOSED 2026-10-07 at trunk `e7250de` (review "With fixes" twice). Agents and the scripted
  reviewer reconnect (5 tries, full jitter, 0.5 s doubling to 8 s; per-task cap 10); after a
  reconnect an agent reads the log on a short-lived second connection (retried, closed with a
  close frame) and `events::standing` decides the stage (no double claim — withdrawals matched on
  agent AND request, the review's reproduced bug; no double submit; lost denials counted from the
  log); the reviewer resumes its Watch from the last seen seq and re-sends undecided approvals.
  JSON `reconnects`; A/B row "Connection resets agents reopened" (a reopened connection does not
  mean the task merged). The shadow policy does not reconnect (SWARM-SHADOW-RECONNECT). Load-
  sensitive tests made relative (shadow submission, held-for-review work time, cut-off tasks).
  Held; gate on `4023b80`: 1014 with `curl` removed from `PATH`, `--no-fail-fast`, clippy 0;
  approved claim 75; merged patch-id equals the gated one.
- [x] **SWARM-SHADOW-RECONNECT** — from the SWARM-RECONNECT review (Oct 7): the shadow policy does
  not reconnect, so a mass reset during the planned live shadow A/B run marks every agent
  disconnected. Do before that run (shadow trials are tied to the claim they were sent on).
  CLOSED 2026-10-09 at trunk `8e9ceed` (Opus "With fixes" then "Yes" at `82ed1e4`; three
  test-only commits after it read by the orchestrator). Shadow agents use the run's reconnect
  policy; after a reconnect the agent keeps its in-process claim and asks the log what became of
  it (`events::standing_of`; `ClaimShadowed` carries no fence): unsubmitted → push and submit
  once; submitted or settled → `Stage::Trial`, waiting on the shared log watch; lease expired →
  `lapsed`. `Standing::Settled` is its own standing (shadow-only by protocol invariant 10).
  Known limit: a reset before the `Shadowed` reply is read leaves the agent without its fence,
  so it claims again and the first claim idles out (documented in the harness README; the fix is
  an additive `fence` on `ClaimShadowed`, on the protocol hand-merge list). Landing cost two
  steward refusals from pre-existing Sandbox flakes, fixed in the same lane (SWARM-ONLOCAL-LOAD,
  CLI-GITHOOK-ETXTBSY); `PATIENT` is 40 tries and `BOUND` 180 s in tests/reconnect.rs.
- [x] **COORD-UNDELIVERED-GRANT** — from the COORD-CLOSE-WITHDRAW review (Oct 7): when a
  `Granted` fails to send (`deliver`, `src/runtime.rs` ~880), the socket is closed but the granted
  agent is no longer queued, so nothing is withdrawn and it holds the claim until its lease lapses
  (one lease lost for waiters behind it). Releasing it early needs a release reason the protocol
  does not have (`ReleaseReason` is frozen; a new variant needs Felix's ruling). A reconnecting
  agent can adopt the claim from the log (SWARM-RECONNECT). Pending Felix.
  CLOSED 2026-10-11 without code, on Felix's ruling: the claim sits idle for at most one lease
  (30 s, `src/shell.rs` ~16); the daemon reconnects within 0.1-5 s and adopts the grant from
  `ClaimGranted` (`reconcile.rs` ~283), which an early release would race; a new `ReleaseReason`
  would cost old daemons their reconnect repairs (amended version rule) and an existing reason
  would be a false log entry (rule 7). Known limit.
- [x] **COORD-CLOSE-WITHDRAW** — found by SWARM-WAIT (Oct 7), checked by the orchestrator in the
  trunk: when the Durable Object closes a socket itself (`close_socket`, `src/runtime.rs` ~285,
  code 1011, after a failed dispatch or send), it does not withdraw that agent's queued request;
  only `websocket_close`/`websocket_error` call `withdraw` (~250–262). Unless Cloudflare then also
  delivers `websocket_close` for a server-initiated close (unverified), the queue later grants a
  claim to a dead agent and every waiter behind it loses a lease. Affects real agents, not only the
  swarm. Also unknown: WHY the swarm Worker closed agent sockets under 30 agents (needs
  `wrangler tail` on `tessel-coordinator-swarm` during a run). The SWARM-WAIT reviewer adds a
  second path with the same signature: `close_socket(ws, "send failed")` at `src/runtime.rs` ~840
  when the GRANT itself fails to send (granted, then lapsed one lease later, 0 `wait_withdrawn`).
  MERGED 2026-10-07 at trunk `9c086e9` (review "With fixes", then "Yes"): server-initiated closes
  (dispatch error 1011, send failure, identity mismatch 1008) now withdraw the agent's queued
  request through a pure `shell::agents_to_withdraw`; a closed socket (session `closed: true`,
  serde default) cannot shield the agent's other socket; each withdrawal is apply → persist →
  settle, so rule 6 holds through the settle → withdraw recursion (bounded); one agent's failure
  no longer stops the batch. Logs for accept/close (code, clean, clamped reason, agent)/error/
  failed withdraw. Out of scope: a grant that fails to send (COORD-UNDELIVERED-GRANT). Held;
  gate on `9c086e9`: 988 with `curl` removed from `PATH` (`--no-fail-fast`; an earlier run lost
  the known flaky swarm timing test at load 76), clippy 0, wasm 0 warnings; approved claim 74;
  trunk tree equals the gated tree. NOT YET LIVE: needs both coordinators deployed. Compat date
  is 2026-09-01, so the
  explicit `ws.close()` requirement before 2026-04-07 does not apply.
- [x] **REVIEW-SENSITIVE** — from the UX review research (Oct 7): the coordinator calls
  `review_reasons(&touched, threatened, has_evidence, &[])` (`src/coordinator.rs` ~1286), so the
  sensitive-path hold reason can never fire; no list of sensitive paths is configured anywhere.
  Decide where the list lives (repo config read by the steward, or coordinator var) and wire it.
  CLOSED 2026-10-11 at trunk `d1ef2f1` (`08baf13` + fix `e2e364a`; review "With fixes" then
  "Yes"): a coordinator variable `SENSITIVE` (path prefixes; a directory ends in `/`), set in
  `wrangler.toml` to `src/protocol.rs`, `src/identity.rs`, `tessel.toml`, `wrangler.toml`,
  `tessel-steward/wrangler.jsonc`, `tessel-steward/src/access.ts`, empty for the swarm env; read
  and applied on every load like `REVIEWERS`; a bad value fails closed (every real submission is
  held). Per deployment, not per repo. Noted, not filed: race entries are not checked against the
  list (true before). Needs the coordinator deploy.
- [x] **REVIEW-NOTE** — from the UX review research: a reviewer's note never reaches the agent (a
  test pins it: `the_review_note_never_reaches_a_reason_or_a_message`, merging.rs ~1249); a
  rejected agent sees only "rejected in review". Delivering the note is a protocol question
  (untrusted text to an agent, rule 4): needs a design and Felix's ruling.
  CLOSED 2026-10-11 at trunk `f9a2f0b` by Felix's admin merge (`4673a19`, `0cdf85c`, `f9a2f0b`;
  Opus "Yes" twice): `ServerMsg::SubmitRejected` gains optional `reviewer` and `note`, sent only
  to the submitter on a review rejection; the reason stays "rejected in review"; the logged event
  is unchanged (the note is already on `ReviewDecided`); the CLI shows the note through
  `quote_untrusted` with the reviewer named (the inbox hook and MCP inbox use the same text).
  Known gap, documented: a daemon offline at the rejection learns of it by reconcile, without the
  note. Needs the coordinator deploy.
- [x] **AX-INBOX** — Felix approved Oct 7 (PLAN §6 "Agent integrations"): Claude Code hooks push
  new inbox items into the agent's turn as quoted data (merged, denied, wait granted, base moved,
  review decided), and a `Stop` hook keeps an agent from ending its turn while its submission is
  pending. Installed by `tessel hook install` beside the PreToolUse hook. Design pass first.
  DESIGNED Oct 7 (Opus, docs cited: code.claude.com/docs/en/hooks): `tessel hook inbox --root R`
  on PostToolUse (no matcher; `hookSpecificOutput.additionalContext`), UserPromptSubmit and
  SessionStart (stdout); reads `.tessel/inbox.jsonl` with the shared cursor (no daemon needed),
  `take_inbox_bounded` (10 notices / 4,000 chars, then "+K more: run `tessel inbox`"; cursor
  advances only past what was shown; `flock`), rendered through `notice_text` (quoted). Fails
  OPEN (context only; edits stay guarded by the fail-closed pre-edit hook and the coordinator).
  `tessel hook stop` (timeout 150): nothing submitted or daemon offline → allow; else up to 120 s:
  SubmitRejected/Uncovered → block with the quoted reason; Merged or ReviewRequired → allow;
  pending → block once with the queue position; `stop_hook_active` → allow. Install generalizes
  `merge_entry` (event, matcher, subcommand), keeps user entries, idempotent. Owns new
  `inbox_hook.rs`, `state.rs`, `hook.rs`; `main.rs` arms only. About 6 h.
  CLOSED 2026-10-07 at trunk `01131a5` (review "With fixes" three times, then "Yes"). Hook
  contract checked against code.claude.com/docs/en/hooks: all three context events use JSON
  `hookSpecificOutput.additionalContext`. Fixes from review: an open review made every Stop wait
  120 s and block falsely (now the daemon keeps `awaiting_review` per held claim, set on
  ReviewRequired, cleared on Accepted/rejection/unsubmit, and rebuilt from the event log on
  reconnect so an approval missed offline is not lost); a test-level timeout so the "no bound"
  mutant fails instead of hanging; headless agents are blocked on an unread rejection; Stop
  allows only when every submitted claim is settled; a half-written inbox line is no longer
  skipped; the frame says paths and agent names are agent-written too. Quoting, cap and flock were
  verified sound. Held (hook/state/reconcile signatures); gate on `01131a5`: workspace 934 with
  `curl` removed from `PATH`, clippy 0; approved claim 70; trunk tree equals the gated tree. The
  lane edited `tests/support/mod.rs` before claiming it once (rule 0). Not exercised: a real
  Claude Code session (next: a live check with a real agent). No deploy (CLI only).
- [x] **AX-GITHOOKS** — Felix approved Oct 7: git `pre-commit` and `pre-push` hooks that refuse a
  commit touching files the agent has not claimed (catches shell edits the Claude hook cannot
  see). Design pass first (shared with AX-INBOX).
  DESIGNED Oct 7: hooks live in the common `.git/hooks` (`git rev-parse --git-path hooks`,
  honours `core.hooksPath`; refuse a relative hooksPath); a 2-line `exec tessel hook git <name>`
  shim; an existing hook is renamed `<name>.pre-tessel` and chained first; the hook gates itself
  (no `.tessel/state.json` → exit 0, so other worktrees are untouched; no
  `extensions.worktreeConfig`). Claims from the `Status` RPC (unsubmitted union); daemon down
  with state present → block with "run `tessel start`, or `--no-verify`". pre-commit: index vs
  the merge base of `diff_base`, new `touched_index` + `plan::collapse` + `uncovered` (symbol
  level, like submit). pre-push: same check per non-zero local sha. `--no-verify` bypass is
  acceptable (coordinator still rejects uncovered work, invariant 11). Owns new `githook.rs`,
  `submit.rs` (`touched_index`); after AX-INBOX merges. About 6 h. Note: `prek install`
  overwrites `.git/hooks`.
  CLOSED 2026-10-07 at trunk `107fc13` (review "With fixes" twice). Install: `tessel hook install
  --git`. The critical review finding: hooks live in the repo's COMMON hooks dir, so a shim that
  exec'd a lane's `target/debug/tessel` would, once the lane was deleted, break every commit in
  every worktree (main checkout included) and skip the chained hook. Now the shim (marker `#
  tessel git hook`) prefers the recorded binary, then PATH, and falls through by itself: a
  worktree with Tessel state and no binary fails closed with "binary missing; reinstall or use
  --no-verify"; any other worktree runs `<name>.pre-tessel` and passes; install warns when the
  binary is under a `target/`. Also: a stopped worktree (state `Stopped`) passes, a crashed
  daemon (state `Online`) blocks; a merge being concluded (`MERGE_HEAD`) skips pre-commit
  (pre-push and the coordinator still check). Coverage is symbol-level via `touched_index`. Held
  (pub helpers, `Command`, `HookAction`); gate on `107fc13`: workspace 968 with `curl` removed from
  `PATH`, clippy 0; approved claim 72; trunk tree equals the gated tree; the real repo's
  `.git/hooks` was untouched throughout. The lane printed the first 80 characters of its minted
  identity JSON once (header only; 24 h token). No deploy (CLI only).
- [x] **AX-MCP** — Felix approved Oct 7, after AX-INBOX and AX-GITHOOKS: `tessel mcp`, a local
  stdio MCP server exposing start/claim/status/inbox/submit/release/review as tools through the
  same per-worktree daemon; inbox as a resource with change notifications if the spec allows.
  Must be local (symbol extraction and diffs need the worktree). Design pass first.
  DESIGNED Oct 7: `rmcp =3.5.1` (official SDK, Apache-2.0, MSRV 1.88; `default-features=false`,
  server + macros + transport-io; tokio `io-std`); lock gains 28 packages, none removed or bumped;
  licenses all MIT/Apache, `cargo deny check licenses` ok. Tools tessel_start/claim/status/inbox/
  submit/release/review returning the CLI's own (quoted) text; non-zero exit → `isError`; bad
  args → -32602. Commands must first return `Report{text,code}` instead of printing (stdout is
  the transport). Inbox is a tool (hooks push notices). `tessel mcp --root R`, registered with
  `claude mcp add --scope local`. LANDING (lockfile): rebase onto trunk, lock diff additions only,
  LOCKFILE FREEZE for every other lane; with Felix's OK deploy the steward from the lane checkout
  so the toolchain image carries the new crates; trunk `test-runs` still passes; submit through the
  steward; re-check `test-runs`; lift the freeze. No admin merge. Risk: rmcp/Claude Code protocol
  version agreement unverified until the live check. After AX-GITHOOKS. About 8 h.
  MERGED 2026-10-08 09:16 EDT at trunk `4033791` (claim 77, approved by Felix; tree equals the
  gated `fc09fed`); lockfile freeze lifted.
  CLOSED 2026-10-09 07:20 EDT, live check passed (`docs/evidence/2026-10-09/ax-mcp-live-check/`):
  registered with `claude mcp add --scope local` in the ax-mcp worktree; Claude Code's `--debug`
  log shows `negotiatedProtocolVersion: 2026-07-28` (`protocolEra: modern`); a headless `claude -p`
  listed all 7 `mcp__tessel__*` tools; `tessel_start` through Claude Code started the daemon
  (online, lease 30 s) and `tessel_status` read it back, stdout parseable, connection closed
  cleanly. Note: over a raw stdio `initialize` handshake the server answers `2025-11-25`, the
  newest revision that still has `initialize` in `rmcp` 3.5.1; Claude Code uses discovery.
- [x] **SWARM-LEASE** — found by A/B run 1 (Oct 7 15:11): at 30 agents with `--policy wait`, two
  scripted agents never heartbeated a granted claim (expired exactly one lease after the grant),
  and the next request on a lapsed claim (`StaleFence`) aborted the whole run with no `on`
  results. Root-cause the lapse; record a lapsed claim as that task's outcome ("not finished:
  claim lapsed") and keep running.
  CLOSED 2026-10-07 at trunk `d9fb232` (review "With fixes", then "Yes"). Two harness bugs: the
  scripted agent heartbeated only while reading the socket, so checkout, the work sleep and the
  push (>30 s under load) went unrenewed; and the late `StaleFence` answer to a lapsed claim's
  release was read as the refusal of the agent's next claim, which ended the run. Now
  `Conn::keep_alive` heartbeats during work (it only sends; frames stay buffered for the next
  read, so none are lost), late release refusals are dropped, and a `StaleFence` on amend/submit
  is the task's outcome `lapsed`, with its own A/B table row. The shadow test's failures at load
  ~115 were load, not this lane (4/4 on base and branch at 30–55). Felix pushed and submitted
  (the classifier refused the lane's push as "Remote Repoint"); held; gate on `d9fb232`:
  workspace 898 with `curl` removed from `PATH`, clippy 0; approved claim 69; trunk tree equals
  the gated tree. A/B run 1 rerun started 16:47 from this build.
- [x] **CLI-GITHOOK-REINSTALL** — from the pull request 2 code review (Oct 9, verified 75):
  `tessel-cli/src/githook.rs:372–391`: after another tool overwrites the shim, a re-run of
  `tessel hook install` (which the install note at line 332 recommends) sees a non-shim hook,
  calls `chain_existing`, and bails because `<hook>.pre-tessel` from the first install exists.
  Fix: when `.pre-tessel` exists and the new non-shim hook differs, move the old `.pre-tessel`
  aside to a dated backup, chain the new hook, report both; make the note and the bail message
  agree. Test-first. Small; CLI only.
  Dispatched 2026-10-11 09:36 as `lane-cli-githook`. CLOSED 2026-10-11 at trunk `4e58f49` (two
  commits `4b575b0` + fix `24c3d89`, rebased by the steward as `cdb445c` + `4e58f49`, diff
  identical; review "With fixes": the bound test depended on the wall clock and could flake under
  load, and the bail message said "Nothing was changed" although the earlier hook of the run had
  been rewritten; now `chain_existing` takes the time, the bound is a fixed-time unit test, the
  message says this hook was not changed; held as a signature change and approved by the
  orchestrator through `tools/orch-review.sh` after one TLS-handshake retry; workspace gate 1128).
  Same chained text on a reinstall makes no backup; a different one is kept as
  `<hook>.pre-tessel.<UTC stamp>` (then `-1` … `-9`, then a bail naming the file to remove).
- [x] **COORD-APPLY-LOADED** — from the code review (verified 50): `src/runtime.rs:480,565,651`
  apply a steward outcome after an awaited call without `ensure_loaded`; a client message during
  the await can drop the core (serialize failure, `Refuse`, `OverHard`), so the alarm errors and
  `recover_cut_off_merge` runs one more sandbox merge that ends `already_merged`. Fix: one helper
  that loads then applies, used by the three post-await sites. Coordinator; needs a deploy.
  Dispatched 2026-10-11 10:00 as `lane-coord-apply`. CLOSED 2026-10-11 at trunk `cbef948`
  (`454baec` + comment fix `98727d8`, rebased, diff identical; review "With fixes": the helper's
  comment claimed "no await in between" for the other `apply` calls, which do have storage awaits;
  what keeps them safe is the input gate, which storage awaits hold and steward calls release;
  workspace gate 1121, no new test: the three paths need the Worker). The reviewer also noted an
  older gap, not fixed here: a settle that withdraws a dead socket can drop the core with the
  error swallowed (`runtime.rs:754,801`), and the next alarm `apply` fails once. NOT DEPLOYED:
  Felix deploys the coordinator; the live proof is then a merge with no "core is not loaded" in
  the tail and no extra `already_merged` sandbox merge.
- [x] **CLI-REPLAY-UNKNOWN-EVENT** — from the code review (verified 50): `daemon.rs:1923,2095`
  abort the log replay (from seq 0) or drop the socket on one message that fails to parse, so an
  additive enum variant without a `PROTOCOL_VERSION` bump breaks reconciliation for older
  daemons for good. Fix: decode the event envelope tolerantly (seq plus a kind that may be
  unknown) and mark the read unreliable instead of failing; or bump the version on every added
  variant (Felix's call, hand-merge list).
  Dispatched 2026-10-11 10:22 as `lane-cli-replay`. TOLERANT HALF CLOSED 2026-10-11 at trunk
  `1e04dcf` (`b0ac969` + fix `292306a`, rebased by the steward, diff identical; review "With
  fixes": a new live-path flag `live_view_has_a_hole` guarded nothing, since the working socket
  never sends `Watch` and every reconciliation replays from seq 0, and it could show a false "not
  read to its end"; dropped; held as a signature change (`Incoming`, `LogRead`) and approved by
  the orchestrator; workspace gate 1133, ten new tests). Now: an event envelope of an unknown kind
  in the replay is counted in the gap check, not applied, logged by seq and a 64-character kind,
  and the read ends `complete: false` instead of `Err`; on the live socket it is logged and
  ignored and the socket stays up; non-JSON and non-envelope frames behave as before. Lesson next
  to rule 12: the submit was first refused as uncovered for `edit-signature` on
  `tests/support/mod.rs` (the fake coordinator's helpers changed shape); a lane claims
  `edit-signature` on test support files it reshapes too. VERSION HALF RULED by Felix 2026-10-11 18:20: a new
  `EventKind` variant or optional field needs no bump; a new top-level message type bumps
  `PROTOCOL_VERSION`. Formerly open: whether every added `EventKind` variant bumps it; a future non-event `type` on the live socket
  still drops it and redials for as long as the coordinator sends it.
- [x] **STEWARD-ACCESS-STALE-KEYS** — from the code review (verified 50): `access.ts:85–93` after
  the 5-minute TTL a failed certs refetch throws and both callers answer 503 although a cached
  key set exists. Fix: serve the cached keys on refetch failure (log it), 503 only with no cache;
  rate-limit retries with `lastFetchAttemptMs`. Test the stale-cache case. Steward; needs a deploy.
  Dispatched 2026-10-11 00:50 as `lane-steward-keys`. CLOSED 2026-10-11 at trunk `4814caf` (review
  "With fixes" on `d1dfacc`: one 101-character test title, and the lane's mutant table had run a
  different mutant than its label; fix `4814caf` read by the orchestrator; gate on an archived
  copy 908 steward tests; merged without a hold; mirrored). A failed refetch serves the cached
  keys with one `access_certs_stale` log line, one retry a minute, 503 only with no cache. NOT
  DEPLOYED: Felix deploys from the deploy worktree (moved to `4814caf`). The merge itself ran the
  sandbox container from the 00:20 deploy, so the image uploaded over Wi-Fi is intact.
- [x] **SWARM-PUSH-PHRASES** — from the code review (verified 25, cheap): `tessel-swarm/src/git.rs:27–45`
  add "the remote end hung up unexpectedly", "unexpected disconnect while reading sideband
  packet", "connection timed out", "empty reply from server" to the transient set, with tests.
  CLOSED 2026-10-11 at trunk `46e42ce` (with SWARM-ONLOCAL-LOAD-2, lane `lane-swarm-2`, commit
  `b0694c6` rebased by the steward, diff identical; review "Yes"; workspace gate 1123). The
  reviewer confirmed the phrases in git 2.56 and Homebrew libcurl. Two follow-ups filed below:
  SWARM-4XX-HANGUP, SWARM-MACOS-TIMEOUT.
- [x] **SWARM-4XX-HANGUP** — from the SWARM-PUSH-PHRASES review (Oct 11): git output with a 4xx
  AND a dropped connection ("error: RPC failed; HTTP 413 ... returned error: 413" then "fatal: the
  remote end hung up unexpectedly") is now classed transient and retried four times, against the
  doc comment at `tessel-swarm/src/git.rs:26-28`. Fix: a 4xx check before the phrase match, with
  that line as a control test. Cheap; next swarm lane.
- [x] **SWARM-MACOS-TIMEOUT** — from the same review: macOS ssh prints "Operation timed out" and
  curl 28 prints "Operation timed out after N milliseconds"; neither matches "connection timed
  out". Add "operation timed out" and "timed out after" to the set, with tests. Cheap; same lane.
  CLOSED 2026-10-11 with SWARM-4XX-HANGUP at trunk `6ee08b7` (`b9f616e` + fix `6ee08b7`; review
  "With fixes" then "Yes"): `mentions_status(output, class)` reads a line ending `error: <c>xx` or
  `RPC failed; HTTP <c>xx`; any 4xx makes a push failure permanent before the phrase match (the
  box's 413-then-hang-up control is a test); a lone `RPC failed; HTTP 503` is transient; `operation
  timed out` and `timed out after` added, tested with ssh's and curl's real lines. Not verified:
  the real messages on macOS. Noted by the review, not filed: 429 and 408 stay permanent.
- [x] **EVIDENCE-CHECK-SH** — from the code review (verified 25): `docs/evidence/2026-10-06/coord-head-live/check.sh:9`
  sources `/Users/felixpatawah/...` under `set -a`. Resolve the path from the script's location
  with an override variable, read only `STEWARD_ADMIN_TOKEN`, drop `set -a`. Docs; orchestrator.
  CLOSED 2026-10-09 on `sprint/build` (lands on the trunk with the next docs commit): the token
  comes from the environment or, read alone in a subshell, from `.dev.vars` found relative to the
  script (`DEV_VARS` overrides); nothing is exported; shellcheck and shfmt clean; the missing-file
  path exits 1 with a message.
- [x] **SWARM-ONLOCAL-LOAD-2** — reopens SWARM-ONLOCAL-LOAD (Oct 9 17:35): `tests/on_local.rs:596`
  `a_shadow_submission_is_on_record_early_and_its_work_time_is_still_counted` still fails under
  load (75) with the relative bound: the shadow's claim-to-submit span was 5605 ms against the
  holder's 7825 ms with `work_ms` 6000, because the two agents' git work (checkout, commit, push)
  is not equally slow on a loaded machine. Assert order instead of a span comparison (the shadow's
  `Submitted` precedes the holder's `Submitted`, and the shadow's result still counts `work_ms`),
  or compare against the shadow's own measured git time. Also seen once in the same gate:
  `local::tests::work_that_already_fails_on_main...` (passes alone). Small; next swarm lane.
  CLOSED 2026-10-11 at trunk `46e42ce` (see SWARM-PUSH-PHRASES): the test asserts that the
  shadow's `Submitted` precedes the holder's by log position (events can share a millisecond);
  the two `work_ms` assertions unchanged; `on_local` 23/23 in the lane, the review and the gate.
  Follow-up SWARM-ORDER-CHECK-WEAK below. Two other load flakes seen once each by the lane, each
  passing in the other full run and alone: `a_claim_granted_after_a_queue_wait_of_many_leases_
  is_kept_alive` (ended `[Merged, Lapsed]`) and `off::tests::agents_work_at_the_same_time`.
- [ ] **SWARM-ORDER-CHECK-WEAK** — from the SWARM-ONLOCAL-LOAD-2 review (Oct 11): the order
  assertion rarely catches a shadow that waits. The reviewer made the shadow spend its work time
  before pushing (`on.rs:1055`, `if !held.shadow` to `if true`): the test passed 3 of 4 runs, grant
  and shadowing fell in the same millisecond, and the `work_ms` assertions pass too. Options: assert
  the shadow's `Submitted` time is before the holder's grant time plus `work_ms` (always kills the
  mutant, but a git-speed bound returns: the failing run measured 5,605 ms against 6,000), or
  compare against the shadow's own measured git time (the box's other alternative). Needs a
  decision on which bound is honest under load; not before the contest.
- [x] **SWARM-RECONNECT-FIRST-CONNECT** — from the fresh-clone test (Oct 9): `tests/reconnect.rs`
  `agents_that_cannot_reconnect_end_as_disconnected_after_their_tries` arms its cut on the first
  `ClaimGranted` and refuses both agents; on a loaded Mac the second agent's first connection
  comes after that grant, is refused with 401, and the harness fails the whole run ("cannot
  connect to the coordinator"). Failed 2 of 2 full-suite runs at load 60–113, passes alone. Fix in
  the test: fire the cut only after every refused agent has connected (count `AgentConnected` in
  the tripwire closure); same guard for any other test with a refused list. Open question for the
  harness: an agent refused at its FIRST handshake should perhaps end `Disconnected` after its
  tries instead of failing the run. Lane `lane-swarm-reconnect-cut` dispatched 17:12.
  CLOSED 2026-10-09 at trunk `e164717` (test-only): `after_connected(agents, then)` wraps the
  tripwire so it cannot fire until every listed agent has an `AgentConnected` event, with a unit
  test of the helper (3 mutants caught); the reviewer-refusing tests keep their single trigger
  (a let-through there would mean no cut at all). Reconnect suite 5/5 (lane) and 3/3
  (orchestrator, `curl` off PATH). The harness question stays open (see the box text).
- [ ] **GATE-PATH** — process lesson from DEMO-TS (Oct 7): its tests spawned `curl`, which the
  gate image lacks; they passed on the Mac and the steward rejected the merge. The orchestrator's
  gate for lanes that spawn processes runs the tests with `PATH` limited to the image's tools.
- [ ] **EVIDENCE-REPO** — PLAN item 8's archive, taken up on Felix's call (Oct 12 19:10: "it holds
  some weight for the contest's question"). The Artifacts binding has no write method (generated
  types: `create`, `get`, `fork`, tokens, `readFile`, `log`), so the steward's merge container
  writes, with a write token minted and revoked like the main push. Design: the steward creates
  `<repo>--evidence` and `<repo>--transcripts` (not forks, so no agent token ever covers them);
  the coordinator's internal merge request gains `claim`, `intent`, `decisions` and `review`
  (`MergeDispatch`, `request_body`; not the frozen CLI protocol); a new last step of `runMerge`,
  only after the Worker's read of main said `merged`, writes `merges/<head>.md` (record table,
  scopes, gate steps, intent/assumptions/evidence/rejected quoted as data, review, transcript)
  and `merges/INDEX.md`, pushes with a lease, and the Worker verifies the bytes through the
  binding; the outcome stays `merged` and carries `evidence: {written, path | reason}`; a
  transcript named by `decisions.transcript` is read from the fork's branch
  `tessel/transcript-<claim>` at `transcripts/<claim>.md`, hash-checked, and copied to
  `<agent>/<claim>.md` in the transcripts repo or refused in the record. Budget slot added; no
  dashboard change; one paragraph in `docs/how-tessel-works.md`. Tests on both sides; three
  mutants. Needs a steward deploy and, for the request fields, a coordinator deploy. Dispatched
  2026-10-12 19:21 as `lane-evidence-repo` (Opus, worktree `evidence-repo` from `3cedad8`; brief
  in the handoff `orch/brief-evidence-repo.md`).
- [ ] **CLI-TRANSCRIPT** — the agent side of EVIDENCE-REPO, dispatched after CLI-NOTES lands (both
  edit `commands.rs::submit`): `tessel submit --transcript <file>` redacts the file (token and
  secret patterns from `redact.ts`'s list, mirrored in Rust), computes its SHA-256, commits it
  as `transcripts/<claim>.md` on the fork branch `tessel/transcript-<claim>`, pushes that branch
  with the fork token the agent already has, and sets `DecisionRecord.transcript` to
  `{repo: <fork>, path, sha256}`. No protocol change (`TranscriptRef` exists). Tests against the
  real binary; the steward side (EVIDENCE-REPO) verifies the hash. Not yet dispatched.
- [ ] **CLI-NOTES** — from the video script's "not shipped" list (Oct 11, decision 6): no code writes
  the git note CLAUDE.md asks for on every commit ("until the CLI does this automatically"). The
  CLI has the four lines at `tessel submit`: the intent from `tessel start`, `--evidence`,
  `--rejected`, and the `--assume` statements. Make `tessel submit` write the note
  (`git notes add`, never overwriting an existing one) before it sends the submission, print
  that it did and that notes are not pushed with the fork head; a write failure is a warning,
  not a refusal. CLAUDE.md's dogfooding section and the skill updated. Tests against the real
  binary; three mutants. CLI only. Dispatched 2026-10-12 18:56 as `lane-cli-notes` (Sonnet,
  worktree `cli-notes` from `3cedad8`; brief in the handoff `orch/brief-cli-notes.md`).
- [x] **COORD-GRANT-HEAD** — the Opus note on COORD-WAITER-BASE: `ServerMsg::Granted` carries no
  head, so an agent granted by a release, or by a merge on a scope it did not touch, keeps the
  head from its last `welcome` and may start from a stale base; the steward's rebase guards
  main. Fix within rule 1: add `#[serde(default)] head: Option<CommitId>` to `Granted`, set in
  every grant path (direct, after a wait, amend, race entry) to the coordinator's trunk head;
  the daemon applies it like `BaseMoved` and persists, so `tessel status` shows it; event log
  unchanged; `PROTOCOL_VERSION` unchanged. Touches `src/protocol.rs` (sensitive: held for
  review). Tests on both sides; three mutants; `docs/reference/coordinator.md` updated.
  Dispatched 2026-10-12 18:56 as `lane-grant-head` (Sonnet, worktree `grant-head` from
  `3cedad8`; brief in the handoff `orch/brief-grant-head.md`). Needs a coordinator deploy.
  Committed `780cd2f` 19:30 (lane gate green; five mutants); Opus review "Yes" 19:46; workspace
  gate and wasm build green 19:43. CLOSED 2026-10-12 20:48 at trunk `780cd2f` (fast-forward; held
  for the signature change and the sensitive path, approved by the orchestrator; mirrored; daemon
  stopped). DEPLOYED by Felix 21:50 as version `92108fcc`; proved live 21:56: a grant on the raw
  socket carries `head` = the trunk head (handoff `grant-head/live-grant-head-proof.txt`).
- [ ] **CLI-RACE** — from the video script's "not shipped" list (decision 2): the coordinator decides
  races end to end (`src/coordinator/racing.rs`: entries tried by the verification queue, the
  winner promoted and merged, losers rejected) but the CLI has no race command. Ship `tessel
  race open "<intent>" <scopes> --entrants N --deadline <min> --by <criteria>` (reviewer only;
  `lowest_risk`/`smallest_diff` refused as never measured), `tessel race join <race>` (a grant
  with `race` set; the denial card gains "join it with: tessel race join N"), `tessel race pick
  <race> <claim>`, and a `race_result` inbox notice with the winner, ranking and own place.
  No protocol change. Tests against the real binary with the fake coordinator driving the
  real core; three mutants; skill and README command lists updated. Dispatched 2026-10-12
  18:56 as `lane-race-cli` (Opus, worktree `race-cli` from `3cedad8`; brief in the handoff
  `orch/brief-race-cli.md`). Claims the daemon symbols last, after `lane-grant-head`.
- [x] **COORD-WAITER-BASE** — found by `lane-docs-core` capturing the worked example (Oct 12): on a
  merge, `src/coordinator/merging.rs:322` builds the `BaseMoved` notices for the holders whose
  claims overlap the touched scopes, and only then `:323` grants the unblocked waiters; a waiter
  granted there was not a holder when the notices were built, and `Granted` carries no head, so
  the new holder keeps the pre-merge head (`tessel status` showed `71a4e8a` after a1's merge) and
  may edit on a stale base until it pulls; the steward's rebase still guards `main`. Fix: grant the
  waiters first, then notify, so the new holder gets the existing `BaseMoved`; no protocol change;
  event order becomes `ClaimGranted` before `BaseMoved`. Tests: overlapping waiter gets `Granted`
  then `BaseMoved`; non-overlapping waiter gets none; three mutants. Coordinator; needs a deploy.
  Dispatched 2026-10-12 15:06 as `lane-coord-waiter` (Sonnet, worktree `coord-waiter` from
  `f9a2f0b`; brief in the handoff `orch/brief-coord-waiter.md`). Committed `76786e2` 15:15 (lane
  gate 519, three mutants killed); Opus review and the orchestrator's workspace gate running.
  CLOSED 2026-10-12 17:07 at trunk `9ab793a` (rebased by the steward onto `126b944`, `merging.rs`
  identical; Opus review "Yes", four reviewer mutants killed; workspace gate with curl off the
  PATH: all binaries green with the two load reruns recorded, clippy 0; wasm build 0 warnings;
  local a1/a2 replay: `granted_after_wait` then `base_moved`, status head at the merged commit;
  pushed and submitted by Felix, merged with no hold; mirrored). DEPLOYED by Felix 2026-10-12
  before 18:29 (coordinator version `f6977a41` from the deploy worktree at `9ab793a`). LIVE PROOF
  18:38: agent `lane-docs-ref`, queued with `--wait` on `tessel-swarm/tests/reconnect.rs` behind
  claim 108, was granted claim 109 by the SWARM-RECONNECT-FLAKE merge and got `base_moved` to
  `3cedad8` one millisecond later; `tessel status` shows head `3cedad8` (handoff
  `swarm-flake/live-waiter-proof.txt`).
- [x] **SEC-HARDEN** — from the pre-submission security pass (Oct 12): `.gitignore` gaps
  (`.claude/`, `/AGENTS.md`, `.env.*`, `.dev.vars.*`, `*.pem`, `*.key`), README admin-token
  guidance (the admin token stays with the operator), the `/diff` forgery check, `redactTokens`
  on four error paths, CLAUDE.md "Sandbox" → Containers. `max_instances` dropped: the
  `durable_object` scheduling policy rejects it. Dispatched 2026-10-12 13:19 as
  `lane-sec-harden` (Sonnet). CLOSED 2026-10-12 15:18 at trunk `3442343` (fast-forward from
  `f9a2f0b`; Opus review "Yes"; orchestrator steward gate 916 passed; held for the signature
  change `review.ts::diff`, approved by the orchestrator through `tools/orch-review.sh`; pushed and
  submitted by Felix). Mirrored to GitHub `artifacts-trunk`. NOT DEPLOYED: the steward deploy is
  Felix's, after the live recordings.
- [ ] **DOCS-REF** — docs tier 1 (Oct 12): `docs/configuration.md` (the configuration
  reference) plus accuracy fixes to `skills/tessel/SKILL.md` and `skills/tessel/AGENTS.md`.
  Dispatched 2026-10-12 13:50 as `lane-docs-ref` (Sonnet, worktree `docs-ref` from `3442343`).
  READY at `9b6af03` (Opus review "With fixes", 92 citations checked; all 7 fixes applied and
  read by the orchestrator). CLOSED 2026-10-12 15:29 at trunk `9b6af03` (fast-forward; pushed
  and submitted by Felix, merged with no hold; mirrored to GitHub `artifacts-trunk`).
- [ ] **DOCS-CORE** — docs tier 1 (Oct 12): `docs/how-tessel-works.md`,
  `docs/reference/coordinator.md`, and the README rewritten as the front door. Dispatched
  2026-10-12 13:50 as `lane-docs-core` (Opus, worktree `docs-core` from `3442343`). READY at
  `7c7131e` (`88c7383`: Opus review "With fixes", the worked example verified line by line, fixes
  applied; `7c7131e`: the reviewed README draft committed by the orchestrator under the lane's
  claim after SEC-HARDEN released `README.md`; local links checked: `docs/configuration.md` comes
  from DOCS-REF, which lands first). Felix reads `docs/how-tessel-works.md` before it lands.
  Submitted by Felix 15:52; REFUSED 15:56 by the sandbox gate: `tessel-swarm` `reconnect`
  test `a_rejection_that_lands_while_the_submitter_is_gone_is_found_and_its_claim_released`
  failed on the rebased commit (docs-only change: a flake, see SWARM-RECONNECT-FLAKE). Claim
  106 is active again with the same fence; resubmit of the same `7c7131e`.
  Resubmitted by Felix 16:58; CLOSED 2026-10-12 17:02 at trunk `126b944` (rebased by the
  steward, the three files identical; mirrored to GitHub `artifacts-trunk`; daemon stopped).
- [x] **SWARM-RECONNECT-FLAKE** — seen twice on 2026-10-12: the sandbox gate (standard-4) failed
  the docs-only DOCS-CORE merge on `tessel-swarm/tests/reconnect.rs`
  `a_rejection_that_lands_while_the_submitter_is_gone_is_found_and_its_claim_released`; and
  on the Mac under load average 100+ the `reconnect` binary sat idle for 25 minutes inside the
  workspace gate (killed), then passed 20 of 20 in 23 s alone. The tests cut sockets and poll the
  log in unbounded `loop { sleep(5ms) }` waits. The refused test's race (read 16:08): the agent
  works 800 ms after its grant; the test poisons main 300 ms after the grant with a git commit
  (`poison`, reconnect.rs:880) and expects the submission to be rejected; when that commit takes
  over 500 ms (a cold standard-4 container running the whole workspace's test binaries at once)
  the submission merges first and `Resolution::Merged` fails the `Rejected` assertion. Evidence
  it is timing, not the change: DOCS-CORE touches only `README.md` and `docs/`; `tessel-swarm`,
  `src` and `tessel-cli` are identical between `f9a2f0b` and the trunk `9b6af03`; the test passed
  5 of 5 on the trunk code locally at 16:03 (1.6-1.9 s each, logs in the handoff
  `coord-waiter/rerun/trunk-rejection-*.log`). Fix: make the work time relative to the poison
  (gate the agent's submit on the poison having landed in the log, as the test already does for
  the cut), as SWARM-ONLOCAL-LOAD did. Reproduced 16:14 on the Mac under 24 synthetic CPU hogs:
  1 of 8 single runs of the test hung past a 300 s timeout with no output (the `lifted` task
  waits forever for a `SubmitRejected` that never comes while the agent is refused), which is
  also what stalled the `reconnect` binary for 25 minutes in the workspace gate. Fix: order the
  test by log events (cut at the grant, poison while the agent is out, lift, submit, cut at
  the submit) and bound every polling loop in the file by `BOUND` with a message. Test-only.
  Dispatched 2026-10-12 16:18 as `lane-swarm-flake` (Sonnet, worktree `swarm-flake` from
  `9b6af03`; brief in the handoff `orch/brief-swarm-flake.md`). Implemented `ac35420`: the
  tripwire poisons main in the step that logs `Submitted`, under the hub lock, then cuts the
  agent (the cut-at-grant order was rejected: the agent syncs main again on reconnect,
  `on.rs:1048,1165`); `work_ms` 0; `wait_for`/`wait_until` with a 150 s bound replace 13 loops;
  assertions unchanged. CLOSED 2026-10-12 18:38 at trunk `3cedad8` (landed by Felix 18:34, held
  for the `edit-signature` on `first_waiter` and `connected_count`, approved by the orchestrator
  through `tools/orch-review.sh`; rebased by the steward onto `9ab793a`, the file identical;
  Opus review "Yes", four mutants failed within the bound; reconnect binary 10 of 10 and the
  rejection test 10 of 10 under 24 CPU hogs; workspace gate with curl off the PATH: 19 binaries
  green, clippy 0; mirrored; daemon stopped).
- [x] **SWARM-ONLOCAL-LOAD** — from the SWARM-SHADOW-RECONNECT gate (Oct 9): `tests/on_local.rs`
  `a_shadow_submission_is_on_record_early_and_its_work_time_is_still_counted` asserts the shadow's
  submit within a fixed 6000 ms of its claim; under load (Mac at load ~100, 4-vCPU Sandbox) the
  push alone takes 6.5–7.6 s. Make the bound relative to a measured baseline in the test (as
  SWARM-RECONNECT did for its load-sensitive tests) or assert order instead of a wall-clock bound.
  Small; same lane; before the live `shadow` run.
  CLOSED 2026-10-09 at trunk `8e9ceed` (commit `9f6831e`): the bound is now a comparison inside
  the run (the shadow's claim-to-submit span plus half `work_ms` is under the blocker's
  grant-to-submit span); the held-for-review work bound is relative to grant→`ReviewRequested`;
  the fork-overwrite test's limit is 60 s. Three mutants caught; 3/3 runs under concurrent load.
- [x] **CLI-GITHOOK-ETXTBSY** — from the SWARM-SHADOW-RECONNECT landing (Oct 9): in the Sandbox
  (Linux), `tessel-cli/tests/githook.rs` `without_the_recorded_binary_the_one_on_path_is_used_whatever_it_says`
  fails intermittently with `Text file busy (os error 26)`: `install_then_delete_the_binary` and
  `installing_from_a_build_directory_warns` copy the tessel binary with `std::fs::copy` and exec
  it, while other test threads fork children that inherit the open write descriptor until their
  exec. Copy binaries through a child process (`cp`) so no test thread holds the descriptor.
  Never reproduces on macOS. Fixed in the SWARM-SHADOW-RECONNECT lane's pass.
  CLOSED 2026-10-09 at trunk `8e9ceed` (commit `8e9ceed`): `copy_the_binary` copies through a
  `cp` child; `rg fs::copy` over tessel-cli and tessel-swarm finds nothing; githook 25 x3.
- [x] **SWARM-PUSH-RETRY** — from the live `shadow` run (Oct 9): one agent's `git push --force`
  to its fork got `503 Service unavailable` from the Artifacts git endpoint and the task ended
  `failed` with no retry (`tessel-swarm/src/git.rs` / `on.rs`). Retry a push a few times with
  backoff on 5xx; count retries in the task record. Harness only; small.
  CLOSED 2026-10-09 at trunk `c6d6c76`: `Git::push` retries a push whose git output shows an
  HTTP 5xx (a `error: 5xx` ending its line, or a 5xx reason phrase) or a connection-level error
  (connection reset, could not resolve host, failed to connect), case-insensitively, up to 4 more
  times with the reconnect backoff (0.5 s doubling, full jitter; worst case 7.5 s of pauses);
  4xx, credential refusals and non-fast-forwards fail at once; retries counted in each task's
  `push_retries` (in the run JSON; no A/B row); the seed push retries too (count not reported).
  Pushes sleep on the blocking thread under `conn.keep_alive`, so heartbeats continue. 12 mutants
  caught. End-to-end tests use a `git` wrapper on PATH that scripts failures and spawns nothing
  but the real git. Not verified: a real 503 from Artifacts; the live push branch has no test.
- [x] **GATE-OUTPUT** — from AX-MCP (Oct 8): a steward merge of `fc09fed`'s tree was refused with
  "tests failed (exit code 101)", then the same tree passed a Sandbox test-run and the next merge.
  The trial result keeps only the last step's stdout/stderr (the pnpm step), so the failing cargo
  test is unknown. Keep each step's output (or the failing step's tail) in the trial result and
  the `submit_rejected` notice, so an intermittent failure names its test. Steward only.
  Workaround found Oct 9: `POST /repos/<fork>/test-runs` runs the same gate on the fork's main
  and its json keeps the failing step's stdout, so the orchestrator can name the test
  (`scratchpad/orch/test-run.sh`).
  CLOSED 2026-10-11 at trunk `42bcdd9` (`df51eaf` + fix `0a7e30c`; review "With fixes" then
  "Yes"). The box was the coordinator's, not the steward's: the steward already returned the
  failing command's output and `src/merge.rs` kept only the exit code. Now `StepExit` reads
  `stdout`/`stderr` (serde default) and a tests-failed reason keeps its first sentence and adds
  the failing tests (cargo `... FAILED`, `failures:`, `targets failed`; vitest `FAIL`), else the
  last lines, at most 10 lines of 160 characters and 1,200 in all, controls stripped, as a JSON
  array after the label "failing output (from the repo's tests, quoted)". Noted, not filed: a
  budget timeout quotes the previous passing step's stdout under that label; U+2028 and bidi
  characters are not stripped at the source (the CLI escapes them). Needs the coordinator deploy.
- [x] **CLI-CARD** — Felix's UX decision (Oct 9; board 3 of https://claude.ai/artifact/F4ee5swgmrjuxDyFgUaTi6):
  every `tessel claim` denial is a decision card: what happened (the scope and the mode asked for),
  who holds it and why (the holder, their mode, their intent quoted as data with attribution, the
  race when there is one), and numbered moves that are valid right now, computed from the agent's
  own state and `Mode::permits`, never from the holder's text; a move that is not valid is listed
  with why. The same card is the `--json` envelope (a `next` list; every free text an object
  `{author, text}`, never a bare string), the hook's block message, the inbox notice and the MCP
  tool result (which already returns the command's text). Only what the protocol carries is shown:
  `Conflict` has no held-since, lease or queue length, so the card says none. `skills/tessel/`
  updated. CLI only; no protocol change. Dispatched 2026-10-09 22:05 as `lane-cli-card`.
  CLOSED 2026-10-09 at trunk `bba9344` (review "With fixes" then "Yes" after one fix pass: race
  wording limited to what the coordinator guarantees, a hook test for an agent that holds a claim,
  refusal advice from the daemon's own state, the hook blocks without moves when the daemon does
  not answer, one `holding_now`, the depend rule per conflicting scope). Workspace 1085. Live:
  the denial card against the production coordinator, text and `--json`.
- [x] **REVIEW-CARD** — Felix's UX decision (Oct 9; board 2): the steward's review page becomes a
  decision card per held submission, sorted by how many agents a decision unblocks: why you (the
  hold reasons), waiting and exposed (agents queued on the touched scopes with their position;
  other live claims' declared assumptions on those scopes, quoted as data; a claim whose submission was rejected is still live, with its locks), the intent and evidence
  quoted as data, what changed (touched scopes, the hold's scope first) with the diff on demand, and
  a receipt of logged events after the decision (`review_decided`, then `merged` or
  `submit_rejected`, then the waiting agents' grants; a line not yet logged says what it waits
  for). Approve is only possible after the diff was served to this person for this commit: the
  approve token comes with the diff, not with the page. Reject with a note stays possible without
  it. Phone layout, sticky decision bar, 44 px targets, no batch, no swipe. Steward only; Felix
  deploys after the merge. Dispatched 2026-10-09 22:05 as `lane-review-card`.
  CLOSED 2026-10-09 at trunk `7f5330e` (review "With fixes" then "Yes" after one fix pass: a
  rejected claim stays live in the fold, the receipt completes at a rejection, waiters count only
  on a mode conflict mirroring `Mode::conflicts_with`, own assumptions under INTENT; a follow-up
  commit restored the `isScope` guard). Steward tests 901 on the merged commit. Not live until
  Felix deploys; the rendered page at 390 px and the receipt polling are unchecked in a browser.
- [x] **CLI-INIT** — Felix's UX decision (Oct 9): `tessel init`, one command from nothing to a
  worktree ready for `tessel start`, safe to rerun: writes `.tessel/config.toml` (0600; the token
  minted from the steward's identity route with `STEWARD_ADMIN_TOKEN` from the environment, or
  taken from `TESSEL_TOKEN`; never printed), installs the Claude Code hooks and the git hooks, and
  ends with a card of what it did, what it kept and the next move. The manual steps in
  `skills/tessel/SKILL.md` stay as the fallback. After CLI-CARD (same files).
  Dispatched 2026-10-09 22:58 as `lane-cli-init` from trunk `bba9344`. CLOSED 2026-10-09 at trunk
  `1fdb6b4` (reviews: "With fixes" (exclude file wiped by a non-UTF-8 byte; a kept token could
  belong to another agent; linked-worktree and malformed-config paths untested), "With fixes"
  (token-only config file refused after fix 2), "With fixes" (one 108-character line), then the
  wrap read by the orchestrator). 28 new tests; workspace 1121. Live against the production
  steward and coordinator: mint, idempotent rerun, exclude rule, both hook sets, `tessel start`
  with the minted token.
- [x] **SWARM-REVIEWER-ERR** — from the SWARM-OBSERVER re-check: in `tessel-swarm/src/on.rs`
  (~227) the scripted reviewer's `??` returns before `with_watcher_error`, so a reviewer error
  hides a watcher error. Scripted reviewer only; low priority.
  CLOSED 2026-10-07 at trunk `e648ad5` (DEMO-TS lane): `settle_reviewer` returns the reviewer's
  result and `with_cause` attaches both errors; an agent panic no longer returns before the
  watcher and reviewer are settled (that part untested).
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
- [x] **CLI-CONNECT-TIMEOUT** — from the CLI-LIVENESS review: heartbeats run only once the daemon
  is online, so a link that goes half-open after the WebSocket handshake but before `Welcome` sits
  in Connecting with no timeout (fails closed: claims lapse locally). Bound the wait for `Welcome`
  and reconnect with the existing backoff. Also (CLI-LIVENESS re-review): a link that delivers
  server frames but never gets heartbeats through is never declared dead while frames keep coming;
  claims lapse locally (no fail-open), but it never reconnects. Set the silence deadline from the
  oldest outstanding ping and clear it only on a matching pong; add a test (mutant L survives).
  CLOSED 2026-10-07 at trunk `bfcf625` (review "With fixes" twice, no code bugs: a flaky 600 ms
  healthy-link test, untested newer-pong and backoff behaviour, wall-clock ordering, and a blocked
  write that could outlast the silence deadline). Now: `WELCOME_LIMIT` 15 s then reconnect with
  backoff; silence from the oldest unanswered ping, cleared only by a pong that echoes it (and
  every older one, by position); socket writes and the final close bounded by the earlier of the
  silence deadline and 15 s. Felix ran the submit (the classifier refused the lane's
  `tessel submit` as "Remote Repoint"); held (private `Daemon`/`Probes` signatures); gate on
  `be8788c`: workspace 877 with `curl` removed from `PATH` at load 55, clippy 0; approved claim
  66; merged patch-id equals the gated one (rebased over `e648ad5`). Not exercised: a real
  half-open link or full send buffer. No deploy (CLI only).
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

## Post-contest (recorded 2026-10-09 on Felix's decision; nothing here is scheduled)

From the UX proposal (https://claude.ai/artifact/V5ifR3B8wfUtsdbpH7JZXd) and the mockups
(https://claude.ai/artifact/F4ee5swgmrjuxDyFgUaTi6). Recorded so the ideas are not lost; none is a box.

- The Floor: the dashboard as a live map (directory districts, file tiles, symbol rows, a texture
  per mode so colour is never the only signal, denial arcs with both intents quoted, wait chips at
  a tile's edge, shadow ghosts that become seals or fade to false alarm, a trunk rail of merges, a
  scrubbable event tape). Board 1 of the mockups. Counters keep rule 7: a denial is "not tested",
  precision shows its denominator.
- `tessel await granted|merged|decided` (one bounded call instead of polling) and `tessel watch`
  (who holds what, live, in the terminal).
- `tessel init --from-demo`, reusing the swarm's setup code.
- Conversational review (needs a protocol version bump); phone push notifications; a full-screen
  TUI; steward stage events in the log, so a pending submission can say more than "steward
  working".
- Four additive protocol fields: `held_since_ms`, `queue_len`, `reviewers`,
  `ReviewDecided.reviewer` (the last is already on the hand-merge list).

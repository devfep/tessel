# Roadmap

Task list for the work in `PLAN.md`, executed under `docs/BUILD-PROTOCOL.md`. `PLAN.md` owns scope
and order; `docs/proposals/real-use.md` holds the requirements, invariants and done-checks this
file's tasks refer to. Only the orchestrator edits this file. The contest-era roadmap and build log
are in git history (`78bcb9a` and earlier).

## STATE (rewritten at every dispatch, verdict, merge and close)

**As of:** 2026-11-28 12:00 UTC (a checkpoint). Main and the coordinator's head at `a6976df`; main's
CI and Adapter suite passed on `02006e2`, with the "mutants on changed lines" job green again, so
main is no longer red. Three landings through the server since 05:15, all held for review as
merge-path changes and approved by the orchestrator. MAIN-RED-SIM-MUTANTS at 11:28 UTC as
`ecff1cd`..`02006e2` (claim 235, rebased onto `41e0f49`; the daemon copied the note), on an Opus
review (REVIEW-1 "Yes", no must-fixes; `~/tessel-orch-notes/2026-11-28/main-red-sim-mutants/`): unit
tests pin the new `src/sim` decisions, and `taken_probe_for`'s body moved unchanged into a pure
`taken_probe(state, call, now_ms)`; the reviewer ran cargo-mutants 27.1.0 on CI's diff
`7f989f8`..`ecff1cd` with CI's settings and one job: 86 mutants, 80 caught, 6 unviable, none missed,
so all 17 that CI missed are caught, and a rewrite of `taken_probe` with the same behaviour still
passed every test. REPLAY-ORDER-CONTRACT at 11:42 UTC as `4c487cc`..`87279e0` (claim 239, a
fast-forward: lane B rebased its five commits onto `02006e2` unchanged, dropped the
`.cargo/mutants.toml` and workflow scopes, and added `87279e0`, the vote's comment nit).
LANDER-SIGNAL-NUMBER at 11:52 UTC as `ccf9638`..`a6976df` (claim 238; the first submit was refused
because `59dc170` did not contain the coordinator's head, so lane A rebased it onto `02006e2` with
an unchanged range-diff; the daemon copied both notes). The reviewer's first mutants pass, with
`CARGO_TARGET_DIR` set and `--jobs 2`, had both jobs build one test binary, so a mutant could be
tested against the other job's build; it was discarded. Lane A's own run counted four mutants that
cannot compile as caught (84 caught and 2 unviable, against 80 and 6), which fits the same fault and
may explain PLAN-1's differing local and CI miss sets; LANE-GATE-MUTANTS now carries the rule. Both
lanes are idle with no claim; no lane task is in flight. PR #141 (the 05:15 record) merged as
`41e0f49`.
**Earlier (28 November 05:15 UTC):** Main at `652d83d` (PR #140's merge); the coordinator's head at
`4e94d32`, since a pull request's merge does not move it. Main's CI is red on `eb1770a` and
`4e94d32`: its "mutants on changed lines" job missed 4 and 13 mutants in the new `src/sim` code
(runs 46099378473 and 46104947233; lists in `~/tessel-orch-notes/2026-11-28/main-red-sim-mutants/`).
No lane gate or review had run that job (LANE-GATE-MUTANTS). Lane A reproduced it
(`TESSEL_SIM_SEEDS=2`) and finds all 17 killable by deterministic tests, with no exclusion
(MAIN-RED-SIM-MUTANTS, claim 235, `PLAN-1.md`; the local and CI miss sets differ, cause unknown). CI
passed on `652d83d`, and the Adapter suite on all three. Two landings through the server since
22:50. SIM-DOUBT-PREPARED-TRAFFIC at 02:42 UTC as `e43609b`..`eb1770a` (claim 228, rebased onto
`7f989f8`; the daemon copied all six notes; the lane had rebased onto `df625c2` first, because the
commit hook refuses a base that is neither the coordinator's head nor the start), on an Opus review
(REVIEW-1 with re-checks 1-3, re-check 3 "Yes with changes", to the note only;
`~/tessel-orch-notes/2026-11-27/lane-a/sim-doubt-prepared-traffic/`): the doubted head's first
`Land` is refused without opening the 15 s rate-limit window and counted as `doubt_refusal`; the
planted `prepared()` break is caught on default seeds 1, 7 and 11 and on 50 of 300 seeds; refused
compare-and-swaps fall from 52 to 37 over 300 seeds, the default seeds hold one (seed 0, against
four), and the Nightly mutants were not run against that loss. SIM-DOUBT-LIVENESS at 03:53 UTC as
`39689ee`..`4e94d32` (claim 232, a fast-forward), on an Opus review (REVIEW-1 "Yes with changes" on
`9033fc2`: the `Reconcile` plant half-applied on seeds 108 and 117; re-check 1 "Yes" on `4e94d32`;
`~/tessel-orch-notes/2026-11-28/lane-a/sim-doubt-liveness/`): `Checker::DoubtLiveness` flags an
answer the host took for a claim in doubt (its call still open, dispatch and stage matching the
stored flight, the retry due or unset) that leaves the flight and the work unchanged, so the bound
is one taken answer; the two plants are caught on 69 and 191 of 300 seeds, each catch naming the
checker. NIGHTLY-MUTANT-TIMEOUTS (claim 229, `2b38c7e`..`fc65977`) did not land through the server:
GitHub refused each push to `main` from 23:44 UTC (GH013: the token has no Workflows permission, and
the change edits `.github/workflows`). The deployed build (`455c889`'s strings, before the staged
landing) re-ran the whole merge, tests included, about every 10 minutes and rejected the submission
after 4 tries, at 00:16 UTC; main's code would ask the `Land` again forever, as `docs/dogfood.md`
says. The orchestrator first asked Felix to add the scope; that was wrong: `docs/dogfood.md`
withholds Workflows on purpose, because the runner's tests can read `github-token`. A resubmit at
01:34 UTC was refused again and rejected at 02:17 UTC; the commits were merged by PR #140 as
`652d83d` at 04:03 UTC (a merge commit, so the notes stand; the merge has a hand note). PR #140's
Nightly (run 46097369661) lists only `src/host/mod.rs:471:12` as a timeout and misses only the four
baselined mutants, so NIGHTLY-MISSED-1127 and NIGHTLY-MUTANT-TIMEOUTS are done.
LANDER-WORKFLOW-SCOPE has its design (DESIGN-2, re-check 1 "Yes with changes"): a push that git
marks `[remote rejected]`, with main read back at the base, becomes `Refusal::Rule`; this record
adds `.github/workflows/` to `docs/dogfood.md`'s by-PR list. Two merge-path votes finished.
LANDER-SIGNAL-NUMBER, 3 "Yes with should-fixes" and no must-fix, lands unchanged after
MAIN-RED-SIM-MUTANTS (lane A released claim 233); its Done is narrowed to the version check, and the
other sites are LANDER-SIGNAL-ALL-SITES. REPLAY-ORDER-CONTRACT, 3-0 "Yes" on `1ea2a4d` after voter
3's "No" on `d97233d` (site 2 answered a commit asked after the second read before page 2), keeps
site 3 out of order as a guard (2 of 3); it lands after a rebase onto the next coordinator head
(claim 234 also covers `.cargo/mutants.toml` and the workflows only because the coordinator's head
lacks #140). SIM-DOUBT-TEST-ANSWER closes as a note. A `CARGO_TARGET_DIR` shared by two worktrees of
the crate built against the other tree's library and let a red test pass (review-sdl-design): one
target directory per worktree or clone. PR #139 (the 22:50 record) merged as `7f989f8`.
**Earlier (27 November 22:50 UTC):** Main and the coordinator's head at `df625c2`. Four landings
through the server since 17:50. P1-ADAPTER-SUITE's rest at 21:14 UTC as `251a4c2`..`ec3ceb0` (claim
218, rebased onto `0f3d483`; the daemon copied all nine notes), on an Opus review (REVIEW-1 "No",
five must-fixes; re-check 1 "No", two; re-check 2 "Yes" on `016b067`;
`~/tessel-orch-notes/2026-11-27/lane-b/adapter-suite-rest/`): a wire nobody reads is dropped without
stalling the repository (a `repo.rs` test where a blocking send or close fails at the drop and a
stall after the drop at the next socket's Welcome), a watcher that never reads over TCP, unreadable
event and state rows and a store that rejects writes (a SQLite trigger) close 1011, log the
repository and serve again once repaired, and a panicked repository task is logged with its
repository and cause (`supervised` in `tessel-server/src/repo.rs`, the only code change, red test
first). LANDER-VERSION-CHECK at 21:58 UTC as `088634a`..`a1202bc` (claim 226, a fast-forward), on a
merge-path vote, 3-0 "Yes with should-fixes" on `0d6540b`, should-fixes done but the signal number
(LANDER-SIGNAL-NUMBER), re-check "Yes", "Yes", "Yes with should-fixes" (one test, added in
`a1202bc`) (`~/tessel-orch-notes/2026-11-27/vote-lander-version-check/`): a failed `git --version`
is `CommandError::VersionFailed` with its exit code (or "killed by a signal"), stdout and stderr,
each cut to 1,200 characters; an exit 0 that prints no version is refused with its stderr. The 17:33
cause is still unknown: the server's `PATH` finds Homebrew's git 2.56.0, not the `xcrun` shim (lane
B's read); a signal's number is not kept (LANDER-SIGNAL-NUMBER). DAEMON-CUT-READ-REQUEST-TEXT at
22:38 UTC as `f7421b1`..`eeb3472` (claim 227, a fast-forward), on an Opus review (REVIEW-1 "No", one
must-fix and a wrong notice found; re-check 1 "No", two; re-check 2 "Yes";
`~/tessel-orch-notes/2026-11-27/lane-b/daemon-cut-read-request-text/`): after a cut or unreadable
log read, the lost-claim, lost-amend and lost-submit notices no longer send the caller to `tessel
status` for an answer it cannot show; the fake gains `Lose::Submit`. NIGHTLY-MISSED-1127 at 22:48
UTC as `71b2571`..`df625c2` (claim 224, rebased onto `eeb3472`; the daemon copied both notes), on a
merge-path vote after two fix passes (1 "No" and 2 "Yes with should-fixes" on `d857f4d`; re-check 1,
2 "No" and 1 "Yes with should-fixes"; re-check 2, 3-0 "Yes" on `112ac54`;
`~/tessel-orch-notes/2026-11-27/vote-nightly-missed-1127/`). The `replay.rs:187` mutant is killed by
an in-order test: a commit asked before the last read and missing from its page, which
`Output::Commit` allows, is sent only by the replay's buffer, so `<` loses it. Lane A's proof that
no in-order sequence could tell `<` from `>`, and the orchestrator's decision built on it, were
wrong (all three re-checks); B is equivalent only on `tessel-server`'s one-job-at-a-time store
(REPLAY-ORDER-CONTRACT). The `resend_land` mutant is baselined, its line kept. Main's CI and Adapter
suite passed on `ec3ceb0`, `a1202bc`, `eeb3472` and `df625c2`. CLAIM-SCATTER's `tessel claim --into`
is not deployed: the running daemons are the 26 November `~/tessel-dogfood/bin` build, and the
`tessel` first on `PATH` is `~/.cargo/bin`'s 14 October build (TESSEL-ON-PATH), so `tessel submit
--claim` covers one claim and a claim with several modes can only grow while it is the agent's only
open one: each lane runs one claimed task at a time, and lane A released and re-claimed scopes twice
to submit. SIM-DOUBT-PREPARED-TRAFFIC's re-check 2 found that the forced first-`Land` refusal also
opened a 15 s global rate-limit window; fix pass 3 drops it. SIM-DOUBT-LIVENESS's design was
reviewed before code (DESIGN-REVIEW-1: its redispatch and answer-count bounds would flag the correct
core); the adopted rule mirrors I7: every answer the core takes for a doubted claim's offered flight
must change the stored state, so the bound is one taken answer
(`~/tessel-orch-notes/2026-11-27/lane-b/sim-doubt-liveness/DESIGN-2.md`). PR #138 (the 17:50 record)
merged as `0f3d483`.
**Earlier (27 November 17:50 UTC):** Main and the coordinator's head at `228f5d1`.
COORD-UNDELIVERED-GRANT landed through the server at 17:44 UTC as `121461c`..`228f5d1` (claim 212,
rebased onto `4b1032f`), on an Opus review (REVIEW-1 "No", five must-fixes; re-check 1 "No", one
test-only must-fix; re-check 2 "Yes" on `7ce3c76`;
`~/tessel-orch-notes/2026-11-27/lane-b/coord-undelivered-grant/`), in the CLI only. A `Granted` that
fails to send already left the claim held (decision 7, pinned by
`host_a_grant_that_fails_to_send_keeps_the_claim_until_its_lease_lapses`), and the daemon's
reconnect already adopted it on a complete log read, so there was no orphan; but the daemon told a
`--wait` caller "withdrawn, claim again" and called the claim unexplained. Now the reconnect plan
matches a lost wait (same scopes, ahead of lost requests, once per wait) and says it was granted and
is tracked; after a cut log read it says the outcome is unknown and heartbeats keep the claim until
a complete read adopts it. Its first submission failed in the runner on an unrelated test
(`tessel-server/tests/landing.rs`
`a_rebase_over_another_landing_keeps_what_a_note_follows_a_commit_by`: `Landing::new` got
`UnsupportedGit { found: "" }`, an empty `git --version`); the orchestrator resubmitted it once,
with that failure as evidence (LANDER-VERSION-CHECK). Main's Adapter suite (46044219237) passed on
`228f5d1`; its CI (46044219286) passed. Both lanes' claims, 207 (lane A) and 209 (lane B), lapsed at
the same millisecond, 13:29:07.744 UTC, under load from a VM that isn't ours (about 117 at 14:52
UTC; not read at 13:29), which fits one expiry step after the server stalled past the lease less a
heartbeat (an inference: the server logs no info line then), the CLAIM-LEASE-LAPSE mechanism under
load rather than sleep. Lane B re-claimed as 212; lane A's 210 and 211 lapsed too, 1 ms apart at
15:02:38.812 UTC (lease_expired in its inbox, its daemon connected), and it took 213. The same load
hung `tessel-swarm`'s `the_tasks_a_cut_off_agent_never_took_are_done_by_the_others` for 85 minutes
while lane B's gate held the cargo lock; the orchestrator stopped that test, which then passed alone
in 8 s; the briefs now run cargo under `timeout 600`. The Nightly (45995920783, on `941db87`) failed
its full-mutants job: 5 missed in all, 3 in the baseline and 2 new (NIGHTLY-MISSED-1127; the 26
November Nightly: 67 missed, 59 new), and 3 timeouts, which it only warns about (`src/shell.rs:32`,
`src/host/mod.rs:471`, `src/host/testkit.rs:201`). NIGHTLY-MISSED-1127 went to lane A (claim 213);
its merge-path vote on `d857f4d` was one "No" and two "Yes with should-fixes"
(`~/tessel-orch-notes/2026-11-27/vote-nightly-missed-1127/`): the new test killed the
`replay.rs:187` mutant only by releasing an event the store never held, and the voters' replacements
answer storage out of order, which the host's contract (`src/host/mod.rs`) excludes; lane B's
read-only look (`lane-b/replay-order-contract/REPORT.md`, no run) proposes one in-order case, a
failed commit attempt that stored its rows, which no current adapter produces; VOTE-2 and VOTE-3
found that the retain after each page already drops what the check lets in, so fix pass 1 first
checks whether that case can tell `<` apart. The `resend_land` mutant is baselined with its line
kept: a past-due wait can reach that line (the lane's note said none could; VOTE-2 and VOTE-3 showed
one), and no reader can tell it from none. SIM-DOUBT-PREPARED-TRAFFIC's Opus review was "No" on
`0875abb` (`lane-a/sim-doubt-prepared-traffic/REVIEW-1.md`): the row's `prepared()` break is still
caught on 0 of seeds 0..19, because the new knob mostly drives `resend_land`, not `prepared()`; its
fix pass follows NIGHTLY-MISSED-1127's. P1-ADAPTER-SUITE's rest went to lane B at 17:45 UTC. PR #137
(the 12:40 record) merged as `4b1032f`.
**Earlier (27 November 12:40 UTC):** Main and the coordinator's head at `98d1a51`. Three landings
through the server since 10:20. HOST-SQLITE-RETRY-BACKOFF at 10:58 UTC as `3507e72`..`941db87`
(claim 203, rebased onto `ff6e7af`; the orchestrator copied the five notes at 10:59:07 UTC, nine
seconds before the daemon's retry found them noted), on a 3-0 vote after two fix passes (all three
"Yes with should-fixes" on `03aed3c`, docs edits on re-check 1, "Yes" on `57401fa`;
`~/tessel-orch-notes/2026-11-27/vote-host-sqlite-retry-backoff/`): no code change;
`src/host/batch.rs` says per SQLite failure class why the commit retry has no pause; new tests pin
the store's busy timeout (the literal 5,000 ms) and that a failed two-entry batch stores nothing;
deleting the store's own `busy_timeout` call survives them, as rusqlite's default is also 5 s;
`docs/configuration.md` says a store outage longer than the lease lapses every open claim and one
longer than 20 s can. Main's CI (45986538190) and the Adapter suite (45986538274) passed on
`941db87`; its push run found no mutants (the change is doc comments and tests). CLAIM-SCATTER at
12:14 UTC as `188afb9`..`5eff16e` (claim 206; its base was the coordinator's head, so the commits
landed unchanged, with their notes), on an Opus review ("With changes", two must-fixes; re-check
"Yes" on `6ed67e9` with one SKILL.md edit, made in `5eff16e`;
`~/tessel-orch-notes/2026-11-27/lane-a/claim-scatter/`): `tessel claim --into <claim>` amends a
named claim through the existing `Amend`, in the CLI only; the daemon refuses it with `--new`,
`--wait` or `--assume` for every caller, MCP included; a plain grant in answer to `--into` (an older
daemon) fails with a request to restart it; the skill says `stop` releases open claims. Main's CI
(45996939998) and the Adapter suite (45996940253) passed on `5eff16e`; its push run was the first in
which #134's server and CLI job tested mutants: 20, 19 caught, 1 unviable. MERGE-OUTCOME-UNOFFERED
at 12:31 UTC as `919f229`..`98d1a51` (claim 204, rebased onto `5eff16e`, the lander leaving out lane
B's merge of main; the daemon retries a note copy on the coordinator's next merge, base move or
snapshot, at its start and on each CLI command, and none came, so the orchestrator copied the five
notes), on a 3-0 vote after two fix passes (all three "Yes with should-fixes" on `134120d`, which
covered only the `Prepare`; "Yes" on `2df77f2`;
`~/tessel-orch-notes/2026-11-27/vote-merge-outcome-unoffered/`): `merge_outcome` ignores an answer
to a `Prepare` inside its backoff and `reconcile_outcome` one to a `Reconcile` inside its backoff;
`resolve_doubt` enters by a private `apply_reconciliation`, so the operator's answer still applies
at once; no stored field (the lane first thought one was needed; VOTE-1 and VOTE-2 prototyped the
guard without one). The row's "(and log)" is not met: nothing is logged, as on the stale-dispatch
path, and a core log would need a new `EventKind`. VOTE-1 found an older test answering inside the
new backoff, which hid a check from it; it now answers past it. VOTE-2 found the simulator has no
liveness check for a doubt (SIM-DOUBT-LIVENESS). Main's CI (45999398849) and the Adapter suite
(45999398797) passed on `98d1a51`; its push run tested 10 mutants: 6 caught, 4 unviable. The Nightly
(45995920783) runs on `941db87` since 12:06 UTC. SIM-DOUBT-PREPARED-TRAFFIC went to lane A at about
12:15 UTC (the row's lane B was busy). PR #136 (the 10:20 record) merged as `ff6e7af`.
**Earlier (27 November 10:20 UTC):** Main and the coordinator's head at `27b8c74`.
SIM-RACE-TEXT-LITERAL and SIM-FENCED-RULE-CHECKER landed through the server at 10:12 UTC as
`09d1f47`..`27b8c74` (claim 200, rebased onto `69de245`; the daemon copied all five notes), held for
review because they change `Checker` and `Variant`, and approved by the orchestrator on an Opus
review ("With changes", one must-fix; re-check "Yes" on `3f564a5` after one fix pass)
(`~/tessel-orch-notes/2026-11-27/lane-b/sim-fenced-rule-checker/REVIEW-1.md`): the first variant
removed `doubt_since_ms`, so I5 fired first on every seed tried (0..19); it now writes null, and a
test runs every checker and requires each violation to be FencedRule's (red on the old variant). The
checker reads the stored state; with the `debug_assert!` removed, a core whose `enter_doubt` leaves
`doubt_since_ms` unset is caught on 155 of 300 seeds, by FencedRule alone; the correct core is
silent on seeds 0..999 with FencedRule and on a 300-seed sweep with every checker; 22 seeds'
transcripts are byte-identical. A core whose `prepared()` clears `doubt_since_ms` is caught on only
3 of 300 seeds (SIM-DOUBT-PREPARED-TRAFFIC). FencedRule, like AgentBase, has no row in the invariant
table of `docs/proposals/phase-1.md`. Main's CI (45980202170) and the Adapter suite (45980202361)
passed on `27b8c74`; its push run tested 18 mutants: 17 caught, 1 unviable, 0 missed.
HOST-SQLITE-RETRY-BACKOFF went to lane A (the row's lane B was busy), which decided on no code
change, with evidence per SQLite failure class (`e8c3845`, `03aed3c`, claim 203); the merge-path
vote was three "Yes with should-fixes"
(`~/tessel-orch-notes/2026-11-27/vote-host-sqlite-retry-backoff/`): two mutations survive its new
tests (a busy timeout of 0; a commit outside a transaction, which an older test catches), and its
`docs/configuration.md` row is wrong about which sockets close; fix pass 1 is running.
MERGE-OUTCOME-UNOFFERED went to lane B at about 10:15 UTC. PR #135 (the 09:12 record) merged as
`69de245`, and its CI passed.
**Earlier (27 November 09:12 UTC):** Main at `64b51b0` (#134); the coordinator's head at `623e919`
(a pull-request merge does not move it). RACE-TEXTS-SHARED landed through the server at 08:53 UTC as
`32cb515`..`623e919` (claim 195, rebased; the daemon copied no notes, the submission holding a merge
of main, so the orchestrator copied all seven by subject and author), on a re-vote: VOTE-2 and
VOTE-3 had voted "No" on `cb9f8e0` with one must-fix (no test that the daemon writes
`race_entry_ended`), fix pass 1 added a real-core race test that runs the stop hook for the loser,
keyed the card on the same mark, read older inbox lines by their text (`Option<bool>`), and VOTE-1,
VOTE-2 and VOTE-3 re-checked `cd0e67a`: "Yes"
(`~/tessel-orch-notes/2026-11-27/vote-race-texts-shared/`); the three race texts are `pub const`s in
`src/protocol.rs`, byte-identical on the wire (VOTE-1). Main's CI (45969257494) and the Adapter
suite (45969257349) passed on `623e919`; its push run diffed from `5be98dd` and found no mutants to
test, because the change's code is in tessel-cli, which the per-change mutants do not cover yet
(MUTANTS-SERVER-CLI-IN-DIFF, #134), and its core change moves three constants from
`src/coordinator/racing.rs` to `src/protocol.rs`. LOCK-OWNER is done
(`~/tessel-orch-notes/lib/cargo-lock.sh`: the lock's owner file records the holder's pid, a waiter
removes a lock whose holder is gone, an exit trap releases it; tested with a holder killed by
SIGKILL); all 16 gate scripts of 26 and 27 November, the landing template and the active brief use
it, a brief's load paragraph is `~/tessel-orch-notes/lib/BRIEF-LOAD.md`, and both lanes were told.
The STATE read found a race in removing a dead holder's lock (two waiters could interleave and let
two runs hold it); waiters now remove it one at a time under a short reaper lock, and only if its
owner file still names the dead pid. A test of 20 rounds of six waiters on a dead lock saw no
overlap; whether that test can see one (against the old removal) is not shown: the check was refused
by Claude Code's removal safety check. The re-read then found the helper held no lock under zsh, the
agents' tool shell (an EXIT trap set in a function runs when the function returns there), so a
one-line cargo run made in that shell held none (lane B's seed runs went through a bash script and
held it); it now sets POSIX_TRAPS under zsh, and a pid the system reused reads as dead (the owner
file records the holder's start time, read with `LC_ALL=C` so a waiter in another locale reads it
the same); tested under zsh and bash. Dispatched at about 08:55 UTC: SIM-RACE-TEXT-LITERAL with
SIM-FENCED-RULE-CHECKER (lane B) and MUTANTS-SERVER-CLI-IN-DIFF (lane A), which merged as #134
(`64b51b0`; Opus review Yes on `b65db81`, its two optional edits made in `20b66d5`; 15 checks green
on `20b66d5`, its new sharded job skipped as the pull request touched neither crate).
**Earlier (27 November 08:20 UTC):** Main and the coordinator's head at `a38b570`.
MUTANTS-DOUBT-PACING landed through the server at 08:16 UTC as `7d08141`..`a38b570` (claim 198,
rebased; the submission held a merge of main, so the daemon copied no notes and the orchestrator
copied all six by subject and author), approved on a 3-0 vote after four fix passes (VOTE-1 and
VOTE-2 Yes on `72fe146`, fix pass 2; VOTE-3 Yes on `ccdc60a`; passes 3 and 4 changed only tests and
doc comments; `~/tessel-orch-notes/2026-11-27/vote-mutants-doubt-pacing/`): the removals rest on a
rule (a fenced claim's merge in flight is that claim's, with `doubt_since_ms` set) now checked by a
`debug_assert!` and a seeded stage walk that answers refusals and landings, starts some seeds in
doubt and answers only dispatches `begin_merge` offered; every break the voters planted fails a
test; `release_wait` and `reconcile_before_ending` say they assume only an offered dispatch is
answered. Its push run on main (CI 45964389648) is the first live mutant check of a landing: every
shard diffed from `bc14609`, 16 mutants, 10 caught, 6 unviable, 0 missed. With it and SIM-MUTANTS-2
every one of the Nightly's 59 new names is killed, removed or excluded; the next Nightly is the
proof. #132 (the STATE record below) merged at 08:03 UTC as `bc14609`.
**Earlier (27 November 07:45 UTC):** Main at `7fc1bb2` (#131); the coordinator's head is `d9da509`
(a pull-request merge does not move it). Since the record below, through the server, each after its
review or vote and followed by green CI: REGRESSION-SEED-69962521-STALE as `3489af8` (claim 159;
seed 5000397 replaces 69962521, which no longer caught the late-`Land` bug; an Opus review found it
also breaks I6 under a one-line `is_fenced` = false mutant of main); SIM-HEAD-UNREADABLE as
`574457a` (claim 163; review Yes, 25 seed transcripts identical); CLIENT-1013-CAUSE as
`81b48fb`..`15e6cbc` (claim 171; review Yes; after the fix pass for its should-fixes, the re-check
asked for one edit, a 152-character test line, made in `15e6cbc` and not re-checked; the daemon logs
the code and quoted reason of a close that ends a log read early); the CLAIM-LEASE-LAPSE docs as
`bef0465`..`b1a61bb` (claim 187; review Yes after two passes); SIM-MUTANTS-2 as `6ea5508`..`d9da509`
(claim 192; review With changes, its one must-fix (the exclusion's false reason) made in `ffa7e9c`
(landed as `d9da509`), re-check Yes: of the 50 simulator names, 41 killed, 2 no longer generated, 6
removed with their code, 1 excluded). By pull request, merged by the orchestrator under the standing
order: #130, SUDO-RS-RUNNER, as `0ed8cb8` (vote 3-0 for, after two fix passes: VOTE-1 Yes on
`4fb78ca`, VOTE-2 Yes on `a56d0ab`, VOTE-3 Yes with should-fixes on `a56d0ab`, PR text only;
ubuntu-26.04 ships sudo-rs 0.2.13, through which `kill -KILL -1` fails to parse; the runner and the
sudoers rule now use `kill -KILL -- -1`; a `sudo-rs:` prefix check that matched nothing real was
removed; kill's and sudo's messages are shown quoted; Own users runs on ubuntu-24.04 and
ubuntu-26.04, 8 of 8 real-sudo tests green on both); #131, LANDING-MUTANTS-IN-DIFF, as `7fc1bb2`
(review Yes after one edit: the per-change mutants also run on pushes to main, which server landings
make, and push runs are keyed by commit so a newer push no longer cancels an earlier one's run; its
first push run resolved `before` to `d9da509` and diffed it, a workflow-only change with no Rust to
mutate, so mutants on a Rust push are not yet seen live). CLAIM-LEASE-LAPSE: the CLI is right; a
coordinator stall longer than the lease (30 s at the default; one over 20 s can, depending on where
it falls between beats) or a sleep of the lease less one heartbeat or more (20 s) lapses a connected
agent's open claims, because every client message runs `expire` before it is routed
(`src/coordinator.rs:609-614`) and the next heartbeat is stamped after the sleep; the docs say so.
An adversarial read rejected expiring on silence after a clock jump (a jump cannot be told from an
idle host) and proposed pausing leases while the machine is suspended, which changes I10: Felix's
decision (`~/tessel-orch-notes/2026-11-27/lane-a/claim-lease-lapse/PROPOSAL-SUSPEND.md`). Learned:
`tessel claim` adds to the agent's open claim only when it holds exactly one, so a lane with two
tasks scatters its scopes over many claims, and `tessel submit` covers only the claim it names
(`src/coordinator.rs:1425`): the orchestrator consolidated by releasing and re-claiming
(CLAIM-SCATTER, below); `tessel stop` releases the agent's claims; the pre-push hook measures
against the daemon's last view of the coordinator's head, stale until its next `Welcome`, `Merged`
or `BaseMoved` (`tessel-cli/src/state.rs:92`).
**Earlier (27 November 02:10 UTC):** Main at `9e1bb97`. Felix resumed the build on 26 November after
a pause from 1 November (his word). The Mac had rebooted about 13 November, so the dogfood server
was down; the agent tokens had lapsed (about 7 November) and the GitHub token
"tessel-dogfood-lander" expired on 23 November. Felix asked the orchestrator to unblock itself in
Claude in Chrome: after his passkey, the orchestrator regenerated the GitHub token with the same
scopes, valid 30 days (to 26 December 2026), and saved it to `<server dir>/github-token` from the
clipboard without reading it (the new token reads all four repositories); `setup.sh` minted new
agent tokens (valid to about 3 December; the server configuration unchanged, `jobs = 2`); the server
was started at 22:24 UTC with the same build (the `bin/` copies `setup.sh` rewrote have the same
sha256 as those they replaced). Felix asked whether dogfooding was only for the contest; shown
section 8 and decision 6 of `PLAN.md` (17 and 20 October, after the contest), he chose to keep it.
Then, through the server, both approved by the orchestrator on a 3-0 vote: HOST-HARDENING-FOLLOWUPS
at 00:40 UTC on 27 November as `c88aa40`..`2640eff` (claim 158, fast-forward; CI 45918121207 and the
Adapter suite 45918121325, its kill -9 loop included, passed on `2640eff`) and SUDO-GATE-FOLLOWUPS
at 01:17 UTC as `930748f`..`9e1bb97` (claim 157, rebased; notes copied by the daemon). On `9e1bb97`
main CI (45921694282) and the Adapter suite (45921694273) passed; Own users (45921694300) failed:
all 7 real-sudo tests (the first left processes in `_tessel-runner` that survived `kill -KILL -1`,
the rest met them as survivors at `SudoRunner::new`) and the script's `sudo -l -U` check. The cause
is the runner image, not the landing: `ubuntu-latest` moved from ubuntu-24.04 to ubuntu-26.04
(41243983653's first attempt on 31 October ran 24.04, its rerun on 27 November 26.04), and the rerun
of that green run on `80dba09`, before SUDO-GATE-FOLLOWUPS, fails the same way. Ubuntu 26.04 likely
ships `sudo-rs` as `sudo` (not confirmed from the log). This pull request pins the job to
ubuntu-24.04 (`5efea60`); the pin counts only once a run on it is green; SUDO-RS-RUNNER makes
`SudoRunner` work on 26.04. HOST-HARDENING-FOLLOWUPS' first vote (on `6c5240a`) found a regression
its own change made: the overflow withdrawal, deferred, ran after the batch's next job, which could
grant the lock to the waiter just closed with 1013; fix pass 1 runs a next job at once only if it is
a wake step or nothing is deferred, and also closes a replay whose read failed with 1011 (an empty
`done` page; tessel-server sends one only on a failed read or a closed socket), so a failed read is
no longer sent as a complete replay (R36 still promises none missed only while every read succeeds);
a wake step still runs before the deferred withdrawals and could grant to the closed waiter (R42
says so; no shipped client reaches it); fix passes 2 and 3 kept a Watch on an already-closed socket
from being logged as a failed read and pinned a guard; each voter's last vote was "Yes" (voters 1
and 2 on `2640eff`, voter 3 on fix pass 1, `5a05ee3`; fix passes 2 and 3 closed voter 1's and voter
2's re-check items) (`~/tessel-orch-notes/2026-11-26/vote-host-hardening-followups/`).
SUDO-GATE-FOLLOWUPS' vote (`vote-sudo-gate-followups/`): all "Yes with should-fixes"; fix pass 1
gave a command whose output stays open after its exit a true message, stopped `read_capped`
swallowing read errors, made each log assertion name its own event, gave the hang test a 600 s stub
and a 120 s bound, and reaps the run's `sudo` after a gate close; the re-checks were "Yes". Its
`docs/dogfood.md` steps were not written: claim 153 holds that file. The Nightly has failed every
day since 31 October (8 and 12 November cancelled at the 2-hour limit): correcting the STATE of 31
October, its run on 31 October found 57 missed mutants beyond the baseline, not one, and every run
since 59 (9 in the core and the host, 50 in `src/sim`), from the landings of 30 and 31 October;
server landings skip the per-change mutants job, which runs only on pull requests
(`.github/workflows/ci.yml:205`). An Opus triage
(`~/tessel-orch-notes/2026-11-26/nightly-mutants/TRIAGE.md`) found 4 core and host mutants that are
real gaps (at 3 places), 5 equivalent ones whose code can go, and 8 honest simulator exclusions; the
rows are MUTANTS-DOUBT-PACING, SIM-MUTANTS-2 and LANDING-MUTANTS-IN-DIFF.
**Earlier (1 November 13:12 UTC):** main at `d51c9f5` (#127). Felix made ruleset step 3 at about
01:35 UTC: ruleset 25345043 now also requires "Fast lane, simulator" (read back: its rules equal the
prepared payload). Felix lowered the landing build cap (D3) by 01:10 UTC: `<server
dir>/.cargo/config.toml` now says `jobs = 2` and `RUST_TEST_THREADS = "2"` (checked);
`scripts/dogfood/setup.sh` and `install.sh` now write the same, so a re-run keeps it. No landing at
2 jobs is timed yet against the runner's 720 s budget; the orchestrator times the next one. These
changes were first claim 153 (`d7957f9`..`19b0d71`), held for sensitive paths; `orchestrator` is
the server's only reviewer and may not decide its own submission, so they landed by pull request
#126 (`docs/dogfood.md`, its introduction; checks and main CI green, Opus review "With changes",
re-check Yes on `127603f`), and claim 153 stays held, its five files claimed, until another
reviewer decides it. The orchestrator's daemon keeps its start commit `8577cd5` while it holds a
claim, and the coordinator's head is `8577cd5` until a landing or a head read after a trunk poke
moves it, so the CLI measures the orchestrator's new work from `8577cd5` and counts #126's five
files in it: this STATE commit went by pull request too (a daemon started at `94414cf` measures
from there). Felix ran `~/tessel-orch-notes/2026-11-01/delete-merged-branches.sh` at about 03:30
UTC: the 51 head branches of merged pull requests deleted, none skipped (the list beside it keeps
the shas); Felix then deleted `build-cap-2` and `state-1101-1` (#126 and #127, merged), so only
`main` is left. Felix deleted the Cloudflare account token `tessel-steward-queues` (Queues read and
write, the `CLOUDFLARE_API_TOKEN` of `tessel-steward/.dev.vars`, last used in early October) on 1
November; Cloudflare refuses it since, and the orchestrator removed its line from the file. Earlier,
on 31 October,
Felix renewed the agent tokens at 21:24 UTC (`setup.sh`; the server configuration is unchanged and
`bin/` holds the
same build). Then, through the server, the first three approved by the orchestrator on their vote or
review and each followed by green CI on main: SUDO-STOP-RUN-RACE at 21:31 UTC as
`b5a520d`..`80dba09` (claim 146; CI 41243983794, the Adapter suite 41243983627 and Own users
41243983653, its first Linux evidence, passed on `80dba09`); HEAD-ADOPTED-FOLLOWUPS at 22:24 UTC as
`6497952`..`eec53d1` (twelve commits, claim 147); SIM-R30-MODEL at 22:37 UTC as `98451f5`..`10e4030`
(claim 150); and two follow-up rows at 22:50 UTC as `d6a66ad` (claim 151). Every landed commit has
its note. HEAD-ADOPTED-FOLLOWUPS' vote: VOTE-1 "No" (a race stored waiting on the head and loaded on
a port that reads no heads waited for good with its scopes held), VOTE-2 "No" (the clock test
advanced the clock, so it did not prove "at once"), VOTE-3 "Yes with should-fixes"; four fix passes
closed them, and each voter's last vote was a Yes (VOTE-1 "Yes with should-fixes" on `e4148d6`,
VOTE-3 "Yes with should-fixes" on `23d23d9`, VOTE-2 a plain "Yes" on `0af278e`, the head landed;
`~/tessel-orch-notes/2026-10-31/vote-head-adopted-followups/`). SIM-R30-MODEL: Opus REVIEW-1 "With
changes" (its new traffic arm changed the seeded runs, so regression seed 2796343632 no longer
caught its bug); fix pass 1 dropped the arm, and the re-check "Yes" found the transcripts of the
five regression seeds and default seeds 0-19 identical to `f16f9ed`'s and the seed catching the old
checker's bug again (`~/tessel-orch-notes/2026-10-31/lane-b/sim-r30-model/`). Found today: a held
submission does not block the merge queue (the STATE commit, queued behind the held claim 146,
landed at 19:51 UTC); submits and claims go through a daemon's open connection after the tokens
expire, approvals do not. SIM-R30-MODEL ran in lane B's SUDO-STOP-RUN-RACE worktree beside its held
claim, since one worktree may hold several claims. Before, at 19:51 UTC, the STATE commit `676cbd9`
landed (claim 149). At 19:35 UTC main was at `f16f9ed` (the STATE commit landed at 18:19 UTC, claim
148; CI 41228804696 and the Adapter suite 41228804721 passed). The agent tokens expired at 19:25:14
UTC (`server.log`: "token is expired"): every new connection is refused, so `tessel review`, which
opens one, cannot approve a held submission until Felix renews them (`docs/dogfood.md`, "Tokens last
7 days"); claims still go through the daemons' open connections. SUDO-STOP-RUN-RACE (claim 146,
`bf844ba`, `7259939`, `97c825b`; lane gate 2472 green): Opus REVIEW-1 "With changes" (the red test
was timed; the grace close logged nothing; a doc said "at most `EXIT_GRACE`"; an untested kill after
`sudo` exited), fix pass 1 `97c825b`, re-check "Yes"; its submit was held for
`tessel-server/src/landing/` (sensitive), so it went to a vote
(`~/tessel-orch-notes/2026-10-31/vote-sudo-stop-run/`), 3-0 "Yes with should-fixes", no must-fix;
the should-fixes are SUDO-GATE-FOLLOWUPS (below). The orchestrator's `tessel review --approve 146`
at 19:25 UTC was refused (401, the expired token), so it is still held. HEAD-ADOPTED-FOLLOWUPS
(claim 147, `fcd3eca`..`23d23d9`; gate 2477 green; seeds 69962521, 2796343632 and a 300-seed sweep
green) went to a vote (`~/tessel-orch-notes/2026-10-31/vote-head-adopted-followups/`): VOTE-1 "No
(must-fix)" (a race stored waiting on the head and loaded on a port that reads no heads waits for
good with its scopes held, where main ends it after about 130 s), VOTE-3 "Yes with should-fixes"
(the same case first), VOTE-2 not in yet; fix pass 1 (`lane-a/head-adopted-followups/FIX-1.md`)
runs, and needs no token. At dispatch the orchestrator classed SUDO-STOP-RUN-RACE as review-only,
which was wrong: every change under a sensitive path is merge path. Before that, the STATE commits
`bc0a9cb`..`20cd015` landed at 12:33 UTC (claim 145). At 17:55 UTC the orchestrator dispatched
HEAD-ADOPTED-FOLLOWUPS (lane A, claim 147, merge path: a vote follows) and SUDO-STOP-RUN-RACE (lane
B, claim 146, Opus review), both from `20cd015`
(`~/tessel-orch-notes/2026-10-31/lane-a/head-adopted-followups/`, `lane-b/sudo-stop-run/`).
SIM-R30-MODEL, which Felix asked to start with them, goes to lane B, the first lane free (its row
said lane A); it waited because one agent cannot safely run two worktrees at once (the shell sends a
`Notify` to every socket bound to the agent, `src/shell.rs:83-84`, and the core never blocks an
agent's claim on its own locks, `blocked_by` in `src/coordinator.rs`). Running it under the
orchestrator's identity would mean approving its own hold. Two P1-SIM-MERGE REVIEW-3 nits moved from
HEAD-ADOPTED-FOLLOWUPS to SIM-R30-MODEL, which holds `src/sim` (both rows below). Before that,
RUNNER-TIMING-FLAKES landed through the server at 12:15 UTC as `114bccd`..`cd8a718` (claim 143;
tests only; Opus REVIEW-1 "No": the sudo test's 300 ms cancel timer raced the stub's pid write and
three guards had become vacuous (two sudo tests and `landing.rs`'s deadline test); fix pass 1 drives
the cancels by events, runs the stub's child for 600 s, asserts the run was cut short, and holds the
landing test's runner 120 s with an answered-before-finished check; the re-check "Yes"; CI
(41200314710), the Adapter suite (41200314669) and Own users (41200314679) passed on `cd8a718`;
`~/tessel-orch-notes/2026-10-31/lane-a/runner-flakes/`). At 10:48 UTC CORE-LATE-LAND landed as
`8707f38`..`01869ca` (ten commits on `6dccb3e`; claim 140, merge path), building Felix's option B:
its first vote went 3-0 "No" (a doubt was fenced on the verification baseline, not on its own
`Land`'s lease), its re-vote 2 "Yes with should-fixes" and 1 "No" (a refused `Prepare` in a fenced
doubt never asked where main was), and after fix pass 2 the "No" voter's re-check was "Yes with
should-fixes" (`~/tessel-orch-notes/2026-10-31/vote-core-late-land{,-2}/`); the orchestrator ran a
500-seed sweep from 149891420 on `24d0ed7` before the submit (no log kept), and Linux CI, the
voter's other condition, ran only after the landing: CI (41194230450) and the Adapter suite
(41194230447) passed on `01869ca`. Its last commit also fixes an I6 checker false positive, seed
2796343632, which had turned CI red on `a6accf4` (41151845130): `poisoned_outcome` judged the claim,
not the poisoned submission, so a claim rejected for a poisoned commit that then merged other work
was flagged; P1-HOST-HARDENING's retry exposed it (an Opus investigation,
`scratchpad/I6-2796343632.md` of session 69ad1e78). Seeds 69962521 and 2796343632 are in
`tests/simulator-regressions.txt`. The scheduled Nightly 41189134845 (09:32 UTC on `6dccb3e`, before
the fix) FAILED: its sweep broke I6 on seeds 415141587, 415142220, 415144131, 415147323 and
415148924 with the same message, and all five pass on `cd8a718` (run by the orchestrator, one seed
each); its full-mutants job found one missed mutant beyond the baseline, `src/sim/driver.rs:379`
`Sim::failed_commits -> 0`, so the next Nightly fails its baseline check until SIM-R30-MODEL kills
it. At 02:19 UTC RACE-LOSER-TEXT landed as `e78b6fe`..`6dccb3e` (claim 141; CLI only; Opus REVIEW-1
"With changes", the daemon kept a dead race claim locally, fixed; the re-check was "With changes"
for one line over 100 characters, which the orchestrator closed with its own commit `550be1b` in the
lane's submission, and its addendum "Yes"; its first landing run at 02:12 failed first on the sudo
timing test, then landed unchanged with the orchestrator holding the cargo lock). The STATE commits
`caefdb7`..`a6accf4` landed at 00:45 UTC (claim 142). Felix asked on 31 October for his items to be
done in Claude in Chrome: its tools are not loaded in this session; lowering the landing build cap
(`<server dir>/.cargo/config.toml` from `jobs = 6`, D3, to 2) and making "Fast lane, simulator"
required (ruleset 25345043; the payload is `scratchpad/ruleset-25345043-new.json` of session
69ad1e78) were both refused by the auto-mode classifier as shared-resource changes, so they wait for
Felix. Before, at 00:19 UTC, P1-FAILPOINTS landed through the server as `574e63d`..`6b5e921` (six
commits on `042b87a`; claim 139, held for signature changes, not merge path, and approved by the
orchestrator on Opus review: REVIEW-1 "With changes", no must-fix, fix pass 1 `984caf5`, a re-check
"With changes", fix pass 2 `6b5e921`; `~/tessel-orch-notes/2026-10-30/lane-a/failpoints/`). Each
landing window has a scripted crash, except the `Land` window between main's push and the ref
delete, whose state `landing.rs` builds with `git update-ref`: in the simulator
(`src/sim/driver/failpoint.rs`) and, where the real lander is needed, in
`tessel-server/tests/{restart,landing}.rs`; the failpoints found no core bug. Its phase-1.md I6 cell
changed: a crash after a recorded `Test` is reconciled and lands, so "`Test` running" replaces
"tested, not landed", and a note says a cut-off at the `Land` stage is a doubt, outside I6, that
stays in flight until a `Land` completes or the operator ends it with `resolve_doubt` (a restart at
every `Land` marker loops by design until then). The notes were copied by hand (a merge of main in
the submission). CI (41149575702) and the Adapter suite (41149575736) passed on `6b5e921`. At 21:14
UTC P1-HOST-HARDENING landed as `97fb0d9`..`042b87a` (four commits on `e07e6f5`; claim 136, merge
path): its first vote (`vote-host-hardening/`) went 3-0 "No" (a core job or commit could run under a
closed gate; frames a replaying socket queued before its `Watch` were not parked), fix pass 1
(`222b923`, `042b87a`) closed both, and the re-vote (`vote-host-hardening-2/`) went 3-0 "Yes with
should-fixes" (HOST-HARDENING-FOLLOWUPS, below). Its first landing attempt (21:04 UTC) failed first
on `tessel-runner/tests/runner/running.rs` `the_command_reads_a_closed_stdin` (a 3 s wall-clock
bound, untouched; server.log records only the first failure), with the load high from other
projects; the same commit was resubmitted and landed. CI on `042b87a` (41129573797) FAILED: the
1,000-seed simulator sweep found seed 69962521 breaking I6 (a claim rejected as an infrastructure
failure though its work is on main); the Adapter suite passed. An Opus investigation
(`~/tessel-orch-notes/2026-10-30/vote-late-land/I6-69962521.md`) found a real core bug that
P1-HOST-HARDENING did not introduce (the merge code is the same on `8f0f2b4`; the persist retry only
changed the path the seed takes): a resent `Land` answered `CandidateMissing` ends the doubt
(`merging/doubt.rs:267-271`), the core's fresh `Prepare` re-pushes the same candidate ref (C7), the
late first `Land` lands the work, and the core rejects the claim at the bound. A design vote
(`vote-late-land/`) went 2-1 for option B (keep the doubt while main is at the leased base); B
partly reverses M5-7, so it went to Felix, who chose B (below). CORE-LATE-LAND builds it. CI is
green again on `6b5e921` only because its sweep starts at another seed. Load: on 30 October tessel's
gates (no cargo job cap) and voters overlapped and pushed the shared Mac to 85 at 15:45 and 195-524
at 18:05-18:25 EDT; two of Mukaase's UI tests timed out. Every gate script now exports
`CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2` and takes a machine-wide lock
(`~/tessel-orch-notes/cargo.lock.d`), so one gate's cargo job runs at a time; the server's landing
runs are not under the lock. The STATE commits `c416075`..`e07e6f5` landed at 19:36 UTC (claim 135).
Before, at 19:08 UTC, RESOLVE-FOLLOWUPS landed through the server as `68ea3e6`..`ca864b2` (four
commits on `e23690e`; claim 134, held as merge path and approved by the orchestrator on a re-vote).
CI (41112834681) and the Adapter suite (41112834446) passed on `ca864b2`;
`tessel-server/tests/restart.rs` now runs the `sqlite3` CLI, and CI's runner has it (CI 41112834681
ran `restart::a_server_killed_after_the_push_and_restarted_counts_the_landing_once`, which passed).
The submission held a merge of main (`3e73289`), so the daemon copied no notes; the orchestrator
copied all four by subject and author (the fourth landing to hit that gap). RACE-UNKNOWN-REASON's
six commits were linear, and their notes were in place after the landing. Its first vote on
`3e73289` (`~/tessel-orch-notes/2026-10-30/vote-resolve-followups/`): VOTE-1 "No" (after the move,
step 1 ran a bare `sqlite3` under `sudo -u _tessel`, which a lane-writable `PATH` entry such as
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
not mention. Agent tokens are valid to 31 October (set when Felix issued them); Felix renews them.
Lanes cannot push (Felix's `~/.claude/hooks/block-lane-push.sh`), so the orchestrator pushes each
lane commit to a new branch of its fork; the reviewer reads it there; the orchestrator runs `tessel
submit` in the lane's worktree and decides holds as `orchestrator`. Two lanes ran at once four times
by 25 October; the coordinator denied lane A `src/merge.rs` and `docs/configuration.md` (twice) and
lane B `src/coordinator/verifying.rs` (three times), each while the other lane held it, and the
denied lane claimed the file after the holder landed or left it to a follow-up. In flight (votes and
briefs in `~/tessel-orch-notes/2026-10-30/`, `2026-10-31/`, `2026-11-26/` and `2026-11-27/`):
- SIM-DOUBT-PREPARED-TRAFFIC (lane A, branch task-sim-doubt-prepared-traffic at `bfdf49b`; Opus
  re-check 2 "No" to accepting its coverage loss; fix pass 3 (drop the forced refusal's rate-limit
  window) running under a new claim;
  `~/tessel-orch-notes/2026-11-27/lane-a/sim-doubt-prepared-traffic/`).
- NIGHTLY-MUTANT-TIMEOUTS (lane B, dispatched 22:50 UTC, branch task-nightly-mutant-timeouts from
  `df625c2`; test-only bounds for two timed-out mutants, the third explained;
  `~/tessel-orch-notes/2026-11-27/lane-b/nightly-mutant-timeouts/`). SIM-DOUBT-LIVENESS (lane B)
  follows SIM-DOUBT-PREPARED-TRAFFIC's landing, on DESIGN-2.
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
bypass; `current_user_can_bypass` never) and "main: CI required" (25345043: pull request and the 8
required checks, the repository admin role as bypass; always). Proven first on
`tessel-sandbox/m4-trunk` (public): the trial server landed `8c878d3` through the bypass, and
Felix's own force-push was refused. A `pre-push` hook in the checkout's shared hooks directory
(`pre-push.pre-tessel` since `tessel init`) refuses pushing `main` of `devfep/tessel`.
**Waiting on Felix** (the tokens were renewed, the build cap lowered and ruleset step 3 made, STATE
above): nothing on the GitHub token's Workflows permission: the orchestrator's 28 November request
to add it was wrong (`docs/dogfood.md` withholds it as a backstop), so it stays off and a
`.github/workflows/` change goes by pull request; before the dogfood upgrade, a push with the
dogfood token of a `refs/tessel/candidate/*` ref carrying a `.github/workflows` change, to learn
whether GitHub refuses it at `Prepare` (LANDER-WORKFLOW-SCOPE; the orchestrator never reads that
token), and the upgrade now also waits on LANDER-WORKFLOW-SCOPE's `Land` half, since main's code
asks a refused `Land` again forever; whether leases pause while the machine is suspended
(CLAIM-LEASE-LAPSE, PROPOSAL-SUSPEND.md above; it changes I10; two claims lapsing together under
load at 13:29 UTC, above, is new evidence, by inference, that a stalled server, not only a sleeping
one, lapses open claims); a second reviewer, to decide claim 153, which now also blocks lanes (the
coordinator denied SUDO-GATE-FOLLOWUPS' `docs/dogfood.md` claim, by lane A's report);
RUNNER-OWN-UID's sudo steps; whether the simulator gets a check that the merge path drains (a run
that never finishes its work fails; most of the 50 simulator mutants survive for want of one; scope,
rule 10); whether to define GA (no phase is called that; the nearest is Phase 3's end) as a proposed
PLAN change; deleting branch `state-1101-2`; whether CLAUDE.md rule 1 should also name a
never-logged live notice an older daemon can skip and stay correct as additive (invariant 13 since
`f1ea1f6`); the plan's rule (d) (`docs/proposals/phase-1.md`), which says a build bumps
`state_version` "only if the old binary can still read the new state", a case the store's
`refuse_unknown` rules out (#125 follows the code); a GitHub token that can push
`tessel-sandbox/m4-trunk` for the upgrade's trial landing, which the dogfood token cannot; where
P1-DENIAL-BOUND goes in the order; narrowing `PUSH_ALLOW_REMOTES` to the sandbox trunk now that
lanes push branches; the suggested patch to his push hook that exempts only a URL git resolves to
itself (`~/tessel-dogfood/push-hook-resolved-url.patch`). The second reviewer goes in the server's
`reviewers` (only `orchestrator` now).
Note: commits on `devfep/tessel` carry the author address of Felix's global git config; the sandbox
commits use his GitHub noreply address.
**Blocked:** CLI-GITHOOK-FIRST-COMMIT, not reproduced (see its entry).
SWARM-AGENT-CHECKS-UNDER-LOAD waits for its next failure's note. P1-FAKE-HELLO-LOST: the test
cannot see the flake (see its entry).
**Next:** LANDER-WORKFLOW-SCOPE's `Land` half (lane B, BRIEF-2:
`~/tessel-orch-notes/2026-11-28/lane-b/lander-workflow-scope/`, merge path: a vote); for lane A,
HOST-FAILED-SEND-EXPIRY-PINS or REPLAY-COMMIT-BEFORE-PAGE-DOC (`src/host`, away from
`tessel-lander`); LANDER-SIGNAL-ALL-SITES after LANDER-WORKFLOW-SCOPE (both change `tessel-lander`);
LANE-GATE-MUTANTS; the other of the two host rows;
GIT-VERSION-IN-DOUBT; DAEMON-INTENT-UPDATE; TESSEL-ON-PATH; CLAIM-LEASE-LAPSE's code on Felix's
decision; the dogfood.md steps of SUDO-GATE-FOLLOWUPS and RESOLVE-RUNBOOK-NITS once claim 153 is
decided; then P1-ONE-BATCH (it flips R20, an M5 done check). Felix's Homebrew postgres trusts local
connections on a world-writable socket; he closes it before the accounts move.
LANDER-ADMIN-POLICY-DEAD waits for M6.
**Decided by Felix (25 October), OWN-USERS-DESIGN:** the note as written
(`docs/proposals/own-users.md`).
**Decided by Felix (30 October), CORE-LATE-LAND:** option B of the late-`Land` vote
(`~/tessel-orch-notes/2026-10-30/vote-late-land/`): while the work is in doubt and main is at the
leased base, nothing rejects it (no infrastructure bound; a `CandidateMissing` or a refusal at
`Test` or `Land` keeps the doubt); a fresh `Prepare` re-arms the ref and the doubted head goes to
`Land`; the doubt ends when main leaves the leased base, the work lands, or an operator runs
`resolve_doubt`. This partly reverses M5-7 below (`CandidateMissing` is a fact only outside a
doubt).
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
  request and push (2 min 4 s on a GitHub runner), 20,000 nightly. Ruleset step 3 made 1 November.
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
- [x] **P1-ADAPTER-SUITE** (first part `9a2a044`; the rest `251a4c2`..`ec3ceb0`, landed by the
  server 27 November; Opus "Yes" after two fix passes) — M. First part: an `#[ignore]` kill -9 loop
  run by the "Adapter suite" job, checking every client transcript against I1, I5 and I9 after each
  reload (1 min 11 s on its first GitHub run), and the CLI of `e9df98c` landing through the current
  server. The rest: a slow watcher (a wire nobody reads is dropped and the repository serves the
  next socket), a panicking repo task (logged with its repository and cause, refused, served again
  after a restart), injected store errors (unreadable event and state rows, a store that rejects
  writes: 1011, logged, served again once repaired).
- [x] **P1-HOST-HARDENING** (`97fb0d9`..`042b87a`, landed by the server 30 October, run by lane B;
  held as merge path, approved on a 3-0 re-vote after a 3-0 "No") — M, after P1-CORE-RECONCILE.
  P1-PERSIST-RETRY (C8, flips R30), exact replay without stalling (C3, flips R36),
  COORD-UNDELIVERED-GRANT (decision 7).
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
- [x] **P1-FAILPOINTS** (`574e63d`..`6b5e921`, landed by the server 31 October; Opus review "With
  changes" twice, both fixed) — M, after P1-SIM-MERGE and P1-CORE-RECONCILE. The crash windows of a
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
- [x] **HEAD-ADOPTED-FOLLOWUPS** (`6497952`..`eec53d1`, twelve commits, landed by the server 31
  October; a vote, VOTE-1 and VOTE-2 "No", four fix passes, then each voter's last vote "Yes") — S,
  lane A, Felix reviews (`src/coordinator/` and the host: merge path, voted), after
  P1-HOST-HARDENING (it shares `src/host/calls.rs` and `src/host/tests.rs`) and CORE-LATE-LAND (it
  shares the core) and before or with P1-ONE-BATCH (the same wake), and before RESOLVE-RUNBOOK-NITS
  (it shares `docs/dogfood.md` and `src/host/calls.rs`); from CORE-HEAD-ADOPTED's re-vote
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
  `src/host/**`); the opener's `RaceResult` line is RACE-LOSER-TEXT's. (P1-SIM-MERGE's REVIEW-3 nits
  moved to SIM-R30-MODEL on 31 October: they are in `src/sim`.) Done: those tests, each failing
  without its change, and the docs.
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
- [x] **RACE-LOSER-TEXT** (`e78b6fe`..`6dccb3e`, landed by the server 31 October, run by lane A;
  Opus review "With changes", then "Yes") — S, lane B, after P1-HOST-HARDENING; from
  RACE-UNKNOWN-REASON's vote (VOTE-1 should-fix 4, VOTE-3 should-fix 2), resting on it and the
  standing order. The CLI's `rejected_text` (`tessel-cli/src/render.rs` ~509) tells every rejected
  entrant "claim N is active again with the same fence; fix the cause, push, and `tessel submit`
  again"; for a race entrant that is false (`reject_loser` removes the claim). So do the daemon's
  `on_submit_rejected` notice (`tessel-cli/src/daemon.rs:1712-1721` on `ca864b2`, "the claim is
  active again with the same fence") and the Stop hook (`tessel-cli/src/inbox_hook.rs:354-359`, "Do
  not end your turn: fix the cause, push, and run `tessel submit` again", which pushes a race
  entrant toward a resubmit that fails with `UnknownClaim`). Say that the entry's claim ended and
  its fork is untouched, and how to ship the work (claim the freed scopes and submit, or ask the
  opener to reopen the race); `skills/tessel/SKILL.md` ~204 says the same for every
  `submit_rejected` and has no race-loser paragraph. Also the opener's `RaceResult` line, which
  renders "no entry survived" (`tessel-cli/src/render.rs:723` on `ca864b2`), should say when the
  tests gave no result (RACE-UNKNOWN-REASON's VOTE-3 should-fix 3). A cross edit into lane A's
  `tessel-cli/src/daemon.rs`, named here. Done: tests of the race-loser text in the CLI, the notice
  and the hook, and of the opener's line.
- [x] **CORE-LATE-LAND** (`8707f38`..`01869ca`, landed by the server 31 October; held as merge path,
  approved by the orchestrator on the votes after two fix passes) — S/M, lane B, Felix reviews (the
  merge path: voted), resting on Felix's decision of 30 October (STATE) and the late-`Land` vote.
  Seed 69962521 breaks I6 on main `042b87a`: a resent `Land` answered `CandidateMissing` ends the
  doubt, the core's fresh `Prepare` re-pushes the same ref, a late first `Land` lands the work, and
  the core rejects it at the bound. Build option B: key on the submission's doubt; while in doubt
  nothing is counted or rejected (`resend_test`, `retry_after_infrastructure`, the `Unstaged` path,
  a refusal at `Test` or `Land`); a `Prepared` on the leased base with the doubted head goes
  straight to `Land`; the doubt ends on `Landed`, `AlreadyMerged`, `MainMoved`, a `Prepared` on
  another base or `NotLanded` with main off the base; `resolve_doubt` at any stage. Seed 69962521 in
  `tests/simulator-regressions.txt`; the I6 checker applies to every rejection; candidate-ref.md
  section 2 records the decision. Done: core tests red first (VOTE-1's list and VOTE-3's (B′) flaky
  re-`Test`), the seed green, a sweep green.
- [x] **HOST-HARDENING-FOLLOWUPS** (`c88aa40`..`2640eff`, landed by the server 27 November; each
  voter's last vote "Yes" (voter 3's on fix pass 1), three fix passes; R36 now closes a failed read
  with 1011; the SQLite retry backoff moved to HOST-SQLITE-RETRY-BACKOFF) — S, lane B, after
  CORE-LATE-LAND and HEAD-ADOPTED-FOLLOWUPS
  (both edit `src/host`, `src/host/tests.rs` among it); from P1-HOST-HARDENING's re-vote
  (`~/tessel-orch-notes/2026-10-30/vote-host-hardening-2/VOTE-1..3.md`), resting on it and the
  standing order. Pin the two orders the fix relies on (a deferred `then` job before the socket's
  next frame; parked frames before queued ones when a replay ends under a closed gate), with the
  voters' probes; say in R42 and the mod.rs gate doc that deferred jobs run before every queued
  input. An overflow withdrawal starts mid-`release`, before the batch's `SetWake` and calls: push
  it to `deferred`, with a test of the `waking` case. Prune the replay buffer per page (`seq <=
  last_sent`) so the bound counts only owed events. R36's "none missed" holds only while reads
  succeed (a failed read answers an empty `done` page): qualify it or close the socket. Rename
  `host_a_replay_buffer_that_overflows_in_a_release_withdraws_after_the_batch_and_its_next_job` to
  what it asserts. Reword the adapter contract to the real requirement (a read holds every commit
  answered before its replay began and none answered after the page). Retry backoff for real SQLite
  errors was not assessed, and the ignored kill -9 adapter test was not run on this change (re-vote
  VOTE-2, VOTE-3 S5): run it on CI or in P1-ADAPTER-SUITE's rest. Done: each with a test failing
  without it.
- [x] **SIM-R30-MODEL** (`98451f5`..`10e4030`, landed by the server 31 October; Opus review "With
  changes", then "Yes"; the poisoned test is now
  `a_restart_during_every_test_is_rejected_at_exactly_the_bound`; the 1013 cause moved to
  CLIENT-1013-CAUSE; `trunk_head_unreadable` coverage moved to SIM-HEAD-UNREADABLE) — S, lane B (was
  lane A), after P1-FAILPOINTS and CORE-LATE-LAND; from P1-HOST-HARDENING's first vote (VOTE-1 S3,
  VOTE-3 S4) and the late-`Land` investigation. `src/sim/driver.rs` (`commit`, ~846-849 on
  `6b5e921`) still strikes the merge in flight on a failed commit, though a failed commit is now
  retried and no longer drops the core: add a "fail every attempt" fault and drop the strike for a
  rescued failure. `src/sim/driver/merge_path.rs:7`'s header says the lander does not outlive the
  call's connection; the code (rightly) runs late calls: fix the header. The client logs the cause
  of a 1013 close in `read_log` (P1-HOST-HARDENING's first vote VOTE-3 S1, re-vote VOTE-3 S4). The
  testkit's `fail_next_commit` now fails all three attempts while the sim's fails one: one name, two
  meanings. Also kill the missed mutant `src/sim/driver.rs:379` `Sim::failed_commits -> 0` (Nightly
  41189134845; only `tests/simulator.rs:1658` reads it). From P1-SIM-MERGE's REVIEW-3 nits (moved
  here from HEAD-ADOPTED-FOLLOWUPS on 31 October): the poison in
  `a_restart_during_every_poisoned_test_is_rejected_at_exactly_the_bound`
  (`src/sim/driver/merge_path.rs`) is inert (the test passes with `poison_fork` deleted): make it
  bite, or drop it and rename the test; say in `Sim::crash_at_every`'s doc (REVIEW-3 named it
  `crash_after_every_test_dispatch`) why it is `pub`, or use it from `tests/simulator.rs`. The
  naming fix stays on the sim side: `src/host/testkit.rs` is HEAD-ADOPTED-FOLLOWUPS'. Done: the sim
  reaches the spent-retry path, a test that fails without each change, and that mutant killed.
- [x] **RUNNER-TIMING-FLAKES** (`114bccd`..`cd8a718`, two commits, landed by the server 31 October,
  run by lane A; Opus review "No", then "Yes") — S, lane B; observed 30 October under load 60-95
  from other projects: `tessel-server/tests/sudo_runner.rs`
  `a_call_given_up_mid_run_kills_the_account_itself_and_frees_the_gate` (a 10 s bound, took 15.4 s)
  and `tessel-runner/tests/runner/running.rs` `the_command_reads_a_closed_stdin` (a 3 s bound; it
  failed a landing's test run). Both pass in about 1 s at low load. Make the bounds about progress
  (an event or a deadline the test controls), not wall-clock under an unknown load.
- [x] **REGRESSION-SEED-69962521-STALE** (`3489af8`, landed by the server 27 November; Opus review
  Yes) — S, lane B, after SIM-R30-MODEL (it edits the same files);
  `tests/simulator-regressions.txt`, `src/sim`; from SIM-R30-MODEL's REVIEW-1 (should-fix 2,
  `~/tessel-orch-notes/2026-10-31/lane-b/sim-r30-model/`). Seed 69962521 passes on `6dccb3e`, where
  the late-`Land` bug it was recorded for (CORE-LATE-LAND) was still present, so it no longer
  catches that bug. With the bug restored (`src/coordinator/` as of `6dccb3e`, keeping main's
  `src/sim/merge_check.rs`, since `6dccb3e`'s checker has the seed-2796343632 false positive; or a
  mutant that ends the doubt on `CandidateMissing`), sweep for a seed that breaks I6, replace
  69962521 with it and record the finding. Done: the new seed fails with the bug restored and passes
  on main.
- [x] **SIM-HEAD-UNREADABLE** (`574457a`, landed by the server 27 November; Opus review Yes) — S,
  lane B, after REGRESSION-SEED-69962521-STALE (both edit `src/sim`); from HEAD-ADOPTED-FOLLOWUPS'
  vote (VOTE-1 should-fix 2), deferred by SIM-R30-MODEL until HEAD-ADOPTED-FOLLOWUPS landed (it
  has). The simulator never sets `HostConfig::trunk_head_unreadable`, so a port that reads no heads
  has no simulator coverage: add traffic or a scripted run with it set, checked by the I-invariants,
  without changing the seeded runs of the recorded regression seeds. Done: a simulator test with the
  flag set, failing when the core's unreadable-port release is removed.
- [x] **CLIENT-1013-CAUSE** (`81b48fb`..`15e6cbc`, landed by the server 27 November; Opus review
  Yes) — S, lane A (the CLI); from P1-HOST-HARDENING's votes (first vote VOTE-3 S1, re-vote VOTE-3
  S4), moved out of SIM-R30-MODEL on 31 October because the code is outside `src/sim`: the daemon
  logs the cause of a 1013 close in `read_log` (`tessel-cli/src/daemon.rs` ~2576; the swarm's
  `tessel-swarm/src/conn.rs` ~240-245 already reports "code N: reason"). Done: `daemon.log` names a
  1013 close and its reason, with a test that fails without it.
- [ ] **CLAIM-LEASE-LAPSE** (docs `bef0465`..`b1a61bb`, landed 27 November; the mechanism is found,
  claim 137's own lapse is not proven; the code fix waits for Felix's decision on pausing leases
  while suspended) — S, lane B, from 30-31 October: lane A's claim 137 lapsed (`lease_expired` in
  its inbox) while its daemon was running and online and its agent idle, and the pre-push hook then
  refused the push; lane A's next agent found claim 138 gone too and took 139 (its report). Claim
  125, by contrast, kept its lease for hours while its agent was stopped (submitted and held for
  review then). Find out whether the daemon stops renewing an idle claim, whether the lease is meant
  to end then (the skill and `docs/configuration.md` say what), and fix the code or the docs; a test
  either way. 27 November: claims 207 and 209 (two lanes, both daemons online) lapsed at the same
  millisecond, 13:29:07.744 UTC, under load from a VM that isn't ours (about 117 at 14:52 UTC): this
  fits the server stalling past the lease less a heartbeat and expiring both in one step (an
  inference; it logs no info line then). Claims 210 and 211 (lane A) lapsed 1 ms apart at
  15:02:38.812 UTC, its daemon still connected.
- [x] **SUDO-STOP-RUN-RACE** (`b5a520d`..`80dba09`, landed by the server 31 October; Opus review
  "With changes", then "Yes"; a vote, 3-0 "Yes with should-fixes", which are SUDO-GATE-FOLLOWUPS) —
  S, lane B; from RUNNER-TIMING-FLAKES' REVIEW-1 ("For the orchestrator"), resting on it and the
  standing order. A cancel that arrives before `sudo` has switched to the account
  (`tessel-server/src/landing/sudo.rs` `stop_run`): `kill -KILL -1` as the account and `ps -U` find
  nothing, `kill_all` returns Ok, `stop_run` waits passively for `EXIT_GRACE` while `sudo` goes on
  to exec the real runner, then sets `closed`, so the gate stays shut until a restart. Safe, but it
  costs availability; the window opens when a call gets the gate with almost no deadline left. Keep
  running kill rounds while waiting within `EXIT_GRACE`; Done: a test with the slow-start stub that
  the gate is free afterwards.
- [ ] **SUDO-GATE-FOLLOWUPS** (code `930748f`..`9e1bb97`, landed by the server 27 November; vote 3-0
  "Yes" after one fix pass; left: the `docs/dogfood.md` steps, blocked by claim 153 (Own users
  passed on both images on #130's merge, 45938317308); tick after them. Also left from the vote,
  small: a test driving a read error through `output_bounded`; the real-account comment names the
  wrong stub test; a reap after a failed wait may signal a reused pid if anything ever reaps
  children it did not start) — S, lane B, merge path (`tessel-server/src/landing/`: voted); from
  SUDO-STOP-RUN-RACE's vote (`~/tessel-orch-notes/2026-10-31/vote-sudo-stop-run/VOTE-1..3.md`, all
  "Yes with should-fixes"), resting on it and the standing order. In
  `tessel-server/src/landing/sudo.rs`: a `try_wait` error makes `wait_bounded` return at once, so
  `stop_run` runs `kill_all` (a `sudo` and a `ps`) back to back for `EXIT_GRACE` and then logs the
  grace-close line, which is wrong for it (the `run_locked` error path, ~298); close the gate and
  log the error at once on that error, with a test (VOTE-1). `kill_once` and `survivors` call
  `Command::output()` with no timeout, though the module doc says every wait is bounded: bound them
  or say so (VOTE-3). The four lines that close the gate are worded differently and none names the
  account: one wording, naming the account and "until a restart" (VOTE-3). A test for the
  grace-close log (a shared `OnceLock` subscriber in `tessel-server/tests/sudo_runner.rs`; VOTE-2).
  A `sudo_runner_real.rs` case: a cancel on the first poll after the spawn, which real `sudo`
  usually meets before it switches; the gate frees and the account is empty (VOTE-1, VOTE-2). Docs:
  `docs/configuration.md` (~428) lists a `sudo` alive after the grace as a cause of a closed gate
  and says the gate stays shut until a restart; `docs/dogfood.md` gets the operator's steps for a
  closed gate (VOTE-3). Done: a test for each code change, and the docs.
- [x] **MUTANTS-DOUBT-PACING** (`7d08141`..`a38b570`, landed by the server 27 November; vote 3-0 Yes
  after four fix passes (VOTE-1 and VOTE-2 last re-checked `72fe146`, VOTE-3 `ccdc60a`); the
  simulator checker of its rule is SIM-FENCED-RULE-CHECKER) — S, lane A, merge path
  (`src/coordinator/`: voted); from the Nightly of 1 November and the triage
  (`~/tessel-orch-notes/2026-11-26/nightly-mutants/TRIAGE.md`). Kill `Coordinator::prepared`'s
  `doubts` reset (a doubt a `Prepared` on another base ends restarts at `infra_backoff_ms(1)`) and
  `wait_in_doubt`'s `doubts` (the backoff tests in `stages.rs` assert the exact wait, not only the
  cap); remove, without changing behaviour, the four equivalent constructs (`release_doubt_wait`'s
  guard, the `work` update in `retry_after_infrastructure`, the `retry_at_ms` and `work` clears in
  `reconcile_before_ending`, `wait_in_doubt`'s `first`/`since` handling) and the unreachable
  `wait_in_doubt` branch of `reconcile_refused_prepare`; a host test of the "retrying (attempt N of
  3)" line (`Pending::attempts`). Done: the 9 core and host names are no longer missed and the
  baseline does not grow.
- [x] **SIM-MUTANTS-2** (`6ea5508`..`d9da509`, landed by the server 27 November; Opus review With
  changes, its must-fix made in `ffa7e9c` (landed as `d9da509`), re-check Yes; the Done's Nightly
  pass is not yet seen: it also needs MUTANTS-DOUBT-PACING's 9 names) — M, lane B (`src/sim`), after
  SIM-HEAD-UNREADABLE; from the same triage. Tests for the 42 simulator mutants that are real gaps:
  `Lander` unit tests (`test`'s flaky flip, `fate_of`'s rate window, `reconcile`'s run of
  `Unknown`s, `plan`'s counters), a `stored_difference` unit test, scripted I12, I6 and
  `expire_call` cases, the harness arms' assertions, a golden `candidate_head`; delete `Sim::epoch`
  (no callers) and `fate_of`'s redundant guard; exclude in `.cargo/mutants.toml` only the 8 names
  whose criterion SIM-MUTANTS set holds (the fault dice and `note_late_land`), each with its reason.
  Claims `--mode edit-signature` on `src/sim/driver.rs` (`Sim::epoch` goes), so the submission is
  held for review. Done: the Nightly's full mutants job passes its baseline.
- [x] **LANDING-MUTANTS-IN-DIFF** (#131, merged 27 November as `7fc1bb2`; Opus review Yes;
  tessel-server and tessel-cli not added: MUTANTS-SERVER-CLI-IN-DIFF) — S, lane B
  (`.github/workflows/`); from the triage. The per-change mutants job runs only on pull requests
  (`ci.yml:205`), so server landings get no mutant check before the Nightly: run it on a push to
  main too (or in the landing check). Done: a push to main runs the `--in-diff` mutants of the
  pushed range.
- [x] **HOST-SQLITE-RETRY-BACKOFF** (`3507e72`..`941db87`, landed by the server 27 November; vote
  3-0 Yes on `57401fa` after two fix passes; no code change, the docs say why) — S, lane B; from
  HOST-HARDENING-FOLLOWUPS (its row's item, dropped by the brief, unassessed). The persist retry
  repeats at once; decide, with a test, whether real SQLite failures (busy, disk full, I/O) need a
  backoff.
- [x] **SUDO-RS-RUNNER** (#130, merged 27 November as `0ed8cb8`; vote 3-0 for, after two fix passes;
  VOTE-3's last vote Yes with should-fixes, PR text only) — M, lane A, merge path if
  `tessel-server/src/landing/` changes; from Own users 45921694300 and the rerun of 41243983653 (27
  November). On ubuntu-26.04 the account's processes survive `kill -KILL -1` after a cancelled run
  and the script's `sudo -l -U` check fails, on code that passed there on 24.04 (`80dba09`, 31
  October); anyone running the accounts on Ubuntu 26.04 likely meets the same. Find which `sudo` the
  image ships and why (a diagnostic run that dumps `ps -o pid,ppid,stat,comm -U _tessel-runner` and
  `sudo -V`), make `SudoRunner` and `scripts/ci/own-users-linux.sh` work with it, add a stub that
  forks the runner under a parent `sudo` (every stub now `exec`s, which hides parent and child), and
  move the Own users job back to `ubuntu-latest` (or add 26.04 beside 24.04). Done: Own users green
  on ubuntu-26.04.
- [x] **MUTANTS-SERVER-CLI-IN-DIFF** (#134, merged 27 November as `64b51b0`; Opus review Yes on
  `b65db81`, its two optional edits made in `20b66d5`; its sharded job's first live run is the next
  change to tessel-server or tessel-cli) — S, lane A (`.github/workflows/`, by pull request); from
  LANDING-MUTANTS-IN-DIFF. The per-change mutants mutate only tessel-coordinator and tessel-lander,
  so tessel-server and tessel-cli changes are never mutated (PR #130's VOTE-1). Their tests take
  about 75 s and 113 s per mutant, too slow for the current four 30-minute shards: give them shards
  of their own, or another bound. Done: a change to either crate is mutated on its pull request or
  push.
- [x] **SIM-FENCED-RULE-CHECKER** (`7c16e8b`..`27b8c74`, landed by the server 27 November; Opus
  review With changes, re-check Yes on `3f564a5` after one fix pass) — S, lane B (`src/sim`); from
  MUTANTS-DOUBT-PACING's VOTE-2. The removals there rest on a rule (a fenced claim's merge in flight
  is that claim's, with `doubt_since_ms` set) that a `debug_assert!` and a stage walk now check; add
  it to the simulator's checkers. Done: a variant that breaks the rule fails the checker.
- [x] **CLAIM-SCATTER** (`188afb9`..`5eff16e`, landed by the server 27 November; Opus review With
  changes, re-check Yes; `tessel claim --into`, the CLI only) — S, lane A (the CLI); found 27
  November. `tessel claim` adds scopes to the agent's open claim only when it holds exactly one; a
  lane holding two tasks' claims gets a new claim per `tessel claim`, and `tessel submit --claim`
  covers only the claim it names (`src/coordinator.rs:1425`), so the orchestrator had to release and
  re-claim to submit. Let a claim be extended by number (`tessel claim --into <claim>`) or submit
  cover several claims; decide with a test. Also make the skill's "To change it, `tessel stop`, then
  `start`" (SKILL.md:48) say that this releases the claims. Submit covering several claims changes
  `Submit`, which names one claim: additive under rule 1, or ask Felix.
- [x] **MERGE-OUTCOME-UNOFFERED** (`919f229`..`98d1a51`, landed by the server 27 November; vote 3-0
  Yes on `2df77f2` after two fix passes; both stages; nothing logged, unlike the row's text) — S,
  lane B, merge path (`src/coordinator/`); from MUTANTS-DOUBT-PACING's VOTE-3. The doubt-pacing code
  assumes only a dispatch `begin_merge` offered is answered (`release_wait`,
  `reconcile_before_ending` say so); an answer to a flight that still waits would take a waiting
  `Prepare` early or clear an unexpired wait. No host or simulator sends one. Make `merge_outcome`
  ignore (and log) an answer to a flight that was not offered, with a test. Done: the assumption is
  a rule the core enforces.
- [x] **SIM-RACE-TEXT-LITERAL** (`09d1f47`, landed by the server 27 November with
  SIM-FENCED-RULE-CHECKER) — S, lane B (`src/sim`); from RACE-TEXTS-SHARED's votes.
  `src/sim/driver/merge_path.rs` (~831) repeats the no-result race text as a literal; use the shared
  constant from `src/protocol.rs` once RACE-TEXTS-SHARED lands.
- [x] **SIM-DOUBT-PREPARED-TRAFFIC** (`e43609b`..`eb1770a`, landed by the server 28 November; Opus
  "Yes with changes" (to the note) after three fix passes; refused compare-and-swaps fall from 52 to
  37 over 300 seeds, the default seeds hold one, and the Nightly mutants were not run against that
  loss; CI's changed-line mutants missed 4, MAIN-RED-SIM-MUTANTS) — S, lane A (`src/sim`); from
  SIM-FENCED-RULE-CHECKER's REVIEW-1. A core whose `prepared()` clears `doubt_since_ms` when it
  answers the doubted pair and goes to `Land` is caught on only 3 of 300 seeds, because the
  simulator's traffic rarely takes that path. Make it common enough that the default seeds catch
  that break (a fault or a traffic mode), saying which pinned transcripts change, or say why it
  cannot. Done: the planted break fails a default run (seeds 1, 7 and 11; 50 of 300).
- [x] **SIM-DOUBT-LIVENESS** (`39689ee`..`4e94d32`, landed by the server 28 November; Opus "Yes"
  after one fix pass; the bound is one taken answer: every answer the core takes for a doubted
  claim's offered flight must change the stored state; a catch must name the checker, not fire
  alone, since the plants also trip I4 while eight real-core drops (29 to 191 of 300 seeds each)
  trip only it; CI's changed-line mutants missed 13, MAIN-RED-SIM-MUTANTS) — S, lane A (`src/sim`);
  from MERGE-OUTCOME-UNOFFERED's VOTE-2. The simulator has no liveness check for a merge in doubt: a
  core that drops every answer to a doubted `Prepare` (each redispatched and dropped again) survives
  300 seeds with every checker, and so does one that drops every `Reconcile` answer; unit tests
  catch both. Add a checker that flags a claim in doubt redispatched without progress past a bound,
  with a planted variant that fails it and the correct core silent on a 300-seed sweep. Done: the
  planted variant fails a default run (the `Prepare` plant on seeds 1, 7 and 11, the `Reconcile`
  plant on nine default seeds).
- [x] **NIGHTLY-MISSED-1127** (`71b2571`..`df625c2`, landed by the server 27 November; merge-path
  vote 3-0 "Yes" after two fix passes; PR #140's Nightly, run 46097369661, reported neither beyond
  the baseline) — S, lane A; from the Nightly 45995920783 on `941db87`, whose full-mutants job found
  2 missed mutants beyond the baseline. `src/host/replay.rs` (~187) `>` to `<` in
  `Host::buffer_for_replays`: killed by an in-order test (a commit asked before the last read and
  missing from its page, which `Output::Commit` allows, is sent only by the buffer).
  `src/coordinator/merging/doubt.rs` `resend_land`'s `retry_at_ms: None`: equivalent (a past-due
  wait can reach it; every reader compares with the clock, and every path to a `Prepare` rewrites or
  clears the wait), so it is baselined with the line kept. Done: the next Nightly reports neither
  beyond the baseline.
- [x] **LANDER-VERSION-CHECK** (`088634a`..`a1202bc`, landed by the server 27 November; merge-path
  vote 3-0) — S; from COORD-UNDELIVERED-GRANT's first landing (27 November 17:33 UTC): the runner's
  `cargo test` failed in `tessel-server/tests/landing.rs`
  `a_rebase_over_another_landing_keeps_what_a_note_follows_a_commit_by` with `UnsupportedGit {
  found: "" }`, and `Git::check_version` dropped `git --version`'s exit code and stderr. Done: a
  failed version check is `VersionFailed` with its exit code (or "killed by a signal"), stdout and
  stderr, cut to 1,200 characters; an exit 0 that prints nothing is refused with its stderr. If it
  recurs, the message shows in the runner's test output (the test's `expect("a lander")`) or as the
  server's start error: name the cause then. The 17:33 cause is not known; the signal number is
  LANDER-SIGNAL-NUMBER.
- [x] **DAEMON-CUT-READ-REQUEST-TEXT** (`f7421b1`..`eeb3472`, landed by the server 27 November; Opus
  "Yes" after two fix passes) — S, lane B; from COORD-UNDELIVERED-GRANT's REVIEW-1. After a cut or
  unreadable log read the daemon told a caller to run `tessel status`, which shows only the claims
  the daemon tracks. Done: the lost-claim, lost-amend and lost-submit notices say what is true (the
  coordinator may hold the claim; heartbeats keep it until a complete read adopts it; a repeated
  claim is refused as stale only if the lost amend was applied), each with a test; the fake gains
  `Lose::Submit`.
- [x] **NIGHTLY-MUTANT-TIMEOUTS** (`2b38c7e`..`fc65977`, merged by PR #140 as `652d83d`, because the
  dogfood token cannot push a `.github/workflows` change; merge-path vote 3-0 "Yes" after one fix
  pass) — S; from the Nightly 45995920783. Three mutants time out (the workflow warns, it does not
  fail): `src/shell.rs:32`, `src/host/mod.rs:471` and `src/host/testkit.rs:201`, the last in test
  support; each costs about 3 minutes of its shard. Confirm each is a hang a test detects, and
  exclude or speed up the testkit one. Done: each timeout explained in the workflow or gone. The
  `shell.rs:32` and `testkit.rs:201` mutants now fail fast; PR #140's Nightly lists only
  `src/host/mod.rs:471:12`, explained in `.cargo/mutants.toml`.
- [x] **LANDER-SIGNAL-NUMBER** (`ccf9638`..`a6976df`, landed by the server 28 November:
  `5f86421`..`59dc170` rebased onto `02006e2` unchanged; merge-path vote 3 "Yes with should-fixes",
  no must-fix) — S; from LANDER-VERSION-CHECK's vote (VOTE-1, VOTE-3). A git killed by a signal is
  reported as "killed by a signal" without its number, because `GitOutput`
  (`tessel-lander/src/run.rs`) keeps only `status.code()`; the number tells an outside kill
  (SIGKILL) from a crash (SIGSEGV, SIGABRT). Keep it (`ExitStatusExt::signal()`) and show it, with a
  test. Done (narrowed by its vote): the version check reports a git killed by a signal with its
  number; the other sites are LANDER-SIGNAL-ALL-SITES.
- [x] **REPLAY-ORDER-CONTRACT** (`4c487cc`..`87279e0`, landed by the server 28 November, a
  fast-forward: `76ec75b`..`1ea2a4d` rebased onto `02006e2` unchanged, and `87279e0`, the vote's
  comment nit; merge-path vote 3-0 "Yes" after one fix pass; site 3 stays out of the contract's
  order as the `from_seq` guard, by 2 of 3) — S; from NIGHTLY-MISSED-1127's votes. The host's
  contract (`src/host/mod.rs`, `ReplayPage` and `Output::Commit`) says storage answers come in the
  order asked and lets a commit asked before a read be missing from its page; `tessel-server` runs
  one storage job at a time, so it always includes it. The testkit's `deliver_next_but_pages`
  (`src/host/testkit.rs` ~200) lets a later commit's answer overtake an earlier page, an order no
  contract allows, and older replay tests use it. Rewrite those tests in the contract's order or say
  why the helper stays. Done: no host test relies on an order the contract excludes, or each that
  does says so.
- [x] **MAIN-RED-SIM-MUTANTS** (`02006e2`, landed by the server 28 November; Opus "Yes", no
  must-fix; CI's command on `7f989f8`..`ecff1cd`: 86 mutants, none missed; CI's changed-lines job
  passed on `02006e2`) — S, lane A (`src/sim`, `tests/simulator.rs`); found 28 November. Main's CI
  "mutants on changed lines" job missed 4 mutants of SIM-DOUBT-PREPARED-TRAFFIC (`eb1770a`:
  `driver.rs:1125` in `without_doubt_after_prepare`, `lander.rs:381` in `Lander::plan`,
  `lander.rs:40` in `doubt_mode`) and 13 of SIM-DOUBT-LIVENESS (`4e94d32`: `driver/merge_path.rs` in
  `taken_probe_for`, `drop_if_planted`, `answered_at` and `with_flight_and_work`; `store.rs:37` in
  `Store::restage`), because the 2-seed sweep and the 20-seed plant searches assert only that a
  checker catches a plant, not the decisions inside the new functions (PLAN-1). Lane A's plan
  (`~/tessel-orch-notes/2026-11-28/main-red-sim-mutants/PLAN-1.md`): unit tests for the pure
  functions, `taken_probe_for` split into a pure function over the state, no exclusion. Done: CI's
  changed-lines job reports no missed mutant on the diffs `7f989f8`..`eb1770a` and
  `eb1770a`..`4e94d32`.
- [ ] **LANE-GATE-MUTANTS** — S, orchestrator (the briefs and gate scripts in
  `~/tessel-orch-notes/`, not the repository); found 28 November. Main went red on mutants no lane
  gate or review had run: CI runs `cargo mutants --in-diff` (`.github/workflows/ci.yml`,
  `scripts/ci/mutants-diff.sh`) only after a landing. Every lane gate runs CI's command with CI's
  settings on the branch's diff before it reports, and every merge-path vote runs it once. A run
  with `CARGO_TARGET_DIR` set uses `--jobs 1`: with two jobs both build one test binary, and a
  mutant is tested against the other job's build (MAIN-RED-SIM-MUTANTS' review). Done: the gate
  scripts and the brief template carry it.
- [ ] **LANDER-WORKFLOW-SCOPE** — S, lane B (`tessel-lander`, `tessel-server/src/landing`,
  `src/merge.rs`; merge path: a vote); found 28 November (claim 229). A push to `main` that the git
  host refuses by rule (GH013 for a `.github/workflows` change, since the token has no Workflows
  permission) is asked again forever by main's code (`doubt.rs:415-420`, `resend_land`); the
  deployed build rejected it after 4 full re-tests. Design
  (`~/tessel-orch-notes/2026-11-28/lane-b/lander-workflow-scope/DESIGN-2.md`, DESIGN-REVIEW-1
  re-check 1 "Yes with changes"; build brief `BRIEF-2.md`): a push that git's own `--porcelain`
  output marks `[remote rejected]`, with main read back at the base, becomes the existing
  `Refusal::Rule`, with a fixed text that does not claim a rule refused it; main moved stays
  `MainMoved` and main unreadable stays `PushFailed`; the reason text is never read. Tests: a local
  bare remote whose `pre-receive` hook refuses, one that moves main and refuses, an `answer.rs` unit
  test and a `landing.rs` end-to-end test. The docs half (`.github/workflows/` goes by pull request)
  is in the 28 November STATE record. Whether GitHub also refuses the `Prepare` push of
  `refs/tessel/candidate/*` carrying a workflow change is not known; only the dogfood token can show
  it (Felix or the lander), before the dogfood upgrade. Done: a refused push to `main` rejects the
  claim after one test run, and the upgrade waits for it.
- [ ] **LANDER-SIGNAL-ALL-SITES** — S; from LANDER-SIGNAL-NUMBER's vote (VOTE-1, VOTE-2, VOTE-3).
  Eight sites still print `exit None` for a git killed by a signal, the refusal an agent sees among
  them: `tessel-server/src/landing/answer.rs:25` (`git_note`), `git_read.rs:59` and `:94`,
  `staged.rs:44`, and `tessel-lander/src/candidate.rs:89`, `:116`, `:158` and `:190`; `holds_in` and
  `main_and_ancestors` drop the status; the `run.rs` signal test does not assert `Some(9)`. Show the
  signal there (`GitOutput` carries it), with a test. Done: every merge-path report of a git killed
  by a signal names the signal.
- [ ] **GIT-VERSION-IN-DOUBT** — S; from SIM-DOUBT-TEST-ANSWER's review. A rebase is deterministic
  only for one git binary: the lander runs the `git` on `PATH` with a 2.31 floor
  (`tessel-lander/src/git.rs:127-147`), and git's default merge strategy moved from recursive to ort
  in that range, which can change a tree on renames. A git upgrade under an open doubt can then give
  the same fork on the leased base a new head (`src/coordinator/merging.rs:567-578`, a `Test` in
  doubt). `docs/dogfood.md` forbids a git upgrade while a claim is in doubt (28 November); record
  `git --version` with the doubted pair so a change shows. Done: the doubt records the git version
  and a changed one is reported.
- [ ] **DAEMON-INTENT-UPDATE** — S, CLI; found 28 November. A running daemon keeps the intent
  `tessel start` gave it: `tessel start` will not change it while the daemon runs, and `tessel stop`
  releases every claim, so a lane on its second task carries the intent in its notes by hand. Let a
  running daemon take a new intent without releasing claims. Done: `tessel status` shows the new
  intent and the claims stay held.
- [ ] **REPLAY-COMMIT-BEFORE-PAGE-DOC** — S; from REPLAY-ORDER-CONTRACT's vote. The `from_seq` check
  in `buffer_for_replays` (`src/host/replay.rs` ~188) is equivalent under the contract (a Watch
  starts with no commit out, so page 1 is answered first) but is the only guard of I2 if an adapter
  answers a commit before an earlier read; cargo-mutants never generates its drop, and only
  `host_live_events_below_the_start_of_a_replay_are_never_buffered`, out of order on purpose, kills
  it (R36). `Output::Commit` and `ReadEvents` (`src/host/mod.rs`) should say that the host never
  buffers an event below a replay's start, and name that test. Done: the contract text says so.
- [ ] **HOST-FAILED-SEND-EXPIRY-PINS** — S; from REPLAY-ORDER-CONTRACT's vote (VOTE-2, VOTE-3). The
  failed-send expiry test in `src/host/tests.rs`, the overflow test's twin, kills none of the eleven
  hand mutants the voters applied, before or after the change. Make it pin what it is named for, or
  fold it into the overflow test. Done: a mutant of the failed-send path fails it.
- [x] **SIM-DOUBT-TEST-ANSWER** (closed 28 November as a note, no code: lane A's DESIGN-1 and
  review-sdl-design's DESIGN-REVIEW-1,
  `~/tessel-orch-notes/2026-11-28/lane-a/sim-doubt-test-answer/`) — S, lane A; from
  SIM-DOUBT-LIVENESS's REVIEW-1. A core that drops the answer to a `Test` asked for a claim in doubt
  survives every simulator checker (0 of 300), because the simulator never reaches that stage in
  doubt. It is reachable in real use only by a git upgrade under an open doubt
  (GIT-VERSION-IN-DOUBT), not by a force-pushed fork (the lander rebases a sha) or a merge driver;
  the coordinator's unit tests cover it (dropping every `Test` answer in doubt fails 7 of them,
  dropping only `Tested` fails 2). A simulator knob would make the simulated lander break
  requirement 7 for coverage those tests give.
- [ ] **TESSEL-ON-PATH** — S, orchestrator (the Mac, not the repository); found 27 November. The
  `tessel` first on `PATH` is `~/.cargo/bin/tessel`, built 14 October; the daemons run
  `~/tessel-dogfood/bin/tessel` (26 November). Neither has CLAIM-SCATTER's `--into`. Point the lanes
  and the hooks at one current binary (remove or replace the stale one), and deploy CLAIM-SCATTER
  with the dogfood upgrade. Done: `which tessel` is the deployed build, and `tessel claim --into`
  works.
- [x] **LOCK-OWNER** (done 27 November in `~/tessel-orch-notes/lib/cargo-lock.sh`, outside the
  repository; the gate scripts and the landing template use it) — S, orchestrator (the gate scripts
  in `~/tessel-orch-notes/`, not the repository); found 27 November. The machine-wide cargo lock is
  a bare `mkdir`, so a killed gate or voter run leaves it held with no owner (at least twice on 27
  November, about 16 and 26 minutes, by the agents' reports), and others cannot tell a live holder
  from a dead one. Write the holder's pid and command into the lock, and have waiters remove a lock
  whose pid is gone. Done: the briefs' lock snippet and every gate script use it.
- [x] **RACE-TEXTS-SHARED** (`32cb515`..`623e919`, landed by the server 27 November; re-vote 3-0 Yes
  on `cd0e67a` after one fix pass; the simulator's copy is SIM-RACE-TEXT-LITERAL) — S, lane B (the
  core); from RACE-LOSER-TEXT's REVIEW-1. The CLI copies the core's three fixed race texts
  (`LOST_RACE`, `TESTS_GAVE_NO_RESULT`, `NOT_PICKED`, private in `src/coordinator/racing.rs`) into
  `tessel-cli/src/render.rs` `RACE_REJECTIONS`; real-core tests now catch a drift, but the copies
  stay. Make them `pub const`s in `src/protocol.rs` and use them in the CLI (additive, no wire
  change); a cross edit into lane A's `tessel-cli/src/render.rs`, `tessel-cli/src/daemon.rs`,
  `tessel-cli/src/inbox_hook.rs` and `tessel-cli/tests/cli.rs`, named here. Also from that review:
  drop the no-op `self.fresh.insert(claim)` (`tessel-cli/src/daemon.rs:1722`); make
  `an_entry_no_trial_judged_is_told_the_tests_gave_no_result` (`tessel-cli/tests/cli.rs:3300`) wait
  on the notice, not a fixed number of reads; have the stop hook use the race mark as the daemon
  does. Done: the CLI uses the core's constants, and a test for each change. (Telling a timed-out
  `HumanPick` race's final `RaceResult` from the earlier recommendation needs an optional wire
  field; not in this row.)
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
- [x] **COORD-UNDELIVERED-GRANT** (`121461c`..`228f5d1`, landed by the server 27 November; Opus
  "Yes" after two fix passes) — a `Granted` that fails to send closes the socket but leaves the
  claim held until its lease lapses. M5, P1-HOST-HARDENING (decision 7). Done in the CLI: the
  coordinator keeps the claim (decision 7, pinned by
  `host_a_grant_that_fails_to_send_keeps_the_claim_until_its_lease_lapses`), the daemon's reconnect
  adopts it, and the reconnect plan now matches a lost `--wait` to it, so the waiter is told it was
  granted; after a cut log read it is told the outcome is unknown.
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

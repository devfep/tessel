# Candidate ref: where the staged landing keeps the prepared head

Status: design note (CANDIDATE-REF, M5), approved by Felix on 25 October (decision M5-7),
including the new `CandidateMissing` answer as a `Call` change (decision 12). No code. It settles
where the head that `Prepare` makes lives until `Land`, so that
P1-SERVER-STAGED, P1-CORE-STAGED-LANDING and P1-CORE-RECONCILE can be built without guessing.
Requirements C5, C6, C7, S1, S3 are those of `docs/proposals/real-use.md`; rows and decisions
are those of `docs/proposals/phase-1.md`.

## The problem

Today one `Prepare` call is the whole merge (`tessel-server/src/landing/mod.rs`, `prepare`):
`run_merge` clones main into `call-N/main`, fetches the agent's fork, rebases, checks coverage,
clones a test tree, runs the runner, pushes main and reads it back. The head exists only in that
call's directory. `CallDir` removes the directory when the call ends, and `clear_leftovers` removes
`git` and every `call-*` at server start. A kill anywhere loses the head; the core's cut-off
recovery (R24) re-dispatches `Prepare`, which rebases again (C7 gives the same head) and, if the
push had happened, answers `AlreadyMerged`.

The staged landing splits that into `Prepare`, a journaled head, `Test`, `Land`. `Test` and
`Land` run later, possibly after a restart, and see only `Request::Test { base, head }` and
`Request::Land { base, head }`. So the head, an object that exists nowhere else, must be stored
by something that outlives a process. The core never runs git (rule 4), so the lander must.

## Decision

1. **`Prepare`** rebases as today, then pushes the head to `refs/tessel/candidate/<head>` on
   `remote`, the repository it lands on, where `<head>` is the 40-hex sha.
2. **`Test`** makes a repository with `git init`, fetches that ref into it, checks `HEAD == head`,
   and runs the runner on it. It holds no workspace that has ever been pushed from.
3. **`Land`** reads the ref first. If it is present: clone main, fetch the ref, check the tip is
   `head`, push `head` to main with the lease on `base`, read main back by ancestry
   (P1-LANDER-ANCESTRY), delete the ref.
4. **A kill anywhere leaves the ref.** A retried `Land` pushes the same head.
5. **A `Test` whose ref is gone** answers the new `CandidateMissing`, never `Landed`. **A `Land`
   whose ref is gone** reads main then: `Landed` if it contains the head, otherwise
   `CandidateMissing`. The core sends the submission back to `Prepare` under a new dispatch id. A
   head that landed unseen comes back as `AlreadyMerged` on that `Prepare` (C7).

Two corrections to the shape in `phase-1.md`.

**The ref is named by the head, not the dispatch id.**
- `Test` and `Land` carry `{base, head}`, not the dispatch id of the `Prepare` that made the ref,
  and the plan gives each stage its own dispatch id. Naming by dispatch id needs a new field on
  both requests, which is a `Call` change (decision 12) for no gain.
- Dispatch ids restart in a fresh store, and DOGFOOD-UPGRADE (d) makes a fresh store the
  rollback. `candidate/7` from before would collide with the new dispatch 7. A sha cannot.
- C7 makes the head a function of the commit and main (the committer and its date are fixed in
  `git.rs`), so a re-`Prepare` after a kill pushes the same sha to the same name. A tip other
  than the name is damage or tampering, and is read as a missing ref.
- The cost: an operator cannot read the dispatch from the name.

**The ref lives on `remote` itself, not in a repository of the lander's.**
- The objects are shared with main, so there is nothing to seed, a push carries only the agent's
  own commits, and nothing new is needed from Felix: the token already covers `remote`. A separate
  repository would gain every main commit after its seed as new, and the token has no Workflows
  permission (`docs/dogfood.md`), so each merged workflow pull request would make every `Prepare`
  push fail until Felix re-seeded it.
- A ref outside `refs/heads` triggers no push workflow (`ci.yml` runs `push` for `main` only),
  matches neither ruleset (both target `~DEFAULT_BRANCH`) and is not fetched by a default clone.
- A commit that touches workflows is refused by GitHub at the candidate push, as it is at the main
  push today. The push fails, the lander answers `Unknown`, the core retries and then rejects as
  infrastructure; only the point moves, to before the tests.
- The first step of P1-SERVER-STAGED checks, against `tessel-sandbox` over HTTPS through the
  askpass hook, that GitHub accepts a push to `refs/tessel/*`. If it does not, the ref is
  `refs/heads/tessel/candidate/<head>` on the same repository; the rest of this note is unchanged,
  because the name is one constant and the fetch refspecs name it. The probe is a task step, not
  something this note ran.

## 1. Each call: inputs, effects, answers, what survives a kill

The core persists the head in its in-flight record before it sends `Test` (rule 5 applies to
calls), and each stage is its own dispatch id. The lander keeps nothing between calls: its
`work_dir` holds only throwaway clones, and the ref on `remote` is the one durable store.

| Call | Inputs | Effects | Answers |
|---|---|---|---|
| `Prepare` | claim, agent, fork commit, scopes | clone main, fetch the agent's fork (read), verify, rebase, coverage, gate; push the candidate; then sweep | `Prepared { base, head }` after the push; `Refused`; `Unknown` for anything that did not finish |
| `Test` | base, head | read the ref; fetch it into a new repository, tip must be `head`; run the runner; no write | `Tested`; `Refused { TestsFailed }`; `CandidateMissing`; `Unknown` |
| `Land` | base, head | read the ref; clone main; fetch the ref; push main with the lease; read back by ancestry; delete the ref | `Landed { head }`; `CandidateMissing`; `Unknown` (lease lost, push or read failed); `Refused { Rule }` |
| `Reconcile` | head | read main, ancestry of head. Reads only | `Reconciled { main, landed }`; `Unknown` |

A rebase whose head equals the base is `AlreadyMerged` today (no push); that stays and pushes
nothing. Under the stand-in the core reads `Prepared` with `base != head` as a merge that landed,
so `Prepare`'s behaviour and the core's reading of it must flip in one commit (section 7).

If `Land` finds the ref present and the head already an ancestor of main (a kill after the push),
it answers `Landed` without pushing and deletes the ref.

What survives a kill, by window (each becomes a P1-FAILPOINTS window, section 7):

| Killed | Left behind | What happens next |
|---|---|---|
| in `Prepare`, before the push | nothing | the core re-dispatches `Prepare`; same head by C7 |
| after the push, before the answer is persisted | the ref; core has no head | re-`Prepare` pushes the same name; stale refs swept |
| after the head is journaled, before `Test` answers | the ref, the head | recovery re-sends `Test` with the journaled head under a new dispatch id, at once; it counts as an infra attempt, not in doubt |
| in `Test` | a repository in `call-N` (removed at start) | same |
| in `Land`, no answer | the ref, maybe main at the head | the claim is in doubt: a fresh `Prepare` goes first, and answers `AlreadyMerged` if it landed |
| after `Land` pushed main, before the read-back | main at the head, the ref | `Unknown`; `Reconcile` says landed; the sweep removes the ref |
| after the read-back, before the delete | main at the head, the ref | the answer is lost: the same |
| after the delete, before the answer persists | main at the head, no ref | `Reconcile` says landed |
| `Land` sent twice (late copy) | none | the ref is gone and main has the head: `Landed`, no push |

What is tested is what lands: `Test` checks the tree's tip is `head`, `Land` checks the fetched
tip is `head` and pushes that sha, so I3 holds across processes. And a `Land` never relies on
memory: it clones main afresh, so a restart changes nothing.

## 2. The missing-ref outcome

**Name:** `CallAnswer::CandidateMissing`, a unit variant, stored as `{"kind":"candidate_missing"}`.
Only `Test` and `Land` give it, and only when the lander positively read the ref as absent
(`ls-remote` exits 0 with no line) or with a different tip, and, for `Land`, main does not contain
the head. A failed read is `Unknown`. `Land` reads the ref before main: a late, abandoned `Land`
can push and delete the ref after a retry has read main, and reading main first would then answer
`CandidateMissing` for a head that is on main. With the ref read first, a ref absent at that
moment means any earlier `Land` had already pushed, so the main read that follows sees the head.

**It changes `Call`.** `CallAnswer` is part of the frozen design, so decision 12 applies and Felix
approves the variant with this note (M5-7). Alternatives, all worse:
- `Refused { Rule { rule: Other } }` needs no `Call` change, but a refusal rejects the work and
  the core never retries it, so the first lost ref would reject good work.
- `Unknown`: the core reconciles; main is still at `base` and the head is not on it, so it re-sends
  `Land`, which finds the ref gone again. Nothing after `Land` counts toward `MAX_INFRA_RETRIES`
  (real-use.md, "Calls"), so the loop never ends and the queue is blocked behind it.
- `Land` carrying the fork commit so the lander can re-`Prepare` itself: a larger `Call` change,
  and it gives `Land` the coverage and gate checks `Prepare` owns.

**Rollback is safe.** `#[serde(other)]` reads a variant an older build does not know as `Unknown`,
the one answer that decides nothing. DOGFOOD-UPGRADE (d) allows rollback only with an idle queue,
so no `Test` or `Land` is outstanding to receive it.

**Before P1-CORE-STAGED-LANDING** the core never sends `Test` or `Land`, so nothing can answer
`CandidateMissing`. Its arms are still required (rule 2): `merge_outcome_of` and `trial_report_of`
read it as the service being unavailable, like every answer of the wrong kind; `head_of` gives
`None`; `moves_main` in `tessel-server/src/repo.rs` gives `false`. The "an answer of another kind"
tests in `src/host/tests.rs` gain it.

**After:** `MergeOutcome` gains `CandidateMissing`, and the core treats it as a fact, not a doubt.
Two things back that: the lander read that main did not contain the head, and `Stop` blocks an
abandoned call from pushing. Once the lander is its own process (P1-LANDER-OUT, M6), the backstop
is C7: if the head did land unseen, the re-`Prepare` rebases onto a main that has it, gets
`head == base` and answers `AlreadyMerged` (`merge.rs`, `prepare`), which the core already reads
as landed. So `in_doubt` stays false and `unanswered_from` is not set. The core clears the
journaled head and re-dispatches `Prepare` with a new dispatch id, counting an infrastructure
failure: `infra_failures` rises, the backoff applies, and the fourth rejects ("infrastructure,
not a code failure; submit again"). That bounds a ref that keeps vanishing (a lane agent with
Felix's credential can write any non-main ref) without rejecting on the first loss. It does not
use `MAX_MAIN_MOVED`, which counts contention. The core test: `CandidateMissing` after a journaled
head re-dispatches `Prepare` under a new id, is not in doubt, and the fourth in a row rejects.

**After (30 October, decided by Felix): a fact outside a doubt, not inside one.** The paragraph
above holds for a `Land` that was never in doubt. For a submission whose `Land` is in doubt, the
two premises fail: `Stop` is check-then-act (a push already started outlives it), and the C7
backstop comes too late, since the core's own re-`Prepare` re-creates the same ref before a copy of
the first `Land` that has not run yet, and a bound that rejects work then loses to that copy
landing it (simulator seed 69962521). The only fence a late copy cannot get round is its lease:
it changes main only while main is still at its `base`. So each doubt carries the lease of its own
latest `Land`: `Submission::doubted_land` holds that `Land`'s `(base, head)`, written at every
doubt, while `unanswered_from` stays the baseline a landing is verified from (the first doubt's).
A doubt with a pair (a "fenced" doubt) is kept on the submission, not on the flight:
- `CandidateMissing`, or a refusal at `Test` or `Land`, keeps it in doubt and goes back to
  `Prepare` after the doubts' capped backoff. A restart that cut off a `Test` or a `Prepare` does
  the same. Nothing is counted and nothing is rejected, and neither is any other failure of the
  merge path in doubt.
- A refusal at `Prepare` (a fork branch force-pushed while in doubt reads `CommitNotInFork`, and a
  late copy can still land the re-made ref) says nothing of where main is, and the refusals that
  persist (that one, a `Conflict` after main moved outside the queue) would otherwise wait for
  ever. So it asks a `Reconcile` first, after the doubts' capped backoff, and nothing is counted:
  main at the base sends the `Land` again (the claim then goes round `Prepare`, `Reconcile` and
  `Land` until the operator answers), main elsewhere ends the doubt so that the fresh `Prepare`
  rejects, and the head on main lands the work.
- A `Prepared` on the pair's base with the pair's head goes straight to `Land`: that head passed
  its `Test`, and the ref `Prepare` re-made lets either copy land it.
- The doubt ends only on `Landed`, `AlreadyMerged` (landed from `unanswered_from`), the operator's
  not-landed answer, a `Prepared` whose base is not the pair's base, a reconcile that finds main
  elsewhere, or `MainMoved` answered to a `Land` (neither `tessel-server`'s lander nor the
  simulator's answers that to a `Land`: both read it as `Unknown`, so it is reachable only with
  another lander). `MainMoved` at `Prepare` or `Test` does not end it by itself: the lander's pin
  and its clone are separate reads, so a late copy landing in between reads as a move. A
  `Reconcile` under a new dispatch id asks first, and its answer lands the work or ends the doubt.
  Then the counting and the bound apply again.
- A doubt with no pair (a marker stored before the staged landing, or an infrastructure failure in
  doubt from the build before it) has no late `Land` to fence, so it behaves as at 042b87a: a
  refusal or `TestsFailed` rejects, an infrastructure failure counts and is rejected at
  `MAX_INFRA_RETRIES` (a `Test` that hangs included), and a `Prepared` does not end it. It gets a
  pair the first time a `Land` is
  sent for it. Until then the operator has nothing to resolve (`resolve_doubt` answers a `Land`),
  which the runbook's "Nothing in flight" check makes moot.
- The cost: a ref that keeps vanishing after a doubted `Land` holds the queue, at the capped
  backoff, until main moves or the operator runs `resolve_doubt`, which works at any stage of a
  fenced doubt. That is the wait `real-use.md` ("Calls") already accepts for a doubt.
- The `Prepare` that `wait_in_doubt` makes is in flight with no call out until `begin_merge`
  offers it (which clears the wait). A wake that finds it before then is not a restart that cut
  a call off, and must not renew the wait.

## 3. Cleanup of orphaned refs

Orphans come from a kill after the push, a rejected submission, a failed delete, and a head the
core abandoned (main moved, a new `Prepare`).

- **Normal path:** `Land` deletes its ref after the read-back found the head on main. A failed
  delete is logged and does not change the answer: the landing is the truth.
- **The sweep:** each `Prepare`, after its candidate push succeeded, lists
  `ls-remote refs/tessel/candidate/*` and deletes the other refs in a separate, best-effort push:
  one refspec per ref, each with `--force-with-lease=<ref>:<listed sha>`, not atomic, so a ref
  that vanished meanwhile fails only itself. Its failure is logged, never an answer. This is
  safe because the merge queue is serial and one submission has one head per base: a `Prepare`
  that follows a `Land` in doubt (see section 2, "After (30 October...)") makes the same head on the
  same base, so the sweep keeps its ref, and an earlier submission's head is dead once its stage
  is over.
- **Parsing:** lines are accepted only as `<40-hex> refs/tessel/candidate/<40-hex>`; anything else
  is skipped, since another writer controls those names (rule 3).
- **Bound:** the sweep reads at most 1000 listed lines and deletes at most 50 refs per `Prepare`,
  so a damaged remote cannot push one call past its deadline; the rest go on later calls. In steady state `remote` holds one
  candidate after a rejected or abandoned submission and none after a landing.
- **Not done:** `Reconcile` and `Test` delete nothing. A read that writes would make a status
  probe a mutation, and `Test` should hold no write path while repo code runs.

Tests: a kill after the push leaves one ref and the next `Prepare` ends with one; a `Prepare`
for the same head keeps its ref; a ref deleted between the listing and the sweep does not fail
the `Prepare`; a listing with a malformed name
deletes nothing; `Land` deletes the ref and a failed delete still answers `Landed`.

## 4. The credential

Nothing new: the candidate push, the reads and the deletes use the lander's existing token through
the askpass hook (`tessel-server/src/credential.rs`) on the host of `remote`, already in the hook's
list. No config key and no step for Felix.

`merge.rs` documents `PushUpdate` as "the one update the write credential may be used for:
main", and `with_push_access` guards it. The candidate writes would use the same credential
outside that port. So `MergePorts` gains `with_candidate_access`, which takes a typed
`CandidateWrite` (`Push { head }` or `Delete { ref, sha }`, as `with_push_access` takes
`&PushUpdate`), checks `Stop` and then runs the closure. The contract is reworded: the write
credential is used for two things, the main push and the candidate writes, each only inside its
port method. `Stop` is checked before every remote write (the candidate push, each sweep push and
the delete), not only on entry to the call. `with_push_access` today checks it only before
the closure.

**No credential is live while repo code runs.** The only process that runs repo code is the
runner, inside `Test`, after the lander's git commands for that call have exited. Git reads the
token only through the askpass hook, which exists only in the environment of a lander git command,
and the runner clears its environment (`tessel-runner/src/supervise.rs`). The test repository's
`origin` is `remote`'s URL without a credential. This is a sequencing guarantee: the token file is
still readable by anything running as the server's user, which RUNNER-OWN-UID closes (D4). What
this design adds is that no `Workspace` exists during `Test`, so the note in `workspace.rs` ("the
tree's `origin` and reflog name [the workspace]") stops applying and repo code has no path to the
clone `Land` pushes from; `Land` makes that clone after the runner returned. Test: a runner test
double asserts, while it runs, that no `Workspace` directory exists under the call and its
environment has neither `GIT_ASKPASS` nor the token path.

## 5. `prepare` and `land` in the lander crate

M4-SANDBOX-WORKSPACE asked whether `tessel_lander::merge::{prepare, land}` stay public.
- `prepare` stays `pub`: the server calls it, in its own crate, as `Prepare`. It still returns
  `Ready` with a `Workspace`; the server pushes the candidate from that workspace and drops it.
- `land` becomes `pub(crate)`. A caller could pass it any `Workspace` and any `PushUpdate`, so the
  push of main would not be tied to a head that was tested. Its callers are a new `pub`
  `land_candidate` (reads the ref, clones main, fetches it, checks the tip, calls `land`, deletes
  the ref) and, until the flip, `run_merge`.
- New `pub` functions: `push_candidate`, `sweep_candidates`, `read_candidate` (tip or absent) and
  `fetch_candidate_tree` (`git init`, fetch the ref, check out the head; returns a `TestTree`, so
  its configuration is the one git wrote, as for a clone). New `Git` commands: fetch, push,
  listing and delete of the candidate ref. `MergeSources` needs no new field.
- `run_merge`, `STAND_IN_MESSAGE` and the stand-in branch in `run` are deleted when `Prepare`
  flips, not kept. `run_package_step` keeps taking a `&TestTree`, so the boundary the
  compile-fail doctests pin is unchanged.

Tests: a compile-fail doctest that `land` is not callable outside the crate; the isolation tests
(`tessel-lander/tests/isolation.rs`) re-run on `land_candidate` and `fetch_candidate_tree` with
hooks, filters and a malicious `.git/config` in the fetched history (S3); a `Land` whose ref has
the wrong tip answers `CandidateMissing` and pushes nothing; an abandoned call performs no
remote write.

As built (P1-SERVER-STAGED, P1-CORE-STAGED-LANDING): `land_candidate` is the only caller that takes
a head from the candidate ref, and it requires the base to be an ancestor of the head before it
pushes, because the lease push rewinds or replaces main when the pair is wrong. A pair that fails
the check is `MergeOutcome::HeadNotOnBase`, which the server answers `CandidateMissing`: the core
goes back to `Prepare` under a new dispatch id and counts an infrastructure failure, so a wrong
pair ends in a rejection after `MAX_INFRA_RETRIES` and does not loop through reconcile. The server's
`Prepare` is `prepare` plus `push_candidate` and `sweep_candidates`; its `Test` and `Land` are
`fetch_candidate_tree` and `land_candidate`. `run_merge` and `land` are no longer called by the
server but stay `pub`: the tests ported from the steward's `runMerge` call them (`merge_steps`,
`test_tree`, the `Repos` support), and rewriting them to the staged calls is its own task. The
isolation tests are `tessel-lander/tests/isolation_candidate.rs`.

## 6. Alternatives considered

| Alternative | Why not |
|---|---|
| A local ref, bundle or bare repository | Dies with the disk (M6 has none), is writable by repo code as the same uid, and keeps a pushable `Workspace` beside the tests |
| A separate candidates repository | Each merged workflow pull request breaks every `Prepare` (no Workflows permission) until Felix re-seeds it |
| `refs/heads/tessel/candidate/<head>` | The fallback if GitHub refuses `refs/tessel/*`; a branch is fetched by a default clone |
| The head in the core's state, or in the agent's fork | The core never runs git (rule 4); the agent can rewrite its fork |
| One branch per repository | A new `Prepare` would overwrite the head a retried `Land` needs |
| Name by dispatch id | Needs new fields on `Test` and `Land`; collides after a fresh store |
| Re-`Prepare` inside `Land` | Needs the fork commit, agent and scopes in `Land` |
| `Unknown` for a missing ref | Loops forever (section 2) |

## 7. Failpoints and the order of tasks

Windows this adds to P1-FAILPOINTS, each with a test that the claim lands once or is re-prepared
and `remote` ends with no ref, or at most one stale one:
1. `Prepare` killed after the candidate push, before the answer is persisted.
2. `Land` killed after the main push, before the read-back.
3. `Land` killed after the read-back, before the ref delete.
4. The ref removed between `Prepare` and `Test`, and between `Test` and `Land`, with main at
   `base` (`CandidateMissing`) and with main at the head (`Landed`).
5. The ref moved to another commit between `Prepare` and `Test`.
6. A late `Land` for a head that landed: `Landed`, no second push.

Order, with the corrections this note makes to `phase-1.md`:
1. CANDIDATE-REF (this note), then MERGE-PATH-REVIEW.
2. **P1-SERVER-STAGED** starts with the probe above. It then adds the variant, the lander
   functions with their tests and the `with_candidate_access` port method (a signature change in
   `tessel-lander/src/merge.rs`), plus `Test` and `Land`, beside `call.rs`, `calls.rs` and
   `repo.rs`. `Prepare` stays the whole merge. The adapter tests make the ref with plain git, so
   no function is dead. It edits no lane A file beyond the three the plan lists.
3. **P1-CORE-STAGED-LANDING** also edits `tessel-server/src/landing/mod.rs` (lane B's): the
   server's `Prepare` becomes rebase-and-push-candidate in the same commit that makes the core
   stage the landing. Server and core are one binary, so one commit flips both. Two commits would
   let a core that still treats `Prepared` as landed read a candidate push as a merge, a false
   `Merged` (rule 6). `run_merge` and the stand-in message go in that commit.
4. P1-CORE-RECONCILE: the "candidate ref" in its in-flight record is the head (the name derives
   from it); the `CandidateMissing` core test and the fixture test (c) are part of it.
5. P1-FAILPOINTS: the windows above.

Not decided here: the `Test` and `Land` deadlines (the stage split of `STEWARD_CALL_TIMEOUT_MS`,
P1-CORE-STAGED-LANDING).

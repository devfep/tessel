# Candidate ref: where the staged landing keeps the prepared head

Status: design note (CANDIDATE-REF, M5), for an adversarial review and then Felix's decision
(M5-7). No code. It settles where the head that `Prepare` makes lives until `Land`, so that
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

1. **`Prepare`** rebases as today, then pushes the head to the lander's candidate repository:
   `refs/heads/tessel/candidate/<head>`, where `<head>` is the 40-hex sha. One candidate
   repository per landing repository, owned by the lander, never an agent's fork.
2. **`Test`** makes a single-branch clone of that ref, checks `HEAD == head`, and runs the runner
   on it. It holds no workspace that has ever been pushed from.
3. **`Land`** reads main and the ref, clones main, fetches the ref, checks the tip is `head`,
   pushes `head` to main with the lease on `base`, reads main back by ancestry (P1-LANDER-ANCESTRY),
   and deletes the ref.
4. **A kill anywhere leaves the ref.** A retried `Land` pushes the same head.
5. **A `Land` (or `Test`) whose ref is gone** answers `Landed` if main already contains the head,
   and otherwise the new answer `CandidateMissing`. The core sends the submission back to
   `Prepare` under a new dispatch id.

One correction to the shape in `phase-1.md`: the ref is named by the head, not by the dispatch id.
Reasons, from the code:

- `Test` and `Land` carry `{base, head}`, not the dispatch id of the `Prepare` that made the
  ref, and the plan gives each stage its own dispatch id. Naming by dispatch id needs a new field
  on both requests, which is a `Call` change (decision 12) for no gain. The head is already there.
- Dispatch ids restart in a fresh store, and DOGFOOD-UPGRADE (d) makes a fresh store the rollback.
  `candidate/7` from before the rollback would collide with the new dispatch 7. A sha cannot.
- C7 makes the head a function of the commit and main, so a re-`Prepare` after a kill pushes the
  same sha to the same name: an idempotent push. A tip other than the name is tampering or damage,
  and is read as a missing ref.
- The cost is that an operator cannot read the dispatch from the ref name. `Prepare` logs
  `dispatch`, `claim`, `base`, `head` when it pushes, which `P1-CORE-RECONCILE`'s status line
  can quote.

Everything else in the recommended shape stands. `refs/heads/` rather than a private namespace
keeps Phase 2's shape: the same ref becomes the head of the candidate pull request.

## 1. Each call: inputs, effects, answers, what survives a kill

The core persists the head in its in-flight record before it sends `Test` (rule 5 applies to
calls), and each stage is its own dispatch id. The lander keeps nothing between calls: its
`work_dir` holds only throwaway clones, and the candidate repository is the one durable store.

| Call | Inputs | Effects | Answers |
|---|---|---|---|
| `Prepare` | claim, agent, fork commit, scopes | read main; clone main; fetch the agent's fork (read); verify, rebase, coverage, gate; list stale candidates; one atomic push to the candidate repository | `Prepared { base, head }` after the push; `Refused` (conflict, uncovered, gate, not in fork, rule); `Unknown` for anything that did not finish |
| `Test` | base, head | list the ref; clone it, tip must be `head`; run the runner; no write | `Tested`; `Refused { TestsFailed }`; `CandidateMissing`; `Unknown` |
| `Land` | base, head | read main; read the ref; clone main; fetch the ref; push main with the lease; read back by ancestry; delete the ref | `Landed { head }`; `CandidateMissing`; `Unknown` (lease lost, push or read failed); `Refused { Rule }` |
| `Reconcile` | head | read main, ancestry of head. Reads only; deletes nothing | `Reconciled { main, landed }`; `Unknown` |

`Prepare` with a head equal to the base is `AlreadyMerged` today (no push needed); that stays and
pushes nothing. Under the stand-in the core reads `Prepared` with `base != head` as a merge that
landed, so `Prepare`'s behaviour and the core's reading of it must flip in one commit (see task
order).

What survives a kill, by window (each becomes a P1-FAILPOINTS window, section 7):

| Killed | Left behind | What happens next |
|---|---|---|
| in `Prepare`, before the push | nothing | the core re-dispatches `Prepare`; same head by C7 |
| after the push, before the answer is persisted | the ref; core has no head | re-`Prepare` pushes the same name; stale refs swept |
| after the head is journaled, before `Test` answers | the ref, the head | recovery re-sends `Test` (it counts as before `Land`: an infra attempt) |
| in `Test` | a clone in `call-N` (removed at start) | same |
| after `Land` pushed main, before the read-back | main at the head, the ref | `Unknown`; `Reconcile` says landed; the sweep removes the ref |
| after the read-back, before the delete | main at the head, the ref | the answer is lost: the same, via `Reconcile` |
| after the delete, before the answer persists | main at the head, no ref | `Reconcile` says landed; nothing else |
| `Land` sent twice (late copy) | none | main contains the head: `Landed`, no push |

Two properties carry the table. What is tested is what lands: `Test` checks the clone's tip is
`head` (the existing `TestTreeNotAtHead` check), `Land` checks the fetched tip is `head`, and
`Land` pushes that sha, so I3 (checks on exactly the landed commit) holds across processes. And a
`Land` never relies on state in memory: it clones main afresh, so a restart changes nothing.

## 2. The missing-ref outcome

**Name:** `CallAnswer::CandidateMissing`, a unit variant, stored as `{"kind":"candidate_missing"}`.
Only `Test` and `Land` give it. The lander gives it only when it positively read the ref as absent
(`ls-remote` exits 0 with no line) or with a different tip, and, for `Land`, main does not contain
the head. A failed read is `Unknown`, never `CandidateMissing`.

**It changes `Call`.** `CallAnswer` is part of the frozen design, so decision 12 applies and Felix
approves the variant with this note (M5-7). Alternatives, all worse:

- `Refused { Rule { .. } }` (`RuleKind` is also a closed enum, so also a `Call` change) and a
  refusal rejects the work: the core never retries a refusal, and the work was good.
- `Unknown`: the core reconciles; main is still at `base` and the head is not on it, so it re-sends
  `Land`, which finds the ref gone again. Nothing after `Land` counts toward `MAX_INFRA_RETRIES`
  (real-use.md, "Calls"), so the loop never ends and the queue is blocked behind it.
- `Land` carrying the fork commit so the lander can re-`Prepare` itself: a larger `Call` change,
  and it gives `Land` the coverage and gate checks `Prepare` owns.

**Rollback is safe.** `#[serde(other)]` reads a variant an older build does not know as `Unknown`,
the one answer that decides nothing. DOGFOOD-UPGRADE (d) already allows rollback only with an idle
queue, so no `Test` or `Land` is outstanding to receive it.

**Before P1-CORE-STAGED-LANDING** the core never sends `Test` or `Land`, so nothing can answer
`CandidateMissing`. Its arms are still required (rule 2): `merge_outcome_of` and `trial_report_of`
read it as the service being unavailable, like every answer of the wrong kind; `head_of` gives
`None`; `moves_main` in `tessel-server/src/repo.rs` gives `false`. The existing tests at
`src/host/tests.rs` for "an answer of another kind" gain it.

**After:** `MergeOutcome` gains `CandidateMissing`. The core treats it as a fact, not a doubt:
the lander has read that main does not contain the head, and one server per repository is the only
writer, so nothing can land the head afterwards (`Stop` already blocks an abandoned call from
pushing). So `in_doubt` stays false and `unanswered_from` is not set. The core clears the journaled
head and re-dispatches `Prepare` with a new dispatch id, counting it as an infrastructure failure:
`infra_failures` rises, the backoff applies, and the fourth rejects ("infrastructure, not a code
failure; submit again"). That bounds a ref that keeps vanishing (a second server on the same
fork, a push that succeeds without writing) without ever rejecting on the first loss. It does
not use `MAX_MAIN_MOVED`, which counts contention. The core test: `CandidateMissing` after a
journaled head re-dispatches `Prepare` under a new id, is not in doubt, and the fourth consecutive
one rejects.

## 3. Cleanup of orphaned refs

Orphans come from a kill after the push, a rejected submission, a failed delete, and a head the
core abandoned (main moved, a new `Prepare`).

- **Normal path:** `Land` deletes its ref after the read-back found the head on main. A failed
  delete is logged and does not change the answer: the landing is the truth.
- **The sweep:** every `Prepare` removes the candidate refs it does not need, in the same atomic
  push that creates the new one (`git push --atomic`, one refspec per deletion). It lists with
  `ls-remote refs/heads/tessel/candidate/*` first. This is safe because the merge queue is
  serial: while a `Land` is in flight or in doubt the core dispatches no `Prepare`, and an
  earlier submission's head is dead once its stage is over. The one live ref at `Prepare` time is
  the same head being re-pushed, which the sweep keeps.
- **Bound:** at most 50 deletions per `Prepare`, and the listing is capped at 1,000 lines, so a
  fork damaged by hand cannot make one call run past its deadline; the rest go on later calls.
  In steady state the candidate repository holds one ref after a rejected or abandoned
  submission and none after a landing. An idle repository keeps at most that one.
- **Not done:** `Reconcile` and `Test` delete nothing. A read that writes would make a status
  probe a mutation, and `Test` should hold no write path while repo code runs.
- **Why one repository per landing repository:** the sweep deletes every candidate it does not
  recognise, so two landing repositories sharing a candidate repository would delete each other's
  refs. A second server on one repository degrades to `CandidateMissing` and a retry, not to a
  wrong landing.

Tests: a kill after the push leaves exactly one ref and the next `Prepare` ends with one; 60
stale refs take two `Prepare`s to clear; a `Prepare` for the same head keeps its ref; `Land`
deletes the ref and a failed delete still answers `Landed`; two landing repositories configured
with the same candidate location are refused at startup.

## 4. The credential

The candidate push, the ref reads and the delete use the lander's existing token through the
askpass hook (`tessel-server/src/credential.rs`): the hook answers for the hosts of `remote` and
`fork` in `Landing::new`, so the host of the candidate repository joins that list, and the push
carries the same `x-access-token`. No new secret, no new code path for credentials.

Config: a `candidates` key beside `remote` and `fork` in `[[repositories]]`
(`tessel-server/src/config.rs`), required with `remote`, validated like `fork` (no `{agent}`
placeholder), and refused when it equals `remote` or any agent fork. A path or a URL, so local and
adapter tests use bare repositories and need no GitHub.

Felix's steps for the dogfood server (M5-7), before the first staged landing:

1. Create a private repository for it with `gh repo create devfep/tessel-candidates --private`.
   A GitHub fork of `devfep/tessel` is not possible on the same account, so it holds no shared
   objects: seed it with the whole of main, and turn Actions off as for the lane forks
   (`docs/dogfood.md`), because a push of a workflow file would otherwise run it.
2. Add it to the repositories of the fine-grained token "tessel-dogfood-lander": Contents read and
   write, Metadata read, nothing else (never Workflows or Administration). One more repository for
   the sandbox landing of DOGFOOD-UPGRADE (e), which needs its own candidate repository.
3. Add `candidates = "https://github.com/devfep/tessel-candidates.git"` to the server config.
   `scripts/dogfood/setup.sh` writes that file and `docs/dogfood.md` documents it, both sensitive
   paths, so those two edits are a pull request Felix merges.

Two risks to check against the real host in P1-SERVER-STAGED, not assumed here:

- **Workflows.** The token has no Workflows permission, so GitHub refuses a push whose new commits
  change `.github/workflows`. A `Prepare` push also sends every main commit the candidate
  repository lacks. If main gained a workflow change since the seed, that push is refused.
  Mitigation: `Prepare` pushes `base` to the candidate repository's `main` in the same atomic push,
  which keeps the delta small, and Felix re-seeds after merging a workflow pull request (one more
  step in his list). If GitHub refuses the mirror too, the failure is a `Refused { Rule }` naming
  it, found on the first staged landing in the sandbox, not on this repository.
- **Size.** Without shared objects, the first push is the whole history; later ones are small only
  while the mirror keeps up. Measured in the adapter test with a path remote, then once over HTTPS.

**No credential is live while repo code runs.** The only process that runs repo code is the
runner, inside `Test`, after the lander's git commands for that call have exited. Git reads the
token only through the askpass hook, which exists only in the environment of a lander git command
(`TESSEL_ASKPASS_TOKEN_FILE` is not in the runner's). The test tree's `origin` is the candidate
URL without credentials. This is a sequencing guarantee, as in `ports.rs` (`revoke_read_access`
is a no-op): the token file is still readable by anything running as the server's user, which is
RUNNER-OWN-UID's job (D4). What this design adds is that no `Workspace` exists during `Test`: the
tree is a clone of the remote, so the note in `workspace.rs` ("the tree's `origin` and reflog name
[the workspace]") stops applying, and repo code has no path to the clone that `Land` pushes from.
`Land` makes that clone later, after the runner returned. Test: a runner test double that, while
it runs, asserts no `Workspace` directory exists under the call and the environment of its process
has neither `GIT_ASKPASS` nor the token path.

## 5. `prepare` and `land` in the lander crate

M4-SANDBOX-WORKSPACE asked whether `tessel_lander::merge::{prepare, land}` stay public. They do
not all stay as they are:

- `prepare` stays `pub`: the server calls it, in its own crate, as `Prepare`. It still returns
  `Ready` with a `Workspace`; the server pushes the candidate from that workspace and drops it.
- `land` becomes `pub(crate)`. A caller could pass it any `Workspace` and any `PushUpdate`, so the
  push of main would not be tied to a head that was tested. Its callers are two new `pub`
  functions: `land_candidate` (reads, clones main, fetches the ref, checks the tip, calls `land`,
  reads back by ancestry, deletes the ref) and, until the flip, `run_merge`.
- New `pub` functions: `push_candidate` (the atomic push and sweep), `clone_candidate_tree` (the
  `Test` clone; returns a `TestTree`), and `read_candidate` (present with tip, or absent). New
  `Git` commands in `git.rs`: fetch and push of the candidate ref, `ls-remote` listing, and a
  deletion; `MergeSources` gains `candidate_remote`.
- `run_merge`, `STAND_IN_MESSAGE` and the stand-in branch in `run` are deleted when `Prepare`
  flips, not kept (replace, don't deprecate). `MergePorts::run_package_step` keeps taking a
  `&TestTree`, so the boundary the compile-fail doctests pin is unchanged.

Tests: the compile-fail doctest that `land` is not callable from outside the crate; the existing
isolation tests (`tessel-lander/tests/isolation.rs`) re-run on `land_candidate` with hooks,
filters and a malicious `.git/config` in the candidate (S3); a `Land` with a ref whose tip is not
the head answers `CandidateMissing` and pushes nothing.

## 6. Alternatives considered

| Alternative | Why not |
|---|---|
| A local ref or bundle in the lander's workspace | Dies with the disk or the container: the hosted server (M6) and Phase 2 have no durable lander disk. It also keeps the `Workspace` that `Land` pushes from alive across `Test`, which repo code runs beside |
| Keep the head in the core's state as a bundle | The core never runs git (rule 4) and the state would hold a repository |
| Push the head to the agent's fork | The agent can rewrite its own branch between `Test` and `Land`; S1 and I3 need a store the agent cannot write |
| Push the head to main's repository as a branch | The lander's token then writes branches there, ruleset A and B apply to them, and a dispatch leaks refs into the real repository |
| A single branch `tessel/candidate` per repository | Cannot hold the head a retried `Land` needs while a new `Prepare` overwrites it; and a late `Land` could not tell its head from the next one |
| Name by dispatch id (the first recommendation) | Needs new fields on `Test` and `Land`; collides after a fresh store (section Decision) |
| Skip the store: re-`Prepare` inside `Land` | Needs the fork commit, agent and scopes in `Land`; duplicates the checks |
| `Unknown` for a missing ref | Loops forever (section 2) |

## 7. Failpoints and the order of tasks

Windows this adds to P1-FAILPOINTS, each with a test that the claim still lands once or is
re-prepared and the candidate repository ends with no ref but at most one stale one:

1. `Prepare` killed after the candidate push, before the answer is persisted.
2. `Prepare` killed between the push's deletions and its creation (atomic: neither happens).
3. `Land` killed after the main push, before the read-back.
4. `Land` killed after the read-back, before the ref delete.
5. The ref removed (by hand) between `Prepare` and `Test`, and between `Test` and `Land`, with main
   at `base` (`CandidateMissing`) and with main at the head (`Landed`).
6. The ref moved to another commit between `Prepare` and `Test`.
7. A late `Land` for a head that landed: `Landed`, no second push.

P1-SIM-MERGE gains two checks beside I3: no `Land` answer is `Landed` unless main contains the
head, and a candidate ref never outlives two further `Prepare`s.

Order, with the corrections this note makes to `phase-1.md`:

1. CANDIDATE-REF (this note), then MERGE-PATH-REVIEW.
2. **P1-SERVER-STAGED** gains `tessel-lander/src/{merge,git,workspace}.rs`,
   `tessel-server/src/config.rs` (lane A's file; lane A stays out while it runs) and
   `docs/configuration.md`, beside `call.rs`, `calls.rs`, `repo.rs`. It adds the variant, the
   lander functions with their lander tests, `Test` and `Land`, and keeps `Prepare` the whole
   merge. Adapter tests make the candidate ref with plain git, so no function is dead.
3. Felix's steps from section 4, and the pull request for `setup.sh` and `dogfood.md`.
4. **P1-CORE-STAGED-LANDING** gains `tessel-server/src/landing/mod.rs`: the server's `Prepare`
   becomes rebase-and-push-candidate in the same commit that makes the core stage the landing.
   Server and core are one binary, so one commit flips both and there is no skew; two commits
   would let a core that still treats `Prepared` as landed read a candidate push as a merge, a
   false `Merged` (rule 6). `run_merge` and the stand-in message go in that commit.
5. P1-CORE-RECONCILE: its "candidate ref" in the in-flight record is the head (the name derives
   from it); the `CandidateMissing` core test and the fixture test (c) are part of it.
6. P1-FAILPOINTS, the section 7 windows. P1-SIM-MERGE gains a lost-ref fault.

Not decided here: how long `Test` and `Land` deadlines are (the stage split of
`STEWARD_CALL_TIMEOUT_MS`, P1-CORE-STAGED-LANDING), and whether Phase 2 keeps a candidate
repository at all or pushes to an agent's org fork.

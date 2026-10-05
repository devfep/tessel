# Build protocol

How tasks in `docs/ROADMAP.md` are executed and verified. Binding for every agent that touches this
repo. `PLAN.md` says what to build and when; this file says how the work is run. Adapted from the
protocol used in the pantry-app repo, at Felix's instruction on 5 October 2026.

## 1. Roles

| Role | Who | Job |
|---|---|---|
| Orchestrator | the session Felix talks to | Dispatches, reviews reports, merges, gates, writes the roadmap and STATE. Never writes product code. |
| Implementer | fresh `general-purpose` subagent, `model: "sonnet"` | Implements exactly one task in its own worktree, runs the gate, commits, reports. |
| Reviewer | fresh `superpowers:code-reviewer` subagent, `model: "opus"` | Reviews `BASE..HEAD` against the task text. Trusts nothing in the report. Never merges. |
| Advisor | `model: "opus"`, read-only | Only when a lane has looped for about 45 minutes: explains the way forward to the lane. No edits, no builds. |

Order is fixed: implementer report → reviewer → fix passes back to the SAME implementer → re-check by
the SAME reviewer → merge on "Yes" only → orchestrator's gate on the merged tree → close the roadmap
box with the merge sha → reclaim worktree and branch → rewrite STATE.

The reviewer's first line is exactly `Ready to merge? Yes`, `No` or `With fixes`. The brief asks for
an adversarial review, the reviewer's own mutants, spec compliance against the task text, and a flag
on speculative or over-engineered code.

## 2. Branches, worktrees, merges

- Integration branch: `sprint/build`. Each task: worktree `.claude/worktrees/<task>` on branch
  `task-<task>`, cut from the local `sprint/build` tip.
- Lanes commit only to their task branch. They never push, never commit to `sprint/build`, never
  `git stash`, and edit only files under their own worktree.
- Before its final gate and report, a lane merges the local `sprint/build` tip into its branch.
- The orchestrator merges every task branch with `tools/merge-one.sh <branch>`: merge-tree guard,
  `git merge --no-ff`, merged tree equals the predicted tree, no deletions, `src/protocol.rs`
  unchanged. A clean merge is not a correct merge: when both sides touched a file, diff the merged
  file against both parents.
- After each merge and gate the orchestrator pushes `sprint/build` and the notes ref to `origin`.
- `main` only moves by pull request from `sprint/build` at a milestone, opened and merged by the
  orchestrator after a full gate plus `/code-review` and `/security-review` on the milestone diff.
  Nothing is pushed to `main` directly.

## 3. Gates

Every implementer runs, and the orchestrator re-runs on the merged tree:

| Area | Checks |
|---|---|
| Rust | `cargo test`; `cargo clippy --all-targets -- -D warnings` clean for files the task owns; `rustfmt --check` on files the task owns; wasm build warning count not above the base |
| Steward | `pnpm typecheck`; `pnpm exec oxlint`; `pnpm exec oxfmt --check`; `pnpm test` |
| Live | anything the task changes in a deployed Worker is exercised on the deployed Worker by the orchestrator, and the output is kept |

Rules for every gate: predict the test count from source before running and compare exactly; prove
a new check can fail (mutant or deliberate break, then restore); exit codes decide, never the words
"should" or "seems"; evidence comes from the committed tree only.

## 4. What every brief carries

1. "Never end your turn waiting for a notification. You will not be woken." Poll your own long runs
   in bounded foreground loops.
2. The worktree path, as an absolute literal path in every command.
3. `git add` explicit paths, never `-A`. Never add `AGENTS.md`, `.dev.vars`, generated types or
   `node_modules`.
4. Which files other live lanes own.
5. The two-strikes rule: the same check failing twice in a row ends the loop until the lane has
   reread the failure and its own diff; three in a row is a stop-and-report.
6. Resource rules (section 5) and the secrets rule: never print, log or commit a value from
   `.dev.vars`; redact Artifacts tokens (`art_v…`) in any output.
7. Commit rules: imperative subject of at most 72 characters, no attribution trailer, and the git
   note from `CLAUDE.md` (intent, assumes, rejected, evidence).
8. The fixed report format: commit sha, files changed, decisions made, gate results with command
   output, mutant table, what was not verified.
9. `src/protocol.rs` is frozen: a lane that needs a change stops and reports it instead.
10. The session scratchpad is shared by every lane. Each lane keeps its scripts and scratch files in
    a subdirectory named for its agent (`scratchpad/<agent>/`) and runs only scripts from there. On
    5 October 2026 a lane ran a reviewer's mutant script from the shared root four times, mutating
    and restoring another lane's worktree while it was under review.
11. Reports and verdicts stay under about 3,000 characters; longer ones are cut off in delivery.

## 5. Resources

Felix runs other sessions on the same Mac. At most two lanes build or test at once, and only in
disjoint areas. One heavy job per lane at a time (one cargo build, one docker build, one dev server).
Kill only PIDs you started, after reading `ps -o args -p <pid>`; no pattern kills. Leave nothing
running: dev servers, log tails and containers are stopped and confirmed gone before a report.
Docker Desktop is quit when no image build needs it. Use `trash`, never `rm -rf`.

## 6. Autonomy

The orchestrator decides without asking: dispatch order, fix-pass rulings, merges on "Yes", gates,
closing boxes, filing new tasks from reviews, reclaiming resources. It reports each closed task in at
most six lines and continues.

Felix decides: scope or schedule changes to `PLAN.md`, any breaking protocol change, account and
billing actions, the contest form and video. Those go under "Pending from Felix" in the STATE block;
work that does not depend on them continues.

For a design question the orchestrator brings one recommendation with its reason, not a list.

## 7. State

The whole state of the build lives in `docs/ROADMAP.md`: the task list and the STATE block at its
top, rewritten (never appended) at every dispatch, verdict, merge and close. Only the orchestrator
edits it. Every decision reaches its document in the same commit it is made.

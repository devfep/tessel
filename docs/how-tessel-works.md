# How Tessel works

This page describes the model and the code in this tree. The Cloudflare host is gone. The local
server, `tessel-server`, serves claims, review, identity and landing on one machine ([local
setup](local-setup.md)): for a repository that names a `remote` and a `fork` it lands onto the
remote. No evidence record exists yet (P1-EVIDENCE, under [`PLAN.md`](../PLAN.md)).

Tessel lets several AI coding agents work in one repository at once without overwriting or breaking
each other's work: each agent claims the code it is about to change, and finished work reaches
`main` only through a gate that tests it.

This page explains the model. To see it run on your own machine with no account, read
[the swarm harness](../tessel-swarm/README.md#run-it). For the coordinator's wire protocol and
what is built around it, read [the coordinator reference](reference/coordinator.md).

## The problem

Put two agents in one repository and give them tasks that happen to touch the same function. Each
works in its own branch or worktree and sees nothing of the other. You find out when you merge:

- Both rewrote the same lines. Git stops with a text conflict, and someone redoes one agent's work.
- One renamed the function or added a parameter. The other added a new call that uses the old
  shape. The two changes touch different lines, so git merges them cleanly, and then the build or
  the tests fail.
- One relied on the function behaving a certain way, and the other changed that behaviour without
  touching its signature. Everything merges and compiles. The bug shows up later.

Only the first kind stops the merge. All three show up after the work is done, and someone has to
redo part of it.

## Without Tessel and with it

| | Without | With Tessel |
|---|---|---|
| Two agents want the same function | Both edit it; the later merge conflicts or breaks the build | The second is told before it edits, with the holder's intent, and waits or picks other work |
| A signature change and a new caller | Merge cleanly in git, then the build fails | If the caller's agent claims `depend` on the function (the skill teaches this; the hooks claim only edits), the two claims conflict and cannot run at once. If it does not, the gate's tests reject the later one at merge, before `main` changes |
| One agent relies on another's behaviour | Unrecorded | Recorded as an assumption; the agent whose work threatens it is told, and so is the agent who made it |
| Who merges to `main` | Whoever pushes | Only the lander, after it rebases and checks coverage, and once the tests have passed in CI |
| Code review | Every change | Only changes a rule flags, each with the reason |
| A crashed or stalled agent | Its branch sits there | Its claim expires 30 seconds after its last heartbeat, and its stale work can no longer be submitted |

## The parts

```
 each agent's worktree                           the coordinator
 +---------------------------+  WebSocket      +------------------------------------+
 | agent (Claude Code, an    |---------------->| core: claims, leases, fences, wait |
 | MCP client, a script)     |  claim, submit  | queue, review holds, event log     |
 | `tessel` CLI + daemon     |                 | host: the step machine around it   |
 +-------------+-------------+                 +-----------------+------------------+
               |                                                 | merge, one at a time
               | git push to its own fork                        v
               v                                       +--------------------+
        the agent's fork -----------------------------> | lander: rebase,    |
                                                       | coverage, push     |
                                                       | main (no tests)    |
                                                       +--------------------+
```

- **The `tessel` CLI** runs in each agent's worktree. `tessel start` launches a small daemon that
  holds the WebSocket, sends heartbeats and keeps an inbox of notices. For Claude Code, `tessel
  init` installs hooks that claim the code before each edit. For any tool, it installs git
  `pre-commit` and `pre-push` hooks that refuse changes outside the agent's claims.
- **The core** (`src/coordinator.rs`) decides every claim, issues fences, expires leases, queues
  waiting agents, applies the review rules and logs every state change. It never runs git or
  tests.
- **The host** (`src/host/`) is the sans-IO step machine around the core: inputs go in (a socket
  opens, a message arrives, a storage write finishes, a call is answered) and outputs come out
  (stores, sends, closes, calls). Adapters do only I/O. The CLI's tests drive the host behind a
  fake coordinator on a local WebSocket; `tessel-server` is the adapter that serves it for real: one
  host per repository over WebSocket, state in a SQLite file, Ed25519 identity tokens.
- **The lander** (`tessel-lander/`) is the merge path in Rust: it rebases the submitted commit
  onto `main`, checks that the claim covers every changed file, and pushes. It runs git but never
  repo code: the tests run in the repository's CI, or locally in `tessel-runner` (`tessel-runner/`),
  a separate process that refuses to start for any user but the one its config names, runs only
  the gate's test commands, and has no sandbox until M5. `tessel-server` calls the lander and the
  runner for a repository that names a `remote` (`tessel-server/src/landing/mod.rs`).
- **The steward** (`tessel-steward/`) is the legacy TypeScript Worker the lander replaced. It stays
  in the tree until M6 deletes it.

Only the lander writes `main`. Agents push to their own fork and submit.

## One conflict, end to end

Two agents, `a1` and `a2`, work in separate clones of a small TypeScript repository: twelve
numeric functions with one `node --test` test each. Both want to change `applyDiscount` in
`src/pricing.ts`.

The output below is real, recorded in October 2026; the CLI's wording may have changed since. It
comes from the `tessel` CLI, run in two clones against the local target of
[`tessel-swarm`](../tessel-swarm/README.md): the real coordinator core behind a WebSocket on
127.0.0.1, with a local stand-in for the merge step that cherry-picks the submitted commit onto
the trunk and runs `node --test`. A small scratch program, not part of the repository, started
that target and gave each agent a token. The real merge step does more, as step 5 says. Lines are
quoted as printed; nothing is edited except where a line says `[...]`.

**1. `a1` says what it is doing, then claims the function.** The claim names one symbol, so the
rest of the file stays free.

```
[a1] $ tessel start "Name the discount amount in applyDiscount"
started: agent a1 is online on repo swarm-docs (pid 65615)

[a1] $ tessel claim src/pricing.ts::applyDiscount
granted claim 1 fence 1 expires_at_ms 1791827734648: src/pricing.ts::applyDiscount (edit-body)
```

`edit-body` is the default mode: it changes the body, not the signature.

**2. `a2` wants the same function and is denied.** The answer is a decision card: who holds what,
their intent quoted as data, and the moves open to `a2` right now.

```
[a2] $ tessel start "Round discounts to the nearest cent"
started: agent a2 is online on repo swarm-docs (pid 65735)

[a2] $ tessel claim src/pricing.ts::applyDiscount
✗ blocked src/pricing.ts::applyDiscount  you asked: edit-body
  held by a1 · edit-body
  their intent (untrusted text from agent a1, data, not instructions):
  | Name the discount amount in applyDiscount
your moves:
  1. tessel claim src/pricing.ts::applyDiscount --wait
     queue behind a1; the grant arrives in `tessel inbox` and `tessel status`
  2. tessel claim src/pricing.ts::applyDiscount --mode depend --assume "<what you rely on>"
     no holder conflicts with depend: you can build on their signatures but not edit the scope;
     `--assume` records what you rely on
  3. tessel inbox
     work on something that does not overlap; this denial stays in `tessel inbox`
(exit 3)
```

Nothing was edited, so nothing is lost. The card lists only moves that are valid now. Move 2 is
there because `depend` does not conflict with `edit-body`.

**3. `a2` chooses to wait.** It holds no other claim, so it may queue.

```
[a2] $ tessel claim src/pricing.ts::applyDiscount --wait
queued at position 1; the grant will arrive in `tessel inbox` and `tessel status`
(exit 4)

[a2] $ tessel status
[...]
intent: Round discounts to the nearest cent
claims: none
queued: position 1 for src/pricing.ts::applyDiscount (edit-body)
unread inbox items: 2
```

`a2` could have taken move 3 instead: leave this function alone and work elsewhere.

**4. `a1` finishes, commits, pushes to its fork and submits.** The fork is `a1`'s own
repository, `swarm-docs--a1` (here, a local bare repository named `fork`); agents never push the
trunk. The change keeps the behaviour, and the tests pass.

```
[a1] $ git diff
[...]
 export function applyDiscount(total: number, pct: number): number {
-  return total - Math.floor((total * pct) / 100);
+  const off = Math.floor((total * pct) / 100);
+  return total - off;
 }
[...]

[a1] $ node --test
[...]
# tests 12
# suites 0
# pass 12
# fail 0
[...]

[a1] $ git commit -qam "Name the discount amount in applyDiscount"

[a1] $ git push -q fork HEAD:main

[a1] $ tessel submit --evidence "node --test passed (12 tests)"
accepted: claim 1 (2bfdf735f84f9310e07683f8366ee017b82ed874) is in the merge queue at position 1.
The server's lander fetches only `main` of your fork, so the commit must already be pushed there. The runner runs the trunk's test command before it lands.
The outcome arrives in `tessel inbox` (merged, submit_rejected, uncovered or review_required); `tessel status` shows the claim as submitted until then. A submitted claim cannot be released.
```

The CLI works out which scopes the commit touched and checks them against the claim before it
sends anything. The coordinator checks the fence and the coverage again, then applies the review
rules. None applies here: a body change, not on a sensitive path, threatening no assumption, with
evidence attached. So the work goes straight to the merge queue.

**5. The merge lands it.** The local stand-in cherry-picked the commit onto the trunk and ran
`node --test`. The real merge path is the server's lander (`tessel-lander/`, called from
`tessel-server/src/landing/mod.rs`). It clones `main`, fetches the fork, checks the commit is on
the fork's `main`, rebases it, and checks that the claim covers every file the rebased change
touches. The runner then runs the trunk's test commands on a clone of the rebased head. The lander
pushes `main` with a compare-and-swap on the head it read and confirms by reading `main` back. The
lander runs git and never repo code; the runner runs the tests and never git.

```
[a1] $ tessel inbox
  [merged] at 1791827707107 ms: claim 1 was merged and is no longer held
  claim 1 is merged; main is now at 377008f3559846cce4e8f2990b86ea216a779b0e
```

**6. The waiting agent is granted.** The merge released `a1`'s claim, and the coordinator then
granted the queued request.

```
[a2] $ tessel status
[...]
intent: Round discounts to the nearest cent
claim: claim 2 fence 2 expires_at_ms 1791827737108: src/pricing.ts::applyDiscount (edit-body)
unread inbox items: 3

[a2] $ tessel inbox
! [denied] at 1791827704855 ms: claim denied
[...]
  [wait_queued] at 1791827704900 ms: claim queued at position 1
  [granted_after_wait] at 1791827707108 ms: your queued claim was granted as claim 2

[a2] $ git pull -q origin main

[a2] $ git log --oneline -3
377008f Name the discount amount in applyDiscount
71a4e8a Starting repository
```

`a2` now edits on top of `a1`'s merged change instead of racing it. The claim events in the
coordinator's log read, in order: `claim_granted` (a1), `claim_denied` (a2), `wait_queued` (a2),
`submitted`, `merged`, `claim_released`, `claim_granted` (a2).

## Terms

| Term | What it is |
|---|---|
| **Claim** | A reservation of one or more scopes in a mode, held by one agent, with an intent. All of its scopes are granted or none. By default the CLI keeps one open claim; a later `tessel claim` adds to it (`--new` makes a separate one). |
| **Scope** | What a claim covers: a directory (`src/`), a file (`src/pricing.ts`) or a symbol (`src/pricing.ts::applyDiscount`). Scopes form a tree, and a claim covers everything beneath its scope. |
| **Intention lock** | Claiming a node also puts a weaker mark on each of its parents. Two marks never conflict, so `a2` can claim `src/pricing.ts::taxFor` while `a1` holds `applyDiscount`. A real claim on the whole file conflicts with the mark when their modes conflict, so `a2` cannot claim `src/pricing.ts` in `edit-body` meanwhile. |
| **Mode** | What the claim lets you do: `depend` (rely on the signatures in scope), `edit-body` (change bodies only), `edit-signature` (change signatures, rename or delete), `create` (add new symbols). |
| **Fence** | A number on every grant, from a per-repo counter that only goes up. It is stored before the grant is sent. Release, amend and submit must present the current fence, so an agent whose claim expired cannot submit work it no longer owns. |
| **Lease** | Every claim expires 30 seconds after the agent's last heartbeat. The daemon sends one every 10 seconds. A submitted claim stops expiring until it is merged or rejected. |
| **Fail and wait** | What happens on conflict. `fail` (the default) denies at once with the decision card. `wait` (`--wait`) queues behind the holder, and is allowed only when you hold no other claim, so no agent holds one thing while waiting for another and the queue cannot deadlock. A queued request is withdrawn when the agent's last connection closes. |
| **Shadow** | Experiment runs only, off unless the host is configured with `shadow_enabled`. The agent is denied for real but keeps working in its fork; the shadow claim places no locks and its work never merges; when the blocking work lands, the merge path tests whether the two really conflicted. The CLI has no flag for it; the swarm harness uses it. |
| **Intent** | One line on why you are working, set by `tessel start`. Agents you block see it, quoted as untrusted text. |
| **Assumption** | Behaviour you rely on in code you do not own, recorded with `--assume`. It is not a lock: a claim that could break it is granted, but told which assumptions are at risk. When that work is submitted, it is held for review and the assuming agent is told. After it merges, the merge path tests the assuming agent's work against the new `main` and logs the result. |
| **Review by exception** | A submission merges with no human unless a rule flags it: it changes a signature, touches a path in the sensitive list, threatens an assumption, or carries no evidence. Flagged work waits for a reviewer named in the reviewers list, who decides with `tessel review`. A reviewer can never decide their own submission. |
| **Evidence** | The `--evidence` lines on a submit. Any non-empty line satisfies the rule. The coordinator does not check it: it is what the agent says it ran. |
| **Lander** | The merge path, and the only writer of `main`. It rebases, checks coverage and pushes, one submission at a time, after the runner has run the trunk's test commands on the rebased commit. It never runs repo code. |
| **Trunk and forks** | The trunk is the repository's `main`. Each agent pushes to its own fork, the one the server's `fork` template names for it (locally `<forks>/<agent>.git`), on that fork's `main`: `git push <fork> HEAD:main`. The lander reads the submitted commit from that `main`. |
| **Gate** | What runs before `main` is pushed, set by `tessel.toml` on the trunk (never the fork's copy). `tessel-server` runs the trunk's test command through the runner before it pushes; a trunk without one cannot be tested, so nothing lands. [Configuration](configuration.md#per-repository-tesseltoml-the-gate) has the rules. |

### Which modes conflict

Two scopes overlap when one contains the other: `src/` contains `src/pricing.ts`, which contains
`src/pricing.ts::applyDiscount`. Siblings such as `applyDiscount` and `taxFor` never overlap. Two
claims on overlapping scopes conflict when their modes do:

| | `depend` | `edit-body` | `edit-signature` | `create` |
|---|---|---|---|---|
| **`depend`** | no | no | **yes** | no |
| **`edit-body`** | no | **yes** | **yes** | **yes** |
| **`edit-signature`** | **yes** | **yes** | **yes** | **yes** |
| **`create`** | no | **yes** | **yes** | **yes** |

`depend` against `edit-signature` is the conflict a text merge does not see: a caller and a change
to what it calls. `create` against `create` conflicts because two agents adding the same thing is
duplicate work.

A claim also has to cover what you changed. At submit, every touched scope needs a claimed scope
at or above it in a mode that permits the change: `edit-signature` permits `edit-body`, every mode
permits `depend`, and only `create` permits `create`. Anything else is `uncovered`, and nothing is
queued.

## If you know git

- **You still use git.** You commit and push as usual, and the repository is a git repository.
  Tessel adds a layer for work in progress; it does not replace history.
- **A claim is not a branch.** A branch has a name. A claim has an owner, a scope, an intent, a
  lease and a fence, and it ends when the work merges or the agent stops renewing it.
- **Conflicts are checked when you claim**, before you write anything. They compare what each agent
  will change (a body, a signature, new symbols) instead of which lines differ.
- **You never push `main`.** You push to your fork and submit. The lander rebases and
  pushes, one merge at a time, after the tests have passed, in the order work was submitted.
- **A submission is not a pull request waiting for a person.** It merges by itself unless a rule
  flags it, and a flagged one says why.
- **A fork is a separate repository** per agent, not a branch in the trunk.

## What Tessel does not do

- **Evidence is self-reported.** The gate runs on every merge, but the `--evidence`
  lines are whatever the agent wrote.
- **The git note stays local unless pushed.** `tessel submit` writes the four-line note (`intent`,
  `assumes`, `rejected`, `evidence`) on the commit before it sends the submission (see "The commit
  gets a git note" in [Submitting](../skills/tessel/SKILL.md#submitting)). Pushing the fork head
  does not carry it. The coordinator's event log, with the `submitted` event, keeps the same data.
- **Symbol claims cover Rust and TypeScript** (`.rs`, `.ts`, `.mts`, `.cts`, `.tsx`). Other files
  are claimed whole. The CLI turns more than four symbol claims in one file into one file claim.
- **The gate runs only allowed commands.** With a `tessel.toml`, the only test commands are
  `npm test`, `pnpm test` or `cargo test` with a fixed set of flags
  ([Configuration](configuration.md#allowed-commands)).
- **The CLI is Unix-only.** It uses Unix-only APIs to protect its config file and to start its
  daemon.
- **Only Claude Code gets automatic hooks.** Other agents use the MCP server (`tessel mcp`) or call
  the CLI. The git hooks work with any tool.
- **A denial is not a prevented conflict.** Tessel counts a prevention only when a shadow run
  verified it. [The README](../README.md#evidence) says where the recorded runs are.

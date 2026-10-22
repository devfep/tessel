# Tessel

Tessel is a transaction layer for many AI coding agents working in one git repository. Before an
agent edits code, it claims what it will touch: a directory, a file, or one function. A claim that
overlaps another agent's work is refused before anything is written, with the other agent's intent
and the moves still open. Finished work goes to the agent's own fork and is submitted; the lander
rebases it, checks that the claim covered every change, and pushes `main` once the repository's
CI has run the tests on it. Work merges with no human unless a rule flags it, and a flagged
change says why. Tessel's first implementation ran on Cloudflare; that host is deleted.
`tessel-server` replaces it as a single server meant to work with GitHub and the team's own CI;
today it runs on one machine and lands onto a git remote you name. [`PLAN.md`](PLAN.md) has the
plan and its status.

It is for you if you run several coding agents (Claude Code, an MCP client, your own scripts) on
one repository at the same time, and you want their work to land without overwriting or breaking
each other's.

To try it, [run the swarm harness](#see-it-run) on your own machine with no account.

## The problem

Two agents in one repository each work in their own branch and see nothing of the other. When
both touch the same function, you find out at merge: a text conflict, or a clean merge that breaks
the build (a renamed function and a new caller of the old name), or a behaviour change that breaks
an assumption nobody wrote down. Each is found after the work is done.

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

Only the lander writes `main`. The core and the host are in `src/`, the lander in
`tessel-lander/`, the CLI in `tessel-cli/`. `tessel-server` serves the host over WebSocket, with a
SQLite file per repository and Ed25519 identity tokens. [How Tessel works](docs/how-tessel-works.md)
explains each part, follows one conflict end to end with real CLI output, and defines every term.

## How it works, in six steps

1. **Set up.** `tessel init` writes the worktree's config with the agent's identity token (the
   server's operator mints it with `tessel-server admin token`) and installs the Claude Code hooks
   and the git `pre-commit` and `pre-push` hooks.
2. **Start.** `tessel start "<what this work is for>"` starts a daemon that connects to the
   repository's coordinator and sends heartbeats.
3. **Claim.** Before an edit, the hook (or `tessel claim`) claims the smallest scope that covers
   the edit: a symbol such as a function in Rust or TypeScript, otherwise the file. The coordinator
   checks it against every other claim and the mode conflict table.
4. **On conflict, decide.** A denied agent gets a decision card: who holds the scope, their intent
   as quoted data, and the valid moves. It can wait in a queue, claim in a mode that does not
   conflict, or work on something else. Nothing it wrote is lost, because it has not written yet.
5. **Submit.** The agent commits, pushes to its fork, and runs `tessel submit --evidence "..."`.
   The coordinator checks the claim's fence (the number that proves the claim has not expired) and
   that the claim covers every change, then applies the review rules.
6. **Merge.** The lander takes one submission at a time. It rebases onto `main`, checks coverage
   again per file, and pushes `main` once the tests have passed: they run in the repository's CI,
   or in the local runner for a single trusted user, never in the lander. The claim is released
   and the next waiting agent is granted.

`tessel-server` runs all six steps for a repository that names a `remote` and a `fork`: its lander
rebases the commit, the runner runs the trunk's test command, and main is pushed and read back. A
repository without a `remote` has no lander, so a submission that reaches the merge is refused and
the claim is active again. No evidence record and no transcript copy exist yet (P1-EVIDENCE).

## See it run

No account needed. The swarm harness runs scripted agents twice on a small TypeScript
repository: once with plain git and no coordination, once against the real coordinator core on
127.0.0.1 with a local stand-in for the lander. You need Rust, git and Node 22.18 or newer.

    cargo build -p tessel-swarm
    target/debug/tessel-swarm run --seed 1 --tasks 10 --agents 4 --overlap 0.5 --out swarm-results

The build takes under a minute and the run about 15 seconds on a recent Mac. The run prints an A/B
table and writes it, with the coordinator's event log, to `swarm-results/`. Which agent wins a
contested function varies with timing, so your numbers can differ from run to run.
[`tessel-swarm/README.md`](tessel-swarm/README.md) explains every row.

## Hosting today

Nothing is deployed from this repository, and the Cloudflare Worker that used to host the
coordinator is deleted. `tessel-server` serves claims, review, identity and landing on one machine.
`tessel-runner` runs a repository's test commands for one trusted user, and the server's lander
calls it. Also in the tree: the core and the portable host in `src/host/`, the
lander in `tessel-lander/`, the CLI against a fake coordinator that drives the real host, and the
swarm harness on 127.0.0.1.

To run `tessel-server` for yourself, follow [`docs/local-setup.md`](docs/local-setup.md).
To coordinate this repository with one, follow [`docs/dogfood.md`](docs/dogfood.md).

## Evidence

The counting rule: **a denial is not a prevented conflict.** Only verified outcomes count.
The `on` runs used the Cloudflare coordinator deployed at the time (now shut down); the `off`
row is a local replay. Their event logs are no longer in the tree; they are in the git tag
`contest-2026-10`, under
[`docs/evidence/2026-10-09`](https://github.com/devfep/tessel/tree/contest-2026-10/docs/evidence/2026-10-09).
`tessel-swarm run --seed 3 --tasks 40 --agents 30 --overlap 0.5` runs the same tasks against the
local core; expect different numbers.

The same 40 tasks (seed 3, 30 agents, overlap 0.5) on 9 October 2026:

- **Without coordination** (plain git, local replay): 14 of 40 landed. 26 were rejected after the
  work was done: 22 textual conflicts, 1 broke the build, 3 broke the tests.
- **With Tessel, waiting on conflict** (live coordinator): 40 of 40 landed, 0 rejected, with 29
  claims queued behind a holder. The 40 include 20 submissions held for review and approved by
  the harness's scripted reviewer. It was slower: 837 seconds of wall time against 14 for the
  local replay, or 2.8 landed per minute against 59.6.
  (`docs/evidence/2026-10-09/ab-wait` at the tag)
- **With Tessel, shadow runs** (live coordinator): 28 claims were denied and their work was done
  anyway in forks that could never merge, then tried against what blocked them. **20 verified
  preventions, 9 false alarms, 3 inconclusive.** A shadow claim can face more than one blocker, so
  the 32 trials outnumber the 28 claims. A false alarm is a denial whose work would have merged
  cleanly, and it counts as a false alarm, never as a prevention.
  (`docs/evidence/2026-10-09/ab-shadow` at the tag)

## What shipped and what did not

Shipped:

- Claims on directories, files and Rust or TypeScript symbols, in four modes, with intention locks.
- Leases, fences, the wait queue, and the decision card with valid moves.
- Assumptions, with a test of the assuming agent's work after the threatening change merges.
- Review by exception, decided with `tessel review`.
- Races: a reviewer runs `tessel race open`, agents `tessel race join`, and `tessel race pick`
  chooses a `human_pick` winner; the coordinator tries each entry's tests and merges at most one.
- The CLI with Claude Code hooks, git hooks, and a local MCP server (`tessel mcp`). One
  `tessel init` sets up a worktree.
- Every grant carries the trunk's head commit.
- The git note at submit: `tessel submit` writes the four-line note (`intent`, `assumes`,
  `rejected`, `evidence`) on the commit before it sends the submission.
- `tessel submit --transcript <file>` redacts a session transcript and attaches its hash.
- Shadow verification and the swarm harness for A/B runs.

Not running today:

- The steward (`tessel-steward/`) and what it did on Cloudflare: Artifacts trunks and forks, the
  container gate, merge records, identity tokens, the dashboard and the review page. It stays in
  the tree as the reference for the lander until M6.

Not shipped:

- Batching merges that commute. Claims carry the scope data, but merges still run one at a time.
- Gates that install dependencies at run time. The containers have no Internet, so the operator
  bakes a repository's dependencies into the toolchain image and writes a `tessel.toml`.
- Symbol claims in languages other than Rust and TypeScript (other files are claimed whole), a CLI
  for systems other than Unix, and automatic hooks for agents other than Claude Code.
- Verified evidence strings: `--evidence` is what the agent says it ran.

## Build it

- [Coordinator reference](docs/reference/coordinator.md): what is built, and the wire protocol.
- [Configuration](docs/configuration.md): the host settings, the `tessel.toml` gate, the CLI.
- [`skills/tessel/SKILL.md`](skills/tessel/SKILL.md): the workflow taught to agents.
- Build the CLI with `cargo install --path tessel-cli`.

Run the tests from a fresh clone. You need Rust stable (`rustup`), and Node 22.18 or newer with
pnpm for the steward's tests.

    git clone https://github.com/devfep/tessel.git && cd tessel
    cargo test --workspace          # protocol, coordinator, lander, CLI and swarm harness
    cargo build -p tessel-cli       # the tessel CLI at target/debug/tessel
    (cd tessel-steward && pnpm install --frozen-lockfile && pnpm test)

## License

MIT. See [LICENSE](LICENSE).

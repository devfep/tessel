# Tessel

Tessel is a transaction layer for many AI coding agents working in one git repository. Before an
agent edits code, it claims what it will touch: a directory, a file, or one function. A claim that
overlaps another agent's work is refused before anything is written, with the other agent's intent
and the moves still open. Finished work goes to the agent's own fork and is submitted; a steward
rebases it, checks that the claim covered every change, runs the tests, and only then pushes
`main`. Work merges with no human unless a rule flags it, and a flagged change says why. Tessel
runs on Cloudflare: Workers, Durable Objects, Artifacts, Queues, Containers and Access.

It is for you if you run several coding agents (Claude Code, an MCP client, your own scripts) on
one repository at the same time, and you want their work to land without overwriting or breaking
each other's.

To try it, [run the swarm harness](#see-it-run) on your own machine with no account, or [sign in to
the live review page](#try-it-live).

Documentation site: <https://devfep.github.io/tessel/> (this README and the pages under docs/
rendered from main).

## The problem

Two agents in one repository each work in their own branch and see nothing of the other. When
both touch the same function, you find out at merge: a text conflict, or a clean merge that breaks
the build (a renamed function and a new caller of the old name), or a behaviour change that breaks
an assumption nobody wrote down. Each is found after the work is done.

## The parts

```
 each agent's worktree                        Cloudflare
 +---------------------------+  WebSocket   +---------------------------------------+
 | agent (Claude Code, an    |------------->| coordinator: Rust Worker, one Durable |
 | MCP client, a script)     |  claim,      | Object per repo: claims, leases,      |
 | `tessel` CLI + daemon     |  submit      | fences, wait queue, review holds,     |
 +-------------+-------------+              | event log                             |
               |                            +-------------------+-------------------+
               | git push to its own fork                       | merge, one at a time
               v                                                v (service binding)
 +-----------------------------------------------------------------------------------+
 | steward: TypeScript Worker                                                        |
 |   Artifacts: the trunk <repo> and one fork <repo>--<agent> per agent              |
 |   container gate: rebase onto main, check coverage, run the tests, push main      |
 |   Queue: a push to the trunk's main tells the coordinator that main moved         |
 |   identity tokens for agents; dashboard and review page behind Cloudflare Access  |
 +-----------------------------------------------------------------------------------+
```

Only the steward writes `main`. [How Tessel works](docs/how-tessel-works.md) explains each part,
follows one conflict end to end with real CLI output, and defines every term.

## How it works, in six steps

1. **Set up.** `tessel init` writes the worktree's config with the agent's identity token and
   installs the Claude Code hooks and the git `pre-commit` and `pre-push` hooks.
2. **Start.** `tessel start "<what this work is for>"` starts a daemon that connects to the
   repository's coordinator, a Durable Object, and sends heartbeats.
3. **Claim.** Before an edit, the hook (or `tessel claim`) claims the smallest scope that covers
   the edit: a symbol such as a function in Rust or TypeScript, otherwise the file. The coordinator
   checks it against every other claim and the mode conflict table.
4. **On conflict, decide.** A denied agent gets a decision card: who holds the scope, their intent
   as quoted data, and the valid moves. It can wait in a queue, claim in a mode that does not
   conflict, or work on something else. Nothing it wrote is lost, because it has not written yet.
5. **Submit.** The agent commits, pushes to its fork, and runs `tessel submit --evidence "..."`.
   The coordinator checks the claim's fence (the number that proves the claim has not expired) and
   that the claim covers every change, then applies the review rules.
6. **Merge.** The steward takes one submission at a time. In a container with no Internet, it
   rebases onto `main`, checks coverage again per file, runs the repository's gate, and pushes
   `main`. The claim is released and the next waiting agent is granted.

## See it run

No Cloudflare account needed. The swarm harness runs scripted agents twice on a small TypeScript
repository: once with plain git and no coordination, once against the real coordinator core on
127.0.0.1 with a local steward. You need Rust, git and Node 22.18 or newer.

    cargo build -p tessel-swarm
    target/debug/tessel-swarm run --seed 1 --tasks 10 --agents 4 --overlap 0.5 --out swarm-results

The build takes under a minute and the run about 15 seconds on a recent Mac. The run prints an A/B
table and writes it, with the coordinator's event log, to `swarm-results/`. Which agent wins a
contested function varies with timing, so your numbers can differ from run to run.
[`tessel-swarm/README.md`](tessel-swarm/README.md) explains every row.

## Try it live

A deployed instance runs on Cloudflare:

- [Review page for `swarm-video7`](https://tessel-steward.devfep.workers.dev/review/swarm-video7):
  two held submissions on a demo repository.
- [Dashboard for `swarm-video7`](https://tessel-steward.devfep.workers.dev/dashboard/swarm-video7).
- [Dashboard for
  `tessel-dogfood`](https://tessel-steward.devfep.workers.dev/dashboard/tessel-dogfood): Tessel's
  own repository. Since 5 October 2026 its changes have landed through Tessel, except a few an
  admin merged by hand, every wire-protocol change among them.

Sign in through Cloudflare Access with any email address, using the one-time PIN it sends. The
pages are for viewing only: approving, rejecting and loading a diff are limited to the listed
reviewer. The two held cards on `swarm-video7` are real submissions that the review rules held:
one threatens another agent's assumption, the other changes a signature.

## Evidence

The counting rule: **a denial is not a prevented conflict.** Only verified outcomes count.
Everything below is in [`docs/evidence/`](docs/evidence/), with the event logs the numbers came
from.

The same 40 tasks (seed 3, 30 agents, overlap 0.5) on 9 October 2026:

- **Without coordination** (plain git, local replay): 14 of 40 landed. 26 were rejected after the
  work was done: 22 textual conflicts, 1 broke the build, 3 broke the tests.
- **With Tessel, waiting on conflict** (live coordinator): 40 of 40 landed, 0 rejected, with 29
  claims queued behind a holder. The 40 include 20 submissions held for review and approved by
  the harness's scripted reviewer. It was slower: 837 seconds of wall time against 14 for the
  local replay, or 2.8 landed per minute against 59.6.
  [`2026-10-09/ab-wait`](docs/evidence/2026-10-09/ab-wait/README.md)
- **With Tessel, shadow runs** (live coordinator): 28 claims were denied and their work was done
  anyway in forks that could never merge, then tried against what blocked them. **20 verified
  preventions, 9 false alarms, 3 inconclusive.** A shadow claim can face more than one blocker, so
  the 32 trials outnumber the 28 claims. A false alarm is a denial whose work would have merged
  cleanly, and it counts as a false alarm, never as a prevention.
  [`2026-10-09/ab-shadow`](docs/evidence/2026-10-09/ab-shadow/README.md)

## What shipped and what did not

Shipped:

- Claims on directories, files and Rust or TypeScript symbols, in four modes, with intention locks.
- Leases, fences, the wait queue, and the decision card with valid moves.
- Assumptions, with a test of the assuming agent's work after the threatening change merges.
- Review by exception, decided with `tessel review` or on the steward's review page.
- Races: a reviewer runs `tessel race open`, agents `tessel race join`, and `tessel race pick`
  chooses a `human_pick` winner; the coordinator tries each entry's tests and merges at most one.
- The steward: Artifacts trunk and forks, the container gate, Queue-driven notices that `main`
  moved, identity tokens, and a dashboard.
- The CLI with Claude Code hooks, git hooks, and a local MCP server (`tessel mcp`). One
  `tessel init` sets up a worktree.
- Every grant carries the trunk's head commit.
- The git note at submit: `tessel submit` writes the four-line note (`intent`, `assumes`,
  `rejected`, `evidence`) on the commit before it sends the submission.
- Merge records: after every merge the steward writes the claim, intent, decisions, review and gate
  results to the Artifacts repository `<repo>--evidence`, and copies a verified transcript to
  `<repo>--transcripts`. `tessel submit --transcript <file>` redacts a transcript and attaches it.
- Shadow verification and the swarm harness for A/B runs.

Not shipped:

- Preview deployments with Workers Builds.
- Batching merges that commute. Claims carry the scope data, but merges still run one at a time.
- Gates that install dependencies at run time. The containers have no Internet, so the operator
  bakes a repository's dependencies into the toolchain image and writes a `tessel.toml`.
- Symbol claims in languages other than Rust and TypeScript (other files are claimed whole), a CLI
  for systems other than Unix, and automatic hooks for agents other than Claude Code.
- Verified evidence strings: `--evidence` is what the agent says it ran.

## Operate it

- [Coordinator reference](docs/reference/coordinator.md): authentication, tokens, a manual
  WebSocket session, configuration, deploy and the gate.
- [Configuration](docs/configuration.md): every setting of both Workers.
- [`skills/tessel/SKILL.md`](skills/tessel/SKILL.md): the workflow taught to agents.
- Build the CLI with `cargo install --path tessel-cli`.
- [Dogfooding gate](docs/reference/coordinator.md#dogfooding-gate): Tessel merges its own code
  through its own gate.

Run the tests from a fresh clone. You need Rust stable (`rustup`) with the
`wasm32-unknown-unknown` target, Node 22.18 or newer, pnpm and `wrangler`.

    git clone https://github.com/devfep/tessel.git && cd tessel
    cargo test --workspace          # protocol, coordinator, CLI and swarm harness
    cargo build -p tessel-cli       # the tessel CLI at target/debug/tessel
    (cd tessel-steward && pnpm install --frozen-lockfile && pnpm test)

Running the coordinator locally is in the
[coordinator reference](docs/reference/coordinator.md#run-locally). Deploying the steward needs an
account with Artifacts enabled.

Keep `STEWARD_ADMIN_TOKEN` with the human operator. Mint each agent's identity token, fork and
fork write token from your own shell, and give each agent only its own identity token (as
`TESSEL_TOKEN`, or in its `.tessel/config.toml`). An agent that holds the admin token can mint a
reviewer identity and approve its own held change. `tessel init --steward <url>` mints the token
for you when `STEWARD_ADMIN_TOKEN` is set for that one command; run it yourself in the agent's
worktree, then start the agent without the variable.

## License

MIT. See [LICENSE](LICENSE).

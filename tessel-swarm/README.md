# tessel-swarm

Reproducible workloads of scripted agents against a small demo repository, run twice: without
coordination (`off`) and with Tessel (`on`). It writes a JSON result per run and a Markdown A/B
table. Evidence rule: only verified outcomes count, and a denial is not a prevented conflict.

## What a run does

A seed generates N tasks. Each is one scripted edit to the demo repository (twelve numeric
TypeScript functions in seven modules, one test file per function, run with `node --test`):

| Task | Edit |
|---|---|
| `body` | rewrites a function body (same behaviour) |
| `signature` | adds a required parameter that scales the result and updates every caller and test |
| `add` | adds `extraN`, a new function that calls an existing one, with its own test |
| `rename` | renames a function everywhere |

`--overlap` is the chance that a task targets a function an earlier task already targeted.
Overlapping tasks conflict textually (two edits to one line) or semantically: a `signature` or
`rename` plus an `add` that calls the old shape merge cleanly in git and then fail the build or the
tests. Each task alone is valid against the starting repository.

**`off`** has no claims. `--agents` agents work at once; task i branches from the trunk after tasks 1
to i minus agents were merged (an agent pulls before its next task) and does not see work still in
flight. Branches are built in parallel, one directory per agent. They are then merged onto the trunk
with plain git, in task order, and the tests run after each merge. Each merge is recorded as
clean, a textual conflict, a broken build (a missing export or module) or broken tests. A merge
that breaks the build or tests is rolled back, so every merge is judged on a green trunk.

**`on`** runs N agents as tokio tasks speaking the real protocol over WebSocket: claim (symbol
scopes where the edit allows, the way the CLI plans them), respect a denial, edit the checkout as
main stands after the grant, commit, push to a fork, `Submit`, wait for `Merged` or
`SubmitRejected`, and release a rejected claim. `--policy wait` queues behind the holder;
`--policy skip` puts the task back and picks other work. A reviewer connection (`swarm-reviewer`)
approves every submission held for review, with a fixed note that marks its approvals in the log;
`--no-reviewer` turns it off, and a held submission then stays held until its agent times out.
It runs against the local target and against the swarm coordinator (below), where it is the only
reviewer. Held and approved counts come from the log.
An agent that times out stops, and the tasks nobody took are recorded as not run, so every task
is merged, rejected or not finished.
Every count about the coordinator comes from reading its event log with `Watch` and running
`Summary::from_events` over it. `SubmitRejected` and `WaitQueued` counts are taken from the same
log, because `Summary` has no field for them. Wall time and agent-minutes are measured by the
harness.

## What the numbers are

- The `off` numbers are **local**. The protocol has no client message for `ReplayMerged`, and a
  client that wrote evidence events would break rule 7, so they are never sent to a coordinator.
  They are computed from the local replay, and `Summary::from_events` over the replay's own events
  gives the replay merge and conflict counts.
- A cell reading `n/a` is a number the run cannot produce. The table never shows zero for it.
- Verified prevention needs shadow runs, which this harness does not make, so that row is `n/a`.
- Waiting is the time from a claim being sent to its answer, summed over attempts.
- Agent-minutes on both sides are the configured `--work-ms` per task plus the measured time to
  edit, test and commit.
- In `on`, the steward reports a failed test run for both build and test failures, and the log
  keeps the rejection reason as text that the harness does not parse, so `on` has no per-kind
  split of its rejections.

## Run it

```
cargo build -p tessel-swarm
target/debug/tessel-swarm run --seed 1 --tasks 10 --agents 4 --overlap 0.5 --out swarm-results
target/debug/tessel-swarm tasks --seed 1 --tasks 10      # print the generated tasks
```

The default target is `local`: the real coordinator core behind a WebSocket on 127.0.0.1, and a
steward that cherry-picks each submitted commit onto a trunk, runs `node --test`, and reports
`Merged`, `Conflict` or `TestsFailed`. It needs `git` and Node 22.18 or newer. The local steward
does not re-check the merged diff against the claim; the core still checks the submitted
`touched` list.

Files written to `--out`: `seed<N>-off.json`, `seed<N>-on.json`, `seed<N>-on-events.json` (the
event log the numbers came from) and `seed<N>-ab.md`.

### Live

```
export STEWARD_ADMIN_TOKEN=...   # the steward's admin token; never printed
target/debug/tessel-swarm run --target live --seed 1 --tasks 10 --agents 6 \
  --coordinator wss://tessel-coordinator-swarm.<account>.workers.dev --steward https://<steward host>
```

This creates a new scratch repository through the steward admin routes (`swarm-s<seed>-<time>`, or
`--repo swarm-<name>`), pushes the starting commit, forks it once per agent, mints a write token
per fork and an identity token per agent (the reviewer and an observer included), and runs `on`
against the swarm coordinator. Scratch repositories stay in the namespace; there is no delete
route.

### The swarm coordinator

The live swarm coordinator is a separate deployment of the same code, `tessel-coordinator-swarm`,
whose only reviewer is the script (`REVIEWERS = "swarm-reviewer"`). The production coordinator's
reviewer stays `felix`, so the script can approve work on the swarm deployment and nowhere else.
The swarm Worker also serves only repos named `swarm-*` (its `ALLOWED_REPO_PREFIX` var; production
leaves it unset and serves all). The signing key is shared, so without that a `tessel-dogfood`
token would open a socket on the swarm Worker, and a merge there would land on the dogfood trunk
through the shared steward. Any other repo gets a 403 before the token is looked at.
`--target live` refuses the production host `tessel-coordinator.devfep.workers.dev` by name, and any
host that is not `tessel-coordinator-swarm.*` or a local dev server.

The `[env.swarm]` block of the root `wrangler.toml` defines it. Wrangler does not inherit bindings,
vars, services, secrets or migrations into an environment, so the block repeats them. To deploy
(not done by this crate):

```
npx wrangler secret put IDENTITY_SIGNING_KEY --env swarm   # the same value as production
npx wrangler deploy --env swarm
```

The key is shared, so identity tokens from the steward's route verify on both deployments; a token
names a repo and an agent, not a Worker. The swarm deployment keeps its own Durable Object
storage, so swarm repos never mix with production ones.

## Safety

- The harness only targets repositories named `swarm-<suffix>`. `tessel-dogfood` and `demo` are
  refused by name, and every network function takes a `ScratchRepo` that only the guard can build.
- Tokens are held in a type that does not print, sent as headers (git: `GIT_CONFIG_*` environment;
  steward: `curl` configuration on stdin), and scrubbed from error text.
- Every temporary directory is removed when a run ends, and the local server closes its listener
  and sockets.
- Dependencies are the workspace's own (tokio, tokio-tungstenite, serde, clap, anyhow, rustls) plus
  `tempfile` for scratch directories. The steward is called with `curl` rather than adding an HTTP
  client crate.

## Reproducibility

The same seed gives the same tasks, the same starting repository (same commit id) and the same
`off` outcomes. An `on` run starts agents concurrently, so which agent claims a contested scope
first varies with timing; the claims, the denials and the merges are real, so repeat a seed a few
times before quoting one run.

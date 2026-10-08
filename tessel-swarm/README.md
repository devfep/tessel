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

**`off`** has no claims. `--agents` agents work at once; task i branches from the trunk after
tasks 1 to i minus agents were merged (an agent pulls before its next
task) and does not see work still in flight. Branches are built in parallel, one directory per
agent. They are then merged onto the trunk with plain git, in task order, and the tests run after
each merge. Each merge is recorded as clean, a textual conflict, a broken build (a missing export
or module) or broken tests. A merge that breaks the build or tests is rolled back, so every merge
is judged on a green trunk.

**`on`** runs N agents as tokio tasks speaking the real protocol over WebSocket: claim (symbol
scopes where the edit allows, the way the CLI plans them), respect a denial, edit the checkout as
main stands after the grant, commit, push to a fork, `Submit`, wait for `Merged` or
`SubmitRejected`, and release a rejected claim. `--policy wait` queues behind the holder;
`--policy skip` puts the task back and picks other work; `--policy shadow` is described below.
A reviewer connection (`swarm-reviewer`) approves every submission held for review, with a fixed
note that marks its approvals in the log; `--no-reviewer` turns it off, and a held submission then
stays held until its agent times out.
It runs against the local target and against the swarm coordinator (below), where it is the only
reviewer. Held and approved counts come from the log.
An agent that times out stops, and the tasks nobody took are recorded as not run, so every task
is merged, rejected, shadowed or not finished.
An agent sends a heartbeat every 8 s while it works as well as while it waits, because a claim
lives for one lease (30 s) after the last one. If a claim lapses anyway, the coordinator refuses
its next message with `StaleFence`. That task is recorded as `lapsed`: not finished, counted in
its own table row, with the note "claim lapsed (lease expired)". Its agent takes another task
and the run goes on.
If an agent's connection to the coordinator ends (closed by the coordinator or broken), the task
it had in hand is recorded as `disconnected`, with the close code and reason in its note, counted
in its own table row; that agent stops and the others take the remaining tasks. A queued request
the coordinator still grants to the agent afterwards lapses one lease later. If the log shows the
task merged or rejected anyway, it is counted as that. Its waiting and work time are what it spent
before the connection ended. A connection that ends while an agent releases a claim whose outcome it
already has also records that task as `disconnected`: the table then under-counts, never inflates.
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
- Verified prevention needs shadow runs, which only `--policy shadow` makes. With any other policy
  that row is `n/a`. A denial is never counted as a prevention.
- Waiting is the time from a claim being sent to its answer, summed over attempts.
- Agent-minutes on both sides are the configured `--work-ms` per task plus the measured time to
  edit, test and commit.
- In `on`, the steward reports a failed test run for both build and test failures, and the log
  keeps the rejection reason as text that the harness does not parse, so `on` has no per-kind
  split of its rejections.

## Connection resets

A connection that ends without the agent asking for it (a network or edge reset, with no close
frame, as the live run of 7 October showed: every socket dropped within a second) does not end the
run. Each agent and the scripted reviewer reopen the connection: up to 5 tries, pausing 0.5 s,
1 s, 2 s, 4 s and 8 s before them, each time with a fresh `hello`. The pauses have no jitter.

A reconnect is not free, and not invisible:

- The coordinator withdrew the agent's queued request when its socket closed, and anything it
  sent while the agent was gone is lost. So the agent reads the event log from seq 0 on the new
  connection and works out where its task stands from the log alone: no claim yet (it sends the
  claim again, and **its wait starts over** in the coordinator's queue, though the time it had
  already waited stays counted in its waiting), a request still queued (it waits for that grant,
  and does not send a second claim), a claim held (it carries on: it does the work again if the
  submission was not logged, and waits for the outcome if it was, without submitting twice), or a
  claim that ended while it was gone. A merge or a rejection found that way is that task's `merged`
  or `rejected`; a lease that ran out is its `lapsed`. A rejected claim that is still open is
  released.
- An agent that is only doing its work (editing, testing, committing) reopens the connection and
  goes on without starting over; its claims were renewed by its next heartbeat, and it learns at
  its next request, which names the fence, if one lapsed.
- An agent that read the log this way keeps watching it on that connection (the protocol has no
  way to stop), so it receives every later event and ignores them. Waiting after such a reset
  costs the coordinator more traffic than waiting before one.
- The reviewer reopens its connection and watches the log from the seq after the last one it saw.
  It keeps the held submissions it has seen and not yet seen decided, and sends an approval again
  after a reconnect for each of them that is still undecided, so a submission is neither missed nor
  decided twice.
- If the tries run out, the agent's task ends as `disconnected`, as it did before reconnecting
  existed, and the reason in the task's `note` names the tries. If the reviewer cannot reconnect,
  the run fails with an error that names them. One task survives at most 10 reconnects.
- The shadow policy does not reconnect: its trials are tied to the claim they were sent on, so an
  agent under it whose connection ends is `disconnected`.

Each task in the JSON has a `reconnects` count, and the run has
`connection_resets_survived_by_agents` (the sum). The A/B table's row "Connection resets
survived" is that sum; the reviewer's reconnects are not in it, though the log's `AgentConnected`
events for `swarm-reviewer` show them. It counts resets that were survived and says nothing about
what a reset cost: the waits that restarted are in the waiting numbers and the merges that came
late are in the wall time.

The local target can cut every socket at once (`Cutter::reset_all`, `Cutter::reset_on`) and refuse
chosen agents' reconnects (`Cutter::refuse_new`), which is how the tests exercise all of this.

## The shadow policy

With `--policy shadow` an agent whose claim conflicts claims with `OnConflict::Shadow`. It is
denied for real, but it still does its task in its own fork, pushes and submits, as a granted
agent does. A shadow submission is recorded and never queued, so the agent never waits for a
merge; the task is recorded as `shadowed`, not as landed or rejected. The agent submits first, so
the work is on record before the work that blocked it can merge, and only then spends the task's
work time. That time counts in the agent-minutes, with its own row for shadow work.

Before it takes another task, a shadow agent waits (counted as waiting) until the trial of its
claim is logged or can no longer run, up to `--task-timeout-s`. Its next push replaces the fork's
`main`, and the trial needs the submitted commit to still be there.

Landed counts under `shadow` are not comparable with `wait` and `skip`: a denied task becomes
shadow work and never lands. The table says so, and only the shadow policy has these rows.

When the blocking work merges, the steward tries each submitted shadow commit against it, first
on the trunk as it was before the merge (work that already fails there is inconclusive) and then
after, and the coordinator logs `DenialVerified`. One extra connection follows the log for the
whole run (the waiting agents read what it has seen, so waiting adds no polling to the
coordinator). After the agents finish, the run waits until every trial the log still owes is
there, for at most `--task-timeout-s`, and only then computes the table.

The row "Conflicts prevented, verified by shadow runs" is `Summary::from_events` over the
coordinator's log: verified preventions (a conflict), false alarms (clean), and beside them the
shadow claims, how many trials were inconclusive (counted as neither) and how many shadow claims
were never tried (the blocker did not merge, or the claim had not submitted before it did). It is
computed from nothing else. A denial alone is never a prevention.

The coordinator must allow shadows (`SHADOW_ENABLED`): the local target does, and the swarm
deployment does (`[env.swarm.vars]`); production does not.

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
  --coordinator wss://tessel-coordinator-swarm.<account>.workers.dev \
  --steward https://<steward host>
```

This creates a new scratch repository through the steward admin routes (`swarm-s<seed>-<time>`, or
`--repo swarm-<name>`), pushes the starting commit, forks it once per agent, mints a write token
per fork and an identity token per agent (the reviewer and an observer included), and runs `on`
against the swarm coordinator. Scratch repositories stay in the namespace; there is no delete
route.

### A named demo repository for real agents

```
export STEWARD_ADMIN_TOKEN=...   # never printed
target/debug/tessel-swarm demo-repo --repo swarm-demo --steward https://<steward host> \
  --agents a1,a2
```

This creates the named repository `swarm-demo` (the same `swarm-<suffix>` rule as a run), pushes
the demo's starting commit to its `main`, and with `--agents` forks it once per agent
(`swarm-demo--a1`, `swarm-demo--a2`). It prints the repo name, the remotes and the commit id, and
nothing else: no token. The same demo gives the same commit id every time, which also holds for
the generated `LICENSE` (MIT, the root file's text) that makes the demo repository permissively
licensed.

The steward's create route returns a write token for the new trunk; the command uses it for the
one push and drops it. It mints no fork tokens and no identities; those come from the steward's
own routes, as for any agent. Artifacts reports an existing name as `ALREADY_EXISTS`, which the
steward answers with HTTP 409; nothing is pushed because the push follows the create. If a later
step fails (the push or a fork), the trunk already exists, so a re-run is refused at the create:
use a new name. Repositories are never deleted: there is no delete route, so choose names you
will keep.

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

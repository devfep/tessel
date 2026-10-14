# Coordinator reference

The coordinator is a Rust Worker with one Durable Object per repository. It speaks the claim
protocol (`src/protocol.rs`) with each agent's `tessel` daemon over a WebSocket. Claims, fences,
the wait queue, leases and the event log survive restarts: every message's state change and
events are stored before any reply is sent. For what these terms mean, read
[How Tessel works](../how-tessel-works.md). For every setting in one place, read
[Configuration](../configuration.md).

## Prerequisites

    rustup target add wasm32-unknown-unknown
    npm i -g wrangler

You also need a deployed steward (`tessel-steward/`) that holds the same `IDENTITY_SIGNING_KEY`:
it mints the identity tokens the coordinator accepts. The manual session below uses `websocat`
(for example `brew install websocat`).

## Authentication

The Worker serves `/repo/<name>/ws` only to a request with `Authorization: Bearer <token>`, where
the token is an identity token the steward signed for that repo and one agent (24 hours). A request
with a missing, expired or wrong token gets `401`, a path the Worker does not serve gets `404`, and
a repo outside `ALLOWED_REPO_PREFIX` gets `403`, all before a WebSocket is
accepted. A `hello` for any other agent gets `not_owner` and the socket is closed. One token opens
sockets for one agent only. The token format is documented in `src/identity.rs`.

`GET /repo/<name>/summary` takes the same token (any agent of the repo may read it) and answers
`{"summary": {...}, "head_seq": n}`: the evidence counters over the repo's whole event log and the
`seq` of its last event (`null` for an empty log). It is a read: no socket, no event appended.
Refusals are the same as for `ws`; any method but `GET` gets `405`.

`IDENTITY_SIGNING_KEY` is a secret on both the coordinator and the steward, and the two must hold
the same value. A missing or empty key refuses every request.

    npx wrangler secret put IDENTITY_SIGNING_KEY

For local runs put it in `.dev.vars` (gitignored):

    IDENTITY_SIGNING_KEY=<a long random value>

### Tokens

Mint one identity token per agent with the steward's admin route (the response has `token`):

    curl -X POST -H "Authorization: Bearer $STEWARD_ADMIN_TOKEN" \
      https://<steward>/repos/demo/agents/a1/identity

The admin token stays with the human operator. Run this call, and the fork and write-token calls,
from the operator's own shell, and give each agent only its own identity token (as `TESSEL_TOKEN`
or in its `.tessel/config.toml`). An agent that holds the admin token can mint a reviewer identity
and approve its own held change.

`tessel init --steward <url>` makes the identity call for you when `STEWARD_ADMIN_TOKEN` is set in
the environment of that command. Run it yourself, in the agent's worktree, with the variable set
for that one command, then start the agent without it; or set `TESSEL_TOKEN` to a token you minted
and `tessel init` writes it to `.tessel/config.toml`.

| What | Route | Lasts | Answers or opens |
|---|---|---|---|
| Trunk repository | `POST /repos/<repo>` | until deleted | creates the trunk with default branch `main`; answers its `remote` and an Artifacts token for the first push |
| Fork | `POST /repos/<repo>/forks/<repo>--<agent>` | until deleted | one per agent; copies the trunk's default branch and answers the fork's `remote`, with no token |
| Identity token | `POST /repos/<repo>/agents/<agent>/identity` | 24 hours | the coordinator, for one repo and one agent |
| Fork write token | `POST /repos/<repo>--<agent>/tokens` | 1 hour | a push to that one fork; a repo that is not a fork gets `403` |

Every route takes `STEWARD_ADMIN_TOKEN`. `tessel init` keeps a token already in
`.tessel/config.toml` for the same repo and agent. To renew one, run `tessel init --steward <url>`
again: with `--steward` it skips the kept token, takes `TESSEL_TOKEN` if set, and otherwise mints a
new token with `STEWARD_ADMIN_TOKEN`.

## Run locally

    npx wrangler dev

In another terminal, with `A1_TOKEN` minted for agent `a1` of repo `demo` as above:

    websocat ws://localhost:8787/repo/demo/ws -H="Authorization: Bearer $A1_TOKEN"

Put the `-H` option after the URL, or write it with `=` as here: a bare `-H` takes the arguments
that follow it, including the URL.

Every message is one JSON object per line. Paste, with the agent the token was minted for:

    {"type":"hello","agent":"a1","base":"abc123"}

Expect:

    {"type":"welcome","head":"abc123","lease_ms":30000,"protocol":1}

Any other message before `hello` is answered with `{"type":"error",...,"code":"no_hello",...}`.
Text that is not a valid message, binary frames and text frames over 64 KiB get `malformed`.

## Try a conflict

You need a second agent, so mint a second token, `A2_TOKEN`, for agent `a2` of repo `demo`. A
token binds one agent, so `a2` cannot reuse `A1_TOKEN`.

In the first terminal (the `a1` socket above), after `hello`, send:

    {"type":"claim","req":1,"intent":{"summary":"rename login","task_ref":null},"scopes":[{"scope":{"kind":"symbol","path":"src/auth.rs","qualified_name":"auth::login"},"mode":"edit_signature"}],"on_conflict":"fail"}

Expect a `granted` reply with a `claim` id and `fence`. Every `granted` (a direct grant, the grant
a queued agent receives, the reply to an `amend` and a race entry's grant) also carries `head`, the
trunk head the coordinator knew when it granted, or `null` when it knows none yet. An agent adopts
it as its view of main, as it does the head in `base_moved`. In a second terminal, open a socket
with `A2_TOKEN`:

    websocat ws://localhost:8787/repo/demo/ws -H="Authorization: Bearer $A2_TOKEN"

Send `{"type":"hello","agent":"a2","base":"abc123"}`, then the same claim with `"mode":"depend"`.
Expect `denied`, whose `conflicts` list names `a1` in `held_by` and shows its intent in
`their_intent`.

With `"on_conflict":"wait"` the second agent gets `queued` instead. When the first agent sends
`{"type":"release","claim":1,"fence":1}` (with the `claim` and `fence` from its `granted` reply),
the second agent receives `granted` on its own socket. A claim that is neither released nor
renewed with `{"type":"heartbeat"}` expires after 30 seconds and its holder receives
`lease_expired`. A queued request is withdrawn when its agent's last socket closes. A `release`,
`amend` or `submit` for a claim that is no longer active gets `stale_fence`; an id that was never
issued gets `unknown_claim`.

Scope paths are repo-relative and canonical: no leading or trailing `/`, no empty, `.` or `..`
segment, no backslash or NUL; only a `dir` scope may be the empty root path. A claim, amend or
submit with another spelling, or with more than 256 scopes, gets `malformed`. The stored state
and each event are limited to 1 MiB; a call that would exceed it gets `malformed` with
"repo state limit reached" and changes nothing.

## Watch the event log

A third socket, opened with any agent's token for the repo, needs no `hello`:

    {"type":"watch","from_seq":0}

It receives every stored event with `seq >= from_seq`, in order, as `{"type":"event",...}`, then
every new event as it is stored. A second `watch` on the same socket is refused with `malformed`.

## Configuration

Set in `wrangler.toml` under `[vars]`. [Configuration](../configuration.md) lists every setting of
both Workers.

- `RUN` names the run that events are tagged with. Required.
- `SHADOW_ENABLED` is `"true"` or `"false"` (default `"false"`; unset means false). It allows
  `on_conflict: "shadow"` claims, which are for experiment runs only. With it off, such a claim
  gets `shadow_disabled`.
- `REVIEWERS`: comma-separated agent ids who may approve or reject held work. Unset or blank means
  nobody can, so held work stays held. A reviewer can never decide their own submission.
- `SENSITIVE`: comma-separated path prefixes; a submission touching one is held for review. Plain
  prefix match, so a directory entry must end in `/` (`src/auth` also matches `src/authz.rs`).
- `ALLOWED_REPO_PREFIX`: when set, the Worker serves only repos whose name starts with it, and
  refuses any other with `403` before it reads the token. Unset serves every repo. The swarm
  deployment sets it to `swarm-`.

A repo with no stored state fails to load when `RUN` is missing or empty, or when `SHADOW_ENABLED`
is set to anything but `"true"` or `"false"`. Those two are read only when a repo has no stored
state yet; an existing repo keeps the values it was created with, and changing them later has no
effect on it. `REVIEWERS` and `SENSITIVE` are applied every time a repo loads. A bad `REVIEWERS`
value means nobody can review; a bad `SENSITIVE` value holds every submission.

## MCP server

`tessel mcp --root <worktree>` serves the CLI commands as tools of a local stdio MCP server
(`tessel_start`, `tessel_claim`, `tessel_status`, `tessel_inbox`, `tessel_submit`,
`tessel_release`, `tessel_review`). It must run on the machine with the worktree, because symbol
extraction and diffs read it. Register it with Claude Code:

    claude mcp add --scope local tessel -- /abs/path/to/tessel mcp --root /abs/path/to/worktree

## Deploy

    npx wrangler deploy
    websocat wss://tessel-coordinator.<your-subdomain>.workers.dev/repo/demo/ws \
      -H="Authorization: Bearer $A1_TOKEN"

The coordinator's `STEWARD` service binding names the `tessel-steward` Worker, and the steward's
`COORDINATOR` binding names this one. Merges and the steward's notices that `main` moved need
both deployed.

## Dogfooding gate

The steward judges a repo by the `tessel.toml` on the trunk commit a run is based on (read through
the Artifacts binding, never from a fork). Tessel's own is at the repository root, and Tessel's own
trunk is the Artifacts repo `tessel-dogfood`.

- **Who may change the gate.** A submission over the coordinator's service binding whose rebased
  diff touches `tessel.toml` is rejected as `gate_changed` ("changes the gate (tessel.toml); only
  an admin merge may change it"). The admin route `POST /repos/<repo>/merges` may merge such a
  change: the head's file must pass the validation below (else `gate_invalid`), and a diff that
  touches only `tessel.toml` is judged by the head's file alone, with no install or tests, so a
  broken trunk gate can be repaired. A diff that also changes code runs the trunk's gate. The
  claim's scopes still have to cover `tessel.toml`.
- **Recovery from a broken trunk gate.** A gate-only admin merge pushes **without running any
  tests**; that is the point, and the admin takes responsibility for the file. Steps, with
  `STEWARD_ADMIN_TOKEN` as the bearer token:
  1. `POST /repos/tessel-dogfood/forks/tessel-dogfood--admin` creates a fork.
  2. `POST /repos/tessel-dogfood--admin/tokens` mints a write token for it.
  3. Push one commit that changes only `tessel.toml` to the fork's default branch.
  4. `POST /repos/tessel-dogfood/merges` with `{"fork":"tessel-dogfood--admin","commit":"<sha>",
     "scopes":[{"scope":{"kind":"file","path":"tessel.toml"},"mode":"edit_body"}]}`. The answer
     is `merged`, or `gate_invalid` if the new file does not pass the validation above.
  5. `POST /repos/tessel-dogfood/test-runs` should answer `step: "test"` again.
- **Allowed commands:** `pnpm test`, `npm test`, or `cargo test` with only `--workspace --locked
  --offline --no-fail-fast --all-targets --all-features --release --lib --bins --tests`. Nothing
  that can pick another toolchain, config or compiler (`+nightly`, `--config`, `-Z`,
  `--manifest-path`, `--target`, `--`) and no shell. A `tessel.toml` that is present but invalid
  fails every other run at `install` (reason `config`); only a missing file falls back to
  `npm test`.
- **Lockfile changes need the new image first.** Containers have no Internet, so dependencies are
  baked into `tessel-steward/toolchain.Dockerfile` from the committed lockfiles. A change to
  `Cargo.lock` or `tessel-steward/pnpm-lock.yaml` fails at `install` (reason `install_failed`)
  until the image holds it, and the admin route runs the same install step. To land one:
  1. From a checkout that has the new lockfiles, start Docker and run `npx wrangler deploy` in
     `tessel-steward/`: it rebuilds the `toolchain` image from them (build context: the repository
     root) and deploys.
  2. Merge the change through the steward as usual; its install step now finds the dependencies.
  3. `POST /repos/tessel-dogfood/test-runs` should answer `step: "test"`, `passed: true`.
- **Mirror:** `tools/mirror.sh` pushes the Artifacts trunk (`tessel-dogfood`) to the GitHub branch
  `artifacts-trunk`, fast-forward only. The first run creates the branch; later runs abort unless
  the push is a fast-forward. Milestone pull requests go from `artifacts-trunk`.

## Tests

    cargo test --workspace

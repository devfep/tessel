# Coordinator reference

This describes the coordinator as it exists in this tree: the claim protocol, the core that decides
it, the host that runs the core and `tessel-server`, which serves the host. The Cloudflare Worker
that used to serve it is deleted. The server lands for a repository that names a `remote`. Read
[`PLAN.md`](../../PLAN.md) for the plan.

The coordinator speaks the claim protocol (`src/protocol.rs`) with each agent's `tessel` daemon
over a WebSocket. Claims, fences, the wait queue, leases and the event log survive restarts: every
message's state change and events are stored before any reply is sent (the host's `Commit`
precedes the sends it carries). For what these terms mean, read
[How Tessel works](../how-tessel-works.md). For the settings in one place, read
[Configuration](../configuration.md).

## What is built

| Part | Where | What it is |
|---|---|---|
| Protocol | `src/protocol.rs` | The wire types, shared by the CLI and the coordinator. Its invariants are at the top of the file. |
| Core | `src/coordinator.rs`, `src/coordinator/` | The pure decisions: claims, fences, leases, queue, review, races, the merge queue. No I/O. |
| Shell helpers | `src/shell.rs` | What can be decided without I/O: path routes, settings parsing, delivery plans, the summary tally. |
| Host | `src/host/` | The sans-IO step machine around the core. Adapters turn what happens into an `Input` and carry out each `Output`. |
| Server | `tessel-server/` | One host per repository, served over WebSocket on a SQLite file per repository, with Ed25519 identity tokens. [Local setup](../local-setup.md) runs it. |
| Runner | `tessel-runner/` | Runs a repository's test commands for one trusted user. Nothing calls it yet. |
| Lander | `tessel-lander/` | The merge path in Rust, ported from the steward. A library until the server calls it. |
| CLI | `tessel-cli/` | The `tessel` binary and per-worktree daemon. Its tests run it against a fake coordinator that drives the real host. |
| Swarm | `tessel-swarm/` | Scripted agents against the real core on 127.0.0.1. |

`docs/reference/runtime-rules.md` lists every rule the old Worker enforced and where it lives now:
a host test, or an adapter test that the server's tasks write.

## The wire

`tessel-server` serves `/tenant/<tenant>/repo/<repo>/ws`, `/summary` and `/trunk-moved`. Each needs
a bearer token ([local setup](../local-setup.md)); `trunk-moved` admits the `lander` service only.

A socket opens for one agent of one repo. The adapter checks who the caller is before the host
sees the socket: a `hello` for any other agent gets `not_owner` and the socket is closed. The
routes `shell::parse_route` knows are `ws` and `summary` for a repo, and a trunk-moved poke.

`summary` answers `{"summary": {...}, "head_seq": n}`: the evidence counters over the repo's whole
event log and the `seq` of its last event (`null` for an empty log). It is a read: no socket, no
event appended.

Every message is one JSON object per line. The first one on a socket is the agent's:

    {"type":"hello","agent":"a1","base":"abc123"}

Expect:

    {"type":"welcome","head":"abc123","lease_ms":30000,"protocol":1}

Any other message before `hello` is answered with `{"type":"error",...,"code":"no_hello",...}`.
Text that is not a valid message, binary frames and text frames over 64 KiB get `malformed`.

## Try a conflict

Two agents, `a1` and `a2`, each on its own socket. A socket binds one agent, so `a2` cannot reuse
`a1`'s. After `hello` on `a1`'s socket, send:

    {"type":"claim","req":1,"intent":{"summary":"rename login","task_ref":null},"scopes":[{"scope":{"kind":"symbol","path":"src/auth.rs","qualified_name":"auth::login"},"mode":"edit_signature"}],"on_conflict":"fail"}

Expect a `granted` reply with a `claim` id and `fence`. Every `granted` (a direct grant, the grant
a queued agent receives, the reply to an `amend` and a race entry's grant) also carries `head`, the
trunk head the coordinator knew when it granted, or `null` when it knows none yet. An agent adopts
it as its view of main, as it does the head in `base_moved`. On `a2`'s socket, send
`{"type":"hello","agent":"a2","base":"abc123"}`, then the same claim with `"mode":"depend"`.
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

A third socket, opened for any agent of the repo, needs no `hello`:

    {"type":"watch","from_seq":0}

It receives every stored event with `seq >= from_seq`, in order, as `{"type":"event",...}`, then
every new event as it is stored. A second `watch` on the same socket is refused with `malformed`.

## Host settings

The host reads four settings when it builds the core (`HostConfig` in `src/host/mod.rs`). The
names are those of the Worker's old variables; `tessel-server` fills `run`, `reviewers` and
`sensitive` from each `[[repository]]`. [Configuration](../configuration.md#host-settings) has the
rules and failure behaviour.

## MCP server

`tessel mcp --root <worktree>` serves the CLI commands as tools of a local stdio MCP server
(`tessel_start`, `tessel_claim`, `tessel_status`, `tessel_inbox`, `tessel_submit`,
`tessel_release`, `tessel_review`). It must run on the machine with the worktree, because symbol
extraction and diffs read it. Register it with Claude Code:

    claude mcp add --scope local tessel -- /abs/path/to/tessel mcp --root /abs/path/to/worktree

## The gate

A repository's gate is the `tessel.toml` on the trunk commit a run is based on, never a fork's
copy. Tessel's own is at the repository root. [Configuration][gate] lists the keys, the allowed
commands and who may change the file.

[gate]: ../configuration.md#per-repository-tesseltoml-the-gate

## Tests

    cargo test --workspace

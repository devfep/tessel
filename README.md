# Tessel coordinator

Rust Worker + one coordinator Durable Object per repo, speaking the claim protocol over WebSocket.
Claims, fences, the wait queue, leases and the event log survive restarts: every message's state
change and events are stored before any reply is sent.

## Prerequisites
    rustup target add wasm32-unknown-unknown
    npm i -g wrangler

## Authentication
The Worker serves `/repo/<name>/ws` only to a request with `Authorization: Bearer <token>`, where
the token is an identity token the steward signed for that repo and one agent (24 hours). Any
other request gets `401 unauthorized` before a WebSocket is accepted. A `hello` for any other
agent gets `not_owner` and the socket is closed. The token format is documented in
`src/identity.rs`.

`IDENTITY_SIGNING_KEY` is a secret on both the coordinator and the steward, and the two must hold
the same value. A missing or empty key refuses every request.

    npx wrangler secret put IDENTITY_SIGNING_KEY

For local runs put it in `.dev.vars` (gitignored):

    IDENTITY_SIGNING_KEY=<a long random value>

Mint a token with the steward's admin route (the response has `token`):

    curl -X POST -H "Authorization: Bearer $STEWARD_ADMIN_TOKEN" \
      https://<steward>/repos/demo/agents/a1/identity

## Run locally
    npx wrangler dev
    # in another terminal (install websocat, e.g. `brew install websocat`):
    websocat ws://localhost:8787/repo/demo/ws -H="Authorization: Bearer $AGENT_TOKEN"

Put the `-H` option after the URL, or write it with `=` as here: a bare `-H` takes the arguments
that follow it, including the URL.

Every message is one JSON object per line. Paste:

    {"type":"hello","agent":"agent-1","base":"abc123"}

Expect:

    {"type":"welcome","head":"abc123","lease_ms":30000,"protocol":1}

Any other message before `hello` is answered with `{"type":"error",...,"code":"no_hello",...}`.
Text that is not a valid message, binary frames and text frames over 64 KiB get `malformed`.

## Try a conflict
Open two terminals on the same repo (both with the `-H=...` option above). In the first, send `hello` as `agent-1`, then:

    {"type":"claim","req":1,"intent":{"summary":"rename login","task_ref":null},"scopes":[{"scope":{"kind":"symbol","path":"src/auth.rs","qualified_name":"auth::login"},"mode":"edit_signature"}],"on_conflict":"fail"}

Expect a `granted` reply with a `claim` id and `fence`. In the second terminal, send `hello` as
`agent-2`, then the same claim with `"mode":"depend"`. Expect `denied`, whose `conflicts` list
names `agent-1` in `held_by` and shows its intent in `their_intent`.

With `"on_conflict":"wait"` the second agent gets `queued` instead. When the first agent sends
`{"type":"release","claim":1,"fence":1}`, the second agent receives `granted` on its own socket.
A claim that is neither released nor renewed with `{"type":"heartbeat"}` expires after 30 seconds
and its holder receives `lease_expired`. A queued request is withdrawn when its agent's last
socket closes. A `release`, `amend` or `submit` for a claim that is no longer active gets
`stale_fence`; an id that was never issued gets `unknown_claim`.

Scope paths are repo-relative and canonical: no leading or trailing `/`, no empty, `.` or `..`
segment, no backslash or NUL; only a `dir` scope may be the empty root path. A claim, amend or
submit with another spelling, or with more than 256 scopes, gets `malformed`. The stored state
and each event are limited to 1 MiB; a call that would exceed it gets `malformed` with
"repo state limit reached" and changes nothing.

## Watch the event log
A third socket needs no `hello`:

    {"type":"watch","from_seq":0}

It receives every stored event with `seq >= from_seq`, in order, as `{"type":"event",...}`, then
every new event as it is stored. A second `watch` on the same socket is refused with `malformed`.

## Configuration
Set in `wrangler.toml` under `[vars]`:

- `RUN` names the run that events are tagged with. Required.
- `SHADOW_ENABLED` is `"true"` or `"false"` (default `"false"`; unset means false). It allows
  `on_conflict: "shadow"` claims, which are for experiment runs only. With it off, such a claim
  gets `shadow_disabled`.

The coordinator refuses to start on a missing `RUN` or any other `SHADOW_ENABLED` value. Both are
read only when a repo has no stored state yet; an existing repo keeps the values it was created
with, and changing them later has no effect on it.

## Deploy
    npx wrangler deploy
    websocat wss://tessel-coordinator.<your-subdomain>.workers.dev/repo/demo/ws \
      -H="Authorization: Bearer $AGENT_TOKEN"

## Tests
    cargo test

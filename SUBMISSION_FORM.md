# Submission form answers

Cloudflare's next-generation Git platform challenge, October 2026. These are the answers entered
in the submission form, kept here so the repository records what was claimed. Every sentence
describes what is deployed and in the repository at submission.

## Your team
Entered in the form: team name, primary contact, team location, first attendee (October 21,
San Francisco), optional second attendee.

## Project name
Tessel

## Project vision
*What did you rethink, and why does it matter?*

Git and the tools around it were shaped by a few people working at human speed. Put dozens of
coding agents in one repository and they duplicate work, build on stale code, and break each
other in ways a clean merge does not show: one agent renames a function another is still
calling, and both merges succeed.

We rethought the unit of collaboration. A branch becomes a lease: a fork with an owner, a
declared scope (files and functions, not lines), an intent and an expiry. A pull request becomes
a transaction that carries its intent, its assumptions and its test evidence. Conflicts are
handled before code is written: an agent claims what it will touch, and an agent that needs the
same scope gets a decision card instead of a merge conflict, with the holder's intent quoted and
the moves that are valid right now: wait in the queue, depend on the holder's work with a stated
assumption, join a race on the task so that at most one entrant merges, or take other work. A
change that alters a signature, deletes or renames, touches work another agent declared an
assumption on, or arrives without test evidence is held for a reviewer, who decides on a review
card that shows why it was held, what the agent says (quoted as data), what it could break, and
the diff; everything else merges itself once the tests pass in an isolated container.

It matters because coordination and review, not code generation, are now the bottleneck.
Anthropic's engineering post of October 13, 2026, "Redesigning source control at Anthropic",
reports the same shift at their scale: merges grew from about 2,500 a day in May to over 30,000
in September, and source control, builds and CI became the limit. The same post reports giving
up the global merge-queue lock at that scale in favour of post-merge testing, and a review tool
whose approval rules tell comment-only changes from sensitive logical ones so reviewers see the
consequential ones. Tessel today keeps one merge in flight, tested before it lands, and already
verifies other agents' work against the new trunk after each merge (assumption trials, shadow
runs); the claims carry the scope data that lets non-overlapping merges run in parallel, which is
the batching item we have not shipped.

We built Tessel with itself: since October 5 our coding agents have claimed, submitted and landed
their work through Tessel; a few changes, every change to the frozen wire protocol among them,
were merged by hand by an admin. And we measured it with a scripted swarm. In a live run, 30
harness agents on 40 overlapping tasks landed all 40 with zero rejected work; replaying the same
tasks locally with plain git and no coordination landed 14 of 40. In a second live run, denied
agents did the work anyway on shadow claims and the system tested what would have happened: of
the trials of 28 shadow claims, 20 confirmed a real conflict, 9 were false alarms and 3 were
inconclusive. We count false alarms as false alarms.

## How you used Cloudflare
*Products and architecture.*

- **Workers**: a Rust coordinator Worker (workers-rs) and a TypeScript steward Worker, joined by
  service bindings in both directions.
- **Durable Objects**: one coordinator object per repository, SQLite-backed, holding claims,
  fencing tokens, the wait queue, leases (alarms), shadow claims, review decisions and an
  append-only event log. Every state change is stored before any reply is sent. Agents connect
  over WebSockets with a signed identity token.
- **Artifacts**: the trunk repository plus one fork per agent, created through the steward, with
  repo-scoped tokens; agents push with plain Git. After each merge the steward writes the
  merge record (claim, intent, decisions, review, tests) to a separate `<repo>--evidence`
  repository and the agent's verified transcript to `<repo>--transcripts`, each with its own
  write token and a read-back hash check, so the record of why a change landed outlives the
  agent's fork.
- **Queues**: Artifacts push events arrive on a queue; a push to a trunk's main is recorded in
  the coordinator's log. When the steward merges, agents whose claims overlap the merged change
  are told their base moved.
- **Containers**: a container-backed Durable Object (`TestRunner`) where the steward rebases
  each submission onto the trunk, runs the repo's gate from `tessel.toml` (cargo test, pnpm
  test) in an isolated container and merges only on a pass. In experiment runs, shadow claims
  are verified the same way. Only the steward writes the trunk.
- **Access**: the dashboard and review pages sit behind Cloudflare Access; the steward checks
  the Access token itself.

For agents: a native `tessel` CLI with a per-worktree daemon, `tessel init` (one rerunnable
command to get a repo ready), Claude Code hooks that claim before an edit and bring inbox
notices into the agent's turn, and an MCP server (`tessel mcp`).

## Instructions to run your project

Prerequisites: Rust stable (`rustup`) with the `wasm32-unknown-unknown` target, Node 22, pnpm,
and `wrangler`.

    git clone https://github.com/devfep/tessel.git && cd tessel
    cargo test --workspace          # protocol, coordinator, CLI and swarm harness
    cargo build -p tessel-cli       # the tessel CLI at target/debug/tessel
    cd tessel-steward && pnpm install --frozen-lockfile && pnpm test

Run the coordinator locally: put `IDENTITY_SIGNING_KEY=<a long random value>` in `.dev.vars`,
then `npx wrangler dev` (port 8787). The README shows how to mint an agent token and drive a
claim and a conflict by hand. Deploying the steward needs an account with Artifacts enabled.

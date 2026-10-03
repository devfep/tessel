# Submission form drafts

**Drafts. Rewrite on Oct 12 to match exactly what shipped.** Every claim here must be true on submission day.

## Your team
- Team name: _TBD (can be your name, or Tessel)_
- Primary contact name / email: _you_
- Team location: _city, country (US or Canada)_
- First attendee name / email: _you_ (must be free Oct 21, San Francisco)
- Second attendee: _optional_

## Project name
Tessel

## Project vision
*What did you rethink, and why does it matter?*

Git and the tools around it assume a few people working at human speed. Put hundreds of coding agents in one codebase and they duplicate each other's work, build on stale code, and break each other in ways Git can't see: one agent renames a function another is still calling, or changes a behaviour another relies on, and every merge is "clean".

We rethought the unit of collaboration. Branches become leases: a fork with an owner, a declared scope, an intent and an expiry. Pull requests become transactions that carry their own intent, assumptions and test evidence. Merge conflicts are prevented before code is written, using claims on functions and files rather than lines, and behavioural conflicts are surfaced through an assumption ledger. When several agents attempt the same task, it becomes a race and the best result ships. Humans review only the changes flagged as risky, each with its reason.

It matters because review and coordination, not code generation, are now the bottleneck. Git records what happened; Tessel coordinates what's happening. We built it with itself, and we measured what it prevented.

## How you used Cloudflare
*Products and architecture.*

- **Workers**: a Rust coordinator Worker (workers-rs) and a TypeScript steward Worker, connected by a service binding.
- **Durable Objects**: one coordinator per repository, SQLite-backed, holding the lock table, leases (via alarms), fencing tokens, races, assumptions and an append-only event log. Agents connect over hibernatable WebSockets.
- **Artifacts**: the main repository plus one fork per agent, created on demand with repo-scoped tokens. Agents use plain Git. Intent and decision records travel as git-notes. Separate Artifacts repos hold the evidence log and redacted transcripts.
- **Queues**: Artifacts push events trigger speculative test-merges and verification.
- **Sandbox SDK**: the steward rebases, runs tests and merges in isolated containers; only the steward writes main.
- **Workers Builds previews**: a stable coordinator coordinates development while changes deploy to a separate next instance.

## Instructions to run your project
_Paste the condensed README: prerequisites, setup steps, start command. Write after the Oct 11 fresh-clone test._

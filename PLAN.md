# Tessel: plan

Single source of truth for what we're building, why, and in what order. The detailed design,
requirements and test strategy are in [`docs/proposals/real-use.md`](docs/proposals/real-use.md),
approved by Felix on 17 October 2026. The plan Tessel was built under for Cloudflare's October 2026
challenge is in git history (`ba36647` and earlier).

## 1. Goal

Tessel is a transaction layer for many AI coding agents working in one repository. It is built for
teams who would trust it with their main branch: it must be robust, well tested, cloud agnostic,
and fit the git host and CI a team already uses.

**Git records what happened. Tessel coordinates what's happening.**

| Today | Tessel |
|---|---|
| Branches are names with no owner, purpose or expiry | **Leases:** a fork bound to a claim, with owner, scope, intent, fence and expiry |
| Worktrees fight over disk and ports | **Disposable forks,** plus shadow forks that check whether a denial was real |
| Pull requests ask a human to read a diff | **Transactions:** fenced, coverage-checked, carrying intent, assumptions and evidence; competing attempts become **races** |
| Merge conflicts are found after the work | **Prevented before writing** (semantic claims); **behavioural ones surfaced** (assumptions); a deterministic merge order |
| Every diff is reviewed line by line | **Review by exception:** humans see flagged changes, with reasons and evidence first |
| Repositories know only the past | **A present tense:** a live claim map, an assumption ledger and verified evidence, with storage still in git |

## 2. Constraints

- **Cloud agnostic from the start.** No platform gets special status. Every platform service sits
  behind a port; the first adapters are the ones a team needs (a single server binary, GitHub, the
  repo's own CI).
- **Robust and well tested.** Every requirement in the proposal becomes a test; CI must be green and
  required on main before the refactor starts.
- **Fit existing workflows.** Code stays on the team's git host; tests run in the team's CI; Tessel
  can run in advisory mode before it lands anything.

## 3. Architecture

```
 agent ── tessel CLI ──WebSocket──► tessel-server (Rust)
 (Rust: tree-sitter,                ├ one task per repo running the portable host
  git, hooks)                       │ (sans-IO core): claims, leases, fences, races,
                                    │ assumptions, review, event log
                                    ├ SQLite store (one writer)
                                    └ git host adapter (GitHub App): candidate ref
                                      → repo's own CI → compare-and-swap landing
```

- The coordinator never runs git or tests itself; only the merge path (steward) writes main.
- Rust throughout. The TypeScript steward and the Cloudflare Worker and Durable Object code stay
  only until the portable host replaces them (Phase 1), then are removed.
- Deferred until real usage data or a user asks: a native parallel merge queue, a self-hosted git
  host and sandbox, several server nodes. Their designs are in the proposal.

## 4. The protocol

`src/protocol.rs`, shared by CLI and coordinator, with its invariants at the top of the file:
hierarchical claims with four semantic modes and intention locks; leases and fencing; coverage;
assumptions, decision records and redacted transcript references (all free text is untrusted
data); races with deterministic ranking; review by exception; evidence (event log, shadow mode, one
`Summary` for every number).

Compatibility: changes are additive (new optional fields with `#[serde(default)]`) or bump
`PROTOCOL_VERSION`. Phase 0 replaces the exact version match with a supported range, announced
through a new optional field.

## 5. User experience

Progressive disclosure:
1. `tessel start "fix token refresh"`: fork, auto-claim on first edit, readable denials.
2. `tessel claim <scope> --edit-body`: explicit planning.
3. `--assume`, `race`, `--shadow`: power features.
4. The WebSocket protocol, documented, for custom clients.

A skill file teaches the workflow; hooks enforce it (auto-claim, refuse commits touching unclaimed
files); the coordinator rejects uncovered submissions; `tessel mcp` exposes the CLI as MCP tools.

## 6. Evidence

**A denial is not a prevented conflict.** Only verified outcomes count: shadow verification
test-merges a denied agent's work against what blocked it, and challenged assumptions are re-tested
on the new main. Evidence comes from CI check results, not from what an agent reports about itself.
No number is inflated in code, docs or the dashboard.

## 7. Phases

Each phase is done when its checks, listed in the proposal, pass in CI.

| Phase | Scope |
|---|---|
| 0. Foundations | CI and nightly jobs; characterisation tests for `runtime.rs`; merge-queue property tests; the four fixes to today's merge path (dispatch fencing, landing by ancestry, `main_moved` misread, verification starvation); versioned state with migrations; the protocol version range; metrics and structured logs |
| 1. Portable host | The sans-IO host and its deterministic simulator; `tessel-server` with SQLite; both native test hosts on the real merge path; Cloudflare and TypeScript steward code removed |
| 2. GitHub | GitHub App adapter (forks as separate repos with Actions disabled, required checks or the host's merge queue, compare-and-swap landing, check results as evidence); authenticated service calls; asymmetric identity keys with rotation and revocation |
| 3. Adoptable | Advisory mode; per-repo review configuration; `tessel admin`; backup, restore drill, retention; signed release binaries and an image |
| 4. Reach | Symbol claims for Python and Go; hooks for a second agent harness; Linux and macOS packages |
| 5. Design partners | Real teams; queue wait and merge duration measured |
| 6. On evidence only | Native parallel merge queue; self-hosted git and sandbox; several nodes |

## 8. Dogfooding

Tessel's own changes go through feature branches and pull requests, with a git note on each commit
(`CLAUDE.md`). Once Phase 1 ships, the repository is coordinated by `tessel-server` again, and
once Phase 2 ships, it lands through the GitHub adapter.

## 9. Decisions (17 October 2026)

1. The plan and phase order in the proposal are approved: GitHub first; the parallel queue,
   self-hosted sandbox and multi-node wait for data.
2. Cloudflare has no special status. Its code is removed as soon as the portable host replaces it,
   with no maintenance period.
3. The GitHub adapter, and any future steward, is Rust inside `tessel-server`.
4. The protocol version range is announced through a new optional field, without a version bump.
5. The contest-era licence whitelist no longer applies; licences are an ordinary engineering
   choice. The crate stays MIT.

## 10. Risks

- **The refactor breaks behaviour nobody wrote down.** Mitigation: characterisation tests of every
  rule in `runtime.rs` before it moves, then the simulator's invariants.
- **Teams won't hand over main.** Mitigation: advisory mode, branch protection, compare-and-swap
  only, a documented uninstall.
- **Symbol extraction is wrong for a language.** Mitigation: file-level claims as the safe
  fallback; languages added one at a time with tests.
- **Solo maintainer.** Mitigation: agents build behind the CI gates; Felix keeps protocol and
  merge-path changes.

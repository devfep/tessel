# Tessel: plan

Single source of truth for what we're building, why, and in what order. The detailed design,
requirements and test strategy are in [`docs/proposals/real-use.md`](docs/proposals/real-use.md),
approved by Felix on 17 October 2026; the phase contents were revised after a second round of
adversarial review and await his confirmation. The plan Tessel was built under for Cloudflare's
October 2026 challenge is in git history (`ba36647` and earlier).

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
  can run in advisory mode (claims only, humans and CI merge) before it lands anything.

## 3. Architecture

```
 agent ── tessel CLI ──WebSocket──► tessel-server (Rust)
 (Rust: tree-sitter,                ├ one task per repo running the portable host
  git, hooks)                       │ (sans-IO core): claims, leases, fences, races,
                                    │ assumptions, review, event log
                                    ├ SQLite store (one writer); identity (mints tokens)
                                    ├ lander, a separate process: deterministic rebase,
                                    │ coverage check, compare-and-swap push; runs git,
                                    │ never repo code
                                    └ git host adapter (GitHub App): candidate pull
                                      request from a fork → repo's own CI (no secrets)
                                      → compare-and-swap landing
```

- The coordinator never runs git or tests itself; only the lander writes main.
- Rust throughout. The TypeScript steward and the Cloudflare Worker and Durable Object code stay
  only until the portable host and the Rust lander replace them (Phase 1), then are removed.
- Deferred until real usage data or a user asks: a native parallel merge queue, a self-hosted git
  host and sandbox, several server nodes. Their designs are in the proposal.

## 4. The protocol

`src/protocol.rs`, shared by CLI and coordinator, with its invariants at the top of the file:
hierarchical claims with four semantic modes and intention locks; leases and fencing; coverage;
assumptions, decision records and redacted transcript references (all free text is untrusted
data); races with deterministic ranking; review by exception; evidence (event log, shadow mode, one
`Summary` for every number).

Compatibility: changes are additive (new optional fields with `#[serde(default)]`) or bump
`PROTOCOL_VERSION`. Phase 1 replaces the exact version match with a supported range, announced
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
on the new main. Evidence records which required checks passed on exactly the landed commit, not
what an agent reports about itself, and is described as that ("required checks passed", never
"verified safe"); test and build paths are review-flagged because an agent can weaken its own
tests. No number is inflated in code, docs or the dashboard.

## 7. Phases

Each phase is done when its checks, listed in the proposal, pass in CI. Sizes are rough
person-weeks for one maintainer working with agents.

| Phase | Scope |
|---|---|
| 0. Foundations (~3) | CI lanes required on main; a GitHub spike (compare-and-swap landing of a tested commit under rulesets); merge-queue property tests; core fixes: dispatch ids, the landed commit on `already_merged`, verification slots and `VerificationDropped` |
| 1. Portable host and lander (~10-12) | `runtime.rs` moved step for step into the sans-IO host; the simulator with invariants I1 to I12 and failpoints; `tessel-server` with SQLite, versioned state, the protocol range, metrics, Ed25519 identity and token minting; the Rust lander and a local test runner; minimal advisory mode; Cloudflare and TypeScript code deleted |
| 2. GitHub landing (~5-7) | GitHub App adapter: human-provisioned fork pool, candidate pull requests whose CI runs without secrets, two rulesets, `updateRefs` compare-and-swap, polling reconciliation; `tessel admin doctor` |
| 3. Adoptable | Trials on GitHub; the `tessel/claims` advisory check; review page and dashboard behind OIDC; per-repo review configuration; `tessel admin`; backup, restore drill, retention; signed releases and an image |
| 4. Reach | Symbol claims for Python and Go; Sandcastle (`mattpocock/sandcastle`) as the second agent harness: a template whose sandboxed agents claim and submit through Tessel, which lands their work in place of an LLM merge step; Linux and macOS packages |
| 5. Design partners | Real teams; queue wait, CI duration and merge outcomes measured; an A/B of Sandcastle's `parallel-planner` with and without Tessel on the same issues (textual conflicts, broken tests on main, wasted runs) |
| 6. On evidence only | Native parallel merge queue; self-hosted git and sandbox; several nodes |

## 8. Dogfooding

Tessel's own changes go through feature branches and pull requests, with a git note on each commit
(`CLAUDE.md`). Once Phase 1 ships, the repository is coordinated by `tessel-server` in advisory
mode (claims released when their pull requests merge), and once Phase 2 ships, it lands through
the GitHub adapter.

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
- **GitHub cannot land exactly the tested commit under its rulesets.** Mitigation: the Phase 0
  spike answers this before the host's design is frozen.
- **Solo maintainer.** Mitigation: agents build behind the CI gates; Felix keeps protocol and
  merge-path changes.

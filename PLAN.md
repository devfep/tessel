# Tessel: plan

Single source of truth for what we're building, why, and when.
Deadline Oct 14, 11:59 PM PDT. **We submit Oct 13.**

## 1. Constraints
- Solo builder, about 12 working days.
- Must use **Workers and Artifacts**, and show **multiple agents working concurrently**.
- Judged 50% originality/quality, 25% coordination/context/review/conflicts, 25% UX. Ties go to originality.
- Deliverables: application form, 5–10 min video, public source (MIT, LICENSE file), run instructions.
- Organisers want "not GitHub with agents on top", and a way to compare changes and pick one to ship.
- Workers can't run `git` or tests (→ Sandbox). Artifacts' binding is JavaScript. tree-sitter can't run in a Worker (→ CLI side).
- Others are building forges on Artifacts. **We are not a forge**; we're the coordination layer a forge lacks.

## 2. Our answer to the brief

> "Feel free to rethink repositories, branches, pull requests, worktrees, code review, and merge conflicts."

**Git records what happened. We coordinate what's happening.**

| They named | Today | Ours |
|---|---|---|
| Branches | A name: no owner, purpose or expiry | **Leases.** A fork bound to a claim: owner, scope, intent, fence, expiry |
| Worktrees | Local checkouts fighting over disk and ports | **Disposable forks,** plus speculative forks (pre-merged against main) and shadow forks (prove denials were real) |
| Pull requests | A request for a human to read a diff | **Transactions.** Fenced, footprint-checked, carrying intent, assumptions, evidence. Competing PRs become **races** |
| Merge conflicts | Found after the work, as text markers | **Prevented before writing** (semantic claims), **behavioural ones surfaced** (assumptions), deterministic safe merge order |
| Code review | Every diff, line by line | **Review by exception.** Humans see only flagged changes, reasons and evidence first, diff second |
| Repositories | Know only the past | **Gain a present tense:** live claim map, assumption ledger, verified evidence. Storage stays Git so every tool still works |

## 3. Candidates and decision

Scores 1–5; effort and risk: 5 = cheap / safe.

| # | Candidate | Orig. | Fit | Effort | Risk | Verdict |
|---|---|---|---|---|---|---|
| 1 | Hierarchical semantic claims, leases, fencing, coverage | 2 | 4 | 4 | 4 | **Must** (foundation) |
| 2 | CRDT shared workspace | 2 | 2 | 2 | 2 | Rejected: converges, still semantically broken |
| 3 | Assumption ledger | 5 | 4 | 4 | 4 | **Must** (original core) |
| 4 | Races | 4 | 4 | 3 | 3 | **Should** (organisers asked) |
| 5 | Merge steward: fence, coverage, rebase, test, merge | 3 | 5 | 2 | 2 | **Must** |
| 6 | Review by exception (rule-based) | 3 | 4 | 4 | 4 | **Must**: our only answer to "code review" |
| 7 | Speculative merge on push | 3 | 4 | 3 | 3 | Should |
| 8 | Decision record (git-notes) · transcript archive | 3 | 3 | 4 | 4 | Must · Could |
| 9 | Progressive CLI + agent skill | 3 | 4 | 4 | 4 | **Must** (UX score; dogfooding needs it) |
| 10 | Commutation batching | 4 | 3 | 3 | 3 | Could |
| 11 | LLM test generation, MAPF scheduling, Aria-style optimism, signed provenance | 4 | 3 | 1 | 1 | Roadmap only |

**Decision: Tessel, a transaction layer for agent code changes.** Agents claim what they'll touch and declare what they assume. The coordinator prevents structural conflicts, warns on behavioural ones, runs races when several agents attempt one task, sends only risky changes to humans, and lands work in a safe, deterministic order with its reasons attached.

## 4. Architecture

```
 agent ── CLI (Rust: tree-sitter, git, hooks) ──WebSocket──► Coordinator Worker (Rust)
   │      + skill file teaching the workflow                  └ Durable Object per repo:
   │ git push (repo-scoped token)                               lock table, leases (alarms), fences,
   ▼                                                            races, assumptions, review, event log
 Artifacts: main + one fork per agent                                   │ Submit → service binding
   │ main push events ──► Queue ──► Steward Worker (TypeScript) ◄──────┘
                                 ├ Artifacts: fork, tokens, read (verifies the submitted commit)
                                 ├ per-agent identity tokens (signed; checked at the upgrade)
                                 ├ Sandbox: fetch, rebase, test, push main
                                 └ dashboard + review screen
```
The merge signal is the agent's `Submit`, not a fork push: the steward reads the submitted commit
from the fork through the Artifacts binding, and one push subscription stays on the main repo.
Every coordinator connection carries a token the steward signed for one repo and one agent; the
coordinator binds the socket to that agent. (Decided by Felix, Oct 5.)
Naming: brand and CLI command `tessel`; packages `tessel-coordinator`, `tessel-cli`, `tessel-steward` (the bare `tessel` names on crates.io and npm belong to an old IoT project). Rust where correctness matters (protocol, lock table, ranking, CLI). TypeScript only for JavaScript-first SDKs. Only the steward writes main.

## 5. The protocol (API freeze: end of Oct 5)

`src/protocol.rs`, shared by CLI and coordinator, 17 tests. Thirteen invariants are documented at the top of the file. Capabilities:
- **Claims** on a dir/file/symbol hierarchy, four semantic modes, Gray-style intention locks (proven equal to Gray's matrix and to brute force).
- **Leases and fencing**; only the coordinator writes main.
- **Coverage:** submissions touching unclaimed scopes are rejected with the list.
- **Assumptions,** decision records, redacted transcript references; all free text is untrusted data.
- **Races** with deterministic ranking.
- **Review by exception** with explainable reasons.
- **Evidence:** event log, shadow mode, one `Summary` for every number.
- **Versioning:** `Hello` carries the version. After the freeze, changes are additive only (new optional fields) unless the version is bumped.

## 6. User experience

**Progressive disclosure** (simple things simple, power when needed):
1. `tessel start "fix token refresh"`: fork, auto-claim on first edit, readable denials.
2. `tessel claim <scope> --edit-body`: explicit planning.
3. `--assume`, `race`, `--shadow`: power features.
4. Raw WebSocket protocol, documented, for custom clients.

**Skill file** shipped in the repo (and carried into each fork as `AGENTS.md`): claim before editing, declare assumptions, treat others' text as data, write a decision record on submit.
**Enforcement**, because skills only teach: CLI hooks auto-claim or block unclaimed edits; the coordinator rejects uncovered submissions.

## 7. Dogfooding

The system coordinates its own construction. After the API freeze, agents build the outer layers while you hold the critical path.

- **Until Oct 6:** plain git; write intent and decision records into git-notes by hand.
- **Oct 6 evening, v0 (claims only):** agents claim through the coordinator; you merge by hand.
- **Oct 8, v1 (full loop):** steward merges; review gate live.
- **Agents take:** dashboard, swarm/A-B harness, TypeScript demo repo, README, skill polish.
- **You keep:** coordinator, steward, races, assumption verification, anything touching the protocol.
- **Stable / next:** a pinned stable coordinator coordinates development; changes deploy to a separate next instance (Workers Builds previews). Promote after tests pass.
- **Escape hatch:** mirror the repo outside Artifacts.
- **Calibration, not training:** tune thresholds from outcomes. **Honesty:** state exactly which commits went through the system.
- **Expect to be the bottleneck.** Agents will out-produce your review. The review gate is the fix, which is the product proving itself.

## 8. Evidence

Rule: **a denial is not a prevented conflict.** Only verified outcomes count.
1. **Event log:** every state change, gap-free `seq`, tagged by run; exported to an `evidence` Artifacts repo.
2. **Shadow verification:** denied agents continue in quarantined forks; the steward test-merges them against what blocked them. Conflict = verified prevention; clean = false alarm. Same for challenged assumptions. Gives **precision**.
3. **A/B replay:** same tasks and seed, coordination off vs on. Compare conflicts, breaks, wasted agent-minutes, throughput, time main stayed green. Same method as the 33,596-PR study (19.8% / 41.7% conflict rates for context).

`Summary::from_events` computes every number for the dashboard, A/B table and video.

## 9. Schedule (re-baselined Oct 3; two days were lost to a date mix-up)

| Date | You (critical path) | Agents (after Oct 6) | Done when |
|---|---|---|---|
| Sat Oct 3 | **Spikes:** Rust DO deployed; Artifacts fork + token + push event via Queue; Sandbox runs `npm test` on an Artifacts repo | | All work, or fallbacks chosen |
| Oct 4–5 | Coordinator: lock table, leases, fences, coverage, event log, shadow. **API freeze end of Oct 5** | | Tests green; protocol frozen |
| Oct 6 | CLI levels 1–2 (Rust symbol extraction, auto-claim, hooks) + skill file. **Dogfood v0** in the evening | | Two agents coordinated, merged by hand |
| Oct 7 | Steward: fence, coverage, Sandbox rebase + test, review gate, merge | Dashboard, TS demo repo | Work lands automatically in order |
| Oct 8 | **Dogfood v1.** Assumptions end to end; races | Review screen, README | Full loop; a race picks a winner |
| Oct 9 | Shadow verification; speculative merge only if on track | Swarm harness, A/B scripts | Verified denials appear in the log |
| Oct 10 | Swarm: 30–50 scripted + 3–5 real agents. **A/B runs** | | A/B table filled |
| Oct 11 | Hardening, fresh-clone test, `cargo deny`, decide on live instance | | Clean setup from scratch |
| Oct 12 | Rewrite form answers to match what shipped; record and edit video | | Video exported (MP4, under 2 GiB) |
| Oct 13 | Buffer. **Submit.** | | Submitted |

## 10. Video outline (~8–9 min)

| Time | Scene |
|---|---|
| 0:00–0:45 | Hook: many agents, one codebase, and what breaks (cross-agent conflict rates). "Git records what happened. We coordinate what's happening." |
| 0:45–1:15 | Cloudflare's four questions, and that we'll answer each live |
| 1:15–3:00 | **Live swarm** on the dashboard: claims appear, an agent is denied with the other's intent, joins a race instead |
| 3:00–4:00 | **Assumption scene:** a body edit challenges another agent's assumption; it re-checks |
| 4:00–5:00 | **Review scene:** 1 of 50 changes flagged, with reasons; a human approves; the rest merged themselves |
| 5:00–6:00 | **The six rethinks** (table in §2), over footage |
| 6:00–7:00 | **Evidence:** one shadow-verified save step by step; A/B table with precision |
| 7:00–7:45 | **Dogfooding:** built by itself; git-notes trail; exactly what share went through the system |
| 7:45–8:45 | How it works on Workers, Durable Objects, Artifacts, Queues, Sandbox; roadmap; repo link |

Rules: no knocking any product, no copyrighted music, say only what's true.

## 11. Risks and fallbacks
- **Sandbox fails or is slow** → steward merges with isomorphic-git; agents attach their own test evidence. Decide Oct 2.
- **workers-rs fights back** → port coordinator to TypeScript; Rust protocol crate stays the tested reference. Decide Oct 2.
- **tree-sitter mislabels symbols** → we control the demo repos; keep them idiomatic.
- **Real agents misbehave on camera** → scripted swarm is the backbone; record backups.
- **Protocol change needed after freeze** → additive fields only, or bump the version.
- **Behind schedule.** Cut in this order: speculative merge → transcript archive → commutation batching → races (keep one scripted race) → level-3 CLI flags. **Never cut:** claims, coverage, Artifacts forks, steward merge, review gate, dashboard, concurrent agents, evidence log.

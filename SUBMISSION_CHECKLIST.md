# Tessel: submission checklist

Sources: Official Rules (PDF), cloudflare.com/git-competition, and the Oct 1 2026 announcement blog post.
Deadline: **October 14, 2026, 11:59 PM PDT.** Submit a day early; late or incomplete entries are void.

Status: ✅ done · 🟡 in progress · ⬜ not started · 👤 only you can confirm

## Eligibility (Rules §3)
- 👤 Legal resident of the US or Canada
- 👤 18 or older as of Oct 1, 2026
- 👤 Not an employee, officer or director of any government agency, public-sector enterprise or state-owned entity
- 👤 Not a Cloudflare employee, or immediate family or household member of one
- 👤 Not on any sanctions list

## Hard technical requirements (Rules §4)
- ✅ Built on Cloudflare Workers (Rust Worker + Durable Object)
- ⬜ **Uses Artifacts.** Per-agent forks, repo-scoped tokens, push events. *Required: without it the entry is void.*
- ⬜ **Multiple agents working on changes concurrently**, shown in the demo
- ✅ Permissive license: MIT
- 🟡 LICENSE file in the repo: present, **replace `<YOUR NAME>`**
- ⬜ `cargo deny check licenses` passes (no GPL in the dependency tree)

## Submission contents (Rules §4, blog)
- ✅ Form reviewed (Oct 3). Fields below; drafts in `SUBMISSION_FORM.md`
- 👤 Team: team name, primary contact name + email, team location, **first attendee name + email (required)**, second attendee (optional)
- ✅ Project name: Tessel
- ⬜ Project vision: "What did you rethink, and why does it matter?"
- ⬜ How you used Cloudflare: products and architecture
- ⬜ 5 to 10 minute demo video, **uploaded directly** (MP4, WebM or MOV, max 2 GiB). Export 1080p H.264 MP4; no hosting link needed
- ⬜ No copyrighted music or footage in the video; use royalty-free audio or none
- 👤 Tick: "built using Cloudflare Workers and Artifacts" (must be true)
- 👤 Tick: "follows the competition terms"
- ℹ️ Submission details and video are kept up to 180 days
- ⬜ Public repository URL (form placeholder is a GitHub URL; any public host works)
- ⬜ Instructions for running or trying the project: a **form text field** (prerequisites, setup steps, start command) plus the README, tested from a fresh clone
- ⬜ Optional: a live instance judges can try (helps the 25% UX score; mind Artifacts billing from Oct 15)
- ⬜ All required application fields completed
- 👤 Select "Yes" to the Terms and Conditions in the form
- 👤 Only one submission per entrant
- 👤 Submit by hand: automated or scripted entry is prohibited

## Content rules (Rules §4)
- ⬜ All original work; no third-party copyrighted material without permission
- ⬜ Demo repository the agents work on is your own or permissively licensed
- ⬜ Any dataset used (e.g. AgenticFlict) checked for license terms
- ⬜ **No knocking GitHub or any other product** in the video or README. Frame everything as what you enable.
- ⬜ No weapons, sexual, political or illegal content
- ⬜ No third-party terms of service violated while building

## What the judges score (Rules §6)
| Weight | Criterion | Our answer |
|---|---|---|
| 50% | Originality and quality of the prototype for agent-oriented collaboration | Transactions for code: hierarchical semantic claims, fencing, commutation-based merge planning, races |
| 25% | Concurrency, coordination, context preservation, review, conflict handling | Live claim map, `BaseMoved`, intent in git-notes, risk gate, deterministic merge order |
| 25% | Ease of use and product/user experience | One-command CLI, live dashboard with stats |

Ties go to the 50% category.

## What the blog says they want
- ⬜ Not "GitHub with agents on top"
- ⬜ Answer their four questions explicitly in the video:
  - How do agents know what other agents are working on? → claims + live map
  - What happens when changes conflict? → conflict modes, denial with intent, races
  - How do you review everything agents produce? → risk-scored gate, humans see only the top slice
  - How do you track *why* a change was made? → intent and evidence in git-notes
- ⬜ Compare multiple changes and decide which ships → **race mode** (protocol ✅, coordinator ⬜)

## Accounts and costs
- 👤 Workers Paid plan (Artifacts open beta requires it)
- ℹ️ Artifacts billing starts Oct 15, after the deadline

## If you are a finalist (Rules §6–7)
- 👤 Free on **Oct 21, 2026** at Moscone West, San Francisco, for a 10-minute live presentation
- 👤 Must be physically present to win
- 👤 Passport or ID, incidentals and meals are on you
- ⬜ A live demo that still works on conference Wi-Fi (record a backup)
- 👤 If you win: may need to sign a declaration of eligibility, liability, IP and publicity release (Rules §7)

## Rights you grant (Rules §9), for awareness
- You keep ownership of your code
- Cloudflare gets a perpetual license to your name, likeness, demo video and presentation
- Submissions are not confidential, and Cloudflare may build similar products

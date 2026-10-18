# GitHub spike: what `updateRefs`, rulesets and checks do

Answers the P0-SPIKE questions in `docs/proposals/real-use.md` ("The GitHub adapter", spike
paragraph). Run on 18 October 2026 against `tessel-sandbox/spike-target` (public, free plan) with a
fork at `devfep/spike-target`. Reproduce with `scripts/spike/` (`setup.sh`, `run.sh user|app|webhooks`,
`teardown.sh`; the header of `run.sh` says the order).

**Status: partly answered.** Everything that runs under devfep's own login is measured below.
The two results that need the lander App's installation token are not run yet (see "Still open").

## Setup under test

- Workflow `check` (job `check`), trigger `pull_request`, read-only token, one `echo ok` step.
- Ruleset A: restrict updates and block force pushes on the default branch; bypass actor is the
  App (ID 5368982, `Integration`, always).
- Ruleset B: required status check `check` pinned to the GitHub Actions app (ID 15368), linear
  history, no bypass actors.
- Each test commit is one commit on a branch of the fork, one commit ahead of main, with a pull
  request from the fork to main. Landing is GraphQL `updateRefs` of `refs/heads/main` from
  `beforeOid` to the head commit.

## 1. Does a fork pull request's check satisfy ruleset B for `updateRefs`?

**Yes, measured with devfep's token (an organisation admin, not a bypass actor of B).**

| Attempt (only B active) | Result |
|---|---|
| `updateRefs` of the fork head 1 s after opening the pull request, check not finished | refused: `Required status check "check" is expected.` main unchanged |
| the same call after the `check` run completed with `success` on the fork pull request head | accepted, main is the head commit |

The commit was never pushed to the target repository: it was reachable only through the fork and
the pull request. The check run reported `app: github-actions`. B has no bypass, so an admin is
held to it; the refusal is the control that shows the rule is being enforced.

Not yet measured: the same landing with the App's token while A is also active (see "Still open").

## 2. Is a stale `beforeOid` refused?

**Yes, measured as devfep with only B active; A's bypass half is open.**

- `updateRefs` with a stale `beforeOid` and a fast-forward `afterOid` with a passing check was
  refused 3 of 3 times and main did not move. Without the compare, it would have been a valid
  fast-forward. The same call with the correct `beforeOid` was accepted.
- The refusal is not a typed rule error. GitHub returns `Something went wrong while executing your
  query ... Please include <request id> when reporting this issue.` with no `type` and no reason.
  Rule refusals are typed (`UNPROCESSABLE`, `Branch 'refs/heads/main' is protected: ...`), and a
  non-fast-forward is `rejecting non-fast-forward update`. So **a stale base cannot be told apart
  from a transient server error by the response**; the lander must read the ref back before it
  decides.
- Ruleset A with the user token (not a bypass actor), check passed, correct `beforeOid`: refused,
  `Cannot update this protected ref.` main unchanged.

Open: the same stale call with the App as bypass actor of A (App-token step below).

## 3. Timings

Three fork pull requests, GitHub's timestamps (one-second resolution), seconds after the commit
was created:

| PR | workflow run created | check started | check completed |
|---|---|---|---|
| x | 7 | 10 | 14 |
| y | 4 | 7 | 10 |
| z | 5 | 7 | 9 |

Pull request creation (API) took 1 s after the commit each time. A trivial workflow therefore
needs about 9 to 14 s from commit to a landable head; real CI adds its own duration. Three
samples on a free-plan public repository say nothing about queueing under load.

Webhooks: a repository webhook to an unreachable host (`*.invalid`) still records each attempt;
`delivered_at` is the time of the attempt (`failed to connect`), so this measures GitHub's
dispatch delay, not a receiver's. Over 12 deliveries, the delay between the event timestamp in
the payload and `delivered_at` was 0 to 3 s (check_run created 0-2, check_run completed 1-2,
pull_request opened 2-3, pull_request closed 2 after the landing). The payload timestamps have
one-second resolution, so each lag is ±1 s. GitHub does not retry these, which matches the
proposal's rule that the poll is authoritative. Delivery to a real receiver and its own latency
were not measured.

## 4. One request, or a durable job finished by reconciliation?

**A durable job.** The landing itself is one atomic request, but it is not the whole operation:

1. Readiness takes seconds and arrives asynchronously (9-14 s here for a trivial check), so the
   lander cannot hold one request open across it; it must wait for the check, from a hint or a
   poll.
2. A refused `updateRefs` can be a rule (typed message), a lost compare (generic server error) or
   a real transient failure, and the generic one is not distinguishable from the other two. The
   lander journals the head before the call (requirement 6), then reads the ref: main equal to
   the head means landed, anything else means retry or re-base.
3. Webhooks arrive within about 3 s, so they work as hints that bring the poll forward.

So the host's `Call` for landing is: journal, `updateRefs`, read back, with the poll as the
authority. This is the shape the proposal already describes. Nothing in the results asks for a
change to it.

## Still open: commands that need the App token

Both need the installation token, minted inside `run.sh app` only. State left in the sandbox:
rulesets A and B active, PR #3 (`spike-z`) open and mergeable, main at the head of PR #2.

Run `scripts/spike/run.sh app` once Felix authorises minting. Expected output:

```
accepted (as expected)  Q1: App bypasses A, check passed on z, correct beforeOid
PR #4 for w: ...
refused (as expected)  Q1 control: App bypasses A but not B, check not finished
TIMING ...
refused (as expected)  Q2a: App bypass actor, stale beforeOid
accepted (as expected)  Q2a control: App, correct beforeOid
```

and exit code 0. Any `UNEXPECTED` line is a finding, not a script bug: the App bypass either
failed to apply, or A's bypass also lets a stale base through.

## Other limits of this spike

- The `.invalid` webhook measures dispatch only.
- A failing check was not tested; the refusal while the check was missing is the only negative
  control for B.
- The workflow has no secrets and no `workflows` permission questions; fork-token settings from
  prerequisite 10 were not exercised (the fork owner is an organisation admin, so approval for
  first-time contributors never applied).
- `setup.sh` is idempotent (second run: no creations). `teardown.sh` was not run; it needs
  `gh auth refresh -s delete_repo`, and the sandbox is still up.

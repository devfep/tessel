# GitHub spike: what `updateRefs`, rulesets and checks do

Answers the P0-SPIKE questions in `docs/proposals/real-use.md` ("The GitHub adapter", spike
paragraph). Run on 18 October 2026 against `tessel-sandbox/spike-target` (public, free plan) with a
fork at `devfep/spike-target`. Reproduce with `scripts/spike/` (`setup.sh`,
`run.sh user|app|webhooks`, `teardown.sh`). Full run, in order: `teardown.sh`, `setup.sh`,
`run.sh user`, `run.sh app`; neither `user` nor `app` can be repeated on the same repository.

**Status: answered.** Questions 1 and 2 were measured twice: with devfep's own login, and with the
lander App's installation token, narrowed to contents write on `spike-target` only. Each
experiment ran once, the user-token steps and the App steps on 18 October 2026 (the App run at
14:16 UTC), so each result is one observation, repeated only where stated.

## Setup under test

- The spike App also holds administration write (to manage rulesets) and checks, statuses and
  actions read. The installation token the script uses for landing is narrowed at mint time to
  contents write only (the proposal's item 1), so the App-stage answers hold for a lander with
  that one write permission, and the token is revoked when the script exits.
- Workflow `check` (job `check`), trigger `pull_request`, read-only token, one `echo ok` step.
- Ruleset A: restrict updates and block force pushes on the default branch; bypass actor is the
  App (ID 5368982, `Integration`, always).
- Ruleset B: required status check `check` pinned to the GitHub Actions app (ID 15368), linear
  history, no bypass actors.
- Each test commit is one commit on a branch of the fork, one commit ahead of main, with a pull
  request from the fork to main. Landing is GraphQL `updateRefs` of `refs/heads/main` from
  `beforeOid` to the head commit.

## 1. Does a fork pull request's check satisfy ruleset B for `updateRefs`?

**Yes, measured with devfep's token (an organisation admin, not a bypass actor of B) and with
the App's token (a bypass actor of A, with rulesets A and B both active).**

| Attempt (only B active) | Result |
|---|---|
| `updateRefs` of the fork head immediately after the pull request opened, check not finished | refused: `Required status check "check" is expected.` main unchanged |
| the same call after the `check` run completed with `success` on the fork pull request head | accepted, main is the head commit |

The commit was never pushed to the target repository: it was reachable only through the fork and
the pull request. The check run reported `app: github-actions`. B has no bypass, so an admin is
held to it; the refusal is the control that shows the rule is being enforced.

With the App token, contents write only, A and B both active:

| Attempt | Result |
|---|---|
| `updateRefs` of PR #3's head (`spike-z`), check completed with `success`, correct `beforeOid` | accepted, main is that head |
| `updateRefs` of PR #4's head (`spike-w`) immediately after it opened, check not finished | refused, `UNPROCESSABLE`: `Branch 'refs/heads/main' is protected: Cannot update this protected ref. Required status check "check" is expected..` (two periods, as logged) |

The first row shows that a token holding contents write alone is enough to land with
`updateRefs` when the App is A's bypass actor and the check has passed. The second row shows that
the bypass actor of A is still subject to ruleset B. The message names both phrases even though
A is bypassed; the same actor, token and call shape succeeded once the check had passed, so the
blocking rule was B. Which rule contributed the "Cannot update" wording was not isolated.

## 2. Is a stale `beforeOid` refused?

**Yes, with devfep's token (only B active) and with the App as bypass actor of A (A and B
active).**

- `updateRefs` with a stale `beforeOid` and a fast-forward `afterOid` with a passing check was
  refused 3 of 3 times, back to back in one run, and main did not move. The three-call loop was
  added to `run.sh` after that run, from an ad hoc loop with the same calls; the committed script
  has not run it. Without the compare, it would have been a valid
  fast-forward. The same call with the correct `beforeOid` was accepted.
- The refusal is not a typed rule error. GitHub returns `Something went wrong while executing your
  query ... Please include <request id> when reporting this issue.` with no `type` and no reason.
  Rule refusals are typed (`UNPROCESSABLE`, `Branch 'refs/heads/main' is protected: ...`; seen in
  the `run.sh user` steps for the missing check and ruleset A). A non-fast-forward update is
  `rejecting non-fast-forward update`; that one came from an ad hoc call, not a script step. So
  **a stale base cannot be told apart
  from a transient server error by the response**; the lander must read the ref back before it
  decides.
- Ruleset A with the user token (not a bypass actor), check passed, correct `beforeOid`: refused,
  `Cannot update this protected ref.` main unchanged.
- The App (bypass actor of A, token narrowed to contents write) with a stale `beforeOid` (the
  head of PR #1, `f8367d0`, two commits behind main, which was PR #3's head when the call ran)
  and PR #4's head, whose check had completed with `success`: refused
  with the same generic `Something went wrong while executing your query ... Please include
  <request id>` error, once, and main did not move. The same call with the correct `beforeOid`
  was accepted. So the bypass of A does not bypass the compare-and-swap.

## 3. Timings

Four fork pull requests, GitHub's timestamps (one-second resolution), seconds after the commit
was created:

| PR | workflow run created | check started | check completed |
|---|---|---|---|
| x | 7 | 10 | 14 |
| y | 4 | 7 | 10 |
| z | 5 | 7 | 9 |
| w (App run) | 4 | 6 | 9 |

Pull request creation (API) took 1 s after the commit each time. A trivial workflow therefore
needs about 9 to 14 s from commit to a landable head; real CI adds its own duration. Four
samples on a free-plan public repository say nothing about queueing under load.

Webhooks: a repository webhook to an unreachable host (`*.invalid`) still records each attempt;
`delivered_at` is the time of the attempt (`failed to connect`), so this measures GitHub's
dispatch delay, not a receiver's. The figures below are from the user-token run only: the hook
recorded 17 deliveries by then, and 11 carry an event timestamp in the payload (3 opened, 3 check_run created, 3 check_run completed, 2 closed), and for those the
delay between that timestamp and `delivered_at` was 0 to 3 s
(check_run created 0-2, check_run completed 1-2,
pull_request opened 2-3, pull_request closed 2 after the landing). The payload timestamps have
one-second resolution, so each lag is ±1 s. All 11 show `redelivery: false`, so no failed
delivery was retried during the run; that matches the proposal's rule that the poll is
authoritative. The hook now holds 25 deliveries; the 8 from the App run were not analysed.
Delivery to a real receiver and its own latency were not measured.

## 4. One request, or a durable job finished by reconciliation?

**A durable job.** The landing itself is one atomic request, but it is not the whole operation:

1. Readiness takes seconds and arrives asynchronously (9-14 s here for a trivial check, four
   samples), so the lander cannot hold one request open across it; it must wait for the check,
   from a hint or a poll.
2. A refused `updateRefs` can be a rule (typed message), a lost compare (generic server error) or
   a real transient failure, and the generic one is not distinguishable from the other two. The
   App as bypass actor gets the same generic error on a stale base. The
   lander journals the head before the call (requirement 6), then reads the ref: main equal to
   the head means landed, anything else means retry or re-base.
3. Webhooks arrive within about 3 s, so they work as hints that bring the poll forward.

So the host's `Call` for landing is: journal, `updateRefs`, read back, with the poll as the
authority. This is the shape the proposal already describes. Nothing in the results asks for a
change to it.

## Other limits of this spike

- The App steps ran once, at 14:16 UTC on 18 October 2026; the stale-base refusal with the App is
  a single observation. Nothing was run to show whether a transient failure and a stale base can
  be told apart by anything but a read-back.
- The `.invalid` webhook measures dispatch only.
- A failing check was not tested; the refusal while the check was missing is the only negative
  control for B.
- The workflow has no secrets and no `workflows` permission questions; fork-token settings from
  prerequisite 10 were not exercised (the fork owner is an organisation admin, so approval for
  first-time contributors never applied).
- `setup.sh` is idempotent (second run: no creations). `teardown.sh` was not run; it needs
  `gh auth refresh -s delete_repo`, and the sandbox is still up, with main at the head of PR #4.

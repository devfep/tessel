# A/B run: seed 1, 6 tasks, overlap 0.5, 3 agents

Same seed, same tasks, same starting repository, same 300 ms of work per task.

| Metric | off: no coordination, plain git, red merges rolled back, local replay | on: Tessel (live coordinator, policy shadow) |
|---|---|---|
| Tasks | 6 | 6 |
| Landed on the trunk | 4 | 4 |
| Rejected after the work was done | 2 | 0 |
| of which textual conflict | 1 | n/a |
| of which broke the build | 0 | n/a |
| of which broke the tests | 1 | n/a |
| Not finished (starved, timed out, failed, not run) | 0 | 0 |
| Run as shadow work (submitted for verification, never to merge) | n/a | 2 |
| Claims denied outright (a denial is not a prevented conflict) | n/a | 2 |
| Claims queued behind a holder (wait policy) | n/a | 0 |
| Conflicts prevented, verified by shadow runs | n/a | verified preventions 2, false alarms 0 (shadow claims 2: inconclusive 0, never verified 0) |
| Held for review (not approved) | n/a | 0 |
| Review approvals (the only reviewer is the script) | n/a | 2 |
| Review rejections | n/a | 0 |
| Wall time (ms) | 3739 | 107194 |
| Landed per minute | 64.1 | 2.2 |
| Agent-minutes of work later rejected | 0.035 | 0.000 |
| Agent-minutes of work in total | 0.097 | 0.155 |
| Agent-minutes on shadow work (never merged) | n/a | 0.053 |

- `off` is a local replay. Its numbers are computed here from plain-git merges onto a trunk in task order with the tests run after each merge, and are not sent to any coordinator. A merge that breaks the build or the tests is rolled back, so each merge is judged on a green trunk. `Summary::from_events` over the replay's own events gives 6 merges and 2 conflicts.
- Every `on` count of the coordinator's behaviour comes from `Summary::from_events` over the coordinator's event log (33 events). "Rejected" and "queued" are counts of `SubmitRejected` and `WaitQueued` events in that log, which `Summary` has no field for.
- `off` models `--agents` agents working at once: task i branches from the trunk after tasks 1 to i minus agents were merged, as an agent that pulls before its next task would, and does not see work still in flight. Red merges are rolled back, so this harness does not measure how long main stayed green.
- Agent-minutes are the same quantity on both sides: the configured work time plus the measured time to edit, run the tests and commit, per task. In `on` the clock starts when the claim is granted.
- A cell reading `n/a` is a number this run cannot produce, not a zero. The log records why a steward rejected a submission only as text, which the harness does not parse, so `on` has no split of its rejections.
- Under the `shadow` policy a denied task is run as shadow work and never lands, so "Landed on the trunk" and "Landed per minute" are not comparable with the `wait` or `skip` policies, which land every task they can. Shadow work counts in the agent-minutes of work in total.

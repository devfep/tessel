# Live A/B runs, 7 October 2026 (all failed; kept as they happened)

Policy `wait`, seed 3, 40 tasks, 30 agents, overlap 0.5, `--task-timeout-s 1800`, against
`tessel-coordinator-swarm`. None wrote `on` results; each folder keeps the run log, the local `off`
replay (`seed3-off.json`, labelled local) and the swarm coordinator's own event log for that repo.

- `ab-wait/` (14:55–15:11, harness `58bbbc3`, repo `swarm-s3-tmjwju`): exit 1 `claim refused
  (StaleFence)`. 36 of 40 merged before the abort. Claims 12 and 19 lapsed one lease after a grant
  from the wait queue. Cause: the scripted agent sent heartbeats only while reading the socket, and
  a late `StaleFence` reply was read as the refusal of the next claim (fixed: SWARM-LEASE).
- `ab-wait2/` (16:46–17:01, harness `d9fb232`, repo `swarm-s3-tmk1p6`): exit 1 `the coordinator
  closed the connection`. 38 of 40 merged. Claims 22 and 36 lapsed one lease after grants that
  came after 6–10 minutes in the queue; an agent's own close aborted the run (fixed: SWARM-WAIT;
  coordinator side: COORD-CLOSE-WITHDRAW).
- `ab-wait3/` (19:31–19:39, harness `43edf24`, repo `swarm-s3-tmk9c4`), with `wrangler tail` on
  the swarm Worker (`wrangler-tail.jsonl`): exit 1 `Connection reset without closing handshake`.
  16 of 40 merged. At 19:39:36–37 every WebSocket dropped at once with no close frame (17 queued
  agents withdrawn in one instant); the Durable Object kept running (one script version, all 376
  invocations ok, CPU 0–24 ms), so the reset sat between this Mac and the edge; cause undecided.
  The scripted reviewer's reset aborted the run (fixed: SWARM-RECONNECT).

No live `on` numbers exist yet. The next run uses the reconnecting harness (trunk `e7250de` or
later) and also records `cf.colo` per socket and a 5 s ping from the Mac.

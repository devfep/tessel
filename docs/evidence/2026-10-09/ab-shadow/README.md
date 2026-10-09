# Live A/B run, policy `shadow`, 9 October 2026 (09:41:54–09:54:33 EDT)

The first completed live `shadow` run at swarm size. Harness `tessel-swarm` from the trunk
`8e9ceed` (SWARM-SHADOW-RECONNECT: shadow agents reconnect), against `tessel-coordinator-swarm`
(version `dffde46a`, shadows enabled) and the steward; policy `shadow`, seed 3, 40 tasks, 30
agents, overlap 0.5, `--task-timeout-s 1800`, scratch repo `swarm-s3-tmn7dv`. Wi-Fi off, so every
connection went over the Thunderbolt Ethernet link, as in the morning's `wait` run.

- `run.log`: the harness's output with the A/B table (also `seed3-ab.md`).
- `seed3-on.json`, `seed3-on-events.json`: the `on` result and the coordinator's event log (207
  events). `seed3-off.json`: the local `off` replay, labelled local.
- `wrangler-tail.jsonl`: `wrangler tail --env swarm --format json` for the whole run (345 events;
  the `/tail-probe-*` requests at the start are the tail's own check).
- `ping-5s.log`: `ping -i 5` to the swarm Worker's hostname: 163 replies, 0 lost, max 45 ms.

Result (`seed3-ab.md`): of 40 tasks, 11 merged, 28 ran as shadow work (denied, worked in the
agent's fork, submitted for verification only) and 1 failed. The coordinator's trials of the 28
shadow claims against what blocked them: **20 verified preventions, 9 false alarms, 3
inconclusive, 0 never verified** (a shadow claim can face more than one blocker, so trials
outnumber claims). Precision of the denials that were tried: 20 of 29. A false alarm is a denial
whose work would have merged cleanly; it is counted as such, not as a save. 0 lapsed claims, 0
closed connections, 0 reconnects; 6 scripted review approvals; wall time 628 s. Shadow agents
waited 206–552 s (mean 400 s) for their trials, which run in the steward's Sandbox one by one.
`off` (local replay) landed 14 of 40 with 26 rejected after the work was done, in 16 s.

The failed task (agent `a18`, task 29): its `git push --force` to the agent's fork
`swarm-s3-tmn7dv--a18` was answered `503 Service unavailable` by the Artifacts git endpoint; the
harness does not retry a push, so the task ended `failed` after 14 s of work. Filed as
SWARM-PUSH-RETRY.

From the tail: edge requests were served at `EWR` (46) and `IAD` (20), one script version. 279
Durable Object invocations, wall time up to 30.6 s (the alarm that drives the trials), mean 3.8 s.
The 65 `/ws` upgrade events carry the outcomes `canceled` (48) and `responseStreamDisconnected`
(17), which the tail reports when a socket ends; the harness recorded no connection that closed
before its work was done.

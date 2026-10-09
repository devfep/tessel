# Live A/B run, policy `wait`, 9 October 2026 (07:25:31–07:40:41 EDT)

The first live run that completed. Harness `tessel-swarm` built from the trunk `4033791` (the
reconnecting harness, SWARM-RECONNECT), against `tessel-coordinator-swarm` (deployed version
`dffde46a`) and the steward; policy `wait`, seed 3, 40 tasks, 30 agents, overlap 0.5,
`--task-timeout-s 1800`, scratch repo `swarm-s3-tmn12j`. The Mac's Wi-Fi was off for the run,
so every connection went over the Thunderbolt Ethernet link (Oct 8 found the Wi-Fi link corrupts
long TLS uploads to Cloudflare; the Oct 7 mass reset was never explained).

- `run.log`: the harness's output as it happened, with the A/B table (also `seed3-ab.md`).
- `seed3-on.json`, `seed3-on-events.json`: the `on` result and the coordinator's event log for the
  scratch repo (274 events). `seed3-off.json`: the local `off` replay, labelled local.
- `wrangler-tail.jsonl`: `wrangler tail --env swarm --format json` for the whole run (started
  07:24, 807 events; the three `/tail-probe-*` requests at the start are the tail's own check).
- `ping-5s.log`: `ping -i 5` to the swarm Worker's hostname from the Mac: 222 replies, 0 lost,
  max 101 ms.

Result (`seed3-ab.md`): `on` landed 40 of 40 with 0 rejections, 0 lapsed claims, 0 closed
connections and 0 reconnects, 29 waits queued behind a holder, 20 scripted review approvals,
wall time 837 s (2.8 landed per minute). `off` (local replay) landed 14 of 40 with 26 rejected
after the work was done (22 textual conflicts, 1 broke the build, 3 broke the tests) in 14 s.
No shadow run in this harness, so no verified-prevention count: a wait is not a prevented conflict.

From the tail: every edge request (48, all `/ws` upgrades plus the probes) was served at `IAD`;
one script version throughout. 759 Durable Object invocations (WebSocket messages and alarms),
wall time 0–20.4 s, mean 0.95 s. The 45 `/ws` upgrade events carry the outcomes `canceled` (20)
and `responseStreamDisconnected` (25): the tail reports a WebSocket upgrade when its socket
ends, timestamped at the upgrade (07:26:43–46 for the 43 sockets of the run, 07:40:40 for the
two final log reads), and the harness recorded no connection that closed before its work was
done. Nothing in the run points at a transport fault on this link.

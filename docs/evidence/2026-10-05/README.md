# Live swarm runs, 5 October 2026

Raw output of `tessel-swarm run --target live` against the deployed swarm coordinator
(`tessel-coordinator-swarm`) and the deployed steward. Each folder holds the A/B table
(`seed1-ab.md`), both sides' JSON, and the coordinator's own event log (`seed1-on-events.json`),
from which every `on` number is computed. `off` is a local plain-git replay and is labelled so.

- `swarm-seed1-wait/`: 16:03 EDT, policy `wait`, 10 tasks, 6 agents, overlap 0.5, task timeout
  120 s, swarm Worker `39c3a7de`. `on` landed 9 of 10; task 6 timed out after 120 s waiting for a
  grant (merges through the live steward take tens of seconds each). Shadows were off.
- `swarm-seed1-shadow/`: 20:49 EDT, policy `shadow`, 6 tasks, 3 agents, overlap 0.5, task timeout
  300 s, swarm Worker `f6d0e4b1` (`SHADOW_ENABLED = "true"`). 2 denied tasks ran as shadow work;
  both were verified by steward trials against the work that blocked them: shadow claim 3 vs
  claim 2 `tests_failed` (clean before claim 2 merged), shadow claim 6 vs claim 5
  `textual_conflict` (events 30 and 31). 2 verified preventions, 0 false alarms, 0 inconclusive,
  0 never verified. Small samples: these runs show the mechanism works live, not a rate.

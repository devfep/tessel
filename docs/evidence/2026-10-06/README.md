# Live swarm runs, 6 October 2026

Raw output of `tessel-swarm run --target live` against the deployed swarm coordinator
(`tessel-coordinator-swarm`) and the deployed steward. Same layout as `../2026-10-05/`. `off` is a
local plain-git replay and is labelled so.

- `swarm-seed2-shadow/`: 19:45 EDT, policy `shadow`, seed 2, 10 tasks, 4 agents, overlap 0.5,
  task timeout 300 s, scratch repo `swarm-s2-tmifca`, swarm Worker `4e65697d` (trunk `7d00ad6`,
  `SHADOW_ENABLED = "true"`), harness built from the trunk `7d00ad6`. `on` landed 7 of 10 with 0
  rejections; 3 denied tasks ran as shadow work and all 3 were verified by steward trials against
  the work that blocked them: shadow claim 5 vs claim 4 `textual_conflict` (event 39), shadow
  claim 7 vs claim 3 `textual_conflict` (event 47), shadow claim 8 vs claim 6 `tests_failed`
  (event 49). Each verification is followed by `claim_released` with reason `settled` (events 40,
  48, 50): the first live `Settled` releases. 3 verified preventions, 0 false alarms, 0
  inconclusive, 0 never verified. Wall time 211 s.

Together with the 5 October shadow run: 5 verified preventions and 0 false alarms over 5 shadow
claims in two runs. Small samples: these runs show the mechanism works live, not a rate.

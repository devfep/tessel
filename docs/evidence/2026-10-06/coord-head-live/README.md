# COORD-HEAD live check, 6 October 2026, 21:58 EDT

Production coordinator `8d66fe32` and steward `31b4be89`, both deployed from trunk `3c50a5e`.
`check.sh` is the script that was run (its last `jq` filter was corrected afterwards; the log
below was read separately). It prints no token.

| Check | Result |
|---|---|
| `POST /repo/demo/trunk-moved` with no token | 401 |
| same, identity token of a non-steward agent | 403 |
| `GET` with the steward's token | 405 |
| `POST` for a repo the coordinator has never seen (`swarm-never-created-zz`) | 204 |
| `demo`: `welcome` head before the poke | `0000000000000000000000000000000000000001` (stale) |
| `demo`: trunk `main` (read through a steward read token) | `58119c247ed0e3a47bfd797d2b4228377805ff43` |
| `demo`: steward poke | 202 |
| `demo`: `welcome` head 5 s later | `58119c247ed0e3a47bfd797d2b4228377805ff43` |

`demo-events.jsonl` is the coordinator's log for `demo` read with `watch`: event 4 is
`base_moved` to `58119c2` by `steward` with `notified: []`.

Not exercised live: a poke arriving during a read, a failed `/head` read, and a poke sent by the
steward's queue consumer itself (the poke above was sent by hand with a steward identity token
minted through the admin route). The queue path is covered by the steward's tests; the next push
to a known trunk's main exercises it.

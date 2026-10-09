# Fresh-clone test, 2026-10-09 (PLAN §9, Oct 11 item, run early)

Clone of GitHub `artifacts-trunk` at `c6d6c76` (the pull request 2 code tree; `8c958b3` adds
docs only) into an empty directory, then what the README and CLAUDE.md say, throttled to two
cargo jobs because other sessions kept the Mac at load 60–113. Script: the session scratchpad
`orch/fresh-clone.sh`; raw output of the first run in `run1.log`.

| Step | Result |
|---|---|
| `git clone --branch artifacts-trunk` | ok, `c6d6c76` |
| toolchain | rustc 1.98.0, cargo 1.98.0, node 22.23.2, pnpm 12.10.1 |
| `cargo test --workspace` (run 1, 16:50–16:57, load 65–113) | exit 101: 1 failed, 1065 passed |
| `cargo deny check` | advisories, bans, licenses, sources ok |
| `pnpm install --frozen-lockfile` (tessel-steward) | ok |
| `pnpm test` | 803 passed, 42 files |
| `pnpm typecheck` | ok |
| `cargo test --workspace --no-fail-fast` (run 2, 16:59–17:03, load 63–97) | exit 101: the same 1 failed, 1065 passed (`run2-tests-summary.txt`) |
| that test alone (`cargo test -p tessel-swarm --test reconnect agents_that_cannot_reconnect`) | ok, 3.5 s |

The one failure, both runs: `tessel-swarm/tests/reconnect.rs:334`
`agents_that_cannot_reconnect_end_as_disconnected_after_their_tries`, panicking at
`reconnect.rs:116` with `cannot connect to the coordinator: HTTP error: 401 Unauthorized`. The
test arms its connection cut on the first `ClaimGranted` and refuses both agents from then on;
on a loaded machine the second agent's first connection arrives after that grant, is refused,
and the harness fails the run. The same tree passed the steward's Sandbox gate. Filed as
SWARM-RECONNECT-FIRST-CONNECT in `docs/ROADMAP.md`; a test-only fix is in progress.

# Dogfood

One `tessel-server` on your Mac coordinates `devfep/tessel`. Two lanes and the orchestrator talk to
it, and its lander pushes `main` on GitHub. The page sets up the server directory, the three
tokens and the two lane forks; [Local setup](local-setup.md) is the same walk against a bare
remote. `<server dir>` is `~/tessel-dogfood`, written out as an absolute path.

Four settings are proposals until you confirm them. D1: the lander pushes `main` over HTTPS with a
token read from a 0600 file. D2: each lane has its own private fork. D3: the tests run on this
Mac. D4: the tokens are per lane, `orchestrator` is the only reviewer, and `sensitive` holds
`src/protocol.rs`, `src/host/`, `.github/` and `docs/BUILD-PROTOCOL.md`.

## What runs where

The runner runs the trunk's `tessel.toml` test command, `cargo test --workspace`, on the rebased
commit in a fresh clone, as you. Clippy, fmt, the named-tests check, actionlint, zizmor and the
steward's tests stay on CI, which runs on `main` after a landing. The runner is not sandboxed until
M5: land only code whose tests you trust. A submission that touches `tessel.toml` is refused; that
file changes through a pull request you merge.

## Build and GitHub, by hand

Build from a checkout of `main` and put `target/debug` on your `PATH`. Then make the two lane
forks. They are plain private repositories: a lane pushes to its own, and the lander fetches its
`main`.

```
cargo build -p tessel-server -p tessel-cli
export PATH="$PWD/target/debug:$PATH"
gh repo create devfep/tessel-lane-a --private
gh repo create devfep/tessel-lane-b --private
```

Run the script. It makes the server directory (`~/tessel-dogfood`, absolute and outside any
checkout), both configurations, the signing key `2026-10` unless it exists, and a token for
`lane-a`, `lane-b` and `orchestrator` in `<server dir>/tokens`, each valid 7 days. Run it again
to refresh the tokens or the configurations. It never calls `gh` and never starts the server.

```
scripts/dogfood/setup.sh <server dir>
```

Make the GitHub token on GitHub: Settings, Developer settings, Personal access tokens,
Fine-grained tokens, Generate new token. Resource owner `devfep`; Only select repositories:
`devfep/tessel`, `devfep/tessel-lane-a` and `devfep/tessel-lane-b`; Repository permissions:
Contents read and write, Metadata read. Copy it, then save it without a shell history or log
seeing it, and run the script again to see its last line say the file is in place:

```
(umask 077 && pbpaste >"<server dir>/github-token")
```

Ruleset step 4 is yours, after D1 and before the first landing. It adds the repository admin role
(id 5) as a bypass actor of ruleset 25345043, "main: CI required", so the lander's compare-and-swap
push of `main` passes. The role can then push `main` directly, skipping the pull request and the
required checks, with the token above or by hand. M6 removes it. The request shape is in GitHub's
REST reference, "Update a repository ruleset".

```
gh api repos/devfep/tessel/rulesets/25345043 \
  | jq '{name, target, enforcement, conditions, rules,
         bypass_actors: (.bypass_actors + [{actor_id: 5, actor_type: "RepositoryRole",
                                            bypass_mode: "always"}])}' \
  | gh api -X PUT repos/devfep/tessel/rulesets/25345043 --input -
```

## The server

Start it in the background. It listens on `127.0.0.1:7878` and logs to `server.log`. What it
keeps in `<server dir>/stores` survives a stop: claims and held submissions. The next
start finds a merge that was in flight and retries it.

```
nohup tessel-server serve --config <server dir>/tessel-server.toml \
  >>"<server dir>/server.log" 2>&1 </dev/null & echo $! >"<server dir>/server.pid"
kill "$(cat "<server dir>/server.pid")"
```

The second command stops it; run the first again to restart. Tokens last 7 days: after that, run
the script again and `tessel init` again in each worktree.

## A lane

In the lane's worktree, `init` keeps the token in `.tessel/config.toml` (mode 0600) and never
prints it; use `lane-b` for the other lane. Each task starts from a new `main`, so a lane pushes
`HEAD:main` to its own fork with `--force` before `tessel submit`.

```
TESSEL_TOKEN=$(cat <server dir>/tokens/lane-a) \
  tessel init --coordinator ws://127.0.0.1:7878/tenant/devfep --repo tessel --agent lane-a
git push --force https://github.com/devfep/tessel-lane-a.git HEAD:main
```

## The orchestrator

The orchestrator is the reviewer. In a checkout of its own, `init` the same way with
`tokens/orchestrator` and `--agent orchestrator`, then decide a held submission by the id
`tessel submit` printed:

```
tessel review <id> --approve --note "looks right"
tessel review <id> --reject --note "why"
```

## A landing

An approval, or a submission that touches no `sensitive` path, starts a landing: rebase, claim
coverage, the runner's tests, a compare-and-swap push of `main`, a read-back, then `merged` in
the lane's inbox. The outcomes and what the agent sees are in
[Landing](configuration.md#landing).

## D3, the load on the Mac

A cold `cargo test --workspace --locked --offline --no-fail-fast` in a fresh clone took 237 s at
a load of 9, with 1888 tests, on 22 October. The runner's `timeout_secs` is 720. A landing's call
deadline is 13 minutes, and the tests get what is left of it less 5 seconds, so the lander
decides a slow run first. Open, D3: accept this, or set `-j 2` and two test threads in the
trunk's `tessel.toml`.

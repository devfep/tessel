# Dogfood

One `tessel-server` on your Mac coordinates `devfep/tessel`. Two lanes and the orchestrator talk to
it, and its lander pushes `main` on GitHub. The page sets up the server directory, the three
tokens and the three forks; [Local setup](local-setup.md) is the same walk against a bare
remote. `<server dir>` is `~/tessel-dogfood` as an absolute path, outside any checkout.

Four settings are proposals until you confirm them. D1: the lander pushes `main` over HTTPS with a
token read from a 0600 file. D2: each lane has its own private fork. D3: the tests run on this
Mac. D4: the tokens are per lane, `orchestrator` is the only reviewer, and `sensitive` holds
`src/protocol.rs`, `src/host/`, `.github/` and `docs/BUILD-PROTOCOL.md`.

## What runs where

The runner runs the trunk's `tessel.toml` test command, `cargo test --workspace`, on the rebased
commit in a fresh clone, as you. Clippy, fmt, the named-tests check and the steward's tests stay
on CI, which runs on `main` after a landing. Each lane runs actionlint and zizmor in its gate
before it submits. The runner is not sandboxed until M5: land only code whose tests you trust. Two
things still go through a pull request you merge: a change to `tessel.toml`, which a submission may
not touch, and a change to this page or `scripts/dogfood/`, the server's own set-up.

## Build and GitHub, by hand

Build from a checkout of `main` and put `target/debug` on your `PATH`. Then make the three
private forks: an agent pushes to its own, and the lander fetches its `main`.

```
cargo build -p tessel-server -p tessel-cli
export PATH="$PWD/target/debug:$PATH"
gh repo create devfep/tessel-lane-a --private
gh repo create devfep/tessel-lane-b --private
gh repo create devfep/tessel-orchestrator --private
```

Run the script. It makes the server directory, both configurations, the signing key `2026-10`
unless it exists, and a token for `lane-a`, `lane-b` and `orchestrator` in `<server dir>/tokens`,
each valid 7 days. It never calls `gh` and never starts the server.

```
scripts/dogfood/setup.sh <server dir>
```

Make the GitHub token on GitHub: Settings, Developer settings, Personal access tokens,
Fine-grained tokens, Generate new token. Resource owner `devfep`; Only select repositories:
`devfep/tessel`, `devfep/tessel-lane-a`, `devfep/tessel-lane-b` and
`devfep/tessel-orchestrator`; Repository permissions: Contents read and write, Metadata read.
Copy it, then save it without a shell history or log seeing it, and run the script again to see
its last line say the file is in place:

```
(umask 077 && pbpaste >"<server dir>/github-token")
```

Ruleset step 4 is yours, after D1 and before the first landing. It adds the repository admin role
(id 5) as a bypass actor of ruleset 25345043, "main: CI required", so the lander's compare-and-swap
push of `main` passes. The role can then push `main` directly, force-push it or delete it,
skipping the pull request and the required checks, with the token above or by hand. M6 removes it.
The request shape is in GitHub's REST reference, "Update a repository ruleset".

```
gh api repos/devfep/tessel/rulesets/25345043 \
  | jq '{name, target, enforcement, conditions, rules,
         bypass_actors: (.bypass_actors + [{actor_id: 5, actor_type: "RepositoryRole",
                                            bypass_mode: "always"}])}' \
  | gh api -X PUT repos/devfep/tessel/rulesets/25345043 --input -
```

## The server

Start it in the background; it listens on `127.0.0.1:7878` and logs to `server.log`. Claims and held
submissions in `<server dir>/stores` survive a stop, and a restart retries a merge in flight.

```
nohup tessel-server serve --config <server dir>/tessel-server.toml \
  >>"<server dir>/server.log" 2>&1 </dev/null & echo $! >"<server dir>/server.pid"
kill "$(cat "<server dir>/server.pid")"
```

The second command stops it; run the first again to restart. Tokens last 7 days. After that, run
the script again; in each worktree, `trash .tessel/config.toml` and `tessel init` again, because
`init` keeps the token already in the file.

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

The orchestrator is the reviewer. In a checkout of its own, `init` as for a lane with
`tokens/orchestrator` and `--agent orchestrator`; its commits land through the server too, pushed
to `tessel-orchestrator`. Decide a held submission by the id `tessel submit` printed:

```
tessel review <id> --approve --note "looks right"
tessel review <id> --reject --note "why"
```

## A landing

An approval, or a submission the coordinator does not hold for review, starts a landing: rebase,
claim coverage, the runner's tests, a compare-and-swap push of `main`, a read-back, then `merged`
in the lane's inbox. [What holds a submission](configuration.md#what-holds-a-submission-for-review)
and the outcomes are in [Landing](configuration.md#landing).

## D3, the load on the Mac

A cold `cargo test --workspace --locked --offline --no-fail-fast` took 237 s on 22 October, in a
fresh worktree, not a fresh clone. The load was 9 at the start and rose to 75 during the run. The
runner's `timeout_secs` is 720. The lander asks the runner for what is left of its 13-minute
deadline less 5 s, about 775 s when the clone, fetch and rebase are quick, so 720 cuts first unless
they take more than about 55 s. Open, D3: the gate's parser refuses `-j 2`, so limiting the
load needs a change, a runner setting or a flag it accepts. Until then D3 is accept or not.

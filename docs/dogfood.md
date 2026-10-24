# Dogfood

One `tessel-server` on your Mac coordinates `devfep/tessel`. Two lanes and the orchestrator talk to
it, and its lander pushes `main` on GitHub. The page sets up the server directory, the three
tokens and the three forks; [Local setup](local-setup.md) is the same walk against a bare
remote. `<server dir>` is `~/tessel-dogfood` as an absolute path, outside any checkout.

Four settings are proposals until you confirm them. D1: the lander pushes `main` over HTTPS with a
token read from a 0600 file. D2: each lane has its own private fork. D3: the tests run on this
Mac, with landing builds capped at 6 jobs (accepted). D4: the tokens are per lane, `orchestrator`
is the only reviewer, and `sensitive` holds the paths the script writes (the protocol, the host,
the landing and runner code, the workflows, `scripts/`, the build and agent configuration, and the
process documents). D4 is a process control, not a security boundary: any process running as you
can read every token and the signing key until M5 runs the server as its own macOS user. The
orchestrator cannot decide its own held submissions, so its changes to sensitive paths go by pull
request.

## What runs where

The runner runs the trunk's `tessel.toml` test command, `cargo test --workspace`, on the rebased
commit in a fresh clone, as you. Clippy, fmt, the named-tests check, actionlint, zizmor and the
steward's tests stay on CI, which runs on `main` after a landing. Each lane also runs actionlint
and zizmor in its gate before it submits. The runner is not sandboxed until M5: land only code
whose tests you trust. Its tests can read `<server dir>` (the tokens, the key and `github-token`)
and run `gh` as you. The lanes' gates already run the same code as you; M5 separates the user. Two
things still go through a pull request you merge: a change to `tessel.toml`, which a submission may
not touch, and a change to this page or `scripts/dogfood/`, the server's own set-up.

## Build and GitHub, by hand

Build from a checkout of `main` and put `target/debug` on your `PATH`. Then make the three
private forks: an agent pushes to its own, and the lander fetches every branch.

```
cargo build -p tessel-server -p tessel-cli
export PATH="$PWD/target/debug:$PATH"
gh repo create devfep/tessel-lane-a --private
gh repo create devfep/tessel-lane-b --private
gh repo create devfep/tessel-orchestrator --private
for fork in lane-a lane-b orchestrator; do
  gh api -X PUT "repos/devfep/tessel-$fork/actions/permissions" -F enabled=false
done
```

The second step turns Actions off on each fork: a plain repository runs `ci.yml` on every push and
`nightly.yml` on its cron, billed as private minutes.

Run the script. It makes the server directory, both configurations, the signing key `2026-10`
unless it exists, and a token for `lane-a`, `lane-b` and `orchestrator` in `<server dir>/tokens`,
each valid 7 days. It copies `tessel-server` and `tessel` into `<server dir>/bin` on every run,
because git's askpass hook is the server's own executable and a later `cargo build` must not
replace it mid-landing. It writes `<server dir>/.cargo/config.toml` with `jobs = 6`, which cargo
reads from the runner's clone upward, so a landing build uses 6 of the 12 cores. The cap limits
compile jobs, not test threads. A `.cargo/config.toml` inside a submission would take precedence
over it, but that file lands only as a held change, because `.cargo/` is sensitive. The `tessel`
copy is the one every worktree runs `init` from, so its git hooks point at a binary that stays.
It never calls `gh` and never starts the server.

```
scripts/dogfood/setup.sh <server dir>
```

Make the GitHub token on GitHub: Settings, Developer settings, Personal access tokens,
Fine-grained tokens, Generate new token. Resource owner `devfep`; Only select repositories:
`devfep/tessel`, `devfep/tessel-lane-a`, `devfep/tessel-lane-b` and
`devfep/tessel-orchestrator`; Repository permissions: Contents read and write, Metadata read,
nothing else. Never Workflows or Administration: without Workflows, GitHub refuses a push that
changes `.github/workflows`, a backstop. Expiry 30 days; record its date in `STATE`. Clear the
clipboard first, save the token without a shell history or log seeing it, clear it again, and run
the script again to see its last line say the file is in place:

```
pbcopy </dev/null
# copy the token on GitHub, then:
(umask 077 && pbpaste >"<server dir>/github-token")
pbcopy </dev/null
```

Ruleset step 4 is yours, after D1 and before the first landing. It splits the protection of `main`
so the lander's compare-and-swap push passes without letting anyone rewrite history. Create A before
changing B, so `main` is never without those two rules. Use GitHub's REST reference, "Create a
repository ruleset" and "Update a repository ruleset", for the request shape.

A is a new ruleset, "main: no rewrite", on the default branch with `deletion` and
`non_fast_forward` and no bypass actors:

```
gh api -X POST repos/devfep/tessel/rulesets --input - <<'JSON'
{"name": "main: no rewrite", "target": "branch", "enforcement": "active",
 "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
 "rules": [{"type": "deletion"}, {"type": "non_fast_forward"}], "bypass_actors": []}
JSON
```

B is ruleset 25345043, "main: CI required". It keeps `pull_request` and `required_status_checks`,
loses `deletion` and `non_fast_forward`, and gains the repository admin role (id 5) as a bypass
actor:

```
gh api repos/devfep/tessel/rulesets/25345043 \
  | jq '{name, target, enforcement, conditions,
         rules: [.rules[] | select(.type != "deletion" and .type != "non_fast_forward")],
         bypass_actors: (.bypass_actors
           | if any(.[]; .actor_id == 5 and .actor_type == "RepositoryRole") then .
             else . + [{actor_id: 5, actor_type: "RepositoryRole", bypass_mode: "always"}] end)}' \
  | gh api -X PUT repos/devfep/tessel/rulesets/25345043 --input -
gh api repos/devfep/tessel/rulesets/25345043 --jq .current_user_can_bypass
```

The last command must print `always`. The bypass still lets any credential acting as you push
`main` without a pull request or the checks; it cannot delete or rewrite `main`, which A holds.
M6 removes the bypass. A pre-push hook in the checkout's shared hooks directory refuses pushing
`main` of `devfep/tessel` from any worktree; `tessel init` keeps it as `pre-push.pre-tessel`.

## The server

Start it from the copy in `<server dir>/bin`, not from `target/debug`. It listens on
`127.0.0.1:7878` and logs to `server.log`. Claims and held submissions in `<server dir>/stores`
survive a stop, and a restart retries a merge in flight.

```
nohup <server dir>/bin/tessel-server serve --config <server dir>/tessel-server.toml \
  >>"<server dir>/server.log" 2>&1 </dev/null & echo $! >"<server dir>/server.pid"
kill "$(cat "<server dir>/server.pid")"
```

The second command stops it; run the first again to restart. Tokens last 7 days. After that, run the
script again; in each worktree, `trash .tessel/config.toml` and `<server dir>/bin/tessel init`
again, because `init` keeps the token already in the file.

## A lane

In the lane's worktree, `init` keeps the token in `.tessel/config.toml` (mode 0600) and never
prints it; use `lane-b` for the other lane. Run `<server dir>/bin/tessel`, here and afterwards:
`init` installs git hooks that point at the running binary's path, and a `target/debug` inside a
worktree disappears with it. A lane pushes each task's commit to a new branch of its own fork,
without force, before `tessel submit`, and never rewrites a branch it pushed; after an amend or a
rebase it pushes a new name.

```
TESSEL_TOKEN=$(cat <server dir>/tokens/lane-a) \
  <server dir>/bin/tessel init --coordinator ws://127.0.0.1:7878/tenant/devfep \
  --repo tessel --agent lane-a
git push https://github.com/devfep/tessel-lane-a.git HEAD:refs/heads/tessel/<claim>-1
```

## The orchestrator

The orchestrator is the reviewer. In a checkout of its own, run `<server dir>/bin/tessel init` as
for a lane with `tokens/orchestrator` and `--agent orchestrator`; its commits land through the
server too, pushed to `tessel-orchestrator`. Decide a held submission by the id `tessel submit`
printed:

```
tessel review <id> --approve --note "looks right"
tessel review <id> --reject --note "why"
```

## Rules while landings run

- Merge no pull request on `devfep/tessel` while a landing is in flight. The fifth time `main` has
  moved under a submission rejects it.
- Tail `server.log` during a landing. At the first timeout or push warning, stop the server, but
  only when no test is running: a stop mid-test orphans the cargo process group. A timeout or a
  failed push is retried up to 3 times, each a cold build.
- Run one lane gate at most while a landing runs.
- A lane that changes `Cargo.lock` runs `cargo fetch` before it submits, because the runner tests
  with `--offline`.

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
they take more than about 55 s. The gate's parser refuses `-j 2`, so the cap lives in
`<server dir>/.cargo/config.toml` (`jobs = 6`) instead. D3 is accepted with that cap.

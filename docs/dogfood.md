# Dogfood

One `tessel-server` on your Mac coordinates `devfep/tessel`. Two lanes and the orchestrator talk to
it, and its lander pushes `main` on GitHub. The page sets up the server directory, the three
tokens and the three forks; [Local setup](local-setup.md) is the same walk against a bare
remote. `<server dir>` is `~/tessel-dogfood` as an absolute path, outside any checkout.

Four settings are proposals until you confirm them. D1: the lander pushes `main` over HTTPS with a
token read from a 0600 file. D2: each lane has its own private fork. D3: the tests run on this Mac,
with landing builds capped at 6 jobs (accepted). D4: the tokens are per lane, `orchestrator` is the
only reviewer, and `sensitive` holds the paths the script writes (the protocol, the host, the
core's merge path, the landing and runner code, the workflows, `scripts/`, the build and agent
configuration, and the process documents). Until the move in [Moving to the service
accounts](#moving-to-the-service-accounts), D4 is a process control, not a security boundary: any
process running as you can read every token and the signing key. After the move the claim is
narrower, and it is this: the token, the key and the store are unreadable to repo code under test.
It is not that the lanes cannot reach them. The lanes run as you, and every sudo step of the move
runs code built, checked out or fetched in your account, so a lane that can write a file that build
reads can get code running as `_tessel` at the next upgrade. What remains is that residual (binaries
are built in your account; CI-built binaries checked by sha256 are the fix, in M6), a test can send
the source of an unlanded private-fork submission out over the network, a service on this Mac that
trusts local connections is open to the runner account (a Homebrew postgres allows its superuser on
loopback TCP and on `/tmp/.s.PGSQL.5432` without a password, and a superuser there can run a command
as you), and you hold three assumptions: sudo has no Touch ID, the steps are run in a shell no lane
controls, and you never type the password into a prompt a lane raised. The new GitHub token is on
the clipboard between the copy and the paste of step 6, where any process of yours, a lane included,
can read it; paste it at once and clear the clipboard. Keeping `main` safe from the lanes is not in
this scope: they push with `gh`'s credential and you are the bypass actor of ruleset B. The
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

## Upgrading the server

An upgrade replaces the binaries in `<server dir>/bin` and keeps the store. Every upgrade follows
these rules; the staged landing (M5) is the first that needs them. Running `setup.sh` again for new
tokens copies the `tessel-server` and `tessel` on your `PATH` into `<server dir>/bin`, so run it
only with binaries built from a checkout of the commit the server already runs; to change the
binaries, upgrade as below first.

- **When.** No upgrade to the staged landing until P1-SERVER-STAGED, P1-CORE-STAGED-LANDING,
  P1-CORE-RECONCILE and RESOLVE-NAMES-HEAD have all landed. P1-CORE-RECONCILE carries a fixture
  test, named in full on the next line, which checks that a store written before the staged landing
  with a merge in flight is decoded by the new core as a marker with no stage (`unstaged`), not as
  a `Prepare`, and that the claim is in doubt from the main that marker recorded:

  ```
  a_marker_stored_before_the_staged_landing_leaves_the_claim_in_doubt_from_its_main
  ```

- **Nothing in flight.** Upgrade only with no dispatch in flight; held submissions are fine. A
  stop mid-test orphans the cargo process group, and an old whole-merge call may already have
  pushed, which the new core reads as a merge in doubt, not one cut off before `Land`. First read
  what the store says, for each repository. Read it as in step 1 of
  [A `Land` in doubt](#a-land-in-doubt): `mode=rw`, never `-readonly` (it fails on a store with no
  `-shm`) and never as root. The read changes nothing, so the server may run. Until the move:

  ```
  /usr/bin/sqlite3 -init /dev/null -cmd ".timeout 5000" \
    "file:<server dir>/stores/<tenant>/<repo>.sqlite?mode=rw" \
    "select json_type(json, '\$.merge_in_flight'), json_type(json, '\$.verification_in_flight') \
     from kv where key = 'core'"
  ```

  After the move to the service accounts the stores are `/var/tessel/stores`, readable only by
  `_tessel`:

  ```
  /usr/bin/sudo -u _tessel /usr/bin/sqlite3 -init /dev/null -cmd ".timeout 5000" \
    "file:/var/tessel/stores/<tenant>/<repo>.sqlite?mode=rw" \
    "select json_type(json, '\$.merge_in_flight'), json_type(json, '\$.verification_in_flight') \
     from kv where key = 'core'"
  ```

  It prints `null|null` when no merge and no verification is in flight; `object` in the first
  column is a merge, a `Land` in doubt included. A merge can also be in flight with no clone on
  disk (between the calls of a staged merge, or in the backoff after a timeout, while an
  abandoned push may still be running), so check the outcomes too: every
  submission that is not held has `merged`, `submit_rejected` or `uncovered` in its lane's inbox
  (or, for a claim settled with `admin resolve --landed` while the server was stopped, the note
  `claim N is gone from the coordinator; forgot it`, which counts as an outcome: that command
  records the landing in the repository's event log, and the lane finds out from that log when it
  reconnects), the lanes are idle, `server.log` has no timeout or push warning after the latest of
  those outcomes, and `<server dir>/lander/devfep/tessel` holds no `call-*` directory.

  The build running now marks a claim `in_doubt` on any infrastructure failure of its one call, and
  clears it only when the work lands or is rejected. That claim has no merge in flight between its
  dispatches, so the query above passes it. Read the claims with the same command (the
  service-account form is the one above with `sudo -u _tessel` and the path
  `/var/tessel/stores`):

  ```
  /usr/bin/sqlite3 -init /dev/null -cmd ".timeout 5000" \
    "file:<server dir>/stores/<tenant>/<repo>.sqlite?mode=rw" \
    "select c.key from kv, json_each(kv.json, '\$.claims') as c \
     where kv.key = 'core' and json_extract(c.value, '\$.work.in_doubt') = 1"
  ```

  It prints nothing when no claim is in doubt. Wait until it does: each printed claim is one the
  running build may have landed, and the new core cannot resolve it (it has no `Land` to ask about).
- **The build.** Build only from a landed `main` whose CI is green, in a checkout of that commit,
  and copy `tessel-server` and `tessel` with `install -m 755` into `<server dir>/bin.next`. Leave
  `<server dir>/bin` alone until the swap: git's askpass hook is the running server's own
  executable.
- **A trial first.** Before the swap, the binaries in `bin.next` land one commit on
  `tessel-sandbox/m4-trunk` over HTTPS through the askpass hook. The trial server needs a
  directory, a configuration and a port of its own, not `setup.sh`'s, which writes this page's
  remote and port, and a GitHub token that can push `tessel-sandbox/m4-trunk`, which the dogfood
  token cannot. If the trial fails, stop here; the running server is untouched. Stop the trial
  server before the swap: its askpass hook is `bin.next/tessel-server`, which the swap renames.
- **The swap.** From inside `<server dir>`: stop the server (the commands are in [The
  server](#the-server)) and wait until its pid has exited, because the store lock refuses a second
  server; then `trash bin.prev` if it exists, `mv bin bin.prev`, `mv bin.next bin`, start it, and
  wait for a new `listening` line in `server.log`. A fix that cannot land through the server goes
  through a pull request you merge.
- **Rollback.** Only with an idle queue: nothing queued, in flight or held. A build refuses a
  store whose `state_version` is newer than it knows (`tessel-server/src/store.rs`). If the new
  build kept the version the old one knows, rollback is the swap the other way: stop, wait for the
  pid, `mv bin bin.bad`, `mv bin.prev bin`, start. It worked when the new `listening` line has no
  store or coordinator error after it: `state_version` covers only the store's shape, and a record
  the new build wrote can still fail to decode in the old one. If the new build raised the
  version, the old build cannot open the store, and rollback means a fresh store and new claims.
  Never put a copy of an older store back: a restored store can reissue fences.
  Rolling back past the commit that adds `doubted_land` to a submission (CORE-LATE-LAND) needs no
  claim in doubt of a `Land`: the old build sends a waiting doubted `Prepare` at once, rejects the
  work under the old rules (the late `Land` can still put it on main), and drops the pair on its
  next write, so a later roll forward finds a doubt with no pair. Check the stored state first: the
  claim query under "Nothing in flight", with `'\$.work.doubted_land' is not null` in place of
  `'\$.work.in_doubt' = 1`, prints nothing.
  Rolling back past the commit that adds `trials_await_head` to a race (CORE-HEAD-ADOPTED) needs
  no race in `Judging` with that flag set: the old build drops the flag and leaves the race
  without trials for good. Check the stored state first.

After the [move to the service accounts](#moving-to-the-service-accounts), every rule above stands;
only the swap changes. `/usr/bin/sudo /opt/tessel/sbin/upgrade.sh --binaries <dir>` does it in
place of the commands above, with the built binaries (`tessel-runner` too) in `<dir>` instead of
`bin.next`. The rollback is
`/usr/bin/sudo /opt/tessel/sbin/upgrade.sh --binaries /opt/tessel/bin.prev`, which works because
the script copies the binaries out before it moves `/opt/tessel/bin`.

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

## Moving to the service accounts

The design is [own-users.md](proposals/own-users.md): `_tessel` (481) runs the server and alone
reads the token, key and store; `_tessel-runner` (482) runs repo code. The scripts are in
`scripts/dogfood/`. Until the move is done the sections above describe how the server runs today,
and `setup.sh <server dir>` is how it is set up. The move replaces both; its last step deletes
`setup.sh`.

The order matters. A failing or skipped probe stops it before the store, key and token move.
Steps 0 to 2 work today. Steps 3 and later wait for RUNNER-OWN-UID's server and runner code (the
`SudoRunner`, and a runner that accepts a root-owned configuration, which `install.sh` does not
write until then): the server in `main` before that runs the runner in process and reads a runner
configuration it owns, so it cannot use this layout.

Every command below that runs under sudo names its program by absolute path, and no script runs
under sudo from a checkout. `sudo` on this Mac has no `secure_path`, so a bare `install` or `mv`
would be found through your `PATH`, which lanes can write (`/opt/homebrew/bin`).

0. First run `scripts/dogfood/own-users.test.sh` in your checkout. It uses fake tools, needs no
   root, and checks the scripts' argument handling and dry runs; a failure stops here. (CI should
   run it too, which is a later workflow pull request.) Then install the scripts from the commit
   that merged them, `$sha`. Use Apple's git, with replace refs off, so that what you read is what
   is installed: `/usr/bin/git --no-replace-objects show "$sha:scripts/dogfood/<name>.sh"`. Read
   each one first, for `accounts`, `install`, `upgrade`, `fetch`, `chown-tree`, `tokens` and
   `check-own-users`, and `toolchain.sha256`. Then:

   ```
   /usr/bin/sudo /usr/bin/install -d -o root -g wheel -m 0755 /opt/tessel /opt/tessel/sbin
   for name in accounts install upgrade fetch chown-tree tokens check-own-users; do
     /usr/bin/git --no-replace-objects show "$sha:scripts/dogfood/$name.sh" |
       /usr/bin/sudo /usr/bin/tee "/opt/tessel/sbin/$name.sh" >/dev/null
   done
   /usr/bin/git --no-replace-objects show "$sha:scripts/dogfood/toolchain.sha256" |
     /usr/bin/sudo /usr/bin/tee /opt/tessel/sbin/toolchain.sha256 >/dev/null
   /usr/bin/sudo /usr/sbin/chown root:wheel /opt/tessel/sbin/*
   /usr/bin/sudo /bin/chmod 0755 /opt/tessel/sbin/*.sh
   /usr/bin/sudo /bin/chmod 0644 /opt/tessel/sbin/toolchain.sha256
   ```

   Each script refuses to make a change unless it is the copy in `/opt/tessel/sbin`. Then read the
   installed copies, `/bin/cat /opt/tessel/sbin/<name>.sh`, before the first run: a lane cannot
   write them, so this is the text that runs as root. A script that changes is installed again the
   same way, from a new `$sha`, and read again.
1. `/usr/bin/sudo /opt/tessel/sbin/accounts.sh --dry-run`, read the plan, then run it without the
   flag. It aborts if 481 or 482 is taken as a user or a group, sets every attribute of an account
   each time, and adds `_tessel-runner` to `/var/at/cron.deny` and `/var/at/at.deny`.
2. `/usr/bin/sudo -v`, then `/opt/tessel/sbin/check-own-users.sh --accounts`. It reads the accounts
   (numbers, shell, home, password, groups: neither may be in `staff` or `admin`, or any group)
   and the mode of your home, and tries as `_tessel-runner` to read `~/repos`, `~/.ssh` and
   `~/.config/gh`, to connect to every loopback port something listens on, and to write every unix
   socket that you own and anyone may write. Exit 0: all passed. Exit 1: a check failed. Exit 2:
   nothing failed but a probe was skipped (for example without `sudo -v`). Anything but 0 stops
   here. A service that trusts local connections is open to the runner, so a failure there is
   yours to clear: stop the service during landings, require a password, or block the runner from
   it. The check shows the postgres case on this Mac, where `local all all trust` lets the runner
   reach a superuser through `/tmp/.s.PGSQL.5432`; a `pf` rule does not close a unix socket.
   Required, for each loopback port the check reports, is a rule such as
   `block out quick on lo0 proto tcp to port 5432 user _tessel-runner`, loaded with
   `/usr/bin/sudo /sbin/pfctl -a tessel -f /etc/pf.anchors/tessel` and enabled with
   `/usr/bin/sudo /sbin/pfctl -e` (an `anchor "tessel"` line in `/etc/pf.conf` keeps it across a
   reboot). Whether `pf` matches the user on `lo0` is not verified; the check's connect probe is
   what shows it, and it runs again.
3. After RUNNER-OWN-UID's code has landed, build the binaries from a landed `main` with green CI
   (`cargo build --release -p tessel-server -p tessel-cli -p tessel-runner`), then
   `/usr/bin/sudo /opt/tessel/sbin/install.sh --binaries "$PWD/target/release" --dry-run`, read it,
   and run it without the flag. It copies the binaries into `/opt/tessel/tmp`, prints their
   sha256 and asks for yes, so what you read is what is installed. It makes the directories of the
   design (root creates a `_tessel`-owned directory only when it is missing, and stops if one
   exists with another owner or as a link), installs the toolchain (its installer is checked
   against `toolchain.sha256`), writes the sudoers rule (checked with `visudo -cf`), the launchd
   plist, which it also disables so that a reboot does not load it, the cargo setting, and the
   runner's configuration (`/var/tessel-runner/tessel-runner.toml`, root-owned) and the server's
   (`/var/tessel/tessel-server.toml`, only if absent). It moves no secret. The server's
   `runner_config` stays in that file: with `[lander.sudo_runner]` the server does not read the
   runner's file, but its path is the one argument the sudoers rule allows the runner to be given,
   so both name the same file.

   Fill the cargo cache now, so that every probe that needs crates runs before any secret moves.
   `fetch.sh` fetches a landed `main` commit (`$sha`) as `_tessel` with Apple's git, shows the
   `.cargo/` diff (none, for `main`), and needs neither the token nor the server configuration for
   the public repository:
   `/usr/bin/sudo /opt/tessel/sbin/fetch.sh https://github.com/devfep/tessel.git "$sha" --remote
   https://github.com/devfep/tessel.git`. Then `/opt/tessel/sbin/check-own-users.sh --installed`.
   Before the move it exits 2, with exactly three skips that are expected: `/var/tessel/github-token
   has not been moved yet`, `read /var/tessel/github-token: /var/tessel/github-token does not
   exist`, and `/var/tessel/keys is empty`. Any other skip or any failure stops here. Then the
   probes only you can run, as the runner account, in `/var/tessel-runner/work`: `cargo test
   --workspace --offline` of a checkout with `HOME=/var/empty` (it needs `/usr/bin/git` and `cc`
   through `xcrun`), whether cargo builds with a read-only `CARGO_HOME`, and `at`, `crontab -e` and
   `launchctl submit -l x -- /usr/bin/id`, each of which must fail. If the `launchctl submit` probe
   succeeds, remove its job: `/usr/bin/sudo -u _tessel-runner /bin/launchctl remove x`. Anything
   that fails stops here.
4. With the server stopped and nothing in flight (`kill "$(cat <server dir>/server.pid)"`) and the
   lanes idle, move the store, the key pair and the token into `/var/tessel`, and only then hand
   them to `_tessel`. Move only the pair you minted, `2026-10.key` and `2026-10.pub`: the server
   trusts every `.pub` in the directory, so a file a lane planted in `<server dir>/keys` must not
   come along. The ownership change happens inside `/var/tessel`, which the lanes cannot reach, so
   nothing can be swapped under it. `chown-tree.sh` refuses anything there that is not a regular
   file or a directory, any file with more than one link (a link a lane planted to `/etc/sudoers`
   is caught here), and anything not owned by you, for the whole tree first, then changes owners
   entry by entry:

   ```
   /usr/bin/sudo /bin/mv <server dir>/stores/* /var/tessel/stores/
   /usr/bin/sudo /bin/mv <server dir>/keys/2026-10.key <server dir>/keys/2026-10.pub \
     /var/tessel/keys/
   /usr/bin/sudo /bin/mv <server dir>/github-token /var/tessel/github-token
   /usr/bin/sudo /opt/tessel/sbin/chown-tree.sh --from "$USER" --to _tessel:_tessel --contents \
     /var/tessel/stores /var/tessel/keys
   /usr/bin/sudo /opt/tessel/sbin/chown-tree.sh --from "$USER" --to _tessel:_tessel \
     /var/tessel/github-token
   ```

   If `chown-tree.sh` refuses (it names the entry), do not go on: leave the daemon stopped, move
   the files back to `<server dir>` as the rollback says, and find who made that entry. It is a
   move, not a copy, so no fence is reissued. The old binaries and configuration stay in
   `<server dir>`. Compare `/var/tessel/tessel-server.toml`, which step 3 wrote with the dogfood
   defaults, with `<server dir>/tessel-server.toml`: `listen`, `[[repository]]`, `reviewers` and
   `sensitive` should agree, and a difference is edited by hand as `_tessel`
   (`/usr/bin/sudo -u _tessel /usr/bin/vi /var/tessel/tessel-server.toml`). Then
   `/usr/bin/sudo /bin/launchctl enable system/dev.tessel.server`,
   `/usr/bin/sudo /bin/launchctl bootstrap system /Library/LaunchDaemons/dev.tessel.server.plist`,
   and tail `/var/log/tessel-server.log`. Mint the agent tokens as yourself with
   `/opt/tessel/sbin/tokens.sh --kid 2026-10` (the key you moved), then run `tessel init` in each
   worktree again with the new token (`~/.tessel/tokens/<agent>`).
5. `/opt/tessel/sbin/check-own-users.sh` with no flag (it should now exit 0), and one landing on
   `tessel-sandbox/m4-trunk` over HTTPS through the askpass hook, on the old token. If either
   fails, roll back (below).
6. Rotate last, because the old token and key were readable by every lane process until now. Make
   a new GitHub token as in [Build and GitHub, by hand](#build-and-github-by-hand), copy it, and:

   ```
   /usr/bin/pbpaste | /usr/bin/sudo -u _tessel /usr/bin/tee /var/tessel/github-token >/dev/null
   /usr/bin/pbcopy </dev/null
   /usr/bin/sudo -u _tessel /bin/chmod 0600 /var/tessel/github-token
   /usr/bin/sudo -u _tessel /opt/tessel/bin/tessel-server admin keygen \
     --config /var/tessel/tessel-server.toml --kid 2026-11
   /usr/bin/sudo -u _tessel /bin/rm /var/tessel/keys/2026-10.key /var/tessel/keys/2026-10.pub
   /opt/tessel/sbin/tokens.sh --kid 2026-11
   /usr/bin/sudo /bin/launchctl kickstart -k system/dev.tessel.server
   /opt/tessel/sbin/check-own-users.sh --installed
   ```

   Both old key files go: the server trusts every `.pub` in the directory, so a `.pub` left behind
   keeps the old tokens valid. The check then requires `/var/tessel/keys` to hold exactly
   `2026-11.key` and `2026-11.pub`. Then delete the old GitHub token on GitHub, and delete
   `scripts/dogfood/setup.sh` and its mentions on this page in a pull request.

After the move: stop with `/usr/bin/sudo /bin/launchctl bootout system/dev.tessel.server` and start
with `launchctl bootstrap system /Library/LaunchDaemons/dev.tessel.server.plist` (the same sudo and
absolute path); read `/var/log/tessel-server.log`. An upgrade is
`/usr/bin/sudo /opt/tessel/sbin/upgrade.sh --binaries <dir>`, with no dispatch in flight and
binaries built from a landed `main` with green CI. It shows the sha256 of the copies it installs,
keeps the old ones in `/opt/tessel/bin.prev`, and puts them back if the new daemon does not start.
A held submission that changes `Cargo.lock`, `.cargo/` or `rust-toolchain.toml` needs its crates
fetched first, because the runner tests with `--offline`: before the orchestrator approves it, run
`/usr/bin/sudo /opt/tessel/sbin/fetch.sh <fork url> <commit>` yourself, in your terminal. It
prints the commit's `.cargo/` diff and asks for a yes, because cargo reads that configuration as
`_tessel`, from inside the checkout. A new Rust channel needs the toolchain installed first
(`install.sh` does it, from `toolchain.sha256`). The orchestrator waits for you.

Rollback, with an idle queue and the lanes idle:
`/usr/bin/sudo /bin/launchctl bootout system/dev.tessel.server`, then
`/usr/bin/sudo /bin/launchctl disable system/dev.tessel.server`, so that a reboot does not start it.
Hand the current store, key and token back to yourself while they are still in `/var/tessel`, where
the lanes cannot reach them, then move them back:
`/usr/bin/sudo /opt/tessel/sbin/chown-tree.sh --from _tessel --to "$USER:$(/usr/bin/id -gn)"
--contents /var/tessel/stores /var/tessel/keys`, the same without `--contents` for
`/var/tessel/github-token`. The globs of the move back must be expanded by root, because
`/var/tessel/stores` is not readable by you, so run each under `sh -c`, with `<server dir>` written
out in full:

```
/usr/bin/sudo /bin/sh -c '/bin/mv /var/tessel/stores/* <server dir>/stores/'
/usr/bin/sudo /bin/sh -c '/bin/mv /var/tessel/keys/*.key /var/tessel/keys/*.pub <server dir>/keys/'
/usr/bin/sudo /bin/mv /var/tessel/github-token <server dir>/github-token
```

Never put an older store back: a restored store can reissue fences. Start the old binary from
`<server dir>/bin` with the old configuration. That token is exposed again, so rotate it at the
next migration. The accounts and files stay; they do nothing while the daemon is disabled.

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

An approval, or a submission the coordinator does not hold for review, starts a landing in three
calls: `Prepare` (rebase, claim coverage, the head pushed to a candidate ref), `Test` (the runner's
tests on a tree made from that ref) and `Land` (a compare-and-swap push of `main`, a read-back by
ancestry, the ref deleted), then `merged` in the lane's inbox. [What holds a submission](configuration.md#what-holds-a-submission-for-review)
and the outcomes are in [Landing](configuration.md#landing).

That is main's build. The server on this page runs `455c889`, which predates the staged landing:
its core makes the whole merge in one `Prepare` and sends no `Test` or `Land`, so it has no `Land`
to be in doubt. The staged landing, and the reconcile below, run only once the server is upgraded
by the rules above.

### A `Land` in doubt

If the lander cannot say whether a `Land` moved main (a lost reply, a timeout, or the server
stopped during the push), the core does not reject the work and does not count an attempt. It asks
the lander whether the head is on main (`Reconcile`) and goes on from the answer, as
[Landing](configuration.md#landing) describes; each failed ask waits 10, 30, then 90 seconds.
`tessel status` in the submitter's worktree shows `[landing in doubt for 3m 20s: ...]`. The
server tells the daemon when the doubt begins and ends, and lists the agent's claims in doubt in
every `Welcome`, so a daemon that was offline, or a server that was stopped and restarted (as
`admin resolve` needs), is right at its next connection: the doubt is shown if it stands and gone
if it ended. The age counts from when that daemon first learned of the doubt, on its own clock.
A lander that never answers holds the queue behind it. A `Land` sent again that is refused, or
whose candidate ref is gone, does not end the doubt: the merge goes back through `Prepare` (which
makes the ref again, and the same head goes straight to `Land`), `tessel status` keeps showing the
doubt, and nothing is rejected. The doubt ends when the work lands, when you answer it with `admin
resolve`, or when main is known to have moved past the base the `Land` leased: a `Reconcile` or a
`Prepare` finds it elsewhere, or a `Land` is answered `main_moved` (the server's lander answers
that as unknown, so with it this one does not happen). While a doubt holds the merge, the core
reads no head and verifies no race: they wait for it.

To find the claim without the submitter, read `server.log` (`/var/log/tessel-server.log` after the
[move to the service accounts](#moving-to-the-service-accounts)): each `Land` or `Reconcile`
that leaves a doubt logs `the landing of claim N is in doubt since <epoch ms>`, names the `base`
the `Land` leased and the `head` it journaled, and gives the command, with `--tenant` and `--repo`.
The lander rebases, so the submitter's fork commit is never on main by its sha; the commit to look
for is that journaled `head`.

To settle it by hand, with the server stopped. Stop it and wait until it has exited:

```
kill "$(cat "<server dir>/server.pid")"
while kill -0 "$(cat "<server dir>/server.pid")" 2>/dev/null; do sleep 1; done
```

Then:

1. Read the journaled stage, claim, head and base. The store is
   `<store_dir>/<tenant>/<repo>.sqlite`, where `store_dir` is the one in
   `<server dir>/tessel-server.toml` (`<server dir>/stores` until the move to the service
   accounts, `/var/tessel/stores` after). Run it as the user that owns the store and without
   `-readonly`; never as root, because Apple's `sqlite3` leaves `-wal` and `-shm` files behind when
   it closes, and root-owned ones stop the `_tessel` server. Until the move, as yourself:

   ```
   /usr/bin/sqlite3 -init /dev/null -cmd ".timeout 5000" \
     "file:<server dir>/stores/<tenant>/<repo>.sqlite?mode=rw" \
     "select json_extract(json, '\$.merge_in_flight.stage.stage'), \
             json_extract(json, '\$.merge_in_flight.claim'), \
             json_extract(json, '\$.merge_in_flight.stage.head'), \
             json_extract(json, '\$.merge_in_flight.stage.base'), \
             json_extract(json, '\$.claims.' || json_extract(json, '\$.merge_in_flight.claim') \
                                 || '.work.doubted_land') \
      from kv where key = 'core'"
   ```

   After the move, the same command under the service account:

   ```
   /usr/bin/sudo -u _tessel /usr/bin/sqlite3 -init /dev/null -cmd ".timeout 5000" \
     "file:/var/tessel/stores/<tenant>/<repo>.sqlite?mode=rw" \
     "select json_extract(json, '\$.merge_in_flight.stage.stage'), \
             json_extract(json, '\$.merge_in_flight.claim'), \
             json_extract(json, '\$.merge_in_flight.stage.head'), \
             json_extract(json, '\$.merge_in_flight.stage.base'), \
             json_extract(json, '\$.claims.' || json_extract(json, '\$.merge_in_flight.claim') \
                                 || '.work.doubted_land') \
      from kv where key = 'core'"
   ```

   The claim must be the one you mean. At stage `land` or `reconcile` the third and fourth columns
   are the head and base. At `prepare` they are empty; at `test` they are the `Test`'s own head and
   base, which can differ from the pair the doubt is about. At `prepare` and `test` (a doubt that
   went round again) the head and base of the `Land` the doubt is about are the last column,
   `["<base>","<head>"]`; use that pair, not the third and fourth columns. `admin resolve` refuses
   a claim with no such pair (see the last point below).

   The store is in WAL mode. `tessel-server` has no signal handler, so both stops above (`kill`
   and `launchctl bootout`) leave the `-wal` and `-shm` files, and a read-only open works. Any
   later `tessel-server admin` command (a refused `admin resolve` too) opens and closes the store
   cleanly and removes both files. With no `-shm`, macOS's `/usr/bin/sqlite3` fails
   `-readonly` with `unable to open database file` although the file is there, which is why this
   read can write. The `file:...?mode=rw` form never creates a file: a wrong path (a mistyped
   tenant directory or repository) also fails with `unable to open database file`, where a bare
   path would make an empty file and fail with `no such table: kv`.
2. In a checkout of the trunk (the lander's own clones are deleted after each call), run
   `git fetch origin main`, then `git merge-base --is-ancestor <head> origin/main`. Exit 0 means
   the head is on main. Exit 1 means it is not. Exit 128 means the checkout does not know the
   commit: fetch again, or answer `--not-landed`.
3. Run, with that claim number, as the user that owns the store (`_tessel` after the
   [move to the service accounts](#moving-to-the-service-accounts); the lock and the write need
   it):

   ```
   <server dir>/bin/tessel-server admin resolve <claim> --landed <head> \
     --tenant <tenant> --repo <repo> --config <server dir>/tessel-server.toml
   <server dir>/bin/tessel-server admin resolve <claim> --not-landed \
     --tenant <tenant> --repo <repo> --config <server dir>/tessel-server.toml
   ```

After the [move to the service accounts](#moving-to-the-service-accounts), stop the server with
`/usr/bin/sudo /bin/launchctl bootout system/dev.tessel.server` (not the pid kill above) and wait
until it has gone:

```
while /usr/bin/sudo /bin/launchctl print system/dev.tessel.server >/dev/null 2>&1; do sleep 1; done
```

Use `--config /var/tessel/tessel-server.toml` and the binary `/opt/tessel/bin/tessel-server`, and
run steps 1 and 3 under `/usr/bin/sudo -u _tessel`. Start it again with
`/usr/bin/sudo /bin/launchctl bootstrap system /Library/LaunchDaemons/dev.tessel.server.plist`,
and read `/var/log/tessel-server.log`.

`--landed <head>` merges the claim from the base its `Land` leased, and is refused unless `<head>`
is exactly the head the `Land` journaled; `admin resolve` prints the claim's base and head either
way. Use it only when step 2 exits 0. `--not-landed` takes no head and is safe whatever main is,
so use it when unsure or when step 2 exits 1 or 128: it sends the claim back to `Prepare`, which
answers `already_merged` if the work is on main after all. Both refuse while a server holds the
store, and when the claim has no `Land` in flight. Then start the server. The answer is the one a
`Reconcile` would have given: the core still applies its own rules. After `--landed`, the lane's
daemon finds the landing in the repository's event log at its next connection and reports `claim N
is gone from the coordinator; forgot it`, not a `merged` notice. The daemon
copies the submission's notes to the landed commit once it reconnects and has fetched the landed
commits; check with `git notes show <landed commit>`, and copy them by hand only if none is there
(`git notes add`, in `CLAUDE.md`). A lane that was waiting (`--wait`) behind the settled claim
was told, when the server stopped, that its queued wait was withdrawn;
`--landed` may have granted that wait, and the daemon then adopts the claim and says so in its
inbox. Use that claim, or `tessel release` it. A lane whose own claim request was still
on the way when the connection dropped is answered by that request's reply for the same claim,
not by an adopted one.

`--landed` also releases the closed races that waited for the head. Their trials are queued and
run once the server is started, so the entrants hear the result then if they are connected. One that
is offline when the trial ends gets no notice or result; the decision is only in the event log. The
command sends no notices itself.

Two doubts that resolving does not fix:

- A `Land` whose push fails for good (a revoked token, a ruleset that refuses the push to `main`)
  is asked again about every 90 seconds once the backoff is capped (10 s, 30 s, then 90 s, plus
  the clone and push of each try), forever, each time with a fresh clone of main. The `server.log`
  shows a repeated `push` warning while the claim stays in doubt. Fix the credential or the
  ruleset; `--not-landed` only sends the work round again through `Prepare`, `Test` and `Land`,
  and `--landed` is for a push that reached main. No command rejects the work. A refusal of a
  `Land` that was sent again behaves the same way. A refusal at `Prepare` (a fork branch deleted
  or force-pushed while in doubt) says nothing of where main is, so the core asks a `Reconcile`
  after the same backoff. Main still at the base sends the `Land` again, and the claim goes round
  `Prepare`, `Reconcile` and `Land` until you answer `--not-landed` (the next refusal then rejects
  it), because a late copy of the `Land` may still land the head. Main elsewhere ends the doubt
  and the fresh `Prepare` rejects the work. The head on main lands it. `server.log` repeats the
  doubt line on each pass, with the reason of the refusal quoted at its end. `--landed` and
  `--not-landed` work while the merge is at `Prepare` or `Test` too, for a doubt that has a `Land`
  behind it.
- A doubt with no `Land` behind it: a store written before the staged landing (`unstaged`) that
  holds a merge in flight, or a claim the running build left `in_doubt`. It starts a fresh
  `Prepare` that counts as an infrastructure failure. If that `Prepare` and the next ones fail
  (a `Test` that hangs included), the claim is rejected after the fourth, as it always was. A
  refusal or failing tests reject it too: there is no late `Land` to fence. `tessel status` does
  not show this doubt, `server.log` does not log it, and `admin resolve` answers `claim N has no
  Land in doubt`, since there is no `Land` to reconcile. It ends when the work lands or is
  rejected. It is reachable only by upgrading with a
  merge in flight or a claim in doubt, which "Nothing in flight" above forbids.

## D3, the load on the Mac

A cold `cargo test --workspace --locked --offline --no-fail-fast` took 237 s on 22 October, in a
fresh worktree, not a fresh clone. The load was 9 at the start and rose to 75 during the run. The
runner's `timeout_secs` is 720. The lander asks the runner for what is left of its 13-minute
deadline less 5 s, about 775 s when the clone, fetch and rebase are quick, so 720 cuts first unless
they take more than about 55 s. The gate's parser refuses `-j 2`, so the cap lives in
`<server dir>/.cargo/config.toml` (`jobs = 6`) instead. D3 is accepted with that cap.

# Own users: the server and the runner as separate macOS accounts

Status: design note (OWN-USERS-DESIGN, M5), reviewed once by an adversarial security review, for
Felix's decision. No code, and nothing in it was run: no account, `dscl` or `sudo` command was
executed to write it. Facts about macOS that this note could not check are marked "to probe" and
are the first step of RUNNER-OWN-UID. Rows and decisions are those of `docs/proposals/phase-1.md`;
D4 is the dogfood setting in `docs/dogfood.md`.

## Scope and the claim this design makes

Today the server, the lander's git commands, the runner and every lane agent run as Felix, so a
test (or a hostile `build.rs`) can read `<server dir>/github-token`, mint agent tokens from the
signing key, and edit the store (D4 says so).

**The claim is: the token, key and store are unreadable to repo code under test.** Repo code then
runs as an account that owns nothing the server trusts. The claim is not that the lanes cannot
reach them. The lanes run as Felix, and every sudo step below runs code that was built, checked
out or fetched in Felix's account: binaries from his `target/`, scripts from a checkout, tools
from his `PATH` (`/opt/homebrew/bin` is writable by him). What the design does about that:
- the lanes cannot read the secrets at rest, which today they can;
- the scripts are read by Felix once, installed root-owned under `/opt/tessel/sbin`, and run
  from there, never from a checkout; they call tools by absolute root-owned paths under `env -i`;
- the launchd plist pins `PATH=/usr/bin:/bin`.

What remains: every upgrade installs binaries built in Felix's account, so a lane that can write
a file that build reads can get code running as `_tessel` at the next upgrade. The later fix is
binaries built in CI and checked by sha256 before they are installed (M6, with the hosted
server); it is not part of this design. The assumptions Felix holds: sudo has no Touch ID (there
is no `sudo_local` here), the steps are run in a shell no lane controls, and the password is
never typed into a prompt a lane raised (`sudo -A`).

Not in scope: keeping `main` safe from the lanes. They push with `gh`'s credential, and Felix is
the admin bypass actor of ruleset B (`docs/BUILD-PROTOCOL.md` keeps that a process control).

## Decision in one paragraph

Two hidden accounts. `_tessel` runs the server and the lander and alone reads the token, key and
store. `_tessel-runner` runs repo code. The server starts the existing `tessel-runner` binary with
`sudo -n -u _tessel-runner`, one run at a time, through a sudoers rule of two commands. After each
run the server kills everything the runner account owns, and the runner empties its work area at
the start of the next. Everything the runner's behaviour depends on (binaries, configuration,
toolchain, dependency cache) is root-owned or `_tessel`-owned and read-only to it.

## 1. Accounts, files and modes

Made with `dscl`, not `sysadminctl`. `sysadminctl -addUser` makes a login-capable standard user
with a UID of 501 or more, a home in `/Users`, and membership of `staff`, the group of Felix's
account. `dscl` makes exactly the record asked for.

```
sudo dscl . -create /Groups/_tessel PrimaryGroupID 481
sudo dscl . -create /Groups/_tessel-runner PrimaryGroupID 482
sudo dscl . -create /Users/_tessel UniqueID 481        # same for _tessel-runner, 482
sudo dscl . -create /Users/_tessel PrimaryGroupID 481
sudo dscl . -create /Users/_tessel UserShell /usr/bin/false
sudo dscl . -create /Users/_tessel NFSHomeDirectory /var/empty
sudo dscl . -create /Users/_tessel Password '*'
sudo dscl . -create /Users/_tessel IsHidden 1
```

The pair 481 and 482 is fixed, in the 480s that Apple's own accounts do not use on this Mac
(users 200 to 308 and 441 and groups 395 to 400 are taken, and OS updates add numbers, so "the
first free one" would collide). `accounts.sh` aborts if either number is taken as a user or a
group. Each account has its own group and no other membership; neither is in `staff`.
`/usr/bin/false`
and a password of `*` mean no login, `su` or screen sharing; `sudo -u` needs neither.

| Path | Owner and mode | Why |
|---|---|---|
| `/opt/tessel/bin/` (`tessel-server`, `tessel`, `tessel-runner`) | root:wheel 0755 | no account can replace a binary the other runs |
| `/opt/tessel/sbin/` (the scripts) | root:wheel 0755 | run from here, never a checkout |
| `/var/tessel/` | `_tessel` 0711 | traversal to `handoff/` only |
| `/var/tessel/{github-token,keys/,stores/,lander/,tmp/}` | `_tessel` 0600 (dirs 0700) | secrets, clones, `_tessel`'s `TMPDIR` |
| `/var/tessel/tessel-server.toml` | `_tessel` 0600 | names the paths above |
| `/var/tessel/handoff/` | `_tessel:_tessel-runner` 0750 | test trees; private forks are not public |
| `/var/tessel/cargo/` | `_tessel` 0755 | the runner reads it; only the fetch step writes |
| `/var/tessel/toolchain/` | root:wheel 0755 | the runner reads, nothing it runs writes |
| `/var/tessel-runner/` | root:wheel 0755 | the runner cannot rename what it depends on |
| `/var/tessel-runner/tessel-runner.toml` | root:wheel 0644 | repo code cannot edit it (below) |
| `/var/tessel-runner/{work,tmp}/` | `_tessel-runner` 0700 | the only things the runner owns |
| `/var/log/tessel-server.log` | `_tessel` 0644 | quoted untrusted text, never the token |
| `/etc/sudoers.d/tessel` | root:wheel 0440 | section 2 |
| `/Library/LaunchDaemons/dev.tessel.server.plist` | root:wheel 0644 | `UserName` `_tessel`, `TMPDIR` `/var/tessel/tmp` |

The configuration is root-owned because it names `PATH`, `CARGO_HOME` and the
timeout. If the runner owned it, a test could point those at directories it writes, and every
later submission would build with a toolchain it chose (rule 6). `RunnerConfig::load`
(`tessel-runner/src/config.rs`) today demands the effective user as owner and mode exactly 0600;
RUNNER-OWN-UID changes it to accept an owner of root or the effective user, with no group or
other write bit, and still `WrongUser` for a login that is not its own.

The agents' tokens are not secrets from the lanes. Felix's shell makes them:
`sudo -u _tessel /opt/tessel/bin/tessel-server admin token ... > ~/.tessel/tokens/<agent>` with
`umask 077`, because `_tessel` cannot write under his home and a file it made there would be
unreadable to him. The key stays with `_tessel`. The `jobs = 6` cap moves to
`/var/tessel/cargo/config.toml`, since cargo would not find `<server dir>/.cargo` above the
runner's tree.

Tests: a Linux CI job (section 8) checks each row's owner and mode and that the runner account
cannot read the token, key, store or `lander/`, nor write under `/opt/tessel`, `/var/tessel`,
`cargo`, `toolchain` or `/var/tessel-runner` except `work/` and `tmp/`; `RunnerConfig::load` tests
for a root-owned 0644 file, a runner-owned file with group write, and a wrong login.

## 2. The runner channel

The server runs `/usr/bin/sudo -n -u _tessel-runner -- /opt/tessel/bin/tessel-runner --config
/var/tessel-runner/tessel-runner.toml`, writes the request JSON to its standard input and reads
one result line, as `main.rs` does today (exit 0 ran, 2 refused, 3 unsupervised). `SudoRunner`
replaces `LocalRunner` behind the same `TestRunner` trait, and `[lander] runner_config` goes. The
only authentication is who may run sudo; there is no socket and nothing listening.

```
Defaults:_tessel env_reset, !setenv, !use_pty, secure_path="/usr/bin:/bin"
Cmnd_Alias TESSEL_RUN = \
  /opt/tessel/bin/tessel-runner --config /var/tessel-runner/tessel-runner.toml, \
  /bin/kill -KILL -1
_tessel ALL = (_tessel-runner) NOPASSWD: TESSEL_RUN
```

What the rule pins, and why `_tessel` cannot widen it:
- **Path and arguments.** A sudoers command with arguments allows exactly those, so `_tessel`
  cannot add a flag, a second config or another binary. No wildcard: `*` also matches `/`.
- **Environment.** `env_reset` with `!setenv` rejects `sudo VAR=x`. The runner no longer reads
  `PATH`, `CARGO_HOME` or `TMPDIR` from its environment (`INHERITED_ENV` in
  `config.rs`): they come from the root-owned file. A test starts it with `LD_PRELOAD`,
  `DYLD_INSERT_LIBRARIES` and a hostile `PATH` and shows the test command sees none.
- **Who can edit it.** The binary, its directories, the rule and the configuration are owned by
  root. `_tessel` cannot change what it can run, and the runner cannot change what it is.
- **Request trees.** `checked_tree` (`tessel-runner/src/request.rs`) demands a tree owned by the
  runner; it drops that demand for a path under `/var/tessel/handoff/` and accepts only such a
  path or one under `work/`.
- **Limits.** The runner sets `RLIMIT_NPROC` and `RLIMIT_CORE`, soft and hard, before it spawns
  anything. `NPROC` is sized from the full suite's peak process count, measured by the probe,
  with headroom: a fork failure would otherwise read as a failing test (rule 6).

Tests: a round trip through the real `sudo` with a request at `MAX_REQUEST_BYTES` (64 KiB) and a
result at the tail limit; the exit codes 0, 2 and 3 arrive; `sudo -n -u _tessel-runner` with
another argument, another binary, `VAR=x` or `-i` fails as `_tessel`; `sudo -l -U _tessel` lists
exactly the two commands.

## 3. Cleanup, serialisation and the deadline

The runner's process-group kill misses a child that calls `setsid` (RUNNER-SANDBOX). The account
boundary replaces it: after every run, and when the server gave the call up at its deadline, the
server runs `sudo -n -u _tessel-runner /bin/kill -KILL -1`. On macOS `kill(-1)` signals every
process of the caller's uid and no one else's. `pkill -U` from outside is not used: `_tessel`
could not signal another account's processes. The server then polls `ps -U _tessel-runner -o
pid=`, which needs no privilege, until it is empty for up to 10 s, repeating the kill. If it is
not empty, or `launchctl print user/<uid>` shows a job, the gate stays closed: the call answers
`Unknown` and no run starts. If `launchctl print` needs root (to probe), the check moves into the
runner: at start it lists its own domain's jobs and exits 3 if any exist.

`cron` and `at` are denied unconditionally, a Felix step: `_tessel-runner` goes into
`/usr/lib/cron/cron.deny` and `at.deny`. Neither can be inspected without root, so denial and
the check script's probe, not the poll, are what stop a job that is not running when checked.

`kill -1` takes the whole account, so no other run may be live. One process-wide async mutex, the
runner gate, covers `{run, kill, wait}` across every repository of the server. The gate holder
is the `SudoRunner` itself: it runs `sudo` as a child it owns, polls the call's `Stop`
(`landing/deadline.rs`), and when the deadline passes or the call is given up it does the kill
itself, waits for the child, and releases the gate. Waiting on `sudo` first would deadlock: the
`spawn_blocking` thread is not cancelled by the call's timeout (`landing/mod.rs`) and `sudo`
exits only after the kill. One server per Mac is supported.

At start the server kills and polls before answering a call, so a stop in the middle of a test
no longer orphans the cargo process group (`docs/dogfood.md` warns of it today). The runner
empties `work/` and `tmp/` at the start of every run, clears `uchg` flags and ACLs first, and
deletes files owned by its uid in `/tmp`, `/var/tmp`, `/Users/Shared` and its
`/var/folders/<xx>/<hash>/` directories (`C`, `0` and `T`). On Linux: a cgroup scope per run.

Tests: a command that forks a `setsid` child sleeping 60 s leaves no process after the run; two
overlapping calls never have two runner processes alive (a double recording start and end); a
call past its deadline frees the gate and leaves none alive; the gate stays closed when a kill
leaves a survivor; the next run starts with `work/` empty even with a `uchg` file and an ACL.

## 4. Toolchain and dependencies

The runner's `HOME` is `/var/empty`, so no `~/.cargo` or `~/.rustup` exists.
- **Toolchain.** `/var/tessel/toolchain`, root-owned, holds the channel in `rust-toolchain.toml`,
  installed by root from the channel's standalone installer, not by Felix's `rustup`, which lives
  in a directory he and the lanes can write. The installer's sha256 comes from the channel
  manifest over HTTPS (`static.rust-lang.org/dist`), is recorded in
  `scripts/dogfood/toolchain.sha256`, and that file is installed with the scripts (to probe that
  the installer unpacks without `rustup`). The runner's `PATH` names the toolchain's `bin`, so no
  `rustup` proxy tries to download and `RUSTUP_HOME` is unused. `rust-toolchain.toml` is a
  sensitive path, so Felix installs a new channel before approving a change to it.
- **Dependencies.** `CARGO_HOME=/var/tessel/cargo`, owned by `_tessel`, read-only to the runner.
  The runner tests with `--offline`, so the crates must be there. Repo code could poison a cache
  it can write, and the poisoned crate would be compiled into later submissions' tests.
- **Who runs `cargo fetch`.** Not the runner, and not an agent. `Cargo.lock`, `.cargo/` and
  `rust-toolchain.toml` are sensitive paths, so a submission that changes them is held and read.
  Fetch is not free of repo code: cargo reads `.cargo/config.toml` in every ancestor of its
  working directory, and a config can name a credential provider that cargo runs, as `_tessel`.
  So fetching is a step Felix runs in his own terminal and authorises with his password,
  `sudo /opt/tessel/sbin/fetch.sh <fork commit>`, with no NOPASSWD rule: an agent has no tty to
  answer it, and a rule would let any lane run it on a commit it pushed. The script, as `_tessel`
  under `env -i` with `PATH` set to the toolchain's `bin`: fetches the commit from the agent's
  private fork using the lander token through the askpass hook; checks it out in a scratch
  directory under `/var/tessel/tmp`, whose ancestors the runner cannot write (a `/tmp` scratch
  would let it plant `/private/tmp/.cargo/config.toml`); prints the commit's `.cargo/` diff against
  main and stops unless Felix confirms; then runs `cargo fetch --locked` with
  `CARGO_HOME=/var/tessel/cargo`. A fetch that was skipped shows as a failing offline build,
  which the agent reads as failing tests; the script says so, and automating it is M6.
- **No warm `CARGO_TARGET_DIR`.** Each run builds in its own copy of the tree, so one submission
  cannot leave artefacts another's tests link (rule 6). The cost is a cold build per landing,
  237 s on 22 October, which a fresh clone costs today: no change.

To probe: whether cargo builds with a read-only `CARGO_HOME` (it takes a lock file there). If
not, the runner copies `/var/tessel/cargo` into `work/` at the start of each run, which costs
seconds. Also whether the whole `cargo test --workspace` passes as `_tessel-runner` with
`HOME=/var/empty`: the repo's real-git tests need `/usr/bin/git` and `cc` through `xcrun`.

Tests: a `build.rs` that writes to `CARGO_HOME`, the toolchain or the configuration fails; a
second submission does not see a file the first wrote to `tmp/` or `work/`.

## 5. The test tree across accounts

`Test` (`docs/proposals/candidate-ref.md`) fetches the candidate ref into a fresh repository.
The fetch uses the token through the askpass hook, but the repository stores no credential. The
lander makes it as `_tessel` in `/var/tessel/handoff/call-N/`, group `_tessel-runner` read-only.

The request's `tree` names it. The runner, as `_tessel-runner`, copies it into `work/` with plain
`std::fs` (links copied as links, never followed; no git, so rule 4 holds) and runs the commands
in the copy, which it owns with everything the tests write there. `_tessel` never touches it.

Removal follows ownership. `_tessel` removes `handoff/call-N` (`CallDir.remove`,
`clear_leftovers`), as it created it; the runner cannot write there. The runner removes the
contents of `work/` and `tmp/` itself at its next start (section 3). No sudo command removes
anything, so the rule has no path argument to widen. Because the tests run on a copy, they cannot
reach the lander's clones, and the `TestTree` types in `workspace.rs` keep their meaning. This
closes M4-SANDBOX-WORKSPACE.

Tests: the runner cannot write `handoff/`; `_tessel` can remove `handoff/call-N` while `work/` is
full of runner-owned files; the start-up sweep empties a `work/` holding a read-only directory
and a symlink to `/var/tessel`, and leaves `/var/tessel` untouched.

## 6. Upgrades

Once `/opt/tessel/bin` belongs to root, each upgrade is a sudo step. A `bin/` that stays writable
by Felix is refused: the lanes run as Felix, so any lane could replace `tessel-server`, which
then runs as `_tessel` and reads the token.

Recommended: `sudo /opt/tessel/sbin/upgrade.sh <built binaries>`. It stops the daemon
(`launchctl bootout system/dev.tessel.server`), keeps the old binaries as `/opt/tessel/bin.prev`,
installs the new ones and starts it. The scripts reach `/opt/tessel/sbin` by Felix's reading
them once and installing them with `sudo install -o root`; a changed script is read again. The
rest of DOGFOOD-UPGRADE (f) stands, and its words "from inside `~/tessel-dogfood`" change to this
script. The binaries are still built in Felix's account (scope section): the residual risk.

## 7. Escape routes, tested from the runner account

`/opt/tessel/sbin/check-own-users.sh` is run by Felix after the accounts exist and before any
code is installed (section 8, step 2), and again after any change. It runs each probe as
`_tessel-runner` through the real rule; the Linux job runs the portable subset.

| Route | Probe | Expected |
|---|---|---|
| `at`, `cron` | `echo id \| at now`; `crontab -e` | refused (denied unconditionally) |
| `launchd` | `launchctl submit -l x -- /usr/bin/id`; a job with `StartInterval` or `WatchPaths` | no job runs, or none survives the kill (to probe) |
| Shared directories | write `/tmp`, `/var/tmp`, `/Users/Shared`, `/var/folders/<uid>`, leave a file | allowed; the next start deletes what it owns |
| `sudoers` | `#includedir` present; a piped round trip through `sudo`; `sudo -n -l`, `sudo -n -u _tessel` as the runner | denied |
| The secrets | read `/var/tessel/{github-token,keys,stores,lander}` | denied |
| The binaries and config | write `/opt/tessel`, `/var/tessel-runner/tessel-runner.toml`, `cargo`, `toolchain` | denied |
| Felix's home | read `~/repos`, `~/.config/gh`, `~/.ssh` | denied: `~` is 0750 and the runner is not in `staff`; the script checks the mode has no other bits |
| The server socket | connect to `127.0.0.1:7878` | opens; the handshake needs an agent token it cannot read (S1) |
| Loopback services | list listeners (`lsof -iTCP -sTCP:LISTEN`), connect to each | see below |
| The full test suite | `cargo test --workspace` as the runner, `HOME=/var/empty` | passes |
| Process limits | a fork bomb | bounded by `RLIMIT_NPROC`, the run timeout and the kill |

**Loopback services.** Felix's Mac runs services that listen on loopback and trust local
connections: a Homebrew postgres on 5432 defaults to `trust`, and a superuser there can run a
command as Felix with `COPY ... TO PROGRAM`, which reaches `gh`'s credential and, through the
bypass, `main`: worse than the lander token. This design does not block loopback by user, because
this repository's own tests bind and connect to loopback. It does one thing: the
check script prints every listener, and a `pf` rule blocks the runner from 5432 and from any
port Felix names (`block out quick on lo0 proto tcp to port 5432 user _tessel-runner`; to probe
that `pf` matches the user on `lo0`). Any other listener, including one Felix starts later, is a
residual risk he must check.

## 8. What RUNNER-OWN-UID builds, and Felix's steps

Order: the scripts first, then the probes that need only accounts, then the install, then the
probes that need the install, and only then the move of the secrets.

Felix's steps on the Mac (sudo), each with its check:
0. Merge the scripts pull request (code item 3), read the scripts, and install them with
   `sudo install -o root -m 0755 ... /opt/tessel/sbin/`.
1. Run `accounts.sh` (the two accounts, 481 and 482) and add the runner to `cron.deny` and
   `at.deny`. Check with `dscl . -read /Users/_tessel`.
2. `check-own-users.sh --accounts`: the probes that need only the accounts (section 7, run as
   `su`-less `sudo -u _tessel-runner`, not through the rule, which is not installed yet). A route
   that works is a finding to fix before step 3.
3. The runner and server code has landed through the old server. `install.sh` makes the
   directories of section 1, the toolchain, the cargo cache, the binaries, the plist (not
   loaded) and the rule (`visudo -cf /etc/sudoers.d/tessel` before it moves into place).
   Then `check-own-users.sh --installed`: the probes that need it (read-only `CARGO_HOME`, the
   installer, the full suite, the rule's refusals), before any secret moves.
4. With the server stopped and nothing in flight, move the store, key and token to
   `/var/tessel`. A move, not a snapshot restore (C11), so no fence is reissued. The old
   binaries and configuration stay in `~/tessel-dogfood`. Load the daemon.
5. `check-own-users.sh` in full, then one landing on `tessel-sandbox/m4-trunk` (DOGFOOD-UPGRADE
   (e)) on the exposed token.
6. Rotate last: a new GitHub token and a new signing key (`kid` `2026-11`), delete the old key
   file, `trash` the old `github-token`, and mint new agent tokens. The old token and key were
   readable by every lane process until now.

Rollback, with an idle queue (DOGFOOD-UPGRADE (d)): `launchctl bootout`; move the current store,
the key and the token back to `~/tessel-dogfood` (not the copy of step 4: restoring an old
store can reissue fences); `chown -R` them to Felix; start the old binary with the old
configuration. That token is exposed again, so rotate at the next migration. The accounts and
files are inert while the daemon is stopped.

Code (lane B):
1. `tessel-runner`: the configuration rules above, its environment from the file, the copy of
   the tree, the start-of-run sweep, the rlimits, `checked_tree`. Tests as above.
2. `tessel-server/src/landing`: `SudoRunner`, the gate, the kill, the poll, the start-up
   cleanup, and the `handoff` directory in place of `call-N/test`.
3. `scripts/dogfood/{accounts,install,upgrade,fetch,check-own-users}.sh`; `setup.sh` splits into
   the Felix part (agent tokens) and the root part; `docs/dogfood.md`'s D4 gains the scoped claim
   and the residual of the scope section, and the steps. Sensitive paths: one pull request Felix
   merges.
4. A Linux CI job (a workflow pull request Felix merges): `useradd` for the two users, the rule
   installed with `visudo -cf`, and the checks that the token is unreadable, the workspace
   unwritable, a `setsid` child killed, and each widened command of section 2 refused.

## 9. Alternatives considered

| Alternative | Why not |
|---|---|
| `sandbox-exec` with a profile that denies the token path | Same account, so what the profile misses is open; Apple documents the tool as deprecated |
| A VM or container per run | Needs a VM on a Mac already at load 55 to 75 (D3); right for the hosted server (M6) |
| launchd `UserName` and no sudo | The server could run as `_tessel`, but a one-shot child as another account needs a persistent runner daemon with a socket, its own authentication and cleanup |

# Own users: the server and the runner as separate macOS accounts

Status: design note (OWN-USERS-DESIGN, M5), for an adversarial security review and then Felix.
No code, and nothing in it was run: no account, `dscl` or `sudo` command was executed to write
it. Facts about macOS that this note could not check are marked "to probe" and become the first
step of RUNNER-OWN-UID. Rows and decisions are those of `docs/proposals/phase-1.md`; D4 is the
dogfood setting in `docs/dogfood.md`.

## Scope

In scope: keeping the lander's GitHub token, the signing key, the store and the lander's
workspace away from repo code under test. Today the server, the lander's git commands, the
runner and every lane agent run as Felix, so a test (or a hostile `build.rs`) can read
`<server dir>/github-token`, mint agent tokens from the key, and edit the store (D4 says so).

Not in scope: keeping `main` safe from the lanes. They run as Felix with `gh`'s credential, and
Felix is the admin bypass actor of ruleset B, so a lane can push `main` without the server. That
is a process control (`docs/BUILD-PROTOCOL.md`), and this design does not change it. What it
does change is that the lanes lose the token, the key and the store, which makes the server
the only holder of the lander's credential.

## Decision in one paragraph

Two hidden accounts. `_tessel` runs the server and the lander and alone reads the token, key and
store. `_tessel-runner` runs repo code. The server starts the existing `tessel-runner` binary
with `sudo -n -u _tessel-runner`, one run at a time, through a sudoers rule that allows exactly
three commands. After each run the server kills everything the runner account owns and wipes its
work area. The binaries are root-owned, so an upgrade is a sudo step for Felix. The runner's
toolchain and dependency cache are read-only to it and filled by someone who is not repo code.

## 1. Accounts, files and modes

Made with `dscl`, not `sysadminctl`. `sysadminctl -addUser` makes a login-capable standard user
with a UID of 501 or more, a home in `/Users`, and membership of `staff` (gid 20). `staff` is
the group of Felix's own account and gives read access under his home and to shared directories,
the one thing the runner must not have. `dscl` makes exactly the record asked for.

```
sudo dscl . -create /Groups/_tessel PrimaryGroupID <gid>
sudo dscl . -create /Groups/_tessel-runner PrimaryGroupID <gid2>
sudo dscl . -create /Users/_tessel UniqueID <uid>      # same for _tessel-runner
sudo dscl . -create /Users/_tessel PrimaryGroupID <gid>
sudo dscl . -create /Users/_tessel UserShell /usr/bin/false
sudo dscl . -create /Users/_tessel NFSHomeDirectory /var/empty
sudo dscl . -create /Users/_tessel Password '*'
sudo dscl . -create /Users/_tessel IsHidden 1
```

Each account has its own group and no other membership. The UIDs and gids are the first free ones
below 500 in the range the setup script checks with `dscl . -list /Users UniqueID`; to probe
whether 300 to 399 is free on the dogfood Mac (Apple keeps parts of the range). `/usr/bin/false`
and a password of `*` mean no login, `su` or screen sharing; `sudo -u` needs neither.

Layout. `/opt/tessel` is the code, `/var/tessel` the server's data, `/var/tessel-runner` the
runner's own area. Nothing lives in Felix's home.

| Path | Owner and mode | Why |
|---|---|---|
| `/opt/tessel/bin/` (`tessel-server`, `tessel`, `tessel-runner`) | root:wheel 0755, parent root-owned | no account can replace a binary the other runs; sudo matches this path |
| `/var/tessel/` | `_tessel` 0755 | traversal to `handoff/` only |
| `/var/tessel/{github-token,keys/,stores/,lander/}` | `_tessel` 0600 (dirs 0700) | the secrets and the lander's clones |
| `/var/tessel/tessel-server.toml` | `_tessel` 0600 | names the paths above |
| `/var/tessel/handoff/` | `_tessel` 0755 | test trees, built with umask 022; public repo content only |
| `/var/tessel/cargo/` | `_tessel` 0755 | the runner reads it; only the fetch step writes |
| `/var/tessel/rustup/` | root:wheel 0755 | the runner reads, nothing it runs writes |
| `/var/tessel-runner/` and `work/`, `tmp/` | `_tessel-runner` 0700 | the runner's copy of a tree and its `TMPDIR` |
| `/var/tessel-runner/tessel-runner.toml` | `_tessel-runner` 0600 | `RunnerConfig::load` already demands owner and 0600 |
| `/var/log/tessel-server.log` | `_tessel` 0644 | quoted untrusted text, never the token (`credential.rs`) |
| `/etc/sudoers.d/tessel` | root:wheel 0440 | section 2 |
| `/Library/LaunchDaemons/dev.tessel.server.plist` | root:wheel 0644 | `UserName` `_tessel`, runs from `/opt/tessel/bin` |

The agents' tokens leave `<server dir>/tokens`: `tessel-server admin token` runs as `_tessel`
and writes to a Felix-owned directory (0600 each). They are not secrets from the lanes, which need
them. The key stays with `_tessel`. The server's `jobs = 6` cap moves from `<server dir>/.cargo`,
which cargo would no longer find above the runner's tree, to `/var/tessel/cargo/config.toml`.

Tests: a Linux CI job (section 8) checks each row's owner and mode and that the runner account
cannot read the token, key, store or `lander/`, nor write anything under `/opt/tessel`,
`/var/tessel`, `cargo` or `rustup`. A unit test in `credential.rs` already refuses a token file
with group or other bits.

## 2. The runner channel

The server runs `/usr/bin/sudo -n -u _tessel-runner -- /opt/tessel/bin/tessel-runner --config
/var/tessel-runner/tessel-runner.toml`, writes the request JSON to its standard input and reads
one result line, as `tessel-runner` does today (`main.rs`; exit 0 ran, 2 refused, 3 unsupervised).
`LocalRunner` is replaced by a `SudoRunner` behind the same `TestRunner` trait (replace, don't
keep both), and the `[lander] runner_config` key goes. The only authentication is who may run
sudo; there is no socket and nothing listening.

```
Defaults:_tessel env_reset, !setenv, !use_pty, secure_path="/usr/bin:/bin"
Cmnd_Alias TESSEL_RUN = \
  /opt/tessel/bin/tessel-runner --config /var/tessel-runner/tessel-runner.toml, \
  /opt/tessel/bin/tessel-runner --reset --config /var/tessel-runner/tessel-runner.toml, \
  /bin/kill -KILL -1
_tessel ALL = (_tessel-runner) NOPASSWD: TESSEL_RUN
```

What the rule pins, and why it cannot be widened by `_tessel`:
- **Path and arguments.** A sudoers command with arguments allows exactly those arguments, so
  `_tessel` cannot add a flag, a second config, or a different binary. No wildcard is used: in
  sudoers `*` also matches `/` and spaces.
- **Environment.** `env_reset` with `!setenv` rejects `sudo VAR=x` and keeps nothing from the
  server. The runner no longer reads `PATH`, `CARGO_HOME`, `RUSTUP_HOME` or `TMPDIR` from its
  environment (`INHERITED_ENV` in `config.rs`): they come from its own 0600 configuration file,
  so sudo's environment cannot matter. A test starts it with `LD_PRELOAD`, `DYLD_INSERT_LIBRARIES`
  and a hostile `PATH` and shows the test command sees none.
- **Who can edit it.** The binary, its directory, the rule file and the configuration are owned by
  root or by `_tessel-runner`, never by `_tessel`. A configuration owned by the runner is the
  runner's own check (`NotOwned`, `WrongMode`, `WrongUser`).
- **What the rule gives `_tessel`.** Running any test command as the runner is already the
  runner's job (the request names the commands), so the rule adds no power beyond that. The
  reverse direction is what matters: nothing the runner owns can make `_tessel` run anything.

The runner and `sudo` pass exit codes and bytes through. To probe: that `sudo` on the dogfood Mac
adds nothing to a piped standard input and output (`!use_pty` is there for that), and that its
`timestamp` and log files are not written under `/var/tessel`.

Tests: a round trip through the real `sudo` with a request of 1 MiB and a result at the tail
limit; each of the exit codes 0, 2 and 3 arrives; `sudo -n -u _tessel-runner` with another
argument, another binary, `VAR=x`, or `-i` fails as `_tessel`; `sudo -l -U _tessel` lists exactly
the three commands.

## 3. Cleanup and serialisation

The runner's process-group kill misses a child that calls `setsid` (RUNNER-SANDBOX,
`supervise.rs`). The account boundary replaces it: after every run, and also when the server
gave the call up at its deadline, the server runs `sudo -n -u _tessel-runner /bin/kill -KILL -1`.
`kill(-1)` signals every process the account may signal, so it takes the runner's children
wherever their group is, and cannot touch `_tessel`'s or Felix's. `pkill -U` from outside is not
used: `_tessel` could not signal another account's processes without root.

Then the server waits for `ps -U _tessel-runner -o pid=` to be empty, which needs no privilege,
for up to 10 s, repeating the kill. If processes remain, the runner gate stays closed: the call
answers `Unknown` and the next run does not start. `sudo` itself is root-owned and survives the
kill; it exits when its command does, so a server that gives up at its deadline kills the
runner account first, and does not try to signal `sudo`.

Because `kill -1` takes the whole account, no other run may be live. One process-wide async
mutex, the runner gate, covers `{run, kill, wait, reset}` across every repository of the server.
Test and trial runs both take it. A second server on the same Mac would break this and is not
supported; a lock file `/var/tessel-runner/lock`, held by flock, refuses it.

The server also runs kill and reset at start, before it answers a call. A stop in the middle of a
test no longer orphans the cargo process group, which `docs/dogfood.md` warns about today.

On Linux (a hosted server, later) the same contract is a cgroup scope per run:
`systemd-run --scope --uid _tessel-runner`, then `systemctl kill`.

Tests: a test command that forks a `setsid` child sleeping 60 s leaves no process after the run;
two overlapping calls never have two runner processes alive at once (a test double that records
start and end times); the gate stays closed when the kill leaves a survivor; the mutex is
released when a call is dropped mid-run.

## 4. Toolchain and dependencies

The runner's `HOME` is `/var/empty`, so a user-level `~/.cargo` and `~/.rustup` cannot exist.
- **Toolchain.** `RUSTUP_HOME=/var/tessel/rustup`, installed by Felix with `sudo` for the channel
  in `rust-toolchain.toml`, owned by root. The runner's `PATH` names that toolchain's `bin`, so
  `rustup` proxies, which would try to download, are not used. Before each `Test` the lander
  compares the toolchain directory's name with the channel in the head's `rust-toolchain.toml`;
  a mismatch is logged and answered `Unknown`, not a test failure (the file is a sensitive path,
  so a change is reviewed and Felix installs the new channel first).
- **Dependencies.** `CARGO_HOME=/var/tessel/cargo`, owned by `_tessel`, read-only to the runner. The
  runner tests with `--offline`, so the crates must be there. Repo code (a dependency's
  `build.rs`) could poison a cache it can write, and the poisoned crate would be compiled into a
  later submission's tests; a read-only cache closes that.
- **Who runs `cargo fetch`.** Not the runner, not repo code. `Cargo.lock`, `.cargo/` and
  `rust-toolchain.toml` are sensitive paths, so any submission that changes them is held and the
  orchestrator has read the change. Before approving such a submission the orchestrator runs
  `scripts/dogfood/fetch.sh <fork commit>`, which checks the commit out in a scratch directory and
  runs `sudo -u _tessel env CARGO_HOME=/var/tessel/cargo cargo fetch --locked` there. Fetch builds
  and runs nothing, and the config it reads is the reviewed one. A fetch that
  was skipped shows as a failing offline build, which the agent reads as failing tests; the
  script prints the reminder, and automating it is M6.
- **No warm `CARGO_TARGET_DIR`.** Each run builds in its own copy of the tree, so one submission
  cannot leave artefacts that another's tests link (rule 6). The cost is a cold build per
  landing, 237 s on 22 October, which is what a fresh clone costs today: no change. A cache
  shared between runs would be faster and is refused for that reason.

To probe: whether cargo builds with a read-only `CARGO_HOME` (it takes a lock file there). If it
does not, the runner copies `/var/tessel/cargo` into its work area at the start of each run, which
costs seconds, and leaves the rest of this section unchanged.

Tests: a `build.rs` that tries to write to `CARGO_HOME` and `RUSTUP_HOME` fails; a second
submission does not see a file the first wrote to `tmp/` or `work/`; a run with a missing crate
reports the fetch reminder in the server log.

## 5. The test tree across accounts

Three directories, three owners. `Test` (see `candidate-ref.md`) fetches the candidate ref into a
fresh repository with no credential. That repository is made by the lander as `_tessel` in
`/var/tessel/handoff/call-N/` and is world-readable, because it holds the repo's public tree.

The request's `tree` names it. The runner, as `_tessel-runner`, first copies it into
`/var/tessel-runner/work/` with plain `std::fs` (symbolic links copied as links, never followed;
no git, so rule 4 holds), then runs the test commands in the copy. The runner owns the copy and
everything the tests write in it; `_tessel` never needs to touch it.

Removal follows ownership:
- `_tessel` removes `handoff/call-N` (`CallDir.remove`, `clear_leftovers`), as it created it.
  Nothing the runner does can add a file there, because the runner cannot write that directory.
- `tessel-runner --reset --config ...`, the third sudo command, removes the contents of `work/`
  and `tmp/` as the runner. The server calls it after the kill, inside the runner gate. It refuses
  to follow symbolic links out of those two directories, and it takes no path argument.

Because the tests run on a copy, they cannot reach the lander's clones, and the `TestTree` types
in `workspace.rs` keep their meaning: the tree is never a `Workspace`. This also closes
M4-SANDBOX-WORKSPACE, which was about code finding the workspace by path.

Tests: the runner cannot write `handoff/`; after a run, `_tessel` can remove `handoff/call-N`
while `work/` is still full of runner-owned files; `--reset` empties a tree that contains a
read-only directory and a symlink to `/var/tessel`, and leaves `/var/tessel` untouched.

## 6. Upgrades

Once `/opt/tessel/bin` belongs to root, each upgrade is a sudo step. The alternative, a `bin/`
that stays writable by Felix, is refused: the lanes run as Felix, so any lane could replace
`tessel-server`, which then runs as `_tessel` and reads the token. That would undo the boundary
in one command.

Recommended: `sudo scripts/dogfood/upgrade.sh <built binaries>` (a Felix-run script, a sensitive
path, so a pull request he merges). It stops the daemon (`launchctl bootout system/dev.tessel
.server`), keeps the old binaries as `/opt/tessel/bin.prev`, installs the new ones, and starts
the daemon again. The rest of DOGFOOD-UPGRADE (f) stands: built only from a landed main with green
CI, and only with no dispatch in flight. Its words "from inside `~/tessel-dogfood`" change to
this script. A fix that cannot land through the server still goes through a pull request.

## 7. Escape routes, tested from the runner account

A one-off script, `scripts/dogfood/check-own-users.sh`, run by Felix once after the migration and
after any change to the rule or the layout, runs each probe as `_tessel-runner` through the real
sudo rule and prints pass or fail. The Linux CI job (section 8) runs the portable subset.

| Route | Probe, as the runner account | Expected |
|---|---|---|
| `at` | `echo id \| at now` | refused, or `atrun` is not loaded (to probe) |
| `cron` | `crontab -e` with a job | refused; if not, `cron.deny` names the account (to probe) |
| `launchctl` | `launchctl submit -l x -- /usr/bin/id` and `launchctl bootstrap` into its own domain | no job runs, or only as itself with no persistence beyond the kill (to probe) |
| Shared directories | write to `/tmp`, `/var/tmp`, `/Users/Shared` and find a file left by a previous run | writes allowed, but the kill and the reset remove what it owns; nothing it leaves is read by `_tessel` |
| The server socket | connect to `127.0.0.1:7878` | the connection opens, the handshake needs an agent token it cannot read (S1) |
| Felix's home | read `~/repos`, `~/.config/gh`, `~/.ssh` | denied; today `~` is 0755 on a default macOS account, so Felix's step 6 is `chmod go-rx ~` |
| The secrets | read `/var/tessel/github-token`, `keys/`, `stores/`, `lander/` | denied |
| The binaries | write `/opt/tessel/bin` | denied |
| Another account | `sudo -n -l`, `sudo -n -u _tessel` | denied |
| Process limits | fork bomb | not bounded; `kern.maxprocperuid` is global. The run timeout and the kill end it |
| Network egress | connect outward | allowed: a test can read the source and send it out. A `pf` rule by `user` is an option, to probe, not part of M5 |

Persistence is the point of the first three rows: whatever the account starts must not outlive
the kill and the reset. The probe for each leaves a marker and checks it after the next run's
cleanup. A route that works is a finding, not a pass: it gets a mitigation (`cron.deny`, an
unloaded `atrun`) added to the setup before RUNNER-OWN-UID closes.

## 8. What RUNNER-OWN-UID builds, and Felix's steps

Code (lane B; the files are its own, plus `tessel-server/src/{main,server,config}.rs` which the
phase plan already lists for this task):
1. `tessel-runner`: the environment from the configuration file; the work-area copy of the tree;
   `--reset`. Tests as above.
2. `tessel-server/src/landing`: `SudoRunner`, the runner gate, the kill, wait and reset, the
   start-up cleanup, and the `handoff` directory in place of `call-N/test`.
3. `scripts/dogfood/{accounts,install,upgrade,fetch,check-own-users}.sh`; `setup.sh` splits into
   the Felix-run part (the tokens) and the root-run part. `docs/dogfood.md` loses D4's caveat
   and gains the steps below. These are sensitive paths, so one pull request Felix merges.
4. A Linux CI job (a workflow pull request Felix merges, after the others in the M5 order): it
   creates the two users with `useradd`, installs the rule with `visudo -cf`, and checks that the
   token is unreadable, the workspace unwritable, a `setsid` child killed, and the rule refuses
   every widened command of section 2. GitHub's runners have passwordless sudo.

Felix's sudo steps on the Mac, in order, each with its check:
1. Read the output of `accounts.sh --dry-run`, then run it: the two groups and users. Check with
   `dscl . -read /Users/_tessel`.
2. `install.sh` creates `/opt/tessel`, `/var/tessel`, `/var/tessel-runner` with the modes of
   section 1 and installs the toolchain and `config.toml`. Check with `check-own-users.sh --modes`.
3. Install the binaries and the rule: `visudo -cf /etc/sudoers.d/tessel` before the move.
4. `chmod go-rx /Users/<felix>`.
5. With the server stopped and no dispatch in flight, move the store, the key and the token to
   `/var/tessel`. A move, not a snapshot restore (C11), so no fence is reissued.
6. Rotate. The old token and key were readable by every lane process: make a new GitHub token
   and a new signing key (`kid` `2026-11`), revoke the old token, and mint new agent tokens.
7. Load the LaunchDaemon, run `check-own-users.sh`, then land once on `tessel-sandbox/m4-trunk`
   (DOGFOOD-UPGRADE (e)).

D4's sentence "process control, not a security boundary" is replaced by what this gives: the
token, key and store are unreadable to repo code and to the lanes. It keeps the out-of-scope
paragraph above.

## 9. Alternatives considered

| Alternative | Why not |
|---|---|
| `sandbox-exec` with a profile that denies the token path | Same account, so everything the profile misses is open; Apple documents the tool as deprecated; kills still need a group |
| A VM or container per run | Docker on macOS needs a VM on a Mac already at load 55 to 75 (D3); heavy for one developer; right for the hosted server (M6) |
| launchd `UserName` and no sudo | The server could run as `_tessel`, but a one-shot child as another account needs a persistent runner daemon with a socket, its own authentication and its own cleanup |
| One extra account for both | The server's token would be readable by the tests |
| `sudo` with `NOPASSWD: ALL` for `_tessel` | Makes `_tessel` root-equivalent and moves the problem |
| Run the runner as root and drop privileges | A bug before the drop is a root bug |

#!/usr/bin/env bash
# Checks the account boundary of docs/proposals/own-users.md with real accounts on a Linux machine
# that has passwordless root, such as a GitHub-hosted runner. It makes the two accounts, the
# layout and the sudoers rule, then tries to cross the boundary and checks that the runner can do
# only what the design allows.
#
# Usage: sudo scripts/ci/own-users-linux.sh <tessel-runner binary>
#
# This changes the machine (users, /opt/tessel, /var/tessel, /etc/sudoers.d/tessel): run it only on
# a disposable one. The macOS specifics (dscl, launchd, cron and at denial, uchg flags) are checked
# by scripts/dogfood/check-own-users.sh on the Mac.
set -euo pipefail

fail() {
	echo "own-users-linux: $*" >&2
	exit 1
}

[ "$#" -eq 1 ] || fail "usage: own-users-linux.sh <tessel-runner binary>"
runner_binary=$1
[ -x "$runner_binary" ] || fail "$runner_binary is not an executable file"
[ "$(id -u)" = 0 ] || fail "run it as root"

server=_tessel
runner=_tessel-runner
config=/var/tessel-runner/tessel-runner.toml
failures=0

ok() { echo "ok: $*"; }
bad() {
	echo "FAIL: $*"
	failures=$((failures + 1))
}

# check <description> <command>...: the command must succeed.
check() {
	local description=$1
	shift
	if "$@" >/dev/null 2>&1; then ok "$description"; else bad "$description"; fi
}

# refuses <description> <command>...: the command must fail.
refuses() {
	local description=$1
	shift
	if "$@" >/dev/null 2>&1; then bad "$description"; else ok "$description"; fi
}

as_server() { sudo -n -u "$server" "$@"; }
as_runner() { sudo -n -u "$runner" "$@"; }
# through_rule <command>...: from the server's account, through the rule, as the server does.
through_rule() { sudo -n -u "$server" sudo -n -u "$runner" -- "$@"; }

echo "-- accounts and layout"
for name in "$server" "$runner"; do
	id "$name" >/dev/null 2>&1 ||
		useradd --system --user-group --no-create-home --home-dir /var/empty --shell /usr/sbin/nologin "$name"
done
install -d -o root -g root -m 0755 /opt/tessel /opt/tessel/bin /var/tessel /var/tessel-runner \
	/var/tessel/toolchain /var/tessel/toolchain/bin
for dir in keys stores lander tmp; do
	install -d -o "$server" -g "$server" -m 0700 "/var/tessel/$dir"
done
install -d -o "$server" -g "$server" -m 0755 /var/tessel/cargo
install -d -o "$server" -g "$runner" -m 0750 /var/tessel/handoff
install -d -o "$runner" -g "$runner" -m 0700 /var/tessel-runner/work /var/tessel-runner/tmp \
	/var/tessel-runner/shared
install -o root -g root -m 0755 "$runner_binary" /opt/tessel/bin/tessel-runner
printf 'not-a-real-token\n' | install -o "$server" -g "$server" -m 0600 /dev/stdin /var/tessel/github-token
install -o root -g root -m 0755 /dev/stdin /var/tessel/toolchain/bin/cargo <<'SH'
#!/bin/sh
case "$(cat marker 2>/dev/null)" in
setsid) setsid sleep 300 >/dev/null 2>&1 </dev/null & ;;
limits) echo "nproc=$(ulimit -Hu) core=$(ulimit -Sc)" ;;
esac
echo ran
SH
install -o root -g root -m 0644 /dev/stdin "$config" <<'TOML'
[runner]
user = "_tessel-runner"
timeout_secs = 120
path = "/var/tessel/toolchain/bin:/usr/bin:/bin"
cargo_home = "/var/tessel/cargo"
tmpdir = "/var/tessel-runner/tmp"
work_dir = "/var/tessel-runner/work"
handoff_dir = "/var/tessel/handoff"
max_processes = 1024
sweep_dirs = ["/var/tessel-runner/shared"]
TOML

echo "-- the sudoers rule"
candidate=$(mktemp)
cat >"$candidate" <<'RULE'
Defaults:_tessel env_reset, !setenv, !use_pty, secure_path="/usr/bin:/bin"
Cmnd_Alias TESSEL_RUN = \
  /opt/tessel/bin/tessel-runner --config /var/tessel-runner/tessel-runner.toml, \
  /bin/kill -KILL -1
_tessel ALL = (_tessel-runner) NOPASSWD: TESSEL_RUN
RULE
check "visudo accepts the rule" visudo -cf "$candidate"
install -o root -g root -m 0440 "$candidate" /etc/sudoers.d/tessel
unlink "$candidate"

echo "-- the runner cannot reach what it must not"
for path in /var/tessel/github-token /var/tessel/keys /var/tessel/stores /var/tessel/lander \
	/var/tessel/tmp; do
	refuses "the runner cannot read $path" as_runner test -r "$path"
done
for path in /opt/tessel/bin /var/tessel /var/tessel/toolchain/bin /var/tessel/cargo /var/tessel-runner \
	"$config" /var/tessel/handoff; do
	refuses "the runner cannot write $path" as_runner test -w "$path"
done
check "the runner can read the handoff directory" as_runner test -r /var/tessel/handoff
check "the runner can write its work directory" as_runner test -w /var/tessel-runner/work
refuses "the runner cannot rewrite its configuration" as_runner sh -c "echo x >> $config"
refuses "the runner cannot run sudo" as_runner sudo -n -l

echo "-- the rule allows two commands and nothing wider"
refuses "another program" through_rule /bin/sh -c id
refuses "another configuration" through_rule /opt/tessel/bin/tessel-runner --config /etc/passwd
refuses "an extra argument" through_rule /opt/tessel/bin/tessel-runner --config "$config" --extra
refuses "another signal" through_rule /bin/kill -TERM -1
refuses "a variable" as_server sudo -n -u "$runner" FOO=1 /opt/tessel/bin/tessel-runner --config "$config"
refuses "a login shell" as_server sudo -n -u "$runner" -i
refuses "root" as_server sudo -n -u root id

# make_tree <name> <marker>: a tree in the handoff directory, made by the server's account.
make_tree() {
	as_server sh -c "umask 022 && mkdir -p /var/tessel/handoff/$1/tree && printf %s '$2' > /var/tessel/handoff/$1/tree/marker"
}

# run_tree <name>: the runner through the rule, as the server does; prints its reply.
run_tree() {
	local request
	request=$(printf '{"tree":"/var/tessel/handoff/%s/tree","tests":[{"dir":"","argv":["cargo","test","--workspace"]}],"timeout_secs":60}' "$1")
	echo "$request" | through_rule /opt/tessel/bin/tessel-runner --config "$config"
}

echo "-- a run"
make_tree call-1 plain
reply=$(run_tree call-1) || reply=""
check "a run passes and replies with a result" jq -e '.passed == true and .step == "test"' <<<"$reply"
check "the tests ran" jq -e '.stdout | contains("ran")' <<<"$reply"
check "the tree was copied into the work directory" test -f /var/tessel-runner/work/tree/marker
check "the copy belongs to the runner" test "$(stat -c %U /var/tessel-runner/work/tree/marker)" = "$runner"
check "the handoff tree is untouched" test "$(stat -c %U /var/tessel/handoff/call-1/tree/marker)" = "$server"

echo "-- the next run starts clean"
as_runner sh -c 'touch /var/tessel-runner/work/leftover /var/tessel-runner/shared/leftover'
run_tree call-1 >/dev/null
refuses "a file left in the work directory is gone" test -e /var/tessel-runner/work/leftover
refuses "a file left in a swept shared directory is gone" test -e /var/tessel-runner/shared/leftover

echo "-- the limits"
make_tree call-3 limits
reply=$(run_tree call-3) || reply=""
check "the process limit is the configured one and core files are off" \
	jq -e '.stdout | contains("nproc=1024 core=0")' <<<"$reply"

echo "-- a child that leaves the process group survives the runner and not the kill"
make_tree call-2 setsid
run_tree call-2 >/dev/null
check "the setsid child outlives the run" test -n "$(ps -U "$runner" -o pid=)"
through_rule /bin/kill -KILL -1 || true
for _ in 1 2 3 4 5 6 7 8 9 10; do
	[ -z "$(ps -U "$runner" -o pid=)" ] && break
	sleep 0.5
done
check "kill -KILL -1 leaves no process of the runner's account" test -z "$(ps -U "$runner" -o pid=)"

if [ "$failures" -ne 0 ]; then
	fail "$failures checks failed"
fi
echo "own-users-linux: all checks passed"

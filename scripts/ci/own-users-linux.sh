#!/usr/bin/env bash
# Checks the account boundary of docs/proposals/own-users.md with real accounts on a Linux machine
# that has passwordless root, such as a GitHub-hosted runner. It makes the two accounts and the
# layout, installs the sudoers rule and the runner's configuration exactly as
# `scripts/dogfood/install.sh --dry-run` prints them (so they cannot drift from what the Mac gets),
# tries to cross the boundary, and runs the real-sudo tests of `SudoRunner` as the server's account.
#
# Usage: sudo scripts/ci/own-users-linux.sh <tessel-runner binary> <sudo_runner_real test binary>
#
# This changes the machine (users, /opt/tessel, /var/tessel, /etc/sudoers.d/tessel): run it only on
# a disposable one. The macOS specifics (dscl, launchd, cron and at denial, uchg flags) are checked
# by scripts/dogfood/check-own-users.sh on the Mac.
set -euo pipefail

fail() {
	echo "own-users-linux: $*" >&2
	exit 1
}

[ "$#" -eq 2 ] || fail "usage: own-users-linux.sh <tessel-runner binary> <test binary>"
runner_binary=$1
test_binary=$2
[ -x "$runner_binary" ] || fail "$runner_binary is not an executable file"
[ -x "$test_binary" ] || fail "$test_binary is not an executable file"
[ "$(id -u)" = 0 ] || fail "run it as root"
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

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

echo "-- what install.sh would write"
scratch=$(mktemp -d)
for binary in tessel-server tessel tessel-runner; do
	printf '#!/bin/sh\n' >"$scratch/$binary"
	chmod 0755 "$scratch/$binary"
done
"$root/scripts/dogfood/install.sh" --binaries "$scratch" --dry-run >"$scratch/dry-run.txt"
# dry_run_text <key>: the text under the dry-run line that contains <key>, without its margin.
dry_run_text() {
	awk -v key="$1" '
		index($0, key) > 0 && !/^  \| / { on = 1; next }
		on && /^  \| / { sub(/^  \| /, ""); print; next }
		on { exit }' "$scratch/dry-run.txt"
}
dry_run_text "would write /var/tessel-runner/tessel-runner.toml (" >"$scratch/runner.toml"
dry_run_text "install as /etc/sudoers.d/tessel (" >"$scratch/sudoers"
[ -s "$scratch/runner.toml" ] || fail "install.sh --dry-run printed no runner configuration"
[ -s "$scratch/sudoers" ] || fail "install.sh --dry-run printed no sudoers rule"

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
# setgid, so that a call directory made by the server takes the runner's group on Linux, as it does
# on macOS where a directory takes its parent's group.
install -d -o "$server" -g "$runner" -m 2750 /var/tessel/handoff
install -d -o "$runner" -g "$runner" -m 0700 /var/tessel-runner/work /var/tessel-runner/tmp
install -o root -g root -m 0755 "$runner_binary" /opt/tessel/bin/tessel-runner
printf 'not-a-real-token\n' | install -o "$server" -g "$server" -m 0600 /dev/stdin /var/tessel/github-token
# The stand-in toolchain: a `cargo` that does what the `marker` file in the tree says.
install -o root -g root -m 0755 /dev/stdin /var/tessel/toolchain/bin/cargo <<'SH'
#!/bin/sh
case "$(cat marker 2>/dev/null)" in
setsid) setsid sleep 300 >/dev/null 2>&1 </dev/null & ;;
# dash (the Ubuntu /bin/sh) names the process limit -p where bash names it -u.
limits) echo "nproc=$(ulimit -Hu 2>/dev/null || ulimit -Hp) core=$(ulimit -Sc)" ;;
env) env ;;
big) head -c 600000 /dev/zero | tr '\0' x ;;
tmp-write) echo x > "$TMPDIR/seen" ;;
tmp-list) ls -A "$TMPDIR" ;;
mess) mkdir -p ../old/ro && chmod 500 ../old/ro && ln -s /var/tessel/keys ../link ;;
slow) sleep 1 ;;
sleep) setsid sleep 300 >/dev/null 2>&1 </dev/null & sleep 300 ;;
esac
echo ran
SH
install -o root -g root -m 0644 "$scratch/runner.toml" "$config"

echo "-- the sudoers rule"
check "visudo accepts the rule" visudo -cf "$scratch/sudoers"
install -o root -g root -m 0440 "$scratch/sudoers" /etc/sudoers.d/tessel
listing=$(sudo -l -U "$server" | awk '/may run the following commands/ { on = 1; next } on && NF { sub(/^[ \t]+/, ""); sub(/\\$/, ""); print }' | tr '\n\t' '  ' | tr -s ' ' | sed -e 's/^ //' -e 's/ $//')
want="($runner) NOPASSWD: /opt/tessel/bin/tessel-runner --config $config, /bin/kill -KILL -1"
if [ "$listing" = "$want" ]; then ok "sudo -l -U lists exactly the two commands"; else bad "sudo -l -U lists: $listing"; fi

echo "-- the runner cannot reach what it must not"
for path in /var/tessel/github-token /var/tessel/keys /var/tessel/stores /var/tessel/lander \
	/var/tessel/tmp; do
	refuses "the runner cannot read $path" as_runner test -r "$path"
done
for path in /opt/tessel/bin /var/tessel /var/tessel/toolchain/bin /var/tessel/cargo /var/tessel-runner \
	"$config" /var/tessel/handoff; do
	refuses "the runner cannot write $path" as_runner test -w "$path"
done
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

echo "-- a call directory is closed to the runner unless its own run holds the gate"
as_server sh -c 'umask 022 && mkdir -p /var/tessel/handoff/other-call/test && chmod 0700 /var/tessel/handoff/other-call'
refuses "the runner cannot enter a closed call directory" as_runner test -r /var/tessel/handoff/other-call/test

echo "-- SudoRunner through the real rule, as the server"
install -o "$server" -g "$server" -m 0755 "$test_binary" /var/tessel/tmp/sudo_runner_real
if sudo -n -u "$server" env HOME=/var/empty /var/tessel/tmp/sudo_runner_real --ignored --test-threads=1; then
	ok "the real-sudo tests of SudoRunner pass"
else
	bad "the real-sudo tests of SudoRunner failed"
fi

echo "-- the next run starts clean in a shared directory"
as_runner touch /tmp/own-users-leftover
# Any run sweeps it; the test binary's last run has already, so make one more.
# The directory is 0750, open to the runner's group, as SudoRunner makes it while its run holds
# the gate; these runs go through the rule directly, without SudoRunner.
make_tree() {
	as_server sh -c "umask 022 && mkdir -p /var/tessel/handoff/$1/test && printf %s '$2' > /var/tessel/handoff/$1/test/marker && chmod 0750 /var/tessel/handoff/$1"
}
make_tree sweep-check plain
printf '{"tree":"/var/tessel/handoff/sweep-check/test","tests":[{"dir":"","argv":["cargo","test","--workspace"]}],"timeout_secs":60}' |
	through_rule /opt/tessel/bin/tessel-runner --config "$config" >/dev/null || bad "the sweep-check run failed"
refuses "a file left in /tmp by the account is gone" test -e /tmp/own-users-leftover

echo "-- the limits"
make_tree limits-check limits
reply=$(printf '{"tree":"/var/tessel/handoff/limits-check/test","tests":[{"dir":"","argv":["cargo","test","--workspace"]}],"timeout_secs":60}' |
	through_rule /opt/tessel/bin/tessel-runner --config "$config") || reply=""
check "the process limit is the default, 1024, and core files are off" \
	jq -e '.stdout | contains("nproc=1024 core=0")' <<<"$reply"

if [ "$failures" -ne 0 ]; then
	fail "$failures checks failed"
fi
echo "own-users-linux: all checks passed"

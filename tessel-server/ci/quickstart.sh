#!/usr/bin/env bash
# Runs the fenced blocks of docs/local-setup.md as written and checks the walk's last step.
#
# The blocks are read from the page at run time, with <server dir> and <remote> replaced by paths
# in a temporary directory, so a page that drifts from the tree fails here. The walk starts a real
# `tessel-server` and two real `tessel` agents, lands one commit on a bare remote through the
# runner, and ends with the commit on the remote's main and the agent's inbox saying merged.
#
# Needs target/debug/{tessel-server,tessel} (cargo build -p tessel-server -p tessel-cli), git and
# cargo on PATH, and port 7878 free (the page's listen address). Gives up after 10 minutes.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
bin="${TESSEL_BIN_DIR:-$root/target/debug}"
page="$root/docs/local-setup.md"
work=$(mktemp -d)
watchdog_pid=""

fail() {
	echo "quickstart: $*" >&2
	exit 1
}

# Ends the script after 10 minutes, whatever the walk is waiting for.
watchdog() {
	local parent=$1 napper
	sleep 600 &
	napper=$!
	trap 'kill "$napper" 2>/dev/null; exit 0' TERM
	wait "$napper" || exit 0
	echo "quickstart: gave up after 10 minutes" >&2
	kill -TERM "$parent"
	# bash runs the parent's TERM trap only once its foreground command returns.
	pkill -TERM -P "$parent" 2>/dev/null || true
}

cleanup() {
	local status=$?
	trap - EXIT
	for dir in "$work"/play/*; do
		[ -d "$dir/.tessel" ] && (cd "$dir" && "$bin/tessel" stop >/dev/null 2>&1) || true
	done
	local pids
	pids=$(jobs -p)
	# shellcheck disable=SC2086 # one pid per word
	[ -z "$pids" ] || kill $pids 2>/dev/null || true
	wait 2>/dev/null || true
	if [ "$status" -ne 0 ] && [ -f "$work/server/server.log" ]; then
		echo "--- server log ---" >&2
		tail -n 60 "$work/server/server.log" >&2
	fi
	rm -rf -- "$work"
	exit "$status"
}
trap cleanup EXIT
trap 'exit 143' TERM

[ -x "$bin/tessel-server" ] && [ -x "$bin/tessel" ] || fail "build first: no binaries in $bin"
if (exec 3<>/dev/tcp/127.0.0.1/7878) 2>/dev/null; then
	fail "port 7878 is in use; the page's server listens there"
fi

export GIT_AUTHOR_NAME="Quickstart Author" GIT_AUTHOR_EMAIL="author@example.invalid"
export GIT_COMMITTER_NAME="Quickstart Committer" GIT_COMMITTER_EMAIL="committer@example.invalid"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export PATH="$bin:$PATH"

server="$work/server"
remote="$work/remote.git"
mkdir -p "$work/play"

# The page's fenced blocks, in order, with the two placeholders filled in.
awk '/^```/ { inside = !inside; next } inside' "$page" |
	sed -e "s|<server dir>|$server|g" -e "s|<remote>|$remote|g" >"$work/walk.sh"
[ -s "$work/walk.sh" ] || fail "no fenced blocks found in $page"
for placeholder in '<server dir>' '<remote>'; do
	grep -q "$placeholder" "$page" || fail "the page no longer uses $placeholder"
done
if grep -q '<server dir>\|<remote>' "$work/walk.sh"; then fail "a placeholder was left over"; fi

watchdog $$ &
watchdog_pid=$!
cd "$work/play"
# Sourced, not run, so the server it starts is a job of this shell and cleanup can end it.
# shellcheck source=/dev/null
source "$work/walk.sh"

landed=$(git --git-dir="$remote" rev-parse main)
[ "$(git --git-dir="$remote" rev-list --count main)" -eq 2 ] ||
	fail "main is not the trunk plus one commit"
[ "$(git --git-dir="$remote" log -1 --format=%s main)" = "change the login" ] ||
	fail "main's newest commit is not the agent's"
git --git-dir="$remote" show main:src/auth/login.rs | grep -q '43' || fail "main lacks the change"
inbox=$(cd "$work/play/a1" && tessel inbox --all)
grep -q "is merged; main is now at $landed" <<<"$inbox" ||
	fail "the agent's inbox does not say merged at $landed: $inbox"

kill "$watchdog_pid" 2>/dev/null || true
echo "quickstart: the page's walk landed $landed on the remote's main and the inbox says merged"

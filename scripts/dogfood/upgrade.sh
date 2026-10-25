#!/bin/bash
# Replaces the server's binaries (docs/proposals/own-users.md, section 6): stop the daemon, keep
# the old binaries as /opt/tessel/bin.prev, install the new ones, start the daemon.
#
# Usage: sudo /opt/tessel/sbin/upgrade.sh --binaries <dir> [--yes] [--dry-run]
#
# <dir> holds tessel-server, tessel and tessel-runner, built from a landed main with green CI.
# The script copies them into /opt/tessel/tmp first and prints the sha256 of those copies, so what
# is shown is what is installed, then asks for a typed yes unless --yes. Run it only with no
# dispatch in flight. If the new daemon does not start, the old binaries are put back and started.
# --dry-run prints each step and needs neither root nor the installed location.
set -euo pipefail

if [ "${TESSEL_CLEAN:-}" != 1 ]; then
	exec /usr/bin/env -i TESSEL_CLEAN=1 PATH=/usr/bin:/bin:/usr/sbin:/sbin /bin/bash "$0" "$@"
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dry=0
yes=0
binaries=""
label=system/dev.tessel.server
plist=/Library/LaunchDaemons/dev.tessel.server.plist
scratch=/opt/tessel/tmp
work=""

fail() {
	echo "upgrade: $*" >&2
	exit 1
}

cleanup() {
	if [ -n "$work" ] && [ -d "$work" ]; then
		/bin/rm -r "$work"
	fi
}
trap cleanup EXIT

usage="usage: upgrade.sh --binaries <dir> [--yes] [--dry-run]"
while [ "$#" -gt 0 ]; do
	case "$1" in
	--dry-run) dry=1 ;;
	--yes) yes=1 ;;
	--binaries)
		[ "$#" -ge 2 ] || fail "$usage"
		binaries=$2
		shift
		;;
	*) fail "$usage" ;;
	esac
	shift
done
[ -n "$binaries" ] || fail "$usage"
for binary in tessel-server tessel tessel-runner; do
	[ -f "$binaries/$binary" ] && [ ! -L "$binaries/$binary" ] && [ -x "$binaries/$binary" ] ||
		fail "$binaries/$binary is not an executable regular file"
done
if [ "$dry" != 1 ]; then
	[ "$(/usr/bin/uname -s)" = Darwin ] || fail "this script upgrades a macOS daemon"
	[ "$(/usr/bin/id -u)" = 0 ] || fail "run it as root: sudo /opt/tessel/sbin/upgrade.sh"
	[ "$here" = /opt/tessel/sbin ] || fail "run the installed copy, /opt/tessel/sbin/upgrade.sh, not $here"
	[ -f "$plist" ] || fail "$plist is missing: run install.sh first"
	[ -e /var/tessel/github-token ] ||
		fail "the move is not done (no /var/tessel/github-token): upgrade.sh would enable a daemon that was disabled"
fi

act() {
	if [ "$dry" = 1 ]; then
		echo "would run: $*"
	else
		"$@"
	fi
}

loaded() {
	/bin/launchctl print "$label" >/dev/null 2>&1
}

if [ "$dry" = 1 ]; then
	echo "would copy the three binaries into $scratch, print their sha256, and ask for yes"
else
	work=$(/usr/bin/mktemp -d "$scratch/upgrade.XXXXXX")
	for binary in tessel-server tessel tessel-runner; do
		/usr/bin/install -m 0755 "$binaries/$binary" "$work/$binary"
		echo "new $binary: $(/usr/bin/shasum -a 256 "$work/$binary" | /usr/bin/awk '{ print $1 }')"
	done
	if [ "$yes" != 1 ]; then
		printf 'No dispatch in flight, and these built from a landed main? Type yes: '
		read -r answer
		[ "$answer" = yes ] || fail "not confirmed; nothing changed"
	fi
fi

if [ "$dry" = 1 ]; then
	act /bin/launchctl bootout "$label"
elif loaded; then
	/bin/launchctl bootout "$label" || fail "cannot stop the daemon; nothing changed"
else
	echo "the daemon was not loaded"
fi
if [ -d /opt/tessel/bin.prev ]; then
	act /bin/rm -r /opt/tessel/bin.prev
fi
act /bin/mv /opt/tessel/bin /opt/tessel/bin.prev

restore() {
	echo "upgrade: the new daemon did not start; putting the old binaries back" >&2
	/bin/launchctl bootout "$label" >/dev/null 2>&1 || true
	/bin/rm -r /opt/tessel/bin 2>/dev/null || true
	/bin/mv /opt/tessel/bin.prev /opt/tessel/bin
	/bin/launchctl enable "$label" || true
	/bin/launchctl bootstrap system "$plist" || echo "upgrade: the old daemon did not start either" >&2
	exit 1
}

if [ "$dry" = 1 ]; then
	act /usr/bin/install -d -o root -g wheel -m 0755 /opt/tessel/bin
	for binary in tessel-server tessel tessel-runner; do
		echo "would run: /usr/bin/install -o root -g wheel -m 0755 <the copy of $binary> /opt/tessel/bin/$binary"
	done
	act /bin/launchctl enable "$label"
	act /bin/launchctl bootstrap system "$plist"
else
	/usr/bin/install -d -o root -g wheel -m 0755 /opt/tessel/bin || restore
	for binary in tessel-server tessel tessel-runner; do
		/usr/bin/install -o root -g wheel -m 0755 "$work/$binary" "/opt/tessel/bin/$binary" || restore
	done
	/bin/launchctl enable "$label" || restore
	/bin/launchctl bootstrap system "$plist" || restore
fi
echo "upgrade: done. The old binaries are in /opt/tessel/bin.prev; tail /var/log/tessel-server.log"

#!/bin/bash
# Replaces the server's binaries (docs/proposals/own-users.md, section 6): stop the daemon, keep
# the old binaries as /opt/tessel/bin.prev, install the new ones, start the daemon.
#
# Usage: sudo /opt/tessel/sbin/upgrade.sh --binaries <dir> [--yes] [--dry-run]
#
# <dir> holds tessel-server, tessel and tessel-runner, built from a landed main with green CI.
# Run it only with no dispatch in flight (a stop in the middle of a test is cleaned up by the
# server's next start, but the landing is retried); it asks for a typed yes unless --yes.
# --dry-run prints each step and needs no root.
set -euo pipefail

if [ "${TESSEL_CLEAN:-}" != 1 ]; then
	exec /usr/bin/env -i TESSEL_CLEAN=1 PATH=/usr/bin:/bin:/usr/sbin:/sbin /bin/bash "$0" "$@"
fi

dry=0
yes=0
binaries=""
label=system/dev.tessel.server
plist=/Library/LaunchDaemons/dev.tessel.server.plist

fail() {
	echo "upgrade: $*" >&2
	exit 1
}

while [ "$#" -gt 0 ]; do
	case "$1" in
	--dry-run) dry=1 ;;
	--yes) yes=1 ;;
	--binaries)
		[ "$#" -ge 2 ] || fail "--binaries needs a directory"
		binaries=$2
		shift
		;;
	*) fail "usage: upgrade.sh --binaries <dir> [--yes] [--dry-run]" ;;
	esac
	shift
done
[ -n "$binaries" ] || fail "usage: upgrade.sh --binaries <dir> [--yes] [--dry-run]"
for binary in tessel-server tessel tessel-runner; do
	[ -f "$binaries/$binary" ] && [ -x "$binaries/$binary" ] || fail "$binaries/$binary is not an executable file"
done
if [ "$dry" != 1 ]; then
	[ "$(/usr/bin/uname -s)" = Darwin ] || fail "this script upgrades a macOS daemon"
	[ "$(/usr/bin/id -u)" = 0 ] || fail "run it as root: sudo /opt/tessel/sbin/upgrade.sh"
	[ -f "$plist" ] || fail "$plist is missing: run install.sh first"
fi

act() {
	if [ "$dry" = 1 ]; then
		echo "would run: $*"
	else
		"$@"
	fi
}

for binary in tessel-server tessel tessel-runner; do
	echo "new $binary: $(/usr/bin/shasum -a 256 "$binaries/$binary" | /usr/bin/awk '{ print $1 }')"
done
if [ "$dry" != 1 ] && [ "$yes" != 1 ]; then
	printf 'No dispatch in flight, and these binaries were built from a landed main? Type yes: '
	read -r answer
	[ "$answer" = yes ] || fail "not confirmed; nothing changed"
fi

act /bin/launchctl bootout "$label" || echo "the daemon was not loaded"
if [ -d /opt/tessel/bin.prev ]; then
	act /bin/rm -r /opt/tessel/bin.prev
fi
act /bin/mv /opt/tessel/bin /opt/tessel/bin.prev
act /usr/bin/install -d -o root -g wheel -m 0755 /opt/tessel/bin
for binary in tessel-server tessel tessel-runner; do
	act /usr/bin/install -o root -g wheel -m 0755 "$binaries/$binary" "/opt/tessel/bin/$binary"
done
act /bin/launchctl bootstrap system "$plist"
echo "upgrade: done. The old binaries are in /opt/tessel/bin.prev; tail /var/log/tessel-server.log"

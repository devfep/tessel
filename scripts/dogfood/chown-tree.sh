#!/bin/bash
# Changes the owner of the files that move between your home and /var/tessel, and nothing else
# (docs/proposals/own-users.md, section 8: the move and the rollback).
#
# Usage: sudo /opt/tessel/sbin/chown-tree.sh --from <user> --to <user:group> [--dry-run] <path>...
#
# `chown -R` follows nothing but it does change a hard link: a lane that plants a link to
# /etc/sudoers in a tree that root then chowns makes sudoers the owner's. So this script refuses a
# path that is not a regular file or a directory (no link, socket or device), a regular file with a
# link count above 1, and any entry whose owner is not --from, and it does so for the whole tree
# before it changes anything. It then changes the owner entry by entry, with `chown -h`, checking
# each entry again just before. Run it with the server stopped and the lanes idle: the second
# check narrows the window for a lane that replaces an entry in between, it does not close it.
# --dry-run prints what it would change and needs no root.
set -euo pipefail

if [ "${TESSEL_CLEAN:-}" != 1 ]; then
	exec /usr/bin/env -i TESSEL_CLEAN=1 PATH=/usr/bin:/bin:/usr/sbin:/sbin /bin/bash "$0" "$@"
fi

dry=0
from=""
to=""
paths=()

fail() {
	echo "chown-tree: $*" >&2
	exit 1
}

usage="usage: chown-tree.sh --from <user> --to <user:group> [--dry-run] <path>..."
while [ "$#" -gt 0 ]; do
	case "$1" in
	--dry-run) dry=1 ;;
	--from)
		[ "$#" -ge 2 ] || fail "$usage"
		from=$2
		shift
		;;
	--to)
		[ "$#" -ge 2 ] || fail "$usage"
		to=$2
		shift
		;;
	-*) fail "$usage" ;;
	*) paths+=("$1") ;;
	esac
	shift
done
[ -n "$from" ] && [ -n "$to" ] && [ "${#paths[@]}" -gt 0 ] || fail "$usage"
if [ "$dry" != 1 ]; then
	[ "$(/usr/bin/uname -s)" = Darwin ] || fail "this script runs on macOS"
	[ "$(/usr/bin/id -u)" = 0 ] || fail "run it as root: sudo /opt/tessel/sbin/chown-tree.sh"
	here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
	[ "$here" = /opt/tessel/sbin ] || fail "run the installed copy, /opt/tessel/sbin/chown-tree.sh, not $here"
fi

# Prints "type links owner" of $1: the file type word of stat, the link count and the owner.
describe() {
	/usr/bin/stat -f '%HT|%l|%Su' "$1"
}

# Fails unless $1 is a regular file with one link or a directory, owned by --from.
vet() {
	local entry=$1 info type links owner
	info=$(describe "$entry")
	type=${info%%|*}
	links=${info#*|}
	owner=${links#*|}
	links=${links%%|*}
	case "$type" in
	"Regular File")
		[ "$links" = 1 ] || fail "$entry has $links links: a hard link, refused"
		;;
	Directory) ;;
	*) fail "$entry is a '$type', not a regular file or a directory: refused" ;;
	esac
	[ "$owner" = "$from" ] || fail "$entry is owned by $owner, not $from: refused"
}

for path in "${paths[@]}"; do
	case "$path" in
	/*) ;;
	*) fail "$path is not an absolute path" ;;
	esac
	[ -e "$path" ] || [ -L "$path" ] || fail "$path does not exist"
	vet "$path"
	if [ -d "$path" ] && [ ! -L "$path" ]; then
		while IFS= read -r -d '' entry; do
			vet "$entry"
		done < <(/usr/bin/find "$path" -mindepth 1 -xdev -print0)
	fi
done

count=0
for path in "${paths[@]}"; do
	while IFS= read -r -d '' entry; do
		vet "$entry"
		if [ "$dry" = 1 ]; then
			echo "would chown $to $entry"
		else
			/usr/sbin/chown -h "$to" "$entry"
		fi
		count=$((count + 1))
	done < <(/usr/bin/find "$path" -xdev -print0)
done
if [ "$dry" = 1 ]; then
	echo "chown-tree: $count entries would change to $to"
else
	echo "chown-tree: $count entries changed to $to"
fi

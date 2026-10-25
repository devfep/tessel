#!/bin/bash
# Makes the two service accounts of docs/proposals/own-users.md, section 1, and denies cron and at
# to the runner account.
#
# Usage: sudo /opt/tessel/sbin/accounts.sh [--dry-run [--dscl <path>]]
#
# _tessel (481) runs the server and the lander; _tessel-runner (482) runs repo code. Each has a
# group of its own with the same number, shell /usr/bin/false, home /var/empty, password '*', and
# is hidden. The script aborts if either number is taken, as a user or as a group, by another
# name. It is safe to run again, also after a run that stopped half way: every attribute of an
# account that exists is set again.
# --dry-run changes nothing and needs no root; --dscl is accepted only with it, so a test can
# put a fake dscl in front of the real one.
set -euo pipefail

if [ "${TESSEL_CLEAN:-}" != 1 ]; then
	exec /usr/bin/env -i TESSEL_CLEAN=1 PATH=/usr/bin:/bin:/usr/sbin:/sbin /bin/bash "$0" "$@"
fi

dscl=/usr/bin/dscl
dry=0
tessel_name=_tessel
tessel_id=481
runner_name=_tessel-runner
runner_id=482
cron_deny=/var/at/cron.deny
at_deny=/var/at/at.deny

fail() {
	echo "accounts: $*" >&2
	exit 1
}

while [ "$#" -gt 0 ]; do
	case "$1" in
	--dry-run) dry=1 ;;
	--dscl)
		[ "$#" -ge 2 ] || fail "--dscl needs a path"
		dscl=$2
		shift
		;;
	*) fail "usage: accounts.sh [--dry-run [--dscl <path>]]" ;;
	esac
	shift
done
if [ "$dscl" != /usr/bin/dscl ] && [ "$dry" != 1 ]; then
	fail "--dscl is only accepted with --dry-run"
fi
if [ "$dry" = 1 ]; then
	# Only a test points these elsewhere, and only in a dry run, which writes nothing.
	cron_deny=${TESSEL_CRON_DENY:-$cron_deny}
	at_deny=${TESSEL_AT_DENY:-$at_deny}
fi
if [ "$dry" != 1 ]; then
	[ "$(/usr/bin/uname -s)" = Darwin ] || fail "this script makes macOS accounts"
	[ "$(/usr/bin/id -u)" = 0 ] || fail "run it as root: sudo /opt/tessel/sbin/accounts.sh"
	here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
	[ "$here" = /opt/tessel/sbin ] || fail "run the installed copy, /opt/tessel/sbin/accounts.sh, not $here"
fi

# Prints the name that holds number $3 in the dscl list of record type $1 and number key $2.
holder() {
	"$dscl" . -list "$1" "$2" | /usr/bin/awk -v n="$3" '$2 == n { print $1 }'
}

# Aborts when `name` or `number` is held by something else, as a user or as a group.
check_free() {
	local name=$1 number=$2 held
	held=$(holder /Users UniqueID "$number")
	if [ -n "$held" ] && [ "$held" != "$name" ]; then
		fail "UID $number is taken by $held"
	fi
	held=$(holder /Groups PrimaryGroupID "$number")
	if [ -n "$held" ] && [ "$held" != "$name" ]; then
		fail "GID $number is taken by $held"
	fi
	if "$dscl" . -read "/Users/$name" UniqueID >/dev/null 2>&1; then
		held=$("$dscl" . -read "/Users/$name" UniqueID | /usr/bin/awk '{ print $2 }')
		[ "$held" = "$number" ] || fail "$name exists with UID $held, not $number"
	fi
	if "$dscl" . -read "/Groups/$name" PrimaryGroupID >/dev/null 2>&1; then
		held=$("$dscl" . -read "/Groups/$name" PrimaryGroupID | /usr/bin/awk '{ print $2 }')
		[ "$held" = "$number" ] || fail "group $name exists with GID $held, not $number"
	fi
}

do_dscl() {
	if [ "$dry" = 1 ]; then
		echo "would run: dscl . $*"
	else
		"$dscl" . "$@"
	fi
}

make_account() {
	local name=$1 number=$2
	do_dscl -create "/Groups/$name" PrimaryGroupID "$number"
	do_dscl -create "/Groups/$name" Password '*'
	if "$dscl" . -read "/Users/$name" UniqueID >/dev/null 2>&1; then
		echo "user $name ($number) exists; setting every attribute again"
	fi
	do_dscl -create "/Users/$name" UniqueID "$number"
	do_dscl -create "/Users/$name" PrimaryGroupID "$number"
	do_dscl -create "/Users/$name" UserShell /usr/bin/false
	do_dscl -create "/Users/$name" NFSHomeDirectory /var/empty
	do_dscl -create "/Users/$name" RealName "Tessel service account $name"
	do_dscl -create "/Users/$name" Password '*'
	do_dscl -create "/Users/$name" IsHidden 1
}

deny_file() {
	local file=$1 name=$2
	if [ -f "$file" ] && /usr/bin/grep -qx -- "$name" "$file"; then
		echo "$name is already in $file"
	elif [ "$dry" = 1 ]; then
		echo "would append $name to $file"
	else
		# A file that does not end in a newline would merge the name into its last line.
		if [ -s "$file" ] && [ "$(/usr/bin/tail -c 1 "$file" | /usr/bin/wc -l)" -eq 0 ]; then
			echo >>"$file"
		fi
		echo "$name" >>"$file"
		echo "appended $name to $file"
	fi
}

check_free "$tessel_name" "$tessel_id"
check_free "$runner_name" "$runner_id"
make_account "$tessel_name" "$tessel_id"
make_account "$runner_name" "$runner_id"
deny_file "$cron_deny" "$runner_name"
deny_file "$at_deny" "$runner_name"
echo "accounts: done. Neither account is in staff or any other group; check with check-own-users.sh"

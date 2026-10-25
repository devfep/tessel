#!/usr/bin/env bash
# Tests accounts.sh and check-own-users.sh without root, against a fake dscl, id and sudo.
#
# Usage: scripts/dogfood/own-users.test.sh
# Nothing here touches an account: accounts.sh runs only with --dry-run, and check-own-users.sh
# only reads.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
accounts=$root/scripts/dogfood/accounts.sh
check=$root/scripts/dogfood/check-own-users.sh
work=$(mktemp -d)
trap 'rm -r "$work"' EXIT
failures=0

# A fake dscl over the files in $1: list/Users and list/Groups hold "name number" lines, and
# rec/<Users|Groups>/<name>/<Attribute> holds one value. The directory is written into the script,
# because accounts.sh clears the environment before it runs.
make_dscl() {
	local dir=$1
	mkdir -p "$dir/list" "$dir/rec"
	: >"$dir/list/Users"
	: >"$dir/list/Groups"
	cat >"$dir/dscl" <<SCRIPT
#!/bin/sh
case "\$2" in
-list) cat "$dir/list\$3" ;;
-read) cat "$dir/rec\$3/\$4" 2>/dev/null | sed "s/^/\$4: /" | grep . || exit 1 ;;
-create) echo "unexpected -create" >&2; exit 99 ;;
esac
SCRIPT
	chmod +x "$dir/dscl"
}

# add_record <dir> <Users|Groups> <name> <number> [attribute value]...
add_record() {
	local dir=$1 kind=$2 name=$3 number=$4
	shift 4
	local key=UniqueID
	[ "$kind" = Groups ] && key=PrimaryGroupID
	echo "$name $number" >>"$dir/list/$kind"
	mkdir -p "$dir/rec/$kind/$name"
	echo "$number" >"$dir/rec/$kind/$name/$key"
	while [ "$#" -ge 2 ]; do
		echo "$2" >"$dir/rec/$kind/$name/$1"
		shift 2
	done
}

add_good_account() {
	local dir=$1 name=$2 number=$3
	add_record "$dir" Groups "$name" "$number"
	add_record "$dir" Users "$name" "$number" PrimaryGroupID "$number" UserShell /usr/bin/false \
		NFSHomeDirectory /var/empty IsHidden 1
}

expect() {
	local label=$1 want=$2 pattern=$3
	shift 3
	local got=0
	"$@" >"$work/out.txt" 2>&1 || got=$?
	if [ "$got" -ne "$want" ] || ! grep -q -- "$pattern" "$work/out.txt"; then
		echo "FAIL: $label: exit $got, want $want, pattern '$pattern'"
		sed 's/^/  | /' "$work/out.txt"
		failures=$((failures + 1))
	else
		echo "ok: $label"
	fi
}

free=$work/free
make_dscl "$free"
expect "a dry run on free numbers plans both accounts" 0 "would run: dscl . -create /Groups/_tessel-runner PrimaryGroupID 482" \
	"$accounts" --dry-run --dscl "$free/dscl"
expect "a dry run makes the user with the shell and the home" 0 "UserShell /usr/bin/false" \
	"$accounts" --dry-run --dscl "$free/dscl"
expect "a dry run appends the runner to cron.deny" 0 "would append _tessel-runner to /var/at/cron.deny" \
	"$accounts" --dry-run --dscl "$free/dscl"

uid_taken=$work/uid
make_dscl "$uid_taken"
add_record "$uid_taken" Users someone 481
expect "a taken UID aborts" 1 "UID 481 is taken by someone" \
	"$accounts" --dry-run --dscl "$uid_taken/dscl"

gid_taken=$work/gid
make_dscl "$gid_taken"
add_record "$gid_taken" Groups somegroup 482
expect "a taken GID aborts" 1 "GID 482 is taken by somegroup" \
	"$accounts" --dry-run --dscl "$gid_taken/dscl"

moved=$work/moved
make_dscl "$moved"
add_record "$moved" Users _tessel 400
expect "an account with another number aborts" 1 "_tessel exists with UID 400, not 481" \
	"$accounts" --dry-run --dscl "$moved/dscl"

done_dir=$work/done
make_dscl "$done_dir"
add_good_account "$done_dir" _tessel 481
add_good_account "$done_dir" _tessel-runner 482
expect "accounts in place are left alone" 0 "user _tessel-runner (482) is in place" \
	"$accounts" --dry-run --dscl "$done_dir/dscl"
if grep -q "would run: dscl" "$work/out.txt"; then
	echo "FAIL: accounts in place were planned again"
	failures=$((failures + 1))
fi

expect "--dscl without --dry-run is refused" 1 "only accepted with --dry-run" "$accounts" --dscl "$free/dscl"
expect "an unknown flag is refused" 1 "usage" "$accounts" --frobnicate

# check-own-users.sh against fakes. $HOME is a directory of our own with the three private
# directories the probes look for, at mode 750.
home=$work/home
mkdir -p "$home/repos" "$home/.ssh" "$home/.config/gh"
chmod 750 "$home"
printf '%s\n' Guest _tessel-runner >"$work/deny"
cat >"$work/id" <<'SCRIPT'
#!/bin/sh
echo "$2"
SCRIPT
chmod +x "$work/id"
cat >"$work/sudo" <<SCRIPT
#!/bin/sh
# sudo -n -u <user> /usr/bin/true | /bin/test -r <path> | <sudo> -n -l
case "\$4" in
/usr/bin/true) exit 0 ;;
/bin/test) exit \$(cat "$work/test-exit") ;;
*) exit \$(cat "$work/test-exit") ;;
esac
SCRIPT
chmod +x "$work/sudo"
echo 1 >"$work/test-exit"
run_check() {
	HOME=$home TESSEL_DSCL=$done_dir/dscl TESSEL_ID=$work/id TESSEL_SUDO=$work/sudo \
		TESSEL_CRON_DENY=$work/deny TESSEL_AT_DENY=$work/deny "$check" "$@"
}
expect "good accounts, denials and a private home pass" 0 "0 failed, 0 skipped" run_check --accounts
echo 0 >"$work/test-exit"
expect "a runner that can read a private directory fails" 1 "FAIL: read $home/repos: _tessel-runner can, want cannot" \
	run_check --accounts
echo 1 >"$work/test-exit"
chmod 755 "$home"
expect "a home that other users can enter fails" 1 "FAIL: $home is 755" run_check --accounts
chmod 750 "$home"
printf '%s\n' Guest >"$work/deny"
expect "a runner not denied cron fails" 1 "FAIL: _tessel-runner is not in" run_check --accounts
expect "an unknown flag is refused by the check" 64 "usage" "$check" --bogus
printf '%s\n' Guest _tessel-runner >"$work/deny"
add_record "$done_dir" Users broken 999
rm "$done_dir/rec/Users/_tessel/UserShell"
expect "a missing shell fails" 1 "FAIL: _tessel shell is 'none'" run_check --accounts
echo "fake sudo that refuses leaves the probes skipped"
cat >"$work/sudo" <<'SCRIPT'
#!/bin/sh
exit 1
SCRIPT
add_good_account "$free" _tessel 481
add_good_account "$free" _tessel-runner 482
HOME=$home TESSEL_DSCL=$free/dscl TESSEL_ID=$work/id TESSEL_SUDO=$work/sudo \
	TESSEL_CRON_DENY=$work/deny TESSEL_AT_DENY=$work/deny "$check" --accounts >"$work/out.txt" 2>&1 && got=0 || got=$?
if [ "$got" -eq 2 ] && grep -q "SKIP: read $home/repos" "$work/out.txt"; then
	echo "ok: probes without sudo are skipped and exit 2"
else
	echo "FAIL: skipped probes: exit $got"
	sed 's/^/  | /' "$work/out.txt"
	failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
	echo "$failures failed"
	exit 1
fi
echo "all passed"

#!/usr/bin/env bash
# Tests the own-users scripts without root: accounts.sh, install.sh, upgrade.sh, fetch.sh,
# chown-tree.sh and tokens.sh with --dry-run or on bad arguments, and check-own-users.sh against a
# fake dscl, id, sudo and lsof.
#
# Usage: scripts/dogfood/own-users.test.sh
# Nothing here touches an account or a real file outside a temporary directory.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
d=$root/scripts/dogfood
work=$(mktemp -d)
trap 'rm -r "$work"' EXIT
failures=0

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

# expect_absent <label> <pattern>: the last command's output must not hold the pattern.
expect_absent() {
	if grep -q -- "$2" "$work/out.txt"; then
		echo "FAIL: $1: found '$2'"
		sed 's/^/  | /' "$work/out.txt"
		failures=$((failures + 1))
	else
		echo "ok: $1"
	fi
}

# A fake dscl over the files in $1: list/Users and list/Groups hold "name number" lines,
# rec/<Users|Groups>/<name>/<Attribute> holds one value, and search/<name> holds the lines that
# `-search /Groups GroupMembership <name>` prints. The directory is written into the script,
# because accounts.sh clears the environment before it runs.
make_dscl() {
	local dir=$1
	mkdir -p "$dir/list" "$dir/rec" "$dir/search"
	: >"$dir/list/Users"
	: >"$dir/list/Groups"
	cat >"$dir/dscl" <<SCRIPT
#!/bin/sh
case "\$2" in
-list) cat "$dir/list\$3" ;;
-read) cat "$dir/rec\$3/\$4" 2>/dev/null | sed "s/^/\$4: /" | grep . || exit 1 ;;
-search) cat "$dir/search/\$5" 2>/dev/null || true ;;
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
		NFSHomeDirectory /var/empty IsHidden 1 Password '*'
}

accounts=$d/accounts.sh
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
expect "a taken UID aborts" 1 "UID 481 is taken by someone" "$accounts" --dry-run --dscl "$uid_taken/dscl"

gid_taken=$work/gid
make_dscl "$gid_taken"
add_record "$gid_taken" Groups somegroup 482
expect "a taken GID aborts" 1 "GID 482 is taken by somegroup" "$accounts" --dry-run --dscl "$gid_taken/dscl"

moved=$work/moved
make_dscl "$moved"
add_record "$moved" Users _tessel 400
expect "an account with another number aborts" 1 "_tessel exists with UID 400, not 481" \
	"$accounts" --dry-run --dscl "$moved/dscl"

done_dir=$work/done
make_dscl "$done_dir"
add_good_account "$done_dir" _tessel 481
add_good_account "$done_dir" _tessel-runner 482
expect "an account that exists gets every attribute set again" 0 \
	"user _tessel-runner (482) exists; setting every attribute again" "$accounts" --dry-run --dscl "$done_dir/dscl"
expect "a half-made account is completed" 0 "would run: dscl . -create /Users/_tessel-runner IsHidden 1" \
	"$accounts" --dry-run --dscl "$done_dir/dscl"
expect "--dscl without --dry-run is refused" 1 "only accepted with --dry-run" "$accounts" --dscl "$free/dscl"
expect "an unknown flag is refused" 1 "usage" "$accounts" --frobnicate

# install.sh, upgrade.sh and fetch.sh, dry runs. The fake binaries are empty executables.
bins=$work/bins
mkdir -p "$bins"
for binary in tessel-server tessel tessel-runner; do
	printf '#!/bin/sh\n' >"$bins/$binary"
	chmod +x "$bins/$binary"
done
install_sh=$d/install.sh
expect "install plans the _tessel directories one by one" 0 "install -d -o _tessel -g _tessel -m 700 /var/tessel/keys" \
	"$install_sh" --binaries "$bins" --dry-run
expect_absent "install does not make /var/tessel owned by _tessel" "-o _tessel -g _tessel -m 711 /var/tessel"
expect "install makes /var/tessel root-owned" 0 "install -d -o root -g wheel -m 755 /var/tessel$" \
	"$install_sh" --binaries "$bins" --dry-run
expect "install stages root's work in /opt/tessel/tmp" 0 "install -d -o root -g wheel -m 700 /opt/tessel/tmp" \
	"$install_sh" --binaries "$bins" --dry-run
expect "install writes the sudoers rule with the two commands" 0 "/bin/kill -KILL -1" \
	"$install_sh" --binaries "$bins" --dry-run
expect "install disables the plist until the move" 0 "launchctl disable system/dev.tessel.server" \
	"$install_sh" --binaries "$bins" --dry-run
expect_absent "install writes no server or runner configuration" "would write /var/tessel/tessel-server.toml"
expect "install writes the cargo config as _tessel" 0 "would write /var/tessel/cargo/config.toml as _tessel" \
	"$install_sh" --binaries "$bins" --dry-run
expect "install refuses a missing binary" 1 "is not an executable regular file" \
	"$install_sh" --binaries "$work/nowhere" --dry-run
expect "install needs --binaries" 1 "usage" "$install_sh" --dry-run

upgrade_sh=$d/upgrade.sh
expect "upgrade keeps the old binaries as bin.prev" 0 "mv /opt/tessel/bin /opt/tessel/bin.prev" \
	"$upgrade_sh" --binaries "$bins" --dry-run
expect "upgrade installs from its own copies" 0 "<the copy of tessel-runner>" "$upgrade_sh" --binaries "$bins" --dry-run
chmod -x "$bins/tessel"
expect "upgrade refuses a binary that is not executable" 1 "is not an executable regular file" \
	"$upgrade_sh" --binaries "$bins" --dry-run
chmod +x "$bins/tessel"

fetch_sh=$d/fetch.sh
sha=0123456789012345678901234567890123456789
expect "fetch plans the cargo fetch from inside the checkout" 0 "cargo fetch --locked" \
	"$fetch_sh" https://github.com/devfep/tessel-lane-b.git "$sha" --dry-run
expect "fetch refuses a URL that is not a github https URL" 1 "must be an https://github.com" \
	"$fetch_sh" http://example.com/x.git "$sha" --dry-run
expect "fetch refuses a commit that is not 40 hex digits" 1 "40 lowercase hex digits" \
	"$fetch_sh" https://github.com/devfep/tessel-lane-b.git abc --dry-run

expect "tokens needs --kid" 1 "usage" "$d/tokens.sh"
expect "tokens refuses a kid with odd characters" 1 "kid may hold only" "$d/tokens.sh" --kid 'a b'
expect "tokens refuses to run from a checkout" 1 "run the installed copy" "$d/tokens.sh" --kid 2026-10

# chown-tree.sh, on a macOS stat. $USER owns everything here, so --from is the current user.
if [ "$(uname -s)" = Darwin ]; then
	me=$(id -un)
	tree=$work/tree
	mkdir -p "$tree/sub"
	echo a >"$tree/a"
	echo b >"$tree/sub/b"
	chown_tree=$d/chown-tree.sh
	expect "chown-tree lists every entry of a clean tree" 0 "4 entries would change" \
		"$chown_tree" --from "$me" --to _tessel:_tessel --dry-run "$tree"
	expect "chown-tree refuses a different owner" 1 "is owned by $me, not nobody" \
		"$chown_tree" --from nobody --to _tessel:_tessel --dry-run "$tree"
	ln "$tree/a" "$tree/a-link"
	expect "chown-tree refuses a hard link" 1 "hard link, refused" \
		"$chown_tree" --from "$me" --to _tessel:_tessel --dry-run "$tree"
	rm "$tree/a-link"
	ln -s /etc/hosts "$tree/sub/symlink"
	expect "chown-tree refuses a symbolic link" 1 "Symbolic Link" \
		"$chown_tree" --from "$me" --to _tessel:_tessel --dry-run "$tree"
	rm "$tree/sub/symlink"
	expect "chown-tree refuses a relative path" 1 "not an absolute path" \
		"$chown_tree" --from "$me" --to _tessel:_tessel --dry-run tree
	expect "chown-tree refuses to run as a non-root user" 1 "run it as root" \
		"$chown_tree" --from "$me" --to _tessel:_tessel "$tree"
fi

# check-own-users.sh against fakes, with --test-tools. $HOME is a directory of our own with the
# three private directories the probes look for, at mode 750.
check=$d/check-own-users.sh
home=$work/home
mkdir -p "$home/repos" "$home/.ssh" "$home/.config/gh"
chmod 750 "$home"
printf '%s\n' Guest _tessel-runner >"$work/deny"
cat >"$work/id" <<'SCRIPT'
#!/bin/sh
echo "$2"
SCRIPT
cat >"$work/lsof" <<'SCRIPT'
#!/bin/sh
echo "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME"
echo "postgres 1 felix 5u IPv4 0 0t0 TCP 127.0.0.1:5432 (LISTEN)"
echo "tessel-se 2 felix 6u IPv4 0 0t0 TCP 127.0.0.1:7878 (LISTEN)"
SCRIPT
# The fake sudo answers `sudo -n /usr/bin/true` from sudo-ready, and a script run as the runner
# with the token in sudo-answer (TESSEL_CAN or TESSEL_CANNOT), or TESSEL_READY for the readiness
# script. An unready sudo prints nothing and fails, as a sudo that wants a password does.
cat >"$work/sudo" <<SCRIPT
#!/bin/sh
[ "\$(cat "$work/sudo-ready")" = yes ] || exit 1
case "\$*" in
*"/usr/bin/true"*) exit 0 ;;
*TESSEL_READY*) echo TESSEL_READY ;;
*"-iTCP"*) "$work/lsof" ;;
*) cat "$work/sudo-answer" ;;
esac
SCRIPT
chmod +x "$work/id" "$work/lsof" "$work/sudo"
echo yes >"$work/sudo-ready"
echo TESSEL_CANNOT >"$work/sudo-answer"
run_check() {
	HOME=$home TESSEL_DSCL=$done_dir/dscl TESSEL_ID=$work/id TESSEL_SUDO=$work/sudo TESSEL_LSOF=$work/lsof \
		TESSEL_CRON_DENY=$work/deny TESSEL_AT_DENY=$work/deny "$check" --test-tools "$@"
}
expect "good accounts, denials, a private home and a closed loopback pass" 0 "0 failed, 0 skipped" \
	run_check --accounts
echo TESSEL_CAN >"$work/sudo-answer"
expect "a runner that can read a private directory fails" 1 "FAIL: read $home/repos: _tessel-runner can, want cannot" \
	run_check --accounts
expect "a runner that can connect to a loopback service fails" 1 "FAIL: _tessel-runner can connect to 127.0.0.1:5432" \
	run_check --accounts
echo TESSEL_CANNOT >"$work/sudo-answer"
chmod 755 "$home"
expect "a home that other users can enter fails" 1 "FAIL: $home is 755" run_check --accounts
chmod 750 "$home"
printf '%s\n' Guest >"$work/deny"
expect "a runner not denied cron fails" 1 "FAIL: _tessel-runner is not in" run_check --accounts
printf '%s\n' Guest _tessel-runner >"$work/deny"
mkdir -p "$done_dir/rec/Groups/staff"
echo "_tessel-runner" >"$done_dir/rec/Groups/staff/GroupMembership"
expect "an account in staff fails" 1 "FAIL: _tessel-runner is a member of staff" run_check --accounts
rm "$done_dir/rec/Groups/staff/GroupMembership"
printf 'staff\t\tGroupMembership = (_tessel-runner)\n' >"$done_dir/search/_tessel-runner"
expect "an account listed in any group fails" 1 "FAIL: _tessel-runner is a member of: staff" run_check --accounts
rm "$done_dir/search/_tessel-runner"
rm "$done_dir/rec/Users/_tessel/UserShell"
expect "a missing shell fails" 1 "FAIL: _tessel shell is 'none'" run_check --accounts
echo /usr/bin/false >"$done_dir/rec/Users/_tessel/UserShell"
echo "no" >"$work/sudo-ready"
expect "probes without sudo are skipped and exit 2" 2 "SKIP: read $home/repos" run_check --accounts
expect "tool overrides without --test-tools are refused" 64 "only with --test-tools" \
	env TESSEL_SUDO="$work/sudo" HOME="$home" "$check" --accounts
expect "an unknown flag is refused by the check" 64 "usage" "$check" --bogus

if [ "$failures" -ne 0 ]; then
	echo "$failures failed"
	exit 1
fi
echo "all passed"

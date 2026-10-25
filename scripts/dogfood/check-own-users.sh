#!/bin/bash
# Checks the accounts and the layout of docs/proposals/own-users.md. Read-only: it changes nothing
# and is safe to run at any time, as any user.
#
# Usage: check-own-users.sh [--accounts | --installed | --all]
#
# --accounts   the two accounts (numbers, shell, home, groups), cron.deny and at.deny, and the
#              mode of your home. Needs nothing installed.
# --installed  the owner and mode of every path in section 1, the toolchain, and what the runner
#              account can and cannot read or write. Needs install.sh to have run.
# --all        both (the default).
#
# Probes that run as _tessel-runner use `sudo -n -u _tessel-runner`, which needs your sudo
# password already given (`sudo -v` first). Without it those probes are skipped, and the script
# says so. Exit 0: every check passed. Exit 1: a check failed. Exit 2: nothing failed, but
# something was skipped. A skipped probe is not a pass; a failing or skipped check stops the order
# in docs/dogfood.md. The probes only test access (`test -r`, `test -w`), and write nothing.
#
# It also lists the processes that listen on a TCP port: a service that trusts local connections
# is reachable from the runner account (own-users.md, section 7), and only you can judge it.
set -euo pipefail

dscl=${TESSEL_DSCL:-/usr/bin/dscl}
sudo=${TESSEL_SUDO:-/usr/bin/sudo}
id=${TESSEL_ID:-/usr/bin/id}
cron_deny=${TESSEL_CRON_DENY:-/var/at/cron.deny}
at_deny=${TESSEL_AT_DENY:-/var/at/at.deny}
mode=all
pass=0
failed=0
skipped=0
tessel_name=_tessel
runner_name=_tessel-runner
channel=1.99.1

case "${1:-}" in
"" | --all) mode=all ;;
--accounts) mode=accounts ;;
--installed) mode=installed ;;
*)
	echo "usage: check-own-users.sh [--accounts | --installed | --all]" >&2
	exit 64
	;;
esac
[ "$#" -le 1 ] || {
	echo "usage: check-own-users.sh [--accounts | --installed | --all]" >&2
	exit 64
}

ok() {
	pass=$((pass + 1))
	echo "ok: $*"
}
bad() {
	failed=$((failed + 1))
	echo "FAIL: $*"
}
skip() {
	skipped=$((skipped + 1))
	echo "SKIP: $*"
}

# Prints "owner:group mode" of $1, or nothing when it does not exist.
stat_of() {
	if [ "$(/usr/bin/uname -s)" = Darwin ]; then
		/usr/bin/stat -f '%Su:%Sg %Lp' "$1" 2>/dev/null || true
	else
		/usr/bin/stat -c '%U:%G %a' "$1" 2>/dev/null || true
	fi
}

expect_mode() {
	local path=$1 want_owner=$2 want_mode=$3 got
	got=$(stat_of "$path")
	if [ "$got" = "$want_owner $want_mode" ]; then
		ok "$path is $want_owner $want_mode"
	elif [ -z "$got" ]; then
		bad "$path does not exist (want $want_owner $want_mode)"
	else
		bad "$path is $got, want $want_owner $want_mode"
	fi
}

attribute() {
	"$dscl" . -read "$1" "$2" 2>/dev/null | /usr/bin/awk 'NR == 1 { print $2 }' || true
}

# is <value> <want> <description>: ok when the value is the wanted one.
is() {
	if [ "$1" = "$2" ]; then
		ok "$3 is $2"
	else
		bad "$3 is '${1:-none}', want $2"
	fi
}

check_account() {
	local name=$1 number=$2 groups
	is "$(attribute "/Users/$name" UniqueID)" "$number" "$name UID"
	is "$(attribute "/Users/$name" PrimaryGroupID)" "$number" "$name GID"
	is "$(attribute "/Groups/$name" PrimaryGroupID)" "$number" "group $name GID"
	is "$(attribute "/Users/$name" UserShell)" /usr/bin/false "$name shell"
	is "$(attribute "/Users/$name" NFSHomeDirectory)" /var/empty "$name home"
	is "$(attribute "/Users/$name" IsHidden)" 1 "$name IsHidden"
	groups=$("$id" -Gn "$name" 2>/dev/null || true)
	if [ -z "$groups" ]; then
		bad "$name is unknown to id"
	elif [ "$groups" = "$name" ]; then
		ok "$name is in no group but its own"
	else
		bad "$name is in the groups: $groups"
	fi
}

check_deny() {
	local file=$1
	if [ -f "$file" ] && /usr/bin/grep -qx -- "$runner_name" "$file"; then
		ok "$runner_name is denied in $file"
	else
		bad "$runner_name is not in $file"
	fi
}

check_home() {
	local got
	got=$(stat_of "$HOME")
	case "${got##* }" in
	*[0-7][0-7][0-7])
		local bits=${got##* }
		if [ "${bits: -1}" = 0 ]; then
			ok "$HOME has no bits for other users ($bits)"
		else
			bad "$HOME is $bits: other users can enter it; chmod o-rwx $HOME"
		fi
		;;
	*) bad "cannot read the mode of $HOME" ;;
	esac
}

runner_can_sudo=unknown
as_runner_ready() {
	if [ "$runner_can_sudo" = unknown ]; then
		if "$sudo" -n -u "$runner_name" /usr/bin/true >/dev/null 2>&1; then
			runner_can_sudo=yes
		else
			runner_can_sudo=no
		fi
	fi
	[ "$runner_can_sudo" = yes ]
}

# probe <description> <r|w> <path> <can|cannot>: the runner account's access to a path.
probe() {
	local description=$1 test_flag=$2 path=$3 expect=$4 result
	if [ ! -e "$path" ]; then
		skip "$description: $path does not exist"
		return
	fi
	if ! as_runner_ready; then
		skip "$description: needs 'sudo -v' and an existing $runner_name"
		return
	fi
	if "$sudo" -n -u "$runner_name" /bin/test "-$test_flag" "$path" >/dev/null 2>&1; then
		result=can
	else
		result=cannot
	fi
	if [ "$result" = "$expect" ]; then
		ok "$description: $runner_name $result"
	else
		bad "$description: $runner_name $result, want $expect"
	fi
}

check_accounts() {
	check_account "$tessel_name" 481
	check_account "$runner_name" 482
	check_deny "$cron_deny"
	check_deny "$at_deny"
	check_home
	local path
	for path in "$HOME/repos" "$HOME/.ssh" "$HOME/.config/gh"; do
		probe "read $path" r "$path" cannot
	done
}

check_installed() {
	expect_mode /opt/tessel root:wheel 755
	expect_mode /opt/tessel/bin root:wheel 755
	expect_mode /opt/tessel/sbin root:wheel 755
	local binary
	for binary in tessel-server tessel tessel-runner; do
		expect_mode "/opt/tessel/bin/$binary" root:wheel 755
	done
	expect_mode /var/tessel "$tessel_name:$tessel_name" 711
	local dir
	for dir in keys stores lander tmp; do
		expect_mode "/var/tessel/$dir" "$tessel_name:$tessel_name" 700
	done
	expect_mode /var/tessel/handoff "$tessel_name:$runner_name" 750
	expect_mode /var/tessel/cargo "$tessel_name:$tessel_name" 755
	expect_mode /var/tessel/toolchain root:wheel 755
	expect_mode /var/tessel/tessel-server.toml "$tessel_name:$tessel_name" 600
	expect_mode /var/tessel-runner root:wheel 755
	expect_mode /var/tessel-runner/tessel-runner.toml root:wheel 644
	expect_mode /var/tessel-runner/work "$runner_name:$runner_name" 700
	expect_mode /var/tessel-runner/tmp "$runner_name:$runner_name" 700
	expect_mode /etc/sudoers.d/tessel root:wheel 440
	expect_mode /Library/LaunchDaemons/dev.tessel.server.plist root:wheel 644
	if [ -e /var/tessel/github-token ]; then
		expect_mode /var/tessel/github-token "$tessel_name:$tessel_name" 600
	else
		skip "/var/tessel/github-token has not been moved yet"
	fi
	if [ -x /var/tessel/toolchain/bin/rustc ]; then
		if /var/tessel/toolchain/bin/rustc -V | /usr/bin/grep -q " $channel "; then
			ok "the toolchain is rustc $channel"
		else
			bad "the toolchain is not rustc $channel"
		fi
	else
		bad "/var/tessel/toolchain/bin/rustc is missing"
	fi
	local path
	for path in /var/tessel/github-token /var/tessel/keys /var/tessel/stores /var/tessel/lander; do
		probe "read $path" r "$path" cannot
	done
	for path in /opt/tessel/bin /var/tessel /var/tessel/cargo /var/tessel/toolchain \
		/var/tessel-runner /var/tessel-runner/tessel-runner.toml /var/tessel/handoff; do
		probe "write $path" w "$path" cannot
	done
	probe "read /var/tessel/handoff" r /var/tessel/handoff can
	probe "write /var/tessel-runner/work" w /var/tessel-runner/work can
	if as_runner_ready; then
		if "$sudo" -n -u "$runner_name" "$sudo" -n -l >/dev/null 2>&1; then
			bad "$runner_name can run sudo"
		else
			ok "$runner_name cannot run sudo"
		fi
	else
		skip "$runner_name and sudo: needs 'sudo -v'"
	fi
}

list_listeners() {
	echo "TCP listeners (judge each: a service that trusts local connections is open to the runner):"
	/usr/sbin/lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | /usr/bin/awk 'NR == 1 || /LISTEN/ { print "  " $1, $3, $9 }' || true
}

case "$mode" in
accounts) check_accounts ;;
installed) check_installed ;;
all)
	check_accounts
	check_installed
	;;
esac
list_listeners
echo "check-own-users: $pass ok, $failed failed, $skipped skipped"
if [ "$failed" -gt 0 ]; then
	exit 1
elif [ "$skipped" -gt 0 ]; then
	exit 2
fi

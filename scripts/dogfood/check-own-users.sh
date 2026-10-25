#!/bin/bash
# Checks the accounts and the layout of docs/proposals/own-users.md. Read-only: it changes nothing
# and is safe to run at any time, as any user.
#
# Usage: check-own-users.sh [--accounts | --installed | --all]
#
# --accounts   the two accounts (numbers, shell, home, password, groups), cron.deny and at.deny,
#              the mode of your home, and what the runner account can reach: your private
#              directories, the loopback services and the world-writable unix sockets.
# --installed  the owner and mode of every path in section 1, the toolchain, the sudoers rule, the
#              launchd plist, and what the runner account can and cannot read or write.
# --all        both (the default).
#
# Probes that run as _tessel-runner use `sudo -n -u _tessel-runner`, which needs your sudo
# password already given (`sudo -v` first). Each probe prints a token from inside the sudo
# (TESSEL_CAN, TESSEL_CANNOT), so a sudo that failed is a skipped probe and never a pass. Exit 0:
# every check passed. Exit 1: a check failed. Exit 2: nothing failed, but something was skipped.
# A failing or skipped check stops the order in docs/dogfood.md. The probes only test access
# (`test -r`, `test -w`, a TCP connect) and write nothing.
#
# --test-tools lets a test replace dscl, id, sudo, lsof and the deny files through TESSEL_DSCL,
# TESSEL_ID, TESSEL_SUDO, TESSEL_LSOF, TESSEL_CRON_DENY and TESSEL_AT_DENY. Without it those
# variables must not be set.
set -euo pipefail

usage="usage: check-own-users.sh [--accounts | --installed | --all] [--test-tools]"
mode=""
test_tools=0
for arg in "$@"; do
	case "$arg" in
	--all | --accounts | --installed)
		[ -z "$mode" ] || {
			echo "$usage" >&2
			exit 64
		}
		mode=${arg#--}
		;;
	--test-tools) test_tools=1 ;;
	--sudo-rule) mode="sudo-rule" ;;
	*)
		echo "$usage" >&2
		exit 64
		;;
	esac
done
mode=${mode:-all}
if [ "$mode" = sudo-rule ] && [ "$test_tools" != 1 ]; then
	echo "$usage" >&2
	exit 64
fi

dscl=/usr/bin/dscl
sudo=/usr/bin/sudo
id=/usr/bin/id
lsof=/usr/sbin/lsof
cron_deny=/var/at/cron.deny
at_deny=/var/at/at.deny
if [ "$test_tools" = 1 ]; then
	dscl=${TESSEL_DSCL:-$dscl}
	sudo=${TESSEL_SUDO:-$sudo}
	id=${TESSEL_ID:-$id}
	lsof=${TESSEL_LSOF:-$lsof}
	cron_deny=${TESSEL_CRON_DENY:-$cron_deny}
	at_deny=${TESSEL_AT_DENY:-$at_deny}
elif [ -n "${TESSEL_DSCL:-}${TESSEL_SUDO:-}${TESSEL_ID:-}${TESSEL_LSOF:-}${TESSEL_CRON_DENY:-}${TESSEL_AT_DENY:-}" ]; then
	echo "check-own-users: TESSEL_* tool overrides are set; they are accepted only with --test-tools" >&2
	exit 64
fi

pass=0
failed=0
skipped=0
tessel_name=_tessel
runner_name=_tessel-runner
channel=1.99.1

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

# Prints "owner:group mode" of $1 (not following a link), or nothing when it does not exist.
stat_of() {
	if [ "$(/usr/bin/uname -s)" = Darwin ]; then
		/usr/bin/stat -f '%Su:%Sg %Lp' "$1" 2>/dev/null || true
	else
		/usr/bin/stat -c '%U:%G %a' "$1" 2>/dev/null || true
	fi
}

expect_mode() {
	local path=$1 want_owner=$2 want_mode=$3 got
	if [ -L "$path" ]; then
		bad "$path is a symbolic link"
		return
	fi
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
	local name=$1 number=$2 member group
	is "$(attribute "/Users/$name" UniqueID)" "$number" "$name UID"
	is "$(attribute "/Users/$name" PrimaryGroupID)" "$number" "$name GID"
	is "$(attribute "/Groups/$name" PrimaryGroupID)" "$number" "group $name GID"
	is "$(attribute "/Users/$name" UserShell)" /usr/bin/false "$name shell"
	is "$(attribute "/Users/$name" NFSHomeDirectory)" /var/empty "$name home"
	is "$(attribute "/Users/$name" IsHidden)" 1 "$name IsHidden"
	is "$(attribute "/Users/$name" Password)" '*' "$name password"
	for group in staff admin; do
		if "$dscl" . -read "/Groups/$group" GroupMembership 2>/dev/null | /usr/bin/grep -qw -- "$name"; then
			bad "$name is a member of $group"
		else
			ok "$name is not a member of $group"
		fi
	done
	member=$("$dscl" . -search /Groups GroupMembership "$name" 2>/dev/null | /usr/bin/awk 'NF { print $1 }' | /usr/bin/paste -sd, - || true)
	if [ -z "$member" ]; then
		ok "$name is a member of no group"
	else
		bad "$name is a member of: $member"
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
	local got bits
	got=$(stat_of "$HOME")
	bits=${got##* }
	case "$bits" in
	[0-7][0-7][0-7])
		if [ "${bits: -1}" = 0 ]; then
			ok "$HOME has no bits for other users ($bits)"
		else
			bad "$HOME is $bits: other users can enter it; chmod o-rwx $HOME"
		fi
		;;
	*) bad "cannot read the mode of $HOME" ;;
	esac
}

# run_as_runner <shell script> <argument>...: prints what the script prints, run by the runner.
run_as_runner() {
	"$sudo" -n -u "$runner_name" /bin/sh -c "$@" 2>/dev/null || true
}

runner_ready=unknown
as_runner_ready() {
	if [ "$runner_ready" = unknown ]; then
		if [ "$(run_as_runner 'echo TESSEL_READY' sh)" = TESSEL_READY ]; then
			runner_ready=yes
		else
			runner_ready=no
		fi
	fi
	[ "$runner_ready" = yes ]
}

# probe <description> <r|w> <path> <can|cannot>: the runner account's access to a path.
probe() {
	local description=$1 flag=$2 path=$3 expect=$4 token
	if [ ! -e "$path" ]; then
		skip "$description: $path does not exist"
		return
	fi
	if ! as_runner_ready; then
		skip "$description: needs 'sudo -v' and an existing $runner_name"
		return
	fi
	# The script is for the runner's shell, which expands its own arguments.
	# shellcheck disable=SC2016
	token=$(run_as_runner 'if test "-$1" "$2"; then echo TESSEL_CAN; else echo TESSEL_CANNOT; fi' sh "$flag" "$path")
	case "$token" in
	TESSEL_CAN) token=can ;;
	TESSEL_CANNOT) token=cannot ;;
	*)
		skip "$description: the probe did not run"
		return
		;;
	esac
	if [ "$token" = "$expect" ]; then
		ok "$description: $runner_name $token"
	else
		bad "$description: $runner_name $token, want $expect"
	fi
}

# probe_connect <port>: whether the runner account can open a TCP connection to <port> on
# 127.0.0.1 or on [::1].
probe_connect() {
	local port=$1 token
	# shellcheck disable=SC2016
	token=$(run_as_runner 'if /usr/bin/nc -z -w 2 127.0.0.1 "$1" >/dev/null 2>&1 || /usr/bin/nc -z -w 2 ::1 "$1" >/dev/null 2>&1; then echo TESSEL_CAN; else echo TESSEL_CANNOT; fi' sh "$port")
	case "$token" in
	TESSEL_CAN) echo can ;;
	TESSEL_CANNOT) echo cannot ;;
	*) echo unknown ;;
	esac
}

# Loopback is open to every account by design, and most listeners are harmless (ControlCenter,
# Dropbox, the simulators). They are listed for you to judge. Only the database ports below are
# probed and can fail the check: a database that trusts local connections gives the runner a
# superuser, and a superuser can run a command as you.
risky_ports=" 5432 3306 6379 27017 "

check_loopback() {
	local out="" port process result
	if "$sudo" -n /usr/bin/true >/dev/null 2>&1; then
		out=$("$sudo" -n "$lsof" -nP -iTCP -sTCP:LISTEN 2>/dev/null || true)
	else
		out=$("$lsof" -nP -iTCP -sTCP:LISTEN 2>/dev/null || true)
		echo "note: lsof ran as you and lists only your processes; 'sudo -v' first to list every user's"
	fi
	echo "TCP listeners (for you to judge; only database ports are probed):"
	echo "$out" | /usr/bin/awk 'NR == 1 || /LISTEN/ { print "  " $1, $3, $9 }'
	while read -r process port; do
		case "$risky_ports" in *" $port "*) ;; *) continue ;; esac
		if ! as_runner_ready; then
			skip "connect to port $port ($process): needs 'sudo -v'"
			continue
		fi
		result=$(probe_connect "$port")
		case "$result" in
		cannot) ok "$runner_name cannot connect to port $port ($process)" ;;
		can) bad "$runner_name can connect to port $port ($process): stop it, require a password, or block it with pf" ;;
		*) skip "connect to port $port: the probe did not run" ;;
		esac
	done < <(echo "$out" | /usr/bin/awk '/LISTEN/ { n = split($9, a, ":"); print $1, a[n] }' | /usr/bin/sort -u)
}

check_unix_sockets() {
	local socket count=0 reached=0 token
	if ! as_runner_ready; then
		skip "unix sockets that you own and anyone may write: needs 'sudo -v'"
		return
	fi
	while IFS= read -r socket; do
		count=$((count + 1))
		# shellcheck disable=SC2016
		token=$(run_as_runner 'if test -w "$1"; then echo TESSEL_CAN; else echo TESSEL_CANNOT; fi' sh "$socket")
		if [ "$token" = TESSEL_CAN ]; then
			reached=$((reached + 1))
			bad "$runner_name can connect to the unix socket $socket: stop its service or make it private"
		fi
	done < <(/usr/bin/find /private/tmp /private/var/tmp /private/var/run /Users/Shared -maxdepth 2 -type s -perm -0002 \
		-user "$(/usr/bin/id -un)" 2>/dev/null || true)
	if [ "$reached" = 0 ]; then
		ok "$runner_name reaches none of the $count unix sockets that you own and anyone may write"
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
	check_loopback
	check_unix_sockets
}

check_plist() {
	local plist=/Library/LaunchDaemons/dev.tessel.server.plist
	[ -f "$plist" ] || return 0
	is "$(/usr/bin/plutil -extract UserName raw -o - "$plist" 2>/dev/null || true)" "$tessel_name" "the plist's UserName"
	is "$(/usr/bin/plutil -extract EnvironmentVariables.PATH raw -o - "$plist" 2>/dev/null || true)" /usr/bin:/bin "the plist's PATH"
	if /bin/launchctl print-disabled system 2>/dev/null | /usr/bin/grep -q '"dev.tessel.server" => disabled'; then
		echo "note: dev.tessel.server is disabled; the move enables it (docs/dogfood.md, step 4)"
	else
		echo "note: dev.tessel.server is not disabled: it loads at boot"
	fi
}

check_sudo_rule() {
	local out rules want
	want="(_tessel-runner) NOPASSWD: /opt/tessel/bin/tessel-runner --config /var/tessel-runner/tessel-runner.toml, /bin/kill -KILL -1"
	if ! "$sudo" -n /usr/bin/true >/dev/null 2>&1; then
		skip "the sudoers rule of $tessel_name: needs 'sudo -v'"
		return
	fi
	out=$("$sudo" -n -l -U "$tessel_name" 2>/dev/null || true)
	# The lines after "may run the following commands" are the whole rule, and must be this one.
	# sudo wraps long lines, so the lines are joined and the white space collapsed first.
	rules=$(echo "$out" | /usr/bin/awk '/may run the following commands/ { on = 1; next } on && NF { sub(/\\$/, ""); print }' |
		/usr/bin/tr '\n\t' '  ' | /usr/bin/tr -s ' ' | /usr/bin/sed -e 's/^ //' -e 's/ $//')
	if [ "$rules" = "$want" ]; then
		ok "$tessel_name may run exactly the runner and kill -KILL -1"
	else
		bad "the sudoers rule of $tessel_name is not exactly the two commands of the design; it is: $rules"
	fi
}

# The server trusts every .pub in keys/. Exactly one key pair must be there, so a rotated key
# is really gone and a planted .pub is not trusted.
check_keys() {
	local listing names
	if ! "$sudo" -n /usr/bin/true >/dev/null 2>&1; then
		skip "the key files: needs 'sudo -v'"
		return
	fi
	listing=$("$sudo" -n /bin/ls -1A /var/tessel/keys 2>/dev/null || true)
	names=$(echo "$listing" | /usr/bin/sed -e 's/\.key$//' -e 's/\.pub$//' | /usr/bin/sort -u)
	if [ -n "$listing" ] && [ "$(echo "$names" | /usr/bin/wc -l | /usr/bin/tr -d ' ')" = 1 ] &&
		[ "$(echo "$listing" | /usr/bin/wc -l | /usr/bin/tr -d ' ')" = 2 ] &&
		echo "$listing" | /usr/bin/grep -qx "$names.key" && echo "$listing" | /usr/bin/grep -qx "$names.pub"; then
		ok "/var/tessel/keys holds exactly $names.key and $names.pub"
	elif [ -z "$listing" ]; then
		skip "/var/tessel/keys is empty: the key has not moved yet"
	else
		bad "/var/tessel/keys must hold exactly one key pair; it holds: $(echo "$listing" | /usr/bin/paste -sd' ' -)"
	fi
}

check_installed() {
	expect_mode /opt/tessel root:wheel 755
	expect_mode /opt/tessel/bin root:wheel 755
	expect_mode /opt/tessel/sbin root:wheel 755
	expect_mode /opt/tessel/tmp root:wheel 700
	local binary
	for binary in tessel-server tessel tessel-runner; do
		expect_mode "/opt/tessel/bin/$binary" root:wheel 755
	done
	expect_mode /var/tessel root:wheel 755
	local dir
	for dir in keys stores lander tmp; do
		expect_mode "/var/tessel/$dir" "$tessel_name:$tessel_name" 700
	done
	expect_mode /var/tessel/handoff "$tessel_name:$runner_name" 750
	expect_mode /var/tessel/cargo "$tessel_name:$tessel_name" 755
	expect_mode /var/tessel/cargo/config.toml "$tessel_name:$tessel_name" 644
	expect_mode /var/tessel/toolchain root:wheel 755
	expect_mode /var/tessel-runner root:wheel 755
	expect_mode /var/tessel-runner/work "$runner_name:$runner_name" 700
	expect_mode /var/tessel-runner/tmp "$runner_name:$runner_name" 700
	expect_mode /var/log/tessel-server.log "$tessel_name:$tessel_name" 644
	expect_mode /etc/sudoers.d/tessel root:wheel 440
	expect_mode /Library/LaunchDaemons/dev.tessel.server.plist root:wheel 644
	check_plist
	check_sudo_rule
	check_keys
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
		if [ "$(run_as_runner '/usr/bin/sudo -n -l >/dev/null 2>&1 && echo TESSEL_CAN || echo TESSEL_CANNOT' sh)" = TESSEL_CANNOT ]; then
			ok "$runner_name cannot run sudo"
		else
			bad "$runner_name can run sudo, or the probe did not run"
		fi
	else
		skip "$runner_name and sudo: needs 'sudo -v'"
	fi
}

case "$mode" in
sudo-rule) check_sudo_rule ;;
accounts) check_accounts ;;
installed) check_installed ;;
all)
	check_accounts
	check_installed
	;;
esac
echo "check-own-users: $pass ok, $failed failed, $skipped skipped"
if [ "$failed" -gt 0 ]; then
	exit 1
elif [ "$skipped" -gt 0 ]; then
	exit 2
fi

#!/bin/bash
# Installs the layout of docs/proposals/own-users.md, section 1: the directories and their modes,
# the binaries, the toolchain, the cargo cache setting, the sudoers rule and the launchd plist
# (disabled, not loaded). It moves no secret: the store, key and token move by hand after the
# probes pass (docs/dogfood.md).
#
# Usage: sudo /opt/tessel/sbin/install.sh --binaries <dir> [--yes] [--dry-run]
#
# <dir> holds tessel-server, tessel and tessel-runner, built from a landed main. It copies them
# into /opt/tessel/tmp first, prints the sha256 of those copies and asks for a typed yes (unless
# --yes), so what is shown is what is installed. Safe to run again (a new toolchain channel).
#
# Root never writes inside a directory that _tessel can rename entries in. /var/tessel and
# /var/tessel-runner are root-owned; root creates the directories below them, owned by their
# users, and only when they do not exist; an existing one must already be a real directory with the
# right owner and group, or the script stops. Files that live inside a _tessel-owned directory are
# written by _tessel. Root's own scratch work (the sudoers candidate, the toolchain download)
# happens in /opt/tessel/tmp, which only root can enter.
#
# It writes the runner's configuration, /var/tessel-runner/tessel-runner.toml (root-owned, so the
# runner's account cannot rewrite it), and, only when it is absent, the server's,
# /var/tessel/tessel-server.toml. The server's `[lander] runner_config` stays: with
# `[lander.sudo_runner]` the server does not read that file, but its path is the one argument the
# sudoers rule allows the runner to be given, so the two must name the same file.
#
# The script refuses to run unless it is the copy in /opt/tessel/sbin. --dry-run prints each step
# and needs neither root nor that location; the accounts must exist first (accounts.sh).
set -euo pipefail

if [ "${TESSEL_CLEAN:-}" != 1 ]; then
	exec /usr/bin/env -i TESSEL_CLEAN=1 PATH=/usr/bin:/bin:/usr/sbin:/sbin /bin/bash "$0" "$@"
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dry=0
yes=0
binaries=""
channel=1.99.1
installer=rust-$channel-aarch64-apple-darwin.tar.xz
prefix=/var/tessel/toolchain
server=_tessel
runner=_tessel-runner
scratch=/opt/tessel/tmp
work=""

fail() {
	echo "install: $*" >&2
	exit 1
}

cleanup() {
	if [ -n "$work" ] && [ -d "$work" ]; then
		/bin/rm -r "$work"
	fi
}
trap cleanup EXIT

usage="usage: install.sh --binaries <dir> [--yes] [--dry-run]"
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
if [ "$dry" != 1 ]; then
	[ "$(/usr/bin/uname -s)" = Darwin ] || fail "this script installs on macOS"
	[ "$(/usr/bin/id -u)" = 0 ] || fail "run it as root: sudo /opt/tessel/sbin/install.sh"
	[ "$here" = /opt/tessel/sbin ] || fail "run the installed copy, /opt/tessel/sbin/install.sh, not $here"
fi

act() {
	if [ "$dry" = 1 ]; then
		echo "would run: $*"
	else
		"$@"
	fi
}

# as_server <command>...: runs the command as _tessel with a clean environment.
as_server() {
	/usr/bin/sudo -n -u "$server" /usr/bin/env -i PATH=/usr/bin:/bin "$@"
}

# ensure_dir <path> <owner> <group> <mode>: creates the directory when it is missing. An existing
# one is never changed, only checked: not a link, a directory, with this owner and group.
ensure_dir() {
	local path=$1 owner=$2 group=$3 mode=$4 got
	if [ -L "$path" ]; then
		fail "$path is a symbolic link"
	elif [ -e "$path" ]; then
		[ -d "$path" ] || fail "$path is not a directory"
		got=$(/usr/bin/stat -f '%Su:%Sg %Lp' "$path")
		[ "$got" = "$owner:$group $mode" ] || fail "$path is $got, want $owner:$group $mode; fix it by hand"
		echo "$path is in place"
	else
		act /usr/bin/install -d -o "$owner" -g "$group" -m "$mode" "$path"
	fi
}

check_prerequisites() {
	local name binary
	for name in "$server" "$runner"; do
		[ "$dry" = 1 ] || /usr/bin/id "$name" >/dev/null 2>&1 || fail "$name does not exist: run accounts.sh"
	done
	for binary in tessel-server tessel tessel-runner; do
		[ -f "$binaries/$binary" ] && [ ! -L "$binaries/$binary" ] && [ -x "$binaries/$binary" ] ||
			fail "$binaries/$binary is not an executable regular file"
	done
	[ -f "$here/toolchain.sha256" ] || fail "$here/toolchain.sha256 is missing"
	/usr/bin/grep -Eq "^[0-9a-f]{64}  $installer\$" "$here/toolchain.sha256" ||
		fail "toolchain.sha256 has no line for $installer"
	if [ -r /etc/sudoers ]; then
		if ! /usr/bin/grep -qx '#includedir /private/etc/sudoers.d' /etc/sudoers; then
			if [ "$dry" = 1 ]; then
				echo "note: /etc/sudoers has no macOS sudoers.d include"
			else
				fail "/etc/sudoers does not include sudoers.d"
			fi
		fi
	elif [ "$dry" != 1 ]; then
		fail "cannot read /etc/sudoers"
	fi
}

make_directories() {
	ensure_dir /opt/tessel root wheel 755
	ensure_dir /opt/tessel/bin root wheel 755
	ensure_dir /opt/tessel/sbin root wheel 755
	ensure_dir "$scratch" root wheel 700
	ensure_dir /var/tessel root wheel 755
	local dir
	for dir in keys stores lander tmp; do
		ensure_dir "/var/tessel/$dir" "$server" "$server" 700
	done
	ensure_dir /var/tessel/handoff "$server" "$runner" 750
	ensure_dir /var/tessel/cargo "$server" "$server" 755
	ensure_dir /var/tessel-runner root wheel 755
	ensure_dir /var/tessel-runner/work "$runner" "$runner" 700
	ensure_dir /var/tessel-runner/tmp "$runner" "$runner" 700
	if [ ! -e /var/log/tessel-server.log ]; then
		act /usr/bin/install -o "$server" -g "$server" -m 0644 /dev/null /var/log/tessel-server.log
	fi
}

install_binaries() {
	local binary
	if [ "$dry" = 1 ]; then
		for binary in tessel-server tessel tessel-runner; do
			echo "would copy $binaries/$binary into $scratch, show its sha256, ask for yes, and install it"
		done
		return
	fi
	work=$(/usr/bin/mktemp -d "$scratch/binaries.XXXXXX")
	for binary in tessel-server tessel tessel-runner; do
		/usr/bin/install -m 0755 "$binaries/$binary" "$work/$binary"
		echo "$binary: $(/usr/bin/shasum -a 256 "$work/$binary" | /usr/bin/awk '{ print $1 }')"
	done
	if [ "$yes" != 1 ]; then
		printf 'Were these built from a landed main with green CI? Type yes: '
		read -r answer
		[ "$answer" = yes ] || fail "not confirmed; nothing was installed"
	fi
	for binary in tessel-server tessel tessel-runner; do
		/usr/bin/install -o root -g wheel -m 0755 "$work/$binary" "/opt/tessel/bin/$binary"
	done
	/bin/rm -r "$work"
	work=""
}

install_toolchain() {
	if [ -x "$prefix/bin/rustc" ] && "$prefix/bin/rustc" -V 2>/dev/null | /usr/bin/grep -q " $channel "; then
		echo "the toolchain $channel is in place"
		return
	fi
	if [ "$dry" = 1 ]; then
		echo "would download https://static.rust-lang.org/dist/$installer into $scratch, check its"
		echo "  sha256 against $here/toolchain.sha256, unpack it and run its install.sh --prefix=$prefix"
		return
	fi
	work=$(/usr/bin/mktemp -d "$scratch/toolchain.XXXXXX")
	/usr/bin/curl -fsSL --proto '=https' -o "$work/$installer" "https://static.rust-lang.org/dist/$installer"
	(cd "$work" && /usr/bin/shasum -a 256 -c "$here/toolchain.sha256") ||
		fail "the downloaded $installer does not match toolchain.sha256"
	/usr/bin/tar -xJf "$work/$installer" -C "$work"
	"$work/${installer%.tar.xz}/install.sh" "--prefix=$prefix" --disable-ldconfig
	/bin/rm -r "$work"
	work=""
}

install_sudoers() {
	local text
	text=$(
		cat <<RULE
Defaults:$server env_reset, !setenv, !use_pty, secure_path="/usr/bin:/bin"
Cmnd_Alias TESSEL_RUN = \\
  /opt/tessel/bin/tessel-runner --config /var/tessel-runner/tessel-runner.toml, \\
  /bin/kill -KILL -1
$server ALL = ($runner) NOPASSWD: TESSEL_RUN
RULE
	)
	if [ "$dry" = 1 ]; then
		echo "would check with visudo -cf, then install as /etc/sudoers.d/tessel (root:wheel 0440):"
		echo "$text" | /usr/bin/sed 's/^/  | /'
		return
	fi
	work=$(/usr/bin/mktemp -d "$scratch/sudoers.XXXXXX")
	echo "$text" >"$work/tessel"
	/usr/sbin/visudo -cf "$work/tessel" || fail "visudo refused the rule; /etc/sudoers.d/tessel is unchanged"
	/usr/bin/install -o root -g wheel -m 0440 "$work/tessel" /etc/sudoers.d/tessel
	/bin/rm -r "$work"
	work=""
}

# put_root <path> <owner> <group> <mode>: writes stdin to a path whose directory only root can
# change, through a temporary file in the same directory.
put_root() {
	local path=$1 owner=$2 group=$3 mode=$4 text
	text=$(/bin/cat)
	if [ "$dry" = 1 ]; then
		echo "would write $path ($owner:$group $mode):"
		echo "$text" | /usr/bin/sed 's/^/  | /'
		return
	fi
	local tmp
	tmp=$(/usr/bin/mktemp "$path.XXXXXX")
	echo "$text" >"$tmp"
	/usr/sbin/chown "$owner:$group" "$tmp"
	/bin/chmod "$mode" "$tmp"
	/bin/mv "$tmp" "$path"
}

install_plist() {
	put_root /Library/LaunchDaemons/dev.tessel.server.plist root wheel 0644 <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key><string>dev.tessel.server</string>
	<key>ProgramArguments</key>
	<array>
		<string>/opt/tessel/bin/tessel-server</string>
		<string>serve</string>
		<string>--config</string>
		<string>/var/tessel/tessel-server.toml</string>
	</array>
	<key>UserName</key><string>$server</string>
	<key>GroupName</key><string>$server</string>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key><string>/usr/bin:/bin</string>
		<key>TMPDIR</key><string>/var/tessel/tmp</string>
	</dict>
	<key>StandardOutPath</key><string>/var/log/tessel-server.log</string>
	<key>StandardErrorPath</key><string>/var/log/tessel-server.log</string>
	<key>RunAtLoad</key><true/>
	<key>KeepAlive</key><false/>
</dict>
</plist>
PLIST
	# Disabled until the move: otherwise launchd loads it at the next boot, before the secrets are
	# here, and a second server would start beside the one that runs from the old directory. The
	# token's arrival in /var/tessel is the mark that the move is done; after it, a re-run (a new
	# channel) must not disable the live daemon.
	if [ -e /var/tessel/github-token ]; then
		echo "the move is done (the token is in /var/tessel): the daemon is left as it is"
	else
		act /bin/launchctl disable system/dev.tessel.server
	fi
}

install_cargo_config() {
	local path=/var/tessel/cargo/config.toml
	if [ "$dry" = 1 ]; then
		echo "would write $path as $server (0644): [build] jobs = 6"
		return
	fi
	printf '[build]\njobs = 6\n' | as_server /usr/bin/tee "$path" >/dev/null
	as_server /bin/chmod 0644 "$path"
}

install_configurations() {
	put_root /var/tessel-runner/tessel-runner.toml root wheel 0644 <<RUNNER
[runner]
user = "$runner"
timeout_secs = 720
path = "$prefix/bin:/usr/bin:/bin"
cargo_home = "/var/tessel/cargo"
tmpdir = "/var/tessel-runner/tmp"
work_dir = "/var/tessel-runner/work"
handoff_dir = "/var/tessel/handoff"
RUNNER
	if [ -e /var/tessel/tessel-server.toml ]; then
		echo "/var/tessel/tessel-server.toml is in place"
		return
	fi
	put_root /var/tessel/tessel-server.toml "$server" "$server" 0600 <<'SERVER'
listen = "127.0.0.1:7878"
store_dir = "/var/tessel/stores"
identity = { keys = "/var/tessel/keys", audience = "dogfood" }

[lander]
work_dir = "/var/tessel/lander"
runner_config = "/var/tessel-runner/tessel-runner.toml"
token_file = "/var/tessel/github-token"

[lander.sudo_runner]

[[repository]]
tenant = "devfep"
name = "tessel"
run = "dogfood"
remote = "https://github.com/devfep/tessel.git"
fork = "https://github.com/devfep/tessel-{agent}.git"
reviewers = ["orchestrator"]
sensitive = [
	"src/protocol.rs", "src/host/", ".github/", "docs/BUILD-PROTOCOL.md", "docs/dogfood.md",
	"scripts/", "tessel-server/ci/", "tessel-server/src/landing/", "tessel-server/src/credential.rs",
	"tessel-runner/src/", "tessel-lander/src/", "src/merge.rs", "src/coordinator.rs",
	"src/coordinator/", "src/shell.rs",
	".cargo/", "rust-toolchain.toml", "Cargo.lock", "CLAUDE.md", "PLAN.md", "skills/", ".claude/",
]
SERVER
}

check_prerequisites
make_directories
install_binaries
install_toolchain
install_sudoers
install_plist
install_cargo_config
install_configurations
echo "install: done. The plist is disabled and no secret has moved; run check-own-users.sh --installed"

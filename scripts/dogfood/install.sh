#!/bin/bash
# Installs the layout of docs/proposals/own-users.md, section 1: the directories and their modes,
# the binaries, the toolchain, the cargo cache, the sudoers rule, the launchd plist (not loaded)
# and the configurations. It moves no secret: the store, the key and the token are moved by hand
# after the probes pass (docs/dogfood.md).
#
# Usage: sudo /opt/tessel/sbin/install.sh --binaries <dir> [--dry-run]
#
# <dir> holds tessel-server, tessel and tessel-runner, built from a landed main. Safe to run
# again. --dry-run prints each step and needs no root; the accounts must exist first
# (accounts.sh), and so must the toolchain.sha256 beside this script.
set -euo pipefail

if [ "${TESSEL_CLEAN:-}" != 1 ]; then
	exec /usr/bin/env -i TESSEL_CLEAN=1 PATH=/usr/bin:/bin:/usr/sbin:/sbin /bin/bash "$0" "$@"
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dry=0
binaries=""
channel=1.99.1
installer=rust-$channel-aarch64-apple-darwin.tar.xz
prefix=/var/tessel/toolchain
server=_tessel
runner=_tessel-runner

fail() {
	echo "install: $*" >&2
	exit 1
}

while [ "$#" -gt 0 ]; do
	case "$1" in
	--dry-run) dry=1 ;;
	--binaries)
		[ "$#" -ge 2 ] || fail "--binaries needs a directory"
		binaries=$2
		shift
		;;
	*) fail "usage: install.sh --binaries <dir> [--dry-run]" ;;
	esac
	shift
done
[ -n "$binaries" ] || fail "usage: install.sh --binaries <dir> [--dry-run]"
if [ "$dry" != 1 ]; then
	[ "$(/usr/bin/uname -s)" = Darwin ] || fail "this script installs on macOS"
	[ "$(/usr/bin/id -u)" = 0 ] || fail "run it as root: sudo /opt/tessel/sbin/install.sh"
fi

act() {
	if [ "$dry" = 1 ]; then
		echo "would run: $*"
	else
		"$@"
	fi
}

# Writes stdin to $1 with owner $2:$3 and mode $4, through a temporary file in the same directory.
put() {
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

check_prerequisites() {
	local name
	for name in "$server" "$runner"; do
		[ "$dry" = 1 ] || /usr/bin/id "$name" >/dev/null 2>&1 || fail "$name does not exist: run accounts.sh"
	done
	local binary
	for binary in tessel-server tessel tessel-runner; do
		[ -f "$binaries/$binary" ] && [ -x "$binaries/$binary" ] || fail "$binaries/$binary is not an executable file"
	done
	[ -f "$here/toolchain.sha256" ] || fail "$here/toolchain.sha256 is missing"
	/usr/bin/grep -Eq "^[0-9a-f]{64}  $installer\$" "$here/toolchain.sha256" ||
		fail "toolchain.sha256 has no line for $installer"
	if [ -r /etc/sudoers ]; then
		/usr/bin/grep -qx '#includedir /private/etc/sudoers.d' /etc/sudoers ||
			fail "/etc/sudoers does not include sudoers.d"
	elif [ "$dry" != 1 ]; then
		fail "cannot read /etc/sudoers"
	fi
}

make_directories() {
	act /usr/bin/install -d -o root -g wheel -m 0755 /opt/tessel /opt/tessel/bin /opt/tessel/sbin
	act /usr/bin/install -d -o "$server" -g "$server" -m 0711 /var/tessel
	local dir
	for dir in keys stores lander tmp; do
		act /usr/bin/install -d -o "$server" -g "$server" -m 0700 "/var/tessel/$dir"
	done
	act /usr/bin/install -d -o "$server" -g "$runner" -m 0750 /var/tessel/handoff
	act /usr/bin/install -d -o "$server" -g "$server" -m 0755 /var/tessel/cargo
	act /usr/bin/install -d -o root -g wheel -m 0755 /var/tessel-runner
	act /usr/bin/install -d -o "$runner" -g "$runner" -m 0700 /var/tessel-runner/work /var/tessel-runner/tmp
	[ -e /var/log/tessel-server.log ] || act /usr/bin/install -o "$server" -g "$server" -m 0644 /dev/null /var/log/tessel-server.log
}

install_binaries() {
	local binary
	for binary in tessel-server tessel tessel-runner; do
		act /usr/bin/install -o root -g wheel -m 0755 "$binaries/$binary" "/opt/tessel/bin/$binary"
	done
}

install_toolchain() {
	if [ -x "$prefix/bin/rustc" ] && "$prefix/bin/rustc" -V 2>/dev/null | /usr/bin/grep -q " $channel "; then
		echo "the toolchain $channel is in place"
		return
	fi
	local work
	if [ "$dry" = 1 ]; then
		echo "would download https://static.rust-lang.org/dist/$installer, check its sha256 against"
		echo "  $here/toolchain.sha256, unpack it and run its install.sh --prefix=$prefix"
		return
	fi
	work=$(/usr/bin/mktemp -d /var/tessel/tmp/toolchain.XXXXXX)
	/usr/bin/curl -fsSL --proto '=https' -o "$work/$installer" "https://static.rust-lang.org/dist/$installer"
	(cd "$work" && /usr/bin/shasum -a 256 -c "$here/toolchain.sha256") ||
		fail "the downloaded $installer does not match toolchain.sha256"
	/usr/bin/tar -xJf "$work/$installer" -C "$work"
	"$work/${installer%.tar.xz}/install.sh" "--prefix=$prefix" --disable-ldconfig
	/bin/rm -r "$work"
}

install_sudoers() {
	local tmp=/var/tessel/tmp/sudoers.candidate
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
	echo "$text" >"$tmp"
	/usr/sbin/visudo -cf "$tmp" || fail "visudo refused the rule; /etc/sudoers.d/tessel is unchanged"
	/usr/bin/install -o root -g wheel -m 0440 "$tmp" /etc/sudoers.d/tessel
	/bin/rm "$tmp"
}

install_plist() {
	put /Library/LaunchDaemons/dev.tessel.server.plist root wheel 0644 <<PLIST
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
}

install_configurations() {
	put /var/tessel-runner/tessel-runner.toml root wheel 0644 <<RUNNER
[runner]
user = "$runner"
timeout_secs = 720
RUNNER
	put /var/tessel/cargo/config.toml "$server" "$server" 0644 <<CARGO
[build]
jobs = 6
CARGO
	if [ -e /var/tessel/tessel-server.toml ]; then
		echo "/var/tessel/tessel-server.toml is in place"
		return
	fi
	put /var/tessel/tessel-server.toml "$server" "$server" 0600 <<'SERVER'
listen = "127.0.0.1:7878"
store_dir = "/var/tessel/stores"
identity = { keys = "/var/tessel/keys", audience = "dogfood" }

[lander]
work_dir = "/var/tessel/lander"
runner_config = "/var/tessel-runner/tessel-runner.toml"
token_file = "/var/tessel/github-token"

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
	"tessel-runner/src/", "tessel-lander/src/", "src/coordinator/merging.rs", "src/shell.rs",
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
install_configurations
echo "install: done. The daemon is not loaded and no secret has moved; run check-own-users.sh --installed"

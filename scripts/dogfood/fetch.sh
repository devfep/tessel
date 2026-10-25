#!/bin/bash
# Fetches the crates a held submission needs into /var/tessel/cargo (docs/proposals/own-users.md,
# section 4), because the runner tests with --offline.
#
# Usage: sudo /opt/tessel/sbin/fetch.sh <fork url> <commit> [--dry-run]
#
# Felix runs this in his own terminal, before the orchestrator approves a submission that changes
# Cargo.lock, .cargo/ or rust-toolchain.toml, and types his password for sudo. There is no NOPASSWD
# rule for it: cargo reads .cargo/config.toml in every ancestor of its directory and can run a
# credential provider named there, as _tessel, so an agent must not be able to start it on a
# commit it pushed. As _tessel, under env -i, the script fetches the commit from the agent's
# private fork with the lander's token (through the askpass hook), checks it out under
# /var/tessel/tmp (nothing the runner can write is above it), prints the commit's .cargo/ diff
# against main and stops unless Felix types yes, then runs `cargo fetch --locked` with the root-
# owned toolchain by absolute path. Fetch builds and runs nothing else.
set -euo pipefail

if [ "${TESSEL_CLEAN:-}" != 1 ]; then
	exec /usr/bin/env -i TESSEL_CLEAN=1 SUDO_USER="${SUDO_USER:-}" PATH=/usr/bin:/bin:/usr/sbin:/sbin /bin/bash "$0" "$@"
fi

dry=0
fork=""
commit=""
config=/var/tessel/tessel-server.toml
toolchain=/var/tessel/toolchain

fail() {
	echo "fetch: $*" >&2
	exit 1
}

usage="usage: fetch.sh <fork url> <commit> [--dry-run]"
while [ "$#" -gt 0 ]; do
	case "$1" in
	--dry-run) dry=1 ;;
	-*) fail "$usage" ;;
	*)
		if [ -z "$fork" ]; then
			fork=$1
		elif [ -z "$commit" ]; then
			commit=$1
		else
			fail "$usage"
		fi
		;;
	esac
	shift
done
[ -n "$fork" ] && [ -n "$commit" ] || fail "$usage"
echo "$fork" | /usr/bin/grep -Eq '^https://github\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+\.git$' ||
	fail "the fork must be an https://github.com/<owner>/<name>.git URL"
echo "$commit" | /usr/bin/grep -Eq '^[0-9a-f]{40}$' || fail "the commit must be 40 lowercase hex digits"
if [ "$dry" != 1 ]; then
	[ "$(/usr/bin/uname -s)" = Darwin ] || fail "this script runs on the Mac that hosts the server"
	[ "$(/usr/bin/id -u)" = 0 ] || fail "run it as root: sudo /opt/tessel/sbin/fetch.sh"
	[ -n "${SUDO_USER:-}" ] || fail "run it through sudo from your own terminal, not from a root shell"
	[ -t 0 ] && [ -t 1 ] || fail "run it in a terminal: it asks you to read a diff"
	[ -x "$toolchain/bin/cargo" ] || fail "$toolchain/bin/cargo is missing: run install.sh"
fi

if [ "$dry" = 1 ]; then
	remote=$(/usr/bin/awk -F'"' '/^remote = /{ print $2 }' "$config" 2>/dev/null || true)
	echo "would, as _tessel under env -i with the lander token through the askpass hook:"
	echo "  fetch main from ${remote:-<remote in $config>} and $commit from $fork into /var/tessel/tmp/fetch.*"
	echo "  print the .cargo/ diff between them and ask for yes if it is not empty"
	echo "  check the commit out and run: $toolchain/bin/cargo fetch --locked (CARGO_HOME=/var/tessel/cargo)"
	exit 0
fi

remote=$(/usr/bin/awk -F'"' '/^remote = /{ print $2 }' "$config")
echo "$remote" | /usr/bin/grep -Eq '^https://github\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+\.git$' ||
	fail "cannot read a github remote from $config"

as_tessel() {
	/usr/bin/sudo -n -u _tessel /usr/bin/env -i HOME=/var/empty \
		PATH="$toolchain/bin:/usr/bin:/bin" \
		GIT_ASKPASS=/opt/tessel/bin/tessel-server GIT_TERMINAL_PROMPT=0 \
		TESSEL_ASKPASS_TOKEN_FILE=/var/tessel/github-token TESSEL_ASKPASS_HOSTS=github.com \
		GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
		CARGO_HOME=/var/tessel/cargo "$@"
}

dir=$(as_tessel /usr/bin/mktemp -d /var/tessel/tmp/fetch.XXXXXX)
trap 'as_tessel /bin/rm -r "$dir"' EXIT
git() { as_tessel /usr/bin/git -C "$dir" "$@"; }

git init -q
git fetch -q --no-tags --depth=1 "$remote" main
main=$(git rev-parse FETCH_HEAD)
git fetch -q --no-tags --depth=1 "$fork" "$commit"
diff=$(git diff "$main" "$commit" -- .cargo | /usr/bin/tr -cd '\11\12\40-\176' | /usr/bin/head -n 200)
if [ -n "$diff" ]; then
	echo "$commit changes .cargo/ (shown as untrusted text, at most 200 lines):"
	echo "$diff" | /usr/bin/sed 's/^/  | /'
	printf 'cargo will read this configuration as _tessel. Type yes to go on: '
	read -r answer </dev/tty
	[ "$answer" = yes ] || fail "not confirmed; nothing was fetched"
fi
git checkout -q --detach "$commit"
as_tessel "$toolchain/bin/cargo" fetch --locked --manifest-path "$dir/Cargo.toml"
echo "fetch: done for $commit"

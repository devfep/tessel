#!/bin/bash
# Mints the agent tokens of lane-a, lane-b and orchestrator as Felix, with the key that only
# _tessel can read (docs/proposals/own-users.md, section 1).
#
# Usage: /opt/tessel/sbin/tokens.sh --kid <kid>
#
# Run it as yourself from the installed copy in /opt/tessel/sbin, not from a checkout and not
# under sudo: it calls sudo itself, for one command, and your shell makes the files, so they are
# yours and mode 0600 in ~/.tessel/tokens. _tessel cannot write under your home.
set -euo pipefail

if [ "${TESSEL_CLEAN:-}" != 1 ]; then
	exec /usr/bin/env -i TESSEL_CLEAN=1 HOME="$HOME" PATH=/usr/bin:/bin /bin/bash "$0" "$@"
fi

fail() {
	echo "tokens: $*" >&2
	exit 1
}

[ "$#" -eq 2 ] && [ "$1" = "--kid" ] || fail "usage: tokens.sh --kid <kid>"
kid=$2
case "$kid" in
"" | *[!A-Za-z0-9._-]*) fail "the kid may hold only letters, digits, '.', '_' and '-'" ;;
esac
[ "$(/usr/bin/id -u)" != 0 ] || fail "run it as yourself, not as root"
[ "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" = /opt/tessel/sbin ] ||
	fail "run the installed copy in /opt/tessel/sbin"

umask 077
/bin/mkdir -p "$HOME/.tessel/tokens"
for agent in lane-a lane-b orchestrator; do
	# The redirect is meant to be yours: _tessel cannot write under your home.
	# shellcheck disable=SC2024
	/usr/bin/sudo -u _tessel /opt/tessel/bin/tessel-server admin token \
		--config /var/tessel/tessel-server.toml --kid "$kid" \
		--tenant devfep --repo tessel --agent "$agent" --ttl 7d \
		>"$HOME/.tessel/tokens/$agent.new"
	/bin/mv "$HOME/.tessel/tokens/$agent.new" "$HOME/.tessel/tokens/$agent"
	echo "minted a token for $agent in $HOME/.tessel/tokens/$agent"
done

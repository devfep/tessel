#!/usr/bin/env bash
# Runs the landing end to end (landing-e2e.sh) with the last CLI release before `Welcome` carried
# the protocol range, against the current server. That CLI does not know the field, so it must
# connect, claim, submit, hear the landing and release its claim all the same. It cannot see the
# field, so it prints no range warning and nothing here looks for one; it just has to work.
#
# TESSEL_OLD_CLI names that CLI's `tessel` binary; the CI job builds it from the commit before
# "Announce the protocol range in Welcome and warn in the CLI" and never builds it in `cargo test`.
# The current `tessel-server` and `tessel` come from TESSEL_BIN_DIR (default target/debug).
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
bin="${TESSEL_BIN_DIR:-$root/target/debug}"
old="${TESSEL_OLD_CLI:?set TESSEL_OLD_CLI to the old releases tessel binary}"

fail() {
  echo "old-cli: $*" >&2
  exit 1
}

[ -x "$old" ] || fail "$old is not an executable"
# The name of the field is in a CLI that reads it and in no CLI that does not: this is the proof that
# the binary under test is the old one, and that the current one is not.
grep -aq protocol_range "$bin/tessel" || fail "the current tessel does not know protocol_range"
if grep -aq protocol_range "$old"; then
  fail "$old knows protocol_range: it is not the release before the field"
fi

dir=$(mktemp -d)
trap 'rm -rf -- "$dir"' EXIT
ln -s "$bin/tessel-server" "$dir/tessel-server"
ln -s "$old" "$dir/tessel"
TESSEL_BIN_DIR="$dir" "$root/tessel-server/ci/landing-e2e.sh"
echo "old-cli: the CLI before the protocol range field landed a commit through the current server"

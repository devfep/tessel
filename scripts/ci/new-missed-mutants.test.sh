#!/usr/bin/env bash
# Tests scripts/ci/new-missed-mutants.sh against planted lists.
#
# Usage: scripts/ci/new-missed-mutants.test.sh
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
script=$root/scripts/ci/new-missed-mutants.sh
work=$(mktemp -d)
trap 'rm -r "$work"' EXIT
failures=0

baseline=$work/baseline.txt
cat >"$baseline" <<'LIST'
src/merge.rs: replace land -> bool with true
src/merge.rs: replace land -> bool with true
src/shell.rs: delete match arm Mode::Edit
LIST

expect() {
  local label=$1 want=$2 missed=$3
  local got=0
  "$script" "$baseline" "$missed" >"$work/out.txt" 2>&1 || got=$?
  if [ "$got" -ne "$want" ]; then
    echo "FAIL: $label: exit $got, want $want"
    sed 's/^/  | /' "$work/out.txt"
    failures=$((failures + 1))
  else
    echo "ok: $label"
  fi
}

# Same mutants at different lines and columns.
cat >"$work/unchanged.txt" <<'LIST'
src/merge.rs:10:5: replace land -> bool with true
src/merge.rs:40:9: replace land -> bool with true
src/shell.rs:7:3: delete match arm Mode::Edit
LIST
expect "unchanged list passes" 0 "$work/unchanged.txt"

# Killing a mutant is fine.
cat >"$work/shrunk.txt" <<'LIST'
src/merge.rs:10:5: replace land -> bool with true
LIST
expect "fewer missed mutants pass" 0 "$work/shrunk.txt"

: >"$work/empty.txt"
expect "empty list passes" 0 "$work/empty.txt"

cat "$work/unchanged.txt" >"$work/new.txt"
echo "src/protocol.rs:3:1: replace Claim::overlaps -> bool with false" >>"$work/new.txt"
expect "planted new missed mutant fails" 1 "$work/new.txt"
grep -q "Claim::overlaps" "$work/out.txt" || {
  echo "FAIL: output does not name the new mutant"
  failures=$((failures + 1))
}

cat "$work/unchanged.txt" >"$work/copy.txt"
echo "src/shell.rs:99:3: delete match arm Mode::Edit" >>"$work/copy.txt"
expect "planted second copy of a baseline name fails" 1 "$work/copy.txt"
grep -q "found 2, baseline allows 1" "$work/out.txt" || {
  echo "FAIL: output does not report the count"
  failures=$((failures + 1))
}

got=0
"$script" "$baseline" "$work/missing.txt" >/dev/null 2>&1 || got=$?
if [ "$got" -ne 2 ]; then
  echo "FAIL: missing file: exit $got, want 2"
  failures=$((failures + 1))
else
  echo "ok: missing file is a usage error"
fi

if [ "$failures" -ne 0 ]; then
  echo "$failures failure(s)"
  exit 1
fi

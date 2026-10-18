#!/usr/bin/env bash
# Checks docs/reference/runtime-rules.md against the code.
#
# - the table has rows, each with a known status (now, after move, dropped) and what that
#   status needs: a "now" row names tests, an "after move" row names a host or adapter test, a
#   "dropped" row gives a reason;
# - every test a "now" row names is in `cargo test -p tessel-coordinator -- --list`;
# - every `fn` of src/runtime.rs (while that file exists) is in the Functions column of a row.
#
# Usage: scripts/ci/check-named-tests.sh [rules-file]
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
rules=${1:-$root/docs/reference/runtime-rules.md}
runtime=$root/src/runtime.rs
failures=0
min_reason=30

trim() {
  local value=$1
  value=${value#"${value%%[![:space:]]*}"}
  echo "${value%"${value##*[![:space:]]}"}"
}

# The text between backticks in $1, one item per line.
ticked() {
  echo "$1" | grep -oE "\`[^\`]+\`" | tr -d "\`" || true
}

fail() {
  echo "check-named-tests: $*" >&2
  failures=$((failures + 1))
}

[[ -f $rules ]] || {
  echo "check-named-tests: no rules table at $rules" >&2
  exit 1
}

# Rule rows are lines that start with "| R<number> |". Cells are split on " | ".
rows=$(grep -E '^\| R[0-9]+ \|' "$rules" || true)

# Any other table line must be the header or the separator, so a malformed row is not skipped.
while IFS= read -r line; do
  [[ $line =~ ^\|\ R[0-9]+\ \| || $line == "| Rule |"* || $line =~ ^\|[-\|[:space:]]+$ ]] ||
    fail "table line is not a rule row (write '| R<number> | ...'): ${line:0:60}"
done < <(grep -E '^[[:space:]]*\|' "$rules" || true)
[[ -n $rows ]] || {
  echo "check-named-tests: $rules has no rule rows" >&2
  exit 1
}

listed=$(cd "$root" && cargo test -p tessel-coordinator -- --list 2>&1 |
  sed -n 's/^\(.*\): test$/\1/p')
[[ -n $listed ]] || {
  echo "check-named-tests: 'cargo test -p tessel-coordinator -- --list' listed no tests" >&2
  exit 1
}

covered_fns=""
seen_rules=""
while IFS= read -r row; do
  IFS='|' read -r _ rule _lines _statement _lives status tests _then fns _ <<<"$row"
  rule=$(trim "$rule")
  status=$(trim "$status")
  names=$(ticked "$tests")

  if grep -qx "$rule" <<<"$seen_rules"; then
    fail "$rule appears in two rows"
  fi
  seen_rules+="$rule"$'\n'

  case $status in
  now)
    [[ -n $names ]] || fail "$rule is 'now' but names no test"
    while IFS= read -r name; do
      [[ -n $name ]] || continue
      grep -qxF "$name" <<<"$listed" || fail "$rule names a test that does not exist: $name"
    done <<<"$names"
    ;;
  "after move")
    [[ -n $names ]] || fail "$rule is 'after move' but names no host or adapter test"
    while IFS= read -r name; do
      [[ -z $name || $name =~ ^(host|adapter)_[a-z0-9_]+$ ]] ||
        fail "$rule names '$name'; an after-move test is named host_* or adapter_*"
    done <<<"$names"
    ;;
  dropped)
    reason=$(trim "$tests")
    [[ $reason == dropped:* && ${#reason} -ge $min_reason ]] ||
      fail "$rule is 'dropped' without a reason of $min_reason characters (dropped: <reason>)"
    ;;
  *)
    fail "$rule has status '$status'; expected now, after move or dropped"
    ;;
  esac
  covered_fns+=$(ticked "$fns")$'\n'
done <<<"$rows"

if [[ -f $runtime ]]; then
  fn_pattern='s/^[[:space:]]*\(pub \)\{0,1\}\(async \)\{0,1\}fn \([A-Za-z0-9_]*\).*/\3/p'
  runtime_fns=$(sed -n "$fn_pattern" "$runtime" | sort -u)
  while IFS= read -r name; do
    grep -qxF "$name" <<<"$covered_fns" || fail "fn $name of src/runtime.rs is in no row"
  done <<<"$runtime_fns"
fi

if ((failures > 0)); then
  echo "check-named-tests: $failures problem(s)" >&2
  exit 1
fi
echo "check-named-tests: $(grep -c . <<<"$rows") rules checked"

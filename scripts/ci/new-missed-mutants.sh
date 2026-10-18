#!/usr/bin/env bash
# Fails when a missed-mutants list has a mutant the baseline does not allow.
#
# Line and column numbers are stripped from both lists, so a mutant keeps its name when code
# above it moves. Stripping can make two mutants identical, so each name is counted
# (`sort | uniq -c`) and the run fails when a name occurs more often than in the baseline. The
# baseline only shrinks: remove a line when its mutant is killed.
#
# Usage: scripts/ci/new-missed-mutants.sh BASELINE MISSED_FILE...
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 BASELINE MISSED_FILE..." >&2
  exit 2
fi

baseline=$1
shift
for file in "$baseline" "$@"; do
  if [ ! -f "$file" ]; then
    echo "error: $file is not a file" >&2
    exit 2
  fi
done

counted_names() {
  cat "$@" | tr -d '\r' | sed -E -e 's/[[:space:]]+$//' -e 's/:[0-9]+:[0-9]+:/:/' -e '/^$/d' |
    LC_ALL=C sort | uniq -c | sed -E 's/^ +//'
}

baseline_counts=$(counted_names "$baseline")
current_counts=$(counted_names "$@")

new=$(awk '
  FILENAME == ARGV[1] { n = $1; allowed[substr($0, length(n) + 2)] = n; next }
  {
    n = $1
    name = substr($0, length(n) + 2)
    if (n > allowed[name] + 0) {
      printf "%s (found %d, baseline allows %d)\n", name, n, allowed[name] + 0
    }
  }
' <(printf '%s\n' "$baseline_counts") <(printf '%s\n' "$current_counts"))

if [ -n "$new" ]; then
  echo "New missed mutants beyond $baseline:" >&2
  printf '%s\n' "$new" >&2
  exit 1
fi
echo "No missed mutants beyond the baseline."

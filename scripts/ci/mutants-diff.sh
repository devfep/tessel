#!/usr/bin/env bash
# Writes the diff a per-change mutants job mutates, limited to the pathspecs after the output path.
#
# A pull request is diffed against its base. A push to main (a server landing, or a merge) is
# diffed over the range it added, `before` to the new head. A push with no usable `before` (a new
# ref, whose `before` is all zeros, or a forced push that dropped it) has no range: the script
# says so and writes an empty diff, so nothing is mutated, never the whole tree.
#
# Usage: EVENT=... BASE_REF=... BEFORE=... HEAD_SHA=... scripts/ci/mutants-diff.sh OUT [PATHSPEC...]
set -euo pipefail

out=${1:?usage: mutants-diff.sh OUT [PATHSPEC...]}
shift
event=${EVENT:?EVENT is not set}
zeros=0000000000000000000000000000000000000000

if [ "$event" = "pull_request" ]; then
  git diff "origin/${BASE_REF:?BASE_REF is not set}...HEAD" -- "$@" >"$out"
else
  before=${BEFORE:?BEFORE is not set}
  if [ "$before" = "$zeros" ] || ! git cat-file -e "$before^{commit}" 2>/dev/null; then
    echo "::warning::No usable base for this push (before is $before): no mutants run."
    : >"$out"
  else
    git diff "$before" "${HEAD_SHA:?HEAD_SHA is not set}" -- "$@" >"$out"
  fi
fi

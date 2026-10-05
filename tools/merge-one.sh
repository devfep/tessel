#!/usr/bin/env bash
# Merge one task branch into sprint/build under the merge-tree guard.
# Usage: tools/merge-one.sh <branch>
set -euo pipefail

readonly SPRINT="sprint/build"
readonly FROZEN="src/protocol.rs"

fail() {
  echo "ABORT: $*" >&2
  exit 1
}

[ "$#" -eq 1 ] || fail "usage: tools/merge-one.sh <branch>"
readonly BRANCH="$1"

cd "$(git rev-parse --show-toplevel)"

[ "$(git rev-parse --abbrev-ref HEAD)" = "$SPRINT" ] || fail "not on $SPRINT"
[ -z "$(git status --porcelain --untracked-files=no)" ] || fail "tracked changes in the tree"
git rev-parse --verify --quiet "refs/heads/$BRANCH" >/dev/null || fail "no branch $BRANCH"

ours="$(git rev-parse HEAD)"
theirs="$(git rev-parse "refs/heads/$BRANCH")"
base="$(git merge-base "$ours" "$theirs")"
echo "ours ${ours:0:7}  theirs ${theirs:0:7}  base ${base:0:7}"

both="$(comm -12 \
  <(git diff --name-only "$base" "$ours" | sort) \
  <(git diff --name-only "$base" "$theirs" | sort))"
echo "both-touched: ${both:-none}"

if predicted="$(git merge-tree --write-tree "refs/heads/$SPRINT" "refs/heads/$BRANCH")"; then
  echo "merge-tree $predicted"
else
  echo "$predicted"
  fail "merge-tree conflict"
fi

deleted="$(git diff --diff-filter=D --name-only "$ours" "$predicted")"
[ -z "$deleted" ] || fail "merge would delete files: $deleted"

git diff --quiet "$ours" "$predicted" -- "$FROZEN" ||
  fail "$FROZEN would change; the protocol is frozen and needs Felix's approval"

git merge --no-ff --no-edit "$BRANCH" || fail "git merge failed"

[ "$(git rev-parse 'HEAD^{tree}')" = "$predicted" ] || fail "tree != merge-tree"

git log --oneline -3

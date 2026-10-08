#!/usr/bin/env bash
# Approve a held submission as the agent `orchestrator`, after the Opus review's Yes and the
# orchestrator's gate (BUILD-PROTOCOL §2). Felix runs it: the session's permission classifier
# refuses the orchestrator's own approval as self-approval.
#
# Usage: tools/orch-review.sh <claim-id> "<note>"
# TESSEL_BIN overrides the tessel binary (default: the ax-mcp worktree's debug build).
#
# Mints an `orchestrator` identity with the steward admin token, starts a daemon in a throwaway
# worktree of the trunk, approves, stops the daemon and removes the worktree. Prints no secrets.
set -euo pipefail

claim="${1:?usage: orch-review.sh <claim-id> <note>}"
note="${2:?usage: orch-review.sh <claim-id> <note>}"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
tessel="${TESSEL_BIN:-$repo_root/.claude/worktrees/ax-mcp/target/debug/tessel}"
wt="$(mktemp -d)/orch-wt"

git -C "$repo_root" fetch -q origin artifacts-trunk
git -C "$repo_root" worktree add -q --detach "$wt" origin/artifacts-trunk
trap 'git -C "$repo_root" worktree remove --force "$wt"' EXIT

set -a
# shellcheck source=/dev/null  # secrets file, gitignored; not available to shellcheck
. "$repo_root/tessel-steward/.dev.vars"
set +a

minted="$(
  printf 'header = "Authorization: Bearer %s"\n' "$STEWARD_ADMIN_TOKEN" |
    curl --fail --silent --show-error --max-time 30 --request POST --config - \
      "https://tessel-steward.devfep.workers.dev/repos/tessel-dogfood/agents/orchestrator/identity"
)"
unset STEWARD_ADMIN_TOKEN

export TESSEL_COORDINATOR=wss://tessel-coordinator.devfep.workers.dev
export TESSEL_REPO=tessel-dogfood
export TESSEL_AGENT=orchestrator
TESSEL_TOKEN="$(jq -er '.token' <<<"$minted")"
export TESSEL_TOKEN
unset minted

cd "$wt"
"$tessel" start "orchestrator: review decision on claim $claim"
"$tessel" review "$claim" --approve --note "$note"
"$tessel" stop

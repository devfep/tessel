#!/usr/bin/env bash
# Creates the spike's test repository, workflow, fork, rulesets (disabled) and webhook.
# Idempotent: a second run changes nothing. Runs with the stored gh login (devfep).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$here/lib.sh"

ensure_repo() {
  if repo_exists "$ORG/$REPO"; then
    log "repo $ORG/$REPO exists"
    return
  fi
  gh repo create "$ORG/$REPO" --public --add-readme \
    --description "Tessel GitHub adapter spike target (throwaway)" >/dev/null
  log "created $ORG/$REPO"
}

ensure_workflow() {
  local path=.github/workflows/check.yml
  if gh api "repos/$ORG/$REPO/contents/$path" >/dev/null 2>&1; then
    log "workflow exists"
    return
  fi
  local body
  body=$(
    cat <<YAML
name: check
on:
  pull_request:
permissions:
  contents: read
jobs:
  $CHECK_NAME:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
YAML
  )
  gh api -X PUT "repos/$ORG/$REPO/contents/$path" -f message="Add the spike check workflow" \
    -f content="$(printf '%s\n' "$body" | base64 | tr -d '\n')" >/dev/null
  log "created workflow"
}

ensure_fork() {
  if repo_exists "$FORK_OWNER/$REPO"; then
    require_our_fork
    log "fork $FORK_OWNER/$REPO exists"
    return
  fi
  gh repo fork "$ORG/$REPO" --clone=false >/dev/null
  for _ in $(seq 1 30); do
    repo_exists "$FORK_OWNER/$REPO" && break
    sleep 2
  done
  repo_exists "$FORK_OWNER/$REPO" || die "fork did not appear after 60 s"
  log "created fork $FORK_OWNER/$REPO"
}

ensure_hook() {
  local existing
  existing=$(gh api "repos/$ORG/$REPO/hooks" \
    --jq ".[] | select(.config.url == \"$HOOK_URL\") | .id")
  if [[ -n $existing ]]; then
    log "webhook exists ($existing)"
    return
  fi
  jq -n --arg url "$HOOK_URL" '{name: "web", active: true,
    events: ["pull_request", "check_run", "check_suite", "push"],
    config: {url: $url, content_type: "json", insecure_ssl: "0"}}' |
    gh api -X POST "repos/$ORG/$REPO/hooks" --input - --jq '"created webhook \(.id)"'
}

ensure_repo
ensure_workflow
ensure_fork
ruleset_apply "$RULESET_A" disabled
ruleset_apply "$RULESET_B" disabled
ensure_hook

#!/usr/bin/env bash
# Shared constants and helpers for the GitHub spike scripts. Sourced, not run.
# shellcheck shell=bash
# shellcheck disable=SC2034 # constants are used by the scripts that source this file

ORG=tessel-sandbox
REPO=spike-target
FORK_OWNER=devfep
APP_ID=5368982
INSTALLATION_ID=172710556
GITHUB_ACTIONS_APP_ID=15368
CHECK_NAME=check
HOOK_URL=https://tessel-spike-unreachable.invalid/hook
RULESET_A=spike-ruleset-a-lander-only
RULESET_B=spike-ruleset-b-required-checks

log() { printf '%s\n' "$*" >&2; }
die() {
  log "error: $*"
  exit 1
}

# Removes anything token-shaped from stdin.
redact() {
  sed -E -e 's/(ghs|ghp|gho|ghu|ghr)_[A-Za-z0-9]+/[redacted]/g' \
    -e 's/(github_pat)_[A-Za-z0-9_]+/[redacted]/g' \
    -e 's/eyJ[A-Za-z0-9_-]+(\.[A-Za-z0-9_-]+)*/[redacted]/g'
}

# Seconds since the epoch for a GitHub ISO-8601 timestamp such as 2026-10-18T12:00:00Z.
epoch() { jq -nr --arg t "$1" '$t | sub("\\.[0-9]+"; "") | fromdateiso8601'; }

repo_exists() { gh api "repos/$1" >/dev/null 2>&1; }

# Dies unless devfep/spike-target is a fork of the sandbox repository: never touch another repo.
require_our_fork() {
  local ours
  ours=$(gh api "repos/$FORK_OWNER/$REPO" \
    --jq ".fork and .parent.full_name == \"$ORG/$REPO\"" 2>/dev/null) || ours=
  [[ $ours == true ]] ||
    die "$FORK_OWNER/$REPO exists but is not a fork of $ORG/$REPO; refusing to use it"
}

ruleset_id() {
  gh api "repos/$ORG/$REPO/rulesets" --jq ".[] | select(.name == \"$1\") | .id"
}

# Prints the ruleset body for the given name and enforcement ("active" or "disabled").
ruleset_body() {
  local name=$1 enforcement=$2
  case $name in
  "$RULESET_A")
    jq -n --arg name "$name" --arg enf "$enforcement" --argjson app "$APP_ID" '{
        name: $name, target: "branch", enforcement: $enf,
        conditions: {ref_name: {include: ["~DEFAULT_BRANCH"], exclude: []}},
        bypass_actors: [{actor_id: $app, actor_type: "Integration", bypass_mode: "always"}],
        rules: [{type: "update"}, {type: "non_fast_forward"}]}'
    ;;
  "$RULESET_B")
    jq -n --arg name "$name" --arg enf "$enforcement" --arg check "$CHECK_NAME" \
      --argjson gha "$GITHUB_ACTIONS_APP_ID" '{
        name: $name, target: "branch", enforcement: $enf,
        conditions: {ref_name: {include: ["~DEFAULT_BRANCH"], exclude: []}},
        bypass_actors: [],
        rules: [{type: "required_linear_history"},
          {type: "required_status_checks", parameters: {
            strict_required_status_checks_policy: false,
            required_status_checks: [{context: $check, integration_id: $gha}]}}]}'
    ;;
  *) die "unknown ruleset $name" ;;
  esac
}

# Creates or updates the named ruleset with the given enforcement. Idempotent.
ruleset_apply() {
  local name=$1 enforcement=$2 id
  id=$(ruleset_id "$name")
  if [[ -z $id ]]; then
    ruleset_body "$name" "$enforcement" |
      gh api -X POST "repos/$ORG/$REPO/rulesets" --input - \
        --jq '"created ruleset \(.name) (\(.enforcement))"'
  else
    ruleset_body "$name" "$enforcement" |
      gh api -X PUT "repos/$ORG/$REPO/rulesets/$id" --input - \
        --jq '"updated ruleset \(.name) (\(.enforcement))"'
  fi
}

main_sha() { gh api "repos/$ORG/$REPO/git/ref/heads/main" --jq .object.sha; }

# gh_as TOKEN ARGS...: runs gh with TOKEN in the environment, or the stored login if empty.
gh_as() {
  local token=$1
  shift
  if [[ -n $token ]]; then
    GH_TOKEN=$token gh "$@"
  else
    gh "$@"
  fi
}

repo_node_id() { gh api "repos/$ORG/$REPO" --jq .node_id; }

# update_ref_as TOKEN_ENV_VALUE BEFORE AFTER: GraphQL updateRefs on main. Prints the
# GraphQL response (redacted) and returns 0 only when GitHub accepted the update.
# The token is passed to gh through the environment, never as an argument. An empty
# first argument uses the stored gh login.
update_ref_as() {
  local token=$1 before=$2 after=$3 out id
  id=$(repo_node_id) || id=
  if [[ -z $id ]]; then
    printf 'error: could not read the repository node id\n'
    return 1
  fi
  # shellcheck disable=SC2016 # GraphQL variables, not shell expansions
  local query='mutation($repo: ID!, $before: GitObjectID!, $after: GitObjectID!) {
    updateRefs(input: {repositoryId: $repo, refUpdates:
      [{name: "refs/heads/main", beforeOid: $before, afterOid: $after}]}) {
      clientMutationId } }'
  out=$(gh_as "$token" api graphql -f query="$query" -f repo="$id" \
    -f before="$before" -f after="$after" 2>&1) || {
    printf '%s\n' "$out" | redact
    return 1
  }
  printf '%s\n' "$out" | redact
  if printf '%s' "$out" | jq -e '.errors' >/dev/null 2>&1; then
    return 1
  fi
}

# expect_refused LABEL PATTERN CMD...: passes only when the command fails with output matching
# the extended regex PATTERN. Any other failure (bad credentials, network) is UNEXPECTED.
expect_refused() {
  local label=$1 pattern=$2 out
  shift 2
  if out=$("$@" 2>&1); then
    log "UNEXPECTED ACCEPT  $label"
    return 1
  fi
  if ! printf '%s' "$out" | grep -qE "$pattern"; then
    log "UNEXPECTED REFUSAL: $label: $(printf '%s' "$out" | redact | head -c 300)"
    return 1
  fi
  log "refused (as expected)  $label"
  printf '%s\n' "$out" | redact | head -c 600 | sed 's/^/    /' >&2
  printf '\n' >&2
}

# expect_accepted LABEL CMD...: passes when the command succeeds.
expect_accepted() {
  local label=$1 out
  shift
  if ! out=$("$@" 2>&1); then
    log "UNEXPECTED REFUSAL: $label: $(printf '%s' "$out" | redact | head -c 300)"
    return 1
  fi
  log "accepted (as expected)  $label"
}

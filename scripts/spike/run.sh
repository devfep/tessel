#!/usr/bin/env bash
# Runs the GitHub spike experiments against tessel-sandbox/spike-target.
#
#   run.sh user      experiments that need only the devfep gh login (questions 1, 2b, 3)
#   run.sh app       experiments as the lander App (questions 1 and 2a with the bypass actor)
#   run.sh webhooks  webhook delivery latency from the hook's recent deliveries
#
# Full run, in this order: teardown.sh, setup.sh, run.sh user, run.sh app. Neither `user` nor
# `app` can be repeated on the same repository (`user` refuses a main that already holds spike
# commits; `app` needs PR spike-z open and would collide on the spike-w branch), so recover with
# teardown.sh then setup.sh. Timestamps are GitHub's, one second resolution. The installation
# token is minted in this file only, narrowed to contents write, kept in APP_TOKEN, revoked on
# exit, and reaches gh and curl through the environment or stdin, never an argument or a file.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$here/lib.sh"

APP_KEY=$HOME/.config/tessel-spike/tessel-lander-spike.pem
APP_TOKEN=
HEAD_SHA=
PR_NUM=
T_PUSH=0
T_PR=0

b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

# Prints a nine-minute App JWT. Only mint_app_token calls this.
app_jwt() {
  local now header payload signature
  [[ -r $APP_KEY ]] || die "App key not readable at $APP_KEY"
  now=$(date +%s)
  header=$(printf '{"alg":"RS256","typ":"JWT"}' | b64url)
  payload=$(printf '{"iat":%d,"exp":%d,"iss":"%s"}' $((now - 60)) $((now + 540)) "$APP_ID" | b64url)
  signature=$(printf '%s.%s' "$header" "$payload" | openssl dgst -sha256 -sign "$APP_KEY" | b64url)
  printf '%s.%s.%s' "$header" "$payload" "$signature"
}

# Mints a one-hour installation token limited to the spike repository into APP_TOKEN.
mint_app_token() {
  local jwt response
  jwt=$(app_jwt)
  response=$(printf 'header = "Authorization: Bearer %s"\n' "$jwt" |
    curl -q -sS --proto '=https' -K - -X POST -H 'Accept: application/vnd.github+json' \
      -d "{\"repositories\":[\"$REPO\"],\"permissions\":{\"contents\":\"write\"}}" \
      "https://api.github.com/app/installations/$INSTALLATION_ID/access_tokens")
  jwt=
  APP_TOKEN=$(printf '%s' "$response" | jq -r '.token // empty')
  if [[ -z $APP_TOKEN ]]; then
    die "no installation token: $(printf '%s' "$response" | jq -r '.message // "unknown error"')"
  fi
  trap revoke_app_token EXIT
}

# Revokes APP_TOKEN. Prints nothing derived from the token.
revoke_app_token() {
  [[ -n $APP_TOKEN ]] || return 0
  printf 'header = "Authorization: Bearer %s"\n' "$APP_TOKEN" |
    curl -q -sS --proto '=https' -K - -X DELETE -o /dev/null \
      'https://api.github.com/installation/token' || log "warning: token revocation failed"
  APP_TOKEN=
}

# Creates a fork branch one commit ahead of upstream main and opens a pull request from it.
# Sets HEAD_SHA, PR_NUM, T_PUSH (commit time) and T_PR (pull request creation time).
create_pr() {
  local name=$1 fork_main commit
  gh api -X POST "repos/$FORK_OWNER/$REPO/merge-upstream" -f branch=main >/dev/null
  fork_main=$(gh api "repos/$FORK_OWNER/$REPO/git/ref/heads/main" --jq .object.sha)
  [[ $fork_main == "$(main_sha)" ]] || die "fork main $fork_main differs from upstream main"
  gh api -X POST "repos/$FORK_OWNER/$REPO/git/refs" -f ref="refs/heads/spike-$name" \
    -f sha="$fork_main" >/dev/null
  commit=$(gh api -X PUT "repos/$FORK_OWNER/$REPO/contents/spike/$name.txt" \
    -f message="Spike commit $name" -f branch="spike-$name" \
    -f content="$(printf '%s\n' "$name" | base64)")
  HEAD_SHA=$(printf '%s' "$commit" | jq -r .commit.sha)
  T_PUSH=$(epoch "$(printf '%s' "$commit" | jq -r .commit.committer.date)")
  local pr
  pr=$(gh api -X POST "repos/$ORG/$REPO/pulls" -f title="Spike $name" \
    -f head="$FORK_OWNER:spike-$name" -f base=main)
  PR_NUM=$(printf '%s' "$pr" | jq -r .number)
  T_PR=$(epoch "$(printf '%s' "$pr" | jq -r .created_at)")
  log "PR #$PR_NUM for $name: head $HEAD_SHA, opened $((T_PR - T_PUSH)) s after the commit"
}

# Waits up to 300 s for the check run on HEAD_SHA to complete and prints its timings.
await_check() {
  local run started completed created
  local status=none
  for _ in $(seq 1 100); do
    run=$(gh api "repos/$ORG/$REPO/commits/$HEAD_SHA/check-runs" \
      --jq "[.check_runs[] | select(.name == \"$CHECK_NAME\")] | first // empty")
    status=$(printf '%s' "${run:-null}" | jq -r '.status // "none"')
    [[ $status == completed ]] && break
    sleep 3
  done
  [[ $status == completed ]] || die "check on $HEAD_SHA not completed after 300 s (status $status)"
  started=$(epoch "$(printf '%s' "$run" | jq -r .started_at)")
  completed=$(epoch "$(printf '%s' "$run" | jq -r .completed_at)")
  created=$(epoch "$(gh api "repos/$ORG/$REPO/actions/runs?head_sha=$HEAD_SHA" \
    --jq '.workflow_runs[0].created_at')")
  log "TIMING $HEAD_SHA: commit->run created $((created - T_PUSH)) s," \
    "commit->check started $((started - T_PUSH)) s," \
    "commit->check completed $((completed - T_PUSH)) s," \
    "PR opened->check completed $((completed - T_PR)) s," \
    "conclusion $(printf '%s' "$run" | jq -r .conclusion)," \
    "app $(printf '%s' "$run" | jq -r .app.slug)"
}

assert_main() {
  [[ $(main_sha) == "$1" ]] || die "main is $(main_sha), expected $1 ($2)"
}

stage_user() {
  local base0 x_head y_head z_head
  gh api "repos/$ORG/$REPO/contents/spike" >/dev/null 2>&1 &&
    die "main already has spike commits; run teardown.sh then setup.sh"
  ruleset_apply "$RULESET_A" disabled
  ruleset_apply "$RULESET_B" active
  base0=$(main_sha)

  log "== Q1 (user token, only ruleset B active)"
  create_pr x
  x_head=$HEAD_SHA
  expect_refused "Q1 control: check not finished" 'Required status check' \
    update_ref_as "" "$base0" "$x_head"
  assert_main "$base0" "refused update must not move main"
  await_check
  expect_accepted "Q1: fork PR check completed" update_ref_as "" "$base0" "$x_head"
  assert_main "$x_head" "Q1 landing"

  log "== Q2a-pre (user token, only ruleset B active): stale beforeOid"
  create_pr y
  y_head=$HEAD_SHA
  await_check
  for _ in 1 2 3; do
    expect_refused "Q2a: stale beforeOid (GitHub returns a generic error)" \
      'Something went wrong' \
      update_ref_as "" "$base0" "$y_head"
  done
  assert_main "$x_head" "stale update must not move main"
  expect_accepted "Q2a control: correct beforeOid" update_ref_as "" "$x_head" "$y_head"
  assert_main "$y_head" "Q2a control landing"

  log "== Q2b (user token, rulesets A and B active)"
  create_pr z
  z_head=$HEAD_SHA
  await_check
  ruleset_apply "$RULESET_A" active
  expect_refused "Q2b: non-bypass actor, check passed, correct beforeOid" \
    'Cannot update this protected ref' \
    update_ref_as "" "$y_head" "$z_head"
  assert_main "$y_head" "ruleset A must hold"
  log "PR for spike-z (head $z_head) is left open for: run.sh app"
}

stage_app() {
  local stale z_head w_head current
  [[ -n $(ruleset_id "$RULESET_A") ]] || die "run setup.sh and run.sh user first"
  ruleset_apply "$RULESET_A" active
  ruleset_apply "$RULESET_B" active
  z_head=$(gh api "repos/$FORK_OWNER/$REPO/git/ref/heads/spike-z" --jq .object.sha)
  current=$(main_sha)
  stale=$(gh api "repos/$ORG/$REPO/commits?sha=main&per_page=3" --jq '.[1].sha')
  mint_app_token

  log "== Q1 and Q2 (App token, rulesets A and B active)"
  expect_accepted "Q1: App bypasses A, check passed on z, correct beforeOid" \
    update_ref_as "$APP_TOKEN" "$current" "$z_head"
  assert_main "$z_head" "App landing of z"

  create_pr w
  w_head=$HEAD_SHA
  expect_refused "Q1 control: App bypasses A but not B, check not finished" \
    'Required status check' \
    update_ref_as "$APP_TOKEN" "$z_head" "$w_head"
  await_check
  expect_refused "Q2a: App bypass actor, stale beforeOid" \
    'Something went wrong' \
    update_ref_as "$APP_TOKEN" "$stale" "$w_head"
  assert_main "$z_head" "stale App update must not move main"
  expect_accepted "Q2a control: App, correct beforeOid" \
    update_ref_as "$APP_TOKEN" "$z_head" "$w_head"
  assert_main "$w_head" "App landing of w"
}

# Prints each delivery's lag behind the event timestamp inside its payload.
stage_webhooks() {
  local hook id d event action delivered ts redelivery
  hook=$(gh api "repos/$ORG/$REPO/hooks" --jq ".[] | select(.config.url == \"$HOOK_URL\") | .id")
  [[ -n $hook ]] || die "no spike webhook; run setup.sh"
  printf 'event\taction\tdelivered_at\tevent_at\tlag_s\tredelivery\tstatus\n'
  for id in $(gh api "repos/$ORG/$REPO/hooks/$hook/deliveries?per_page=100" --jq '.[].id'); do
    d=$(gh api "repos/$ORG/$REPO/hooks/$hook/deliveries/$id")
    event=$(printf '%s' "$d" | jq -r .event)
    action=$(printf '%s' "$d" | jq -r '.action // "-"')
    delivered=$(printf '%s' "$d" | jq -r .delivered_at)
    redelivery=$(printf '%s' "$d" | jq -r .redelivery)
    ts=$(printf '%s' "$d" | jq -r '.request.payload |
      (.check_run.completed_at // .check_run.started_at
        // (if .action == "closed" then .pull_request.closed_at else .pull_request.created_at end)
        // empty)')
    [[ -n $ts ]] || continue
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$event" "$action" "$delivered" "$ts" \
      "$(($(epoch "$delivered") - $(epoch "$ts")))" "$redelivery" \
      "$(printf '%s' "$d" | jq -r .status)"
  done
}

case ${1:-} in
user) stage_user ;;
app) stage_app ;;
webhooks) stage_webhooks ;;
*) die "usage: run.sh user|app|webhooks" ;;
esac

#!/usr/bin/env bash
# Live check of COORD-HEAD on the production coordinator. Prints no token.
set -euo pipefail

steward=https://tessel-steward.devfep.workers.dev
coord=tessel-coordinator.devfep.workers.dev
set -a
# shellcheck source=/dev/null
. /Users/felixpatawah/repos/tessel/tessel-steward/.dev.vars
set +a

admin_post() {
  printf 'header = "Authorization: Bearer %s"\n' "$STEWARD_ADMIN_TOKEN" |
    curl --fail --silent --show-error --max-time 30 --request POST --config - "${steward}$1"
}
identity() { admin_post "/repos/$1/agents/$2/identity" | jq -er .token; }
welcome_head() {
  printf '{"type":"hello","agent":"%s","base":"0000000"}\n' "$2" |
    timeout 15 websocat -n1 "wss://${coord}/repo/$1/ws" -H="Authorization: Bearer $3" |
    jq -r 'select(.type=="welcome") | .head'
}
trunk_head() {
  local minted remote token
  minted="$(admin_post "/repos/$1/read-tokens")"
  remote="$(jq -er .remote <<<"$minted")"
  token="$(jq -er .token <<<"$minted")"
  GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=http.extraHeader \
    GIT_CONFIG_VALUE_0="Authorization: Bearer ${token}" \
    git ls-remote "$remote" refs/heads/main | cut -f1
}
poke() {
  curl --silent --output /dev/null --write-out '%{http_code}' --max-time 30 --request "$2" \
    --config - "https://${coord}/repo/$1/trunk-moved" <<<"${3:+header = \"Authorization: Bearer $3\"}"
}

repo="${1:-demo}"
check_tok="$(identity "$repo" orch-check)"
steward_tok="$(identity "$repo" steward)"
echo "auth: no token $(poke "$repo" POST '')"
echo "auth: non-steward token $(poke "$repo" POST "$check_tok")"
echo "auth: GET with steward token $(poke "$repo" GET "$steward_tok")"
unknown_tok="$(identity swarm-never-created-zz steward)"
echo "unknown repo, steward token $(poke swarm-never-created-zz POST "$unknown_tok")"

before="$(welcome_head "$repo" orch-check "$check_tok")"
trunk="$(trunk_head "$repo")"
echo "$repo: welcome head before ${before:-none}; trunk main ${trunk}"
echo "$repo: steward poke $(poke "$repo" POST "$steward_tok")"
for i in 1 2 3 4 5 6; do
  sleep 5
  after="$(welcome_head "$repo" orch-check "$check_tok")"
  echo "$repo: welcome head after $((i * 5)) s: ${after:-none}"
  [ "$after" = "$trunk" ] && break
done
printf '{"type":"watch","from_seq":0}\n' |
  timeout 10 websocat "wss://${coord}/repo/${repo}/ws" -H="Authorization: Bearer $check_tok" |
  jq -c 'select(.event.event=="base_moved") | .event | {seq, head, by}' | tail -3 || true

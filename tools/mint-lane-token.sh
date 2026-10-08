#!/usr/bin/env bash
# Mints a 1 h write token for one lane fork and stores it, unprinted, in a 0600 git include file.
# Usage: bash tools/mint-lane-token.sh <lane> <dir>   e.g. lint-rule2 <scratchpad>/lane-lint-rule2
# The lane pushes with GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=include.path GIT_CONFIG_VALUE_0=<file>.
set -euo pipefail

lane="${1:?usage: mint-lane-token.sh <lane> <dir>}"
dir="${2:?usage: mint-lane-token.sh <lane> <dir>}"
fork="tessel-dogfood--lane-${lane}"
out="${dir}/push-auth.gitconfig"

set -a
# shellcheck source=/dev/null
. "$(dirname "$0")/../tessel-steward/.dev.vars"
set +a

minted="$(
  printf 'header = "Authorization: Bearer %s"\n' "$STEWARD_ADMIN_TOKEN" |
    curl --fail --silent --show-error --max-time 30 --request POST --config - \
      "https://tessel-steward.devfep.workers.dev/repos/${fork}/tokens"
)"
token="$(jq -er '.token' <<<"$minted")"
expires="$(jq -r '.expiresAt' <<<"$minted")"
unset minted

mkdir -p "$dir"
umask 077
printf '[http]\n\textraHeader = Authorization: Bearer %s\n' "$token" >"$out"
unset token
chmod 600 "$out"
echo "wrote ${out} (mode 600); token for ${fork} expires ${expires}"

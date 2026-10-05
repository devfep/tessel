#!/usr/bin/env bash
# Mirror the Artifacts trunk of Tessel to the GitHub branch sprint/build, fast-forward only.
# Run from a checkout of the GitHub repo, on a machine where `git push origin` is already
# authorised (the local gh login). No GitHub credential ever goes to Cloudflare.
#
# Usage: STEWARD_URL=https://... STEWARD_ADMIN_TOKEN=... tools/mirror.sh [--dry-run]
#
# The steward admin route mints a 10-minute read token for the Artifacts repo. The token is sent
# in a curl config read from stdin and in git's environment, never in an argument list, and the
# script prints only shas and fixed messages.
set -euo pipefail

readonly ARTIFACTS_REPO="${ARTIFACTS_REPO:-tessel}"
readonly TRUNK_REF="refs/heads/main"
readonly MIRROR_BRANCH="sprint/build"
readonly STAGING_REF="refs/tessel/artifacts-trunk"
readonly HTTPS_PATTERN='^https://'
readonly GITHUB_PATTERN='^(https://github\.com/|git@github\.com:)'

fail() {
  echo "ABORT: $*" >&2
  exit 1
}

dry_run=""
case "${1:-}" in
"") ;;
--dry-run) dry_run="yes" ;;
*) fail "usage: tools/mirror.sh [--dry-run]" ;;
esac

: "${STEWARD_URL:?set STEWARD_URL to the steward Worker URL}"
: "${STEWARD_ADMIN_TOKEN:?set STEWARD_ADMIN_TOKEN}"
command -v jq >/dev/null || fail "jq is required"

cd "$(git rev-parse --show-toplevel)"
origin_url="$(git remote get-url origin)"
[[ "$origin_url" =~ $GITHUB_PATTERN ]] || fail "origin is not a GitHub remote"

cleanup() {
  git update-ref -d "$STAGING_REF" 2>/dev/null || true
}
trap cleanup EXIT

minted="$(
  printf 'header = "Authorization: Bearer %s"\n' "$STEWARD_ADMIN_TOKEN" |
    curl --fail --silent --show-error --max-time 30 --request POST --config - \
      "${STEWARD_URL%/}/repos/${ARTIFACTS_REPO}/read-tokens"
)" || fail "the steward did not mint a read token"
remote="$(jq -er '.remote' <<<"$minted")" || fail "the steward's answer has no remote"
token="$(jq -er '.token' <<<"$minted")" || fail "the steward's answer has no token"
unset minted
[[ "$remote" =~ $HTTPS_PATTERN ]] || fail "the steward's remote is not an https URL"

export GIT_TERMINAL_PROMPT=0
GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=http.extraHeader \
  GIT_CONFIG_VALUE_0="Authorization: Bearer ${token}" \
  git fetch --quiet --no-tags -- "$remote" "+${TRUNK_REF}:${STAGING_REF}" ||
  fail "fetching the Artifacts trunk failed"
unset token

new="$(git rev-parse --verify "${STAGING_REF}^{commit}")"
git fetch --quiet --no-tags origin "+refs/heads/${MIRROR_BRANCH}:refs/remotes/origin/${MIRROR_BRANCH}" ||
  fail "fetching origin/${MIRROR_BRANCH} failed"
old="$(git rev-parse --verify "refs/remotes/origin/${MIRROR_BRANCH}^{commit}")"
echo "artifacts trunk ${new:0:12}  origin/${MIRROR_BRANCH} ${old:0:12}"

if [ "$new" = "$old" ]; then
  echo "already up to date"
  exit 0
fi
git merge-base --is-ancestor "$old" "$new" ||
  fail "not a fast-forward: origin/${MIRROR_BRANCH} has commits the Artifacts trunk lacks"

if [ -n "$dry_run" ]; then
  git push --dry-run origin "${new}:refs/heads/${MIRROR_BRANCH}"
  echo "dry run: would mirror ${old:0:12}..${new:0:12}"
else
  git push origin "${new}:refs/heads/${MIRROR_BRANCH}"
  echo "mirrored ${old:0:12}..${new:0:12}"
fi

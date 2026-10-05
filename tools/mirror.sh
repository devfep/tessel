#!/usr/bin/env bash
# Mirror the Artifacts trunk of Tessel to the GitHub branch artifacts-trunk, fast-forward only.
# The first run creates the branch from the Artifacts trunk (nothing to fast-forward from);
# every later run aborts unless the push is a fast-forward.
# Run from a checkout of the GitHub repo, on a machine where `git push origin` is already
# authorised (the local gh login). No GitHub credential ever goes to Cloudflare.
#
# Usage: STEWARD_URL=https://... STEWARD_ADMIN_TOKEN=... tools/mirror.sh [--dry-run]
#
# The steward admin route mints a 10-minute read token for the Artifacts repo. The token is sent
# in a curl config read from stdin and in git's environment, never in an argument list, and the
# script prints only shas and fixed messages.
#
# Test seam: MIRROR_ALLOW_LOCAL_REMOTES=1 skips the GitHub and https checks on the two remotes so
# a test can use local bare repos. Never set it in real use.
set -euo pipefail

readonly ARTIFACTS_REPO="${ARTIFACTS_REPO:-tessel-dogfood}"
readonly TRUNK_REF="refs/heads/main"
readonly MIRROR_BRANCH="artifacts-trunk"
readonly MIRROR_REF="refs/heads/${MIRROR_BRANCH}"
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
if [ -z "${MIRROR_ALLOW_LOCAL_REMOTES:-}" ]; then
  [[ "$origin_url" =~ $GITHUB_PATTERN ]] || fail "origin is not a GitHub remote"
fi

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
if [ -z "${MIRROR_ALLOW_LOCAL_REMOTES:-}" ]; then
  [[ "$remote" =~ $HTTPS_PATTERN ]] || fail "the steward's remote is not an https URL"
fi

export GIT_TERMINAL_PROMPT=0
GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=http.extraHeader \
  GIT_CONFIG_VALUE_0="Authorization: Bearer ${token}" \
  git fetch --quiet --no-tags -- "$remote" "+${TRUNK_REF}:${STAGING_REF}" ||
  fail "fetching the Artifacts trunk failed"
unset token

new="$(git rev-parse --verify "${STAGING_REF}^{commit}")"

# Exit 0: the branch exists. Exit 2: it does not. Anything else is a failure to ask.
ls_status=0
git ls-remote --exit-code --quiet origin "$MIRROR_REF" >/dev/null || ls_status=$?
case "$ls_status" in
0)
  git fetch --quiet --no-tags origin "+${MIRROR_REF}:refs/remotes/origin/${MIRROR_BRANCH}" ||
    fail "fetching origin/${MIRROR_BRANCH} failed"
  old="$(git rev-parse --verify "refs/remotes/origin/${MIRROR_BRANCH}^{commit}")"
  echo "artifacts trunk ${new:0:12}  origin/${MIRROR_BRANCH} ${old:0:12}"
  if [ "$new" = "$old" ]; then
    echo "already up to date"
    exit 0
  fi
  git merge-base --is-ancestor "$old" "$new" ||
    fail "not a fast-forward: origin/${MIRROR_BRANCH} has commits the Artifacts trunk lacks"
  range="${old:0:12}..${new:0:12}"
  ;;
2)
  echo "artifacts trunk ${new:0:12}  origin/${MIRROR_BRANCH} does not exist"
  echo "first run: creating ${MIRROR_BRANCH} from the Artifacts trunk (no fast-forward check)"
  range="new branch at ${new:0:12}"
  ;;
*) fail "could not check origin for ${MIRROR_BRANCH} (git ls-remote exit ${ls_status})" ;;
esac

if [ -n "$dry_run" ]; then
  git push --dry-run origin "${new}:${MIRROR_REF}"
  echo "dry run: would mirror ${range}"
else
  git push origin "${new}:${MIRROR_REF}"
  echo "mirrored ${range}"
fi

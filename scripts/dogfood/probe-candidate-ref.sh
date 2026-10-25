#!/usr/bin/env bash
# Probes whether the git host accepts the candidate ref (docs/proposals/candidate-ref.md): pushes a
# throwaway commit to <namespace>/<sha> of a sandbox remote over HTTPS through the server's
# askpass hook, fetches it into a fresh repository, deletes it with a lease and checks it is gone.
#
# Usage: scripts/dogfood/probe-candidate-ref.sh <token file> <askpass program> [remote [namespace]]
#   token file       the lander's token, mode 0600 (it is read by the hook only, never printed)
#   askpass program  the tessel-server executable that serves as GIT_ASKPASS, e.g.
#                    /opt/tessel/bin/tessel-server
#   remote           default https://github.com/tessel-sandbox/m4-trunk.git
#   namespace        default refs/tessel/candidate; try refs/heads/tessel/candidate if the host
#                    refuses the first (the lander's namespace is one constant, CANDIDATE_NAMESPACE)
#
# Pushes one ref and deletes it. Exits 0 and prints "ok" only when every step worked.
set -euo pipefail

fail() {
	echo "probe-candidate-ref: $*" >&2
	exit 1
}

[ "$#" -ge 2 ] && [ "$#" -le 4 ] || fail "usage: probe-candidate-ref.sh <token file> <askpass program> [remote [namespace]]"
token_file=$1
askpass=$2
remote=${3:-https://github.com/tessel-sandbox/m4-trunk.git}
namespace=${4:-refs/tessel/candidate}

case "$token_file" in
/*) ;;
*) fail "the token file must be an absolute path" ;;
esac
[ -f "$token_file" ] || fail "no token file at $token_file"
[ -x "$askpass" ] || fail "the askpass program is not executable: $askpass"
case "$namespace" in
refs/tessel/candidate | refs/heads/tessel/candidate) ;;
*) fail "the namespace is refs/tessel/candidate or refs/heads/tessel/candidate, got: $namespace" ;;
esac
host=${remote#*://}
host=${host%%/*}

tmp=$(mktemp -d)
trap 'find "$tmp" -delete' EXIT

git_hooked() {
	env -i PATH="$PATH" HOME="$tmp/home" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
		GIT_ASKPASS="$askpass" TESSEL_ASKPASS_TOKEN_FILE="$token_file" \
		TESSEL_ASKPASS_HOSTS="$host" GIT_TERMINAL_PROMPT=0 \
		GIT_AUTHOR_NAME=probe GIT_AUTHOR_EMAIL=probe@example.invalid \
		GIT_COMMITTER_NAME=probe GIT_COMMITTER_EMAIL=probe@example.invalid \
		git "$@"
}

mkdir -p "$tmp/home" "$tmp/src" "$tmp/dst"
git_hooked -C "$tmp/src" init --quiet --initial-branch=probe
git_hooked -C "$tmp/src" commit --quiet --allow-empty -m "candidate ref probe"
sha=$(git_hooked -C "$tmp/src" rev-parse HEAD)
reference="$namespace/$sha"

echo "1 push $reference to $remote"
git_hooked -C "$tmp/src" push --quiet -- "$remote" "+$sha:$reference"

echo "2 ls-remote"
listed=$(git_hooked ls-remote -- "$remote" "$reference")
[ "$listed" = "$(printf '%s\t%s' "$sha" "$reference")" ] || fail "the ref is not listed at the pushed commit: $listed"

echo "3 fetch into a fresh repository"
git_hooked -C "$tmp/dst" init --quiet
git_hooked -C "$tmp/dst" fetch --quiet --no-tags -- "$remote" "+$reference:$reference"
[ "$(git_hooked -C "$tmp/dst" rev-parse "$reference")" = "$sha" ] || fail "the fetched tip is not the pushed commit"

echo "4 delete with a lease"
git_hooked -C "$tmp/src" push --quiet --force-with-lease="$reference:$sha" -- "$remote" ":$reference"
[ -z "$(git_hooked ls-remote -- "$remote" "$reference")" ] || fail "the ref is still listed after the delete"

echo "ok: $namespace works on $remote"

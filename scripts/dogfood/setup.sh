#!/usr/bin/env bash
# Sets up the server directory that coordinates devfep/tessel on this machine (docs/dogfood.md).
#
# Usage: scripts/dogfood/setup.sh <server dir>
#
# Makes <server dir>/{keys,stores,lander,tokens}, writes tessel-runner.toml and tessel-server.toml,
# makes the signing key unless it exists, and mints a token for each of lane-a, lane-b and
# orchestrator. Safe to run again: it rewrites the two configurations and the tokens, and keeps
# the key. It never calls gh, never pushes and never starts the server. Needs `tessel-server` on
# PATH. Tokens are written by redirection and never printed.
set -euo pipefail

kid="2026-10"
tenant="devfep"
repo="tessel"
agents=(lane-a lane-b orchestrator)
token_ttl="7d"
runner_timeout_secs=720

fail() {
	echo "dogfood setup: $*" >&2
	exit 1
}

[ "$#" -eq 1 ] || fail "usage: setup.sh <server dir>"
dir=$1
case "$dir" in
/*) ;;
*) fail "the server directory must be an absolute path, got: $dir" ;;
esac
case "$dir" in
*[\"\\]*) fail "the server directory may not contain a quote or a backslash: $dir" ;;
esac
command -v tessel-server >/dev/null ||
	fail "tessel-server is not on PATH: cargo build -p tessel-server, then add target/debug"

umask 077
mkdir -p "$dir/keys" "$dir/stores" "$dir/lander" "$dir/tokens"

printf '[runner]\nuser = "%s"\ntimeout_secs = %s\n' "$(id -un)" "$runner_timeout_secs" \
	>"$dir/tessel-runner.toml"
chmod 600 "$dir/tessel-runner.toml"

cat >"$dir/tessel-server.toml" <<TOML
listen = "127.0.0.1:7878"
store_dir = "$dir/stores"
identity = { keys = "$dir/keys", audience = "dogfood" }

[lander]
work_dir = "$dir/lander"
runner_config = "$dir/tessel-runner.toml"
token_file = "$dir/github-token"

[[repository]]
tenant = "$tenant"
name = "$repo"
run = "dogfood"
remote = "https://github.com/$tenant/$repo.git"
fork = "https://github.com/$tenant/$repo-{agent}.git"
reviewers = ["orchestrator"]
sensitive = ["src/protocol.rs", "src/host/", ".github/", "docs/BUILD-PROTOCOL.md"]
TOML

if [ -e "$dir/keys/$kid.key" ]; then
	echo "kept the signing key $kid"
else
	tessel-server admin keygen --config "$dir/tessel-server.toml" --kid "$kid"
fi

for agent in "${agents[@]}"; do
	tessel-server admin token --config "$dir/tessel-server.toml" --kid "$kid" \
		--tenant "$tenant" --repo "$repo" --agent "$agent" --ttl "$token_ttl" \
		>"$dir/tokens/$agent.new"
	mv "$dir/tokens/$agent.new" "$dir/tokens/$agent"
	chmod 600 "$dir/tokens/$agent"
	echo "minted a token for $agent, valid $token_ttl, in $dir/tokens/$agent"
done

github_token="$dir/github-token"
mode=""
if [ -f "$github_token" ]; then
	mode=$(stat -c '%a' "$github_token" 2>/dev/null || stat -f '%Lp' "$github_token")
fi
if [ "$mode" = "600" ]; then
	echo "the GitHub token file is in place with mode 600"
else
	echo "no GitHub token file with mode 600 at $github_token: make it as docs/dogfood.md says"
fi

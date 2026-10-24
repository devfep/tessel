#!/usr/bin/env bash
# Sets up the server directory that coordinates devfep/tessel on this machine (docs/dogfood.md).
#
# Usage: scripts/dogfood/setup.sh <server dir>
#
# Makes <server dir>/{keys,stores,lander,tokens}, writes tessel-runner.toml and tessel-server.toml,
# makes the signing key unless it exists, and mints a token for each of lane-a, lane-b and
# orchestrator. Safe to run again: it rewrites the two configurations and the tokens, and keeps
# the key. It never calls gh, never pushes and never starts the server. Needs `tessel-server` and
# `tessel` on PATH, and copies both into <server dir>/bin: git's askpass hook is the server's own
# current_exe, so it must run from a copy that a later cargo build cannot replace mid-landing, and
# the docs run `tessel init` from the `tessel` copy so the git hooks it installs point at it.
# <server dir>/.cargo/config.toml caps landing builds at 6 jobs, as cargo reads it upward from the
# runner's clone; a submission's own .cargo/ is held as sensitive. Tokens are never printed.
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
for binary in tessel-server tessel; do
	command -v "$binary" >/dev/null ||
		fail "$binary is not on PATH: cargo build -p tessel-server -p tessel-cli, add target/debug"
done

umask 077
mkdir -p "$dir/keys" "$dir/stores" "$dir/lander" "$dir/tokens" "$dir/bin" "$dir/.cargo"

for binary in tessel-server tessel; do
	install -m 755 "$(command -v "$binary")" "$dir/bin/$binary"
	echo "copied $binary into $dir/bin"
done

printf '[build]\njobs = 6\n' >"$dir/.cargo/config.toml"

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
sensitive = [
	"src/protocol.rs", "src/host/", ".github/", "docs/BUILD-PROTOCOL.md", "docs/dogfood.md",
	"scripts/", "tessel-server/ci/", "tessel-server/src/landing/", "tessel-server/src/credential.rs",
	"tessel-runner/src/", "tessel-lander/src/", "src/coordinator/merging.rs", "src/shell.rs",
	".cargo/", "rust-toolchain.toml", "Cargo.lock", "CLAUDE.md", "PLAN.md", "skills/", ".claude/",
]
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

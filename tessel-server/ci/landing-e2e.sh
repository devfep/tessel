#!/usr/bin/env bash
# Lands one commit through a real `tessel-server` and two real `tessel` agents, and checks main.
#
# A bare git repository is main. Agent a1 claims a file, changes it, pushes the commit to its fork
# and submits it. The server rebases it onto main, the runner runs the trunk's test command on the
# rebased head, and only then does the server push main with a compare-and-swap. Agent a2 is
# refused the file while a1 holds it and gets it once a1's work has landed.
#
# Needs target/debug/{tessel-server,tessel} (cargo build -p tessel-server -p tessel-cli), git and
# cargo on PATH. With TESSEL_E2E_DIR set it works in that empty directory and leaves everything
# there; without it, it works in a temporary directory and removes it.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
bin="${TESSEL_BIN_DIR:-$root/target/debug}"
if [ -n "${TESSEL_E2E_DIR:-}" ]; then
  work="$TESSEL_E2E_DIR"
  mkdir -p "$work"
  keep=1
else
  work=$(mktemp -d)
  keep=0
fi
server_pid=""

cleanup() {
  local status=$?
  for dir in "$work"/agents/*; do
    [ -d "$dir" ] && (cd "$dir" && "$bin/tessel" stop >/dev/null 2>&1) || true
  done
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ] && [ -f "$work/server.log" ]; then
    echo "--- server log ---" >&2
    tail -n 60 "$work/server.log" >&2
  fi
  if [ "$keep" -eq 0 ]; then
    rm -rf -- "$work"
  fi
  exit "$status"
}
trap cleanup EXIT

fail() {
  echo "landing-e2e: $*" >&2
  exit 1
}

export GIT_AUTHOR_NAME="E2E Author" GIT_AUTHOR_EMAIL="author@example.invalid"
export GIT_COMMITTER_NAME="E2E Committer" GIT_COMMITTER_EMAIL="committer@example.invalid"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

mkdir -p "$work"/{keys,stores,lander,forks,tmp,seed,agents}
main="$work/main.git"

# A main with a tiny crate whose test says it ran, and the gate that runs `cargo test --offline`.
git init --quiet --bare --initial-branch=main "$main"
git init --quiet --initial-branch=main "$work/seed"
mkdir -p "$work/seed/src"
printf 'instance = "standard-1"\n\n[[test]]\nargv = ["cargo", "test", "--offline"]\n' \
  >"$work/seed/tessel.toml"
printf '[package]\nname = "landing-e2e"\nversion = "0.1.0"\nedition = "2021"\n' \
  >"$work/seed/Cargo.toml"
cat >"$work/seed/src/lib.rs" <<'RUST'
pub fn greeting() -> &'static str {
    "hello"
}

#[test]
fn the_runner_ran_this_on_the_rebased_head() {
    std::fs::write(std::env::temp_dir().join("tessel-ran"), greeting()).unwrap();
    assert!(greeting().starts_with("hello"));
}
RUST
printf 'target/\n' >"$work/seed/.gitignore"
git -C "$work/seed" add --all
git -C "$work/seed" commit --quiet -m "seed"
git -C "$work/seed" push --quiet "$main" HEAD:main
base=$(git --git-dir="$main" rev-parse main)
for agent in a1 a2; do
  git init --quiet --bare --initial-branch=main "$work/forks/$agent.git"
done

# The runner may start only for the user its configuration names.
printf '[runner]\nuser = "%s"\n' "$(id -un)" >"$work/tessel-runner.toml"
chmod 600 "$work/tessel-runner.toml"
cat >"$work/tessel-server.toml" <<TOML
listen = "127.0.0.1:0"
store_dir = "$work/stores"

[[repository]]
tenant = "acme"
name = "demo"
run = "ci"
remote = "$main"
fork = "$work/forks/{agent}.git"

[lander]
work_dir = "$work/lander"
runner_config = "$work/tessel-runner.toml"

[identity]
keys = "$work/keys"
audience = "landing-e2e"
TOML

"$bin/tessel-server" admin keygen --config "$work/tessel-server.toml" --kid k1
token() {
  "$bin/tessel-server" admin token --config "$work/tessel-server.toml" --kid k1 \
    --tenant acme --repo demo --agent "$1"
}

TMPDIR="$work/tmp" "$bin/tessel-server" serve --config "$work/tessel-server.toml" \
  >"$work/server.log" 2>&1 &
server_pid=$!
addr=""
for _ in $(seq 1 100); do
  addr=$(sed -n 's/.*listening addr=\([0-9.:]*\).*/\1/p' "$work/server.log" | head -n 1)
  [ -n "$addr" ] && break
  kill -0 "$server_pid" 2>/dev/null || fail "the server exited before it listened"
  sleep 0.2
done
[ -n "$addr" ] || fail "the server never said it was listening"

# One checkout and one daemon per agent, each with its own token.
join() {
  local agent=$1
  git clone --quiet "$main" "$work/agents/$agent"
  (
    cd "$work/agents/$agent"
    TESSEL_TOKEN=$(token "$agent") "$bin/tessel" init \
      --coordinator "ws://$addr/tenant/acme" --repo demo --agent "$agent" >/dev/null
    "$bin/tessel" start "change the greeting as $agent" >/dev/null
  )
}
join a1
join a2

(cd "$work/agents/a1" && "$bin/tessel" claim src/lib.rs) || fail "a1 was refused src/lib.rs"
if (cd "$work/agents/a2" && "$bin/tessel" claim src/lib.rs >"$work/a2-claim.log" 2>&1); then
  fail "a2 was granted src/lib.rs while a1 holds it"
fi

(
  cd "$work/agents/a1"
  sed -i.bak 's/"hello"/"hello from a1"/' src/lib.rs
  rm -f src/lib.rs.bak
  git commit --quiet --all -m "greet from a1"
  git push --quiet "$work/forks/a1.git" HEAD:main
  "$bin/tessel" submit --evidence "the e2e script ran it"
)

# The server answers the submission with the landing, so main moves on its own.
landed=""
for _ in $(seq 1 600); do
  landed=$(git --git-dir="$main" rev-parse main)
  [ "$landed" != "$base" ] && break
  kill -0 "$server_pid" 2>/dev/null || fail "the server exited before it landed the commit"
  sleep 0.5
done
[ "$landed" != "$base" ] || fail "main never moved from $base"

[ "$(git --git-dir="$main" rev-list --count "$base..main")" -eq 1 ] ||
  fail "main took more than the one commit"
[ "$(git --git-dir="$main" rev-parse "main~1")" = "$base" ] ||
  fail "the commit is not on the old main"
git --git-dir="$main" show main:src/lib.rs | grep -q 'hello from a1' || fail "main lacks the change"
[ -f "$work/tmp/tessel-ran" ] || fail "the runner's command did not run (no marker)"
[ "$(cat "$work/tmp/tessel-ran")" = "hello from a1" ] ||
  fail "the runner's command ran on something other than the landed change"

# The server logs the read-back line after it pushed main and read it back, so main moves first.
readback=""
for _ in $(seq 1 60); do
  readback=$(grep "landed: main was read back at the head" "$work/server.log" || true)
  [ -n "$readback" ] && break
  kill -0 "$server_pid" 2>/dev/null || fail "the server exited before it logged the landing"
  sleep 0.5
done
[ -n "$readback" ] || fail "the server did not log a read-back landing"
grep -q "$landed" <<<"$readback" || fail "the server log does not name the head it landed"

# a1's claim is released by the landing, so a2 can have the file.
granted=""
for _ in $(seq 1 60); do
  if (cd "$work/agents/a2" && "$bin/tessel" claim src/lib.rs >/dev/null 2>&1); then
    granted=yes
    break
  fi
  sleep 0.5
done
[ -n "$granted" ] || fail "a2 never got src/lib.rs after the landing"

echo "landing-e2e: landed $landed on main after the runner ran, claim released, a2 granted"

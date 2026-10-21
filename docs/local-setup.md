# Local setup

One developer, one `tessel-server` binary, a SQLite file per repository, your own git remote.
A repository that names a `remote` and a `fork` is landed by the server: it rebases a submitted
commit onto main, runs the trunk's `tessel.toml` test command on the result, pushes main only if
main has not moved, and reads main back before it tells the agent the commit merged. The keys are
in [Landing](configuration.md#landing). A repository without a `remote` has no lander: the inbox
shows `submit_rejected` ("this server has no lander configured; nothing can land until one is")
and the claim is active again. A submission that touches a `sensitive` path waits for a reviewer
first. The runner is not sandboxed until M5: a trunk's test command runs as you and can write
to a path `remote`, so land only a repository whose tests you trust. Build with `cargo build -p
tessel-server -p tessel-cli` and put `target/debug` on your `PATH`. Run the blocks below in order
from one empty directory; `<server dir>` and `<remote>` are paths outside any checkout. CI runs
this page as written (`tessel-server/ci/quickstart.sh`).

## The remote and the server

A bare repository stands in for your remote; its trunk holds the gate and a crate to test. The
server directory holds the private signing key and the runner's configuration (mode 0600, naming
your user). Relative paths in `tessel-server.toml` read from it, and [every
key](configuration.md#server-tessel-server) is documented. End the server with `kill %1`.

```
git init --quiet --bare --initial-branch=main <remote>
git init --quiet --initial-branch=main trunk && cd trunk && mkdir -p src/auth
printf 'instance = "standard-1"\n[[test]]\nargv = ["cargo", "test", "--offline"]\n' >tessel.toml
printf '[package]\nname="demo"\nedition="2021"\n[lib]\npath="src/auth/login.rs"\n' >Cargo.toml
printf 'pub fn login() -> u8 { 42 }\n#[test]\nfn t() { assert!(login() > 0); }\n' >src/auth/login.rs
git add --all && git commit --quiet -m trunk && git push --quiet <remote> HEAD:main && cd ..
mkdir -p <server dir>/forks && cd <server dir>
printf '[runner]\nuser = "%s"\n' "$(id -un)" >tessel-runner.toml && chmod 600 tessel-runner.toml
cat >tessel-server.toml <<'TOML'
listen = "127.0.0.1:7878"
store_dir = "stores"
lander = { work_dir = "lander", runner_config = "tessel-runner.toml" }
identity = { keys = "keys", audience = "local" }
[[repository]]
tenant = "me"
name = "demo"
run = "local"
remote = "<remote>"
fork = "<server dir>/forks/{agent}.git"
reviewers = ["reviewer"]
sensitive = ["src/auth/"]
TOML
tessel-server admin keygen --config tessel-server.toml --kid k1
tessel-server serve --config tessel-server.toml >server.log 2>&1 &
until grep -q listening server.log; do sleep 0.2; done
cd -
```

## An agent

The fork is a bare repository the `fork` template names. In the agent's checkout, `init` stores the
token in `.tessel/config.toml` (mode 0600) and never prints it. `submit` exits 7: the path is in
`sensitive`, so the submission is held and prints the claim id.

```
git init --quiet --bare --initial-branch=main <server dir>/forks/a1.git
git clone --quiet <remote> a1 && cd a1
TESSEL_TOKEN=$(cd <server dir> && tessel-server admin token --config tessel-server.toml \
  --kid k1 --tenant me --repo demo --agent a1) \
  tessel init --coordinator ws://127.0.0.1:7878/tenant/me --repo demo --agent a1
tessel start "change the login" && tessel claim src/auth/login.rs
sed -i.bak 's/42/43/' src/auth/login.rs && rm src/auth/login.rs.bak
git commit --quiet -a -m "change the login" && git push --quiet <server dir>/forks/a1.git HEAD:main
tessel submit --evidence "cargo test passed" || test $? -eq 7
cd ..
```

## The reviewer

A `reviewers` entry may approve, from a checkout of its own. After the approval the runner runs the
trunk's test command on the rebased commit, main is pushed and read back, the agent's inbox says
merged and the log shows `change the login` on the remote's `main`.

```
git clone --quiet <remote> reviewer && cd reviewer
TESSEL_TOKEN=$(cd <server dir> && tessel-server admin token --config tessel-server.toml \
  --kid k1 --tenant me --repo demo --agent reviewer) \
  tessel init --coordinator ws://127.0.0.1:7878/tenant/me --repo demo --agent reviewer
tessel start "review the login change" && tessel review 1 --approve --note "looks right"
(cd ../a1 && until tessel inbox --all | grep 'is merged'; do sleep 1; done)
git -C <remote> log --oneline -1
tessel stop && (cd ../a1 && tessel stop)
```

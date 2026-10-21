# Local setup

One developer, one `tessel-server` binary, a SQLite file per repository, and your own git remote.
Landing is not in this build: a submitted commit is held, and landing arrives with
P1-SERVER-LANDING. Build with `cargo build -p tessel-server -p tessel-cli`; the binaries
`tessel-server` and `tessel` are in `target/debug`, which must be on your `PATH`.

## The server

Write `tessel-server.toml`: a `reviewers` entry may approve a held submission, and a submission
touching a `sensitive` prefix is held. [Every key](configuration.md#server-tessel-server).

```toml
listen = "127.0.0.1:7878"
store_dir = "stores"

[[repository]]
tenant = "me"
name = "demo"
run = "local"
reviewers = ["reviewer"]
sensitive = ["src/auth/"]

[identity]
keys = "keys"
audience = "local"
```

```
tessel-server admin keygen --config tessel-server.toml --kid k1
tessel-server serve --config tessel-server.toml          # runs until you end it
```

## An agent

Mint a token for one agent and set its worktree up. `init` stores the token in
`.tessel/config.toml` (mode 0600) and never prints it, so only `init` needs the variable.

```
TESSEL_TOKEN=$(tessel-server admin token --config tessel-server.toml --kid k1 \
  --tenant me --repo demo --agent a1) \
  tessel init --coordinator ws://127.0.0.1:7878/tenant/me --repo demo --agent a1
tessel start "change the login"
tessel claim src/auth/login.rs
```

Edit and commit, then `tessel submit --evidence "cargo test passed"`. It exits 7: the path is
sensitive, so the submission is held and prints the claim id.

## The reviewer

The reviewer works in a checkout of its own. Mint its token with `--agent reviewer`, run the same
`init` with that agent, then use the claim id `submit` printed:

```
tessel start "review the login change"
tessel review 1 --approve --note "looks right"
```

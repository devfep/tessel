# Local setup

One developer, one `tessel-server` binary, a SQLite file per repository, your own git remote.
Landing is not in this build. A held submission waits for a reviewer; after approval this build
refuses the merge, the inbox shows `submit_rejected` ("the steward refused the request (fork
missing or not a fork of this repo)") and the claim is active again, until P1-SERVER-LANDING.
Build with `cargo build -p tessel-server -p tessel-cli`; put `target/debug` on your `PATH`.

## The server

In a server directory outside any checkout (it holds the private signing key), write
`tessel-server.toml` and run the server commands there: relative paths read from the current
directory. [Every key](configuration.md#server-tessel-server).

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

In the agent's checkout, mint a token and run `init`, which stores it in `.tessel/config.toml`
(mode 0600) and never prints it, so only `init` needs the variable.

```
TESSEL_TOKEN=$(cd <server dir> && tessel-server admin token --config tessel-server.toml \
  --kid k1 --tenant me --repo demo --agent a1) \
  tessel init --coordinator ws://127.0.0.1:7878/tenant/me --repo demo --agent a1
tessel start "change the login"
tessel claim src/auth/login.rs
```

Edit and commit, then `tessel submit --evidence "cargo test passed"`. It exits 7: the path is
in `sensitive`, so the submission is held and prints the claim id.

## The reviewer

A `reviewers` entry may approve. In a checkout of its own it mints a token with `--agent reviewer`
as above, runs `init` with that agent, then uses the claim id `submit` printed:

```
tessel start "review the login change"
tessel review 1 --approve --note "looks right"
```

# Configuration reference

This page covers the settings in this tree. The Cloudflare Worker and its `wrangler.toml` are
deleted; the local server, `tessel-server`, has its [own section](#server-tessel-server). The
steward (`tessel-steward/`) stays until M6 as the reference for the lander, so its settings are
listed as they are in its own files.

Every setting Tessel reads, where it is set, its default, what it does, and the code that reads it.
The code references were written against commit `9e363c1` and the `src/shell.rs` ones re-checked
after the Worker's deletion. A setting that is not on this page is not configurable: the limits in
[Fixed limits](#fixed-limits) are constants in the code.

Contents: [`tessel.toml`](#per-repository-tesseltoml-the-gate) ·
[Host settings](#host-settings) · [Server](#server-tessel-server) ·
[Steward](#steward-tessel-stewardwranglerjsonc) · [Agent and worktree](#agent-and-worktree) ·
[CLI commands and flags](#cli-commands-and-flags) · [Fixed limits](#fixed-limits) ·
[What is pluggable](#what-is-pluggable)

## Per repository: `tessel.toml` (the gate)

The gate is the install and test commands run before a submission merges. The rules below are
the steward's, which the lander ports. A
repository declares it in `tessel.toml` at the root. The steward reads the file from the trunk
commit a run is based on, never from the agent's fork (`tessel-steward/src/gate-plan.ts:39`).

The file is strict. An unknown key, a file over 4096 bytes, invalid TOML, or a command outside the
allowed list makes the whole file invalid (`tessel-steward/src/tessel-config.ts:179`).

### Keys

| Key | Type | Default | Effect |
|---|---|---|---|
| `instance` | string, required | none | Container size for the run. One of `standard-1`, `standard-2`, `standard-3`, `standard-4`. |
| `[install]` | table, optional | no install step | Only the keys `cargo` and `pnpm`. |
| `install.cargo` | boolean | `false` | When `true`, the steward runs `cargo fetch --locked --offline` in the repo root before the tests. |
| `install.pnpm` | array of directories, at most 4 | `[]` | In each directory the steward runs `pnpm install --offline --frozen-lockfile --ignore-scripts`. |
| `[[test]]` | 1 to 6 tables, required | none | The test commands, run in order. The first non-zero exit ends the run. |
| `test.argv` | array of 1 to 24 strings, required | none | The command, run without a shell. |
| `test.dir` | directory | `"."` | Working directory, relative to the repo root. |

Code: keys and limits `tessel-steward/src/tessel-config.ts:5-46`, install parsing `:114`, test
parsing `:141`. The install argv arrays are fixed by the steward, so the file chooses where to
install, not how (`tessel-steward/src/gate-steps.ts:12-19,25`).

### Instances

| `instance` | Memory | Source |
|---|---|---|
| `standard-1` | 4 GiB | `tessel-steward/src/tessel-config.ts:14` |
| `standard-2` | 6 GiB | same |
| `standard-3` | 8 GiB | same |
| `standard-4` | 12 GiB | same |

`lite` is not accepted. It is the instance a repository without `tessel.toml` gets
(`tessel-steward/src/tessel-config.ts:7`, `gate-plan.ts:102`).

### Allowed commands

A `test.argv` must be one of (`tessel-steward/src/tessel-config.ts:76`):

- `["pnpm", "test"]` exactly;
- `["npm", "test"]` exactly;
- `["cargo", "test", ...flags]` where every flag is in this list (`:27`): `--workspace`,
  `--locked`, `--offline`, `--no-fail-fast`, `--all-targets`, `--all-features`, `--release`,
  `--lib`, `--bins`, `--tests`.

Not allowed: a `+toolchain` argument, `--config`, `-Z`, `--manifest-path`, `--target`, a `--`
passthrough, environment assignments, and a shell. Each argument must match
`^[A-Za-z0-9._/=:@+,%-]+$` and be at most 200 characters (`:43-45`). A directory must be `.` or
`/`-separated segments that start with a letter, digit or `_` and continue with `A-Z a-z 0-9 . _ -`,
so `..` is refused (`:46`).

### Without the file

A repository with no `tessel.toml` on its trunk gets the legacy gate (`gate-plan.ts:17`,
`container-step.ts:246-272`):

| Part | Value |
|---|---|
| Image | `tests` (`tessel-steward/Dockerfile`: Node 22.23.3, `git`, `ca-certificates`) |
| Instance | `lite` |
| Network | off |
| Dependency check | The run is refused if `package.json` is missing, is not a JSON object, or declares any of `dependencies`, `devDependencies`, `optionalDependencies`, `peerDependencies`, `bundleDependencies`, `bundledDependencies`, `workspaces` (`dependency-check.ts:16-44`). |
| Test | `npm test` in the repo root |

A repository with an invalid `tessel.toml` does not fall back to this gate. Every run fails at the
install step with reason `config` (`gate-plan.ts:19`).

### Images

With a `tessel.toml` the steward uses the `toolchain` image (`gate-plan.ts:13`). The Internet is off
in both images (`gate-plan.ts:68-103`), so every dependency must already be in the image. It holds
the Rust toolchain and the crates of `Cargo.lock`, and the pnpm store of
`tessel-steward/pnpm-lock.yaml` (`tessel-steward/toolchain.Dockerfile`). A repository other than
Tessel needs its own toolchain image built from its lockfiles, and the steward's
`containers[].images.toolchain` entry pointed at it.

### Time budget

All install commands share 60 s. All `[[test]]` commands share 327 s. A command that runs out of
its share ends as a timeout (exit 124 or 137), not as a failing test
(`tessel-steward/src/step-budget.ts:33-65,77`, asserted in `step-budget.test.ts:36`).

### Who may change the gate

| Path | Result | Code |
|---|---|---|
| A submission (service binding) whose rebased diff touches `tessel.toml` | Rejected as `gate_changed`: "changes the gate (tessel.toml); only an admin merge may change it" | `merge-steps.ts:151-163`, `src/merge.rs:225` |
| Admin merge (`POST /repos/<repo>/merges` with `STEWARD_ADMIN_TOKEN`) whose head `tessel.toml` fails validation | Rejected as `gate_invalid`: "changes the gate (tessel.toml) to a file the steward cannot accept" | `merge-steps.ts:164-166`, `src/merge.rs:229` |
| Admin merge with a valid head file and a diff that touches only `tessel.toml` | Merged without running the install or tests, so a broken trunk gate can be repaired | `merge-steps.ts:168` |
| Admin merge with a valid head file and a diff that also changes other files | Tested by the trunk's gate as usual | `merge-steps.ts:168` |

A claim's scopes must still cover `tessel.toml` in every case.

### Example

This is Tessel's own `tessel.toml`, without its comments:

```toml
instance = "standard-4"

[install]
cargo = true
pnpm = ["tessel-steward"]

[[test]]
argv = ["cargo", "test", "--workspace", "--locked", "--offline", "--no-fail-fast"]

```

## Host settings

The host reads four settings when it builds the core (`HostConfig`, `src/host/mod.rs`). They keep
the names of the Worker's old variables. `tessel-server` supplies `run`, `reviewers` and
`sensitive` from each [`[[repository]]`](#server-tessel-server); it has no key for
`shadow_enabled`, so it never enables shadow claims.

| Name | `HostConfig` field | If unset | Read | Effect |
|---|---|---|---|---|
| `RUN` | `run` | A repo with no stored state fails to load: "the RUN variable is missing or empty" | Only when a repo has no stored state | Run tag stamped on every event. A repo keeps the run it was created with (`src/shell.rs:235-238`). |
| `SHADOW_ENABLED` | `shadow_enabled` | `false` | Only when a repo has no stored state | Allows `on_conflict: shadow` claims. When `false`, such a claim is refused with `shadow_disabled` (`src/coordinator.rs:772`). Only exactly `"true"` or `"false"` is accepted (`src/shell.rs:227`). No `tessel` command sends shadow claims. |
| `REVIEWERS` | `reviewers` | Nobody may review | Every load | Comma-separated agent ids allowed to decide a held submission. An id needs the agent-id form (`A-Z a-z 0-9 . _ -`, first a letter or digit, at most 128 bytes) (`src/shell.rs:148`). |
| `SENSITIVE` | `sensitive` | No sensitive paths | Every load | Comma-separated path prefixes. A submission that touches a path starting with one is held for review (`src/protocol.rs:694-728`). |

`RUN` and `SHADOW_ENABLED` are fixed per repo when its state is first created. Changing them later
does not change an existing repo. `REVIEWERS` and `SENSITIVE` are applied each time the host loads
a repo (`src/shell.rs:235-257`, `src/host/core_slot.rs`), so a change takes effect for existing
repos at their next load.

### Failure behaviour

| Bad value | Result | Code |
|---|---|---|
| `REVIEWERS` is malformed (an empty entry, or an id that is not an agent id) | Nobody can review. Work that needs review stays held. The error is logged without the value. | `src/shell.rs:166` |
| `SENSITIVE` is malformed (an empty entry, a leading `/`, a `..` segment, a `*`) | Every submission is held for review: the empty prefix matches every path. Logged without the value. | `src/shell.rs:177-205` |
| `SHADOW_ENABLED` is neither `"true"` nor `"false"` | A new repo fails to load. | `src/shell.rs:227` |
| `RUN` missing or empty | A new repo fails to load. | `src/shell.rs:105` |

`SENSITIVE` entries are plain prefixes with no globs. End a directory with `/`: `src/auth` also
matches `src/authz.rs`.

### What holds a submission for review

A submission merges unattended unless one of these applies (`src/protocol.rs:694-728`):

| Reason | Condition |
|---|---|
| `signature_change` | A touched scope is in `edit_signature` mode. |
| `sensitive_path` | A touched path starts with a `SENSITIVE` entry (`sensitive` in the server's `[[repository]]`). |
| `threatens_assumptions` | The work threatens an assumption another agent declared. |
| `no_test_evidence` | The submission carries no `--evidence`. |

`--evidence` is self-reported. The coordinator accepts any non-empty evidence list
(`src/coordinator.rs:1271`); the CLI drops blank entries (`tessel-cli/src/submit.rs:63-75`).
Nothing checks that the tests ran.

## Server: `tessel-server`

`tessel-server serve --config <file>` reads one TOML file. An unknown key, in the file or in a
table, makes it invalid, and an error names the file and the key.

| Key | Type | Default | Effect |
|---|---|---|---|
| `listen` | socket address | required | Where the server listens, `127.0.0.1:7878` for example. |
| `store_dir` | path | required | The directory of stores, one SQLite file per repository, and the lock that keeps a second server out. |
| `[[repository]]` `tenant`, `name`, `run` | names | at least one | A repository this server serves, and the run its evidence log is named for. `tenant` and `name` are lowercase letters, digits and interior hyphens, at most 63 characters. |
| `[[repository]]` `reviewers` | array of agent names | `[]` | The agents that may approve or reject a held submission, which is what `tessel review` does. Empty: nobody may, and a held submission stays held. Each entry has the agent-name form (`A-Z a-z 0-9 . _ -`, first a letter or digit); a bad entry makes the file invalid and the error names the file, the repository and the key. Read as `REVIEWERS` above. |
| `[[repository]]` `sensitive` | array of path prefixes | `[]` | A submission that touches a path starting with an entry is held for review. No globs; end a directory with `/`. A bad entry (empty, a leading `/`, a `..` segment, a `*`, a comma, a space at either end) makes the file invalid, naming the file, the repository and the key. Read as `SENSITIVE` above. |
| `[[repository]]` `remote` | path or URL | none | The git remote main lives on. A repository with a `remote` is landed by the server ([Landing](#landing)); one without has no lander and every merge is refused. Only a path, or a URL that needs no credential, works until M4 mints the git host's token. |
| `[[repository]]` `fork` | path or URL with `{agent}` | required with `remote` | Where an agent's fork is, `{agent}` standing for the agent's name, as in the swarm's `<forks>/<agent>.git`: `/srv/forks/{agent}.git`. `{agent}` must appear exactly once, and `fork` without `remote` is invalid; the error names the file and the key. The name is substituted only if it passes the protocol's agent-name rule (the one the verifier enforces: letters, digits, `.`, `_` and `-`, starting with a letter or digit, at most 128 bytes), checked again at every call; any other name is refused as a missing fork and no path is made. On a case-insensitive filesystem names that differ only by case share one fork, so mint tokens for lowercase names. The agent pushes the commit to the fork's `main` before `tessel submit`. |
| `[lander]` `work_dir` | path | required with a `remote` | Where clones live while a call runs, under `<work_dir>/<tenant>/<repo>/`. The directory is the server's: it removes what a killed server left there when it starts. |
| `[lander]` `runner_config` | path | required with a `remote` | The `tessel-runner.toml` the tests run under ([Runner](#runner-tessel-runner)). It is checked when the server starts, so a file the runner would refuse stops the server, and again before every run. |
| `[identity]` `keys` | path | none | The directory of Ed25519 keys. |
| `[identity]` `audience` | name | none | The `aud` a token must carry. |

Without an `[identity]` table the server logs one warning at start and answers 401 on every
authenticated route. A `[lander]` table without a repository that names a `remote` makes the file
invalid, and so does a `remote` without the table; the error names the file, the table and the key.

### Landing

A merge is one `Prepare` call until the staged landing (HOST-MERGE-STANDIN), and for a repository
with a `remote` the server answers it in process with the lander and the runner. In order:

1. Read main's head from the remote and pin it.
2. Clone main, fetch the `main` of the agent's fork and check the commit is on it.
3. Rebase the commit onto main with a fixed committer, so a retry gives the same commit.
4. Check the claim covers every changed file and that `tessel.toml` is untouched.
5. Clone the rebased head into a test tree and run the trunk's `[[test]]` commands there with the
   runner. The install step is not run: the machine's own toolchain and dependencies are used, and
   `[install]` in `tessel.toml` is ignored. A trunk with no valid `tessel.toml` cannot be tested, so
   nothing lands; the server logs why.
6. Push main with a compare-and-swap on the head read in step 1, then read main back. Only a head
   found on main is answered as merged.

| Outcome | The host is told | The agent sees |
|---|---|---|
| Main read back at the head | `Prepared`, so the claim merges | `merged` with the head |
| The commit conflicts with main | refused | `submit_rejected`, the number of files that conflicted |
| The change is not covered by the claim, or touches `tessel.toml` | refused | `submit_rejected` with the fixed reason |
| The tests failed | refused | `submit_rejected` with the exit code and no test output |
| The commit is not on the agent's fork | refused | `submit_rejected` with the fixed reason |
| Main moved before the push | `Unknown`, which the core retries | nothing yet |
| Anything that did not finish: a clone, a push, a runner that refuses to start, a timeout | `Unknown`, which the core retries | nothing; after three retries, `submit_rejected` as infrastructure |

The first failing test, the conflicting file names (with their total) and every infrastructure
error go to the server log, cut to a length and quoted, and never to an agent. A submission whose
main already holds the same change is answered as merged at main's head.

Each call is bounded by its own deadline, 13 minutes from when the host issued it. The tests get
what is left less 5 seconds, and never more than `timeout_secs` of the runner's configuration. A
call still running at its deadline is answered `Unknown`, its clones are removed, and what it left
running cannot push main. A server killed mid-merge is woken for each repository with stored state
when it restarts, finds the merge in flight and retries it after the first backoff, 10 seconds.
`Test` and `Land` are refused: the host does not issue them while `Prepare` is the whole merge.

The runner has no sandbox until M5. A trunk's test command runs as the runner's user and can write
to a path `remote`, which a landing does not prevent: use this only for a repository whose tests
you trust.

No evidence record is written until P1-EVIDENCE. After a landing the server tells the host main
moved, as the `trunk-moved` route does for a push it did not make; the route stays for those.

### Identity

A token is `<payload>.<signature>`, sent as `Authorization: Bearer <token>`. The payload is
base64url, without padding, of the JSON `{"v":1,"kid":..,"aud":..,"tenant":..,"repo":..,
"agent":..,"role":"agent"|"service","exp_ms":..}`. The signature is Ed25519 over the payload text
as sent, made with the secret key of `kid`. The server checks, in this order, the size (at most
1 KiB), the shape, the version, that `kid` is known, the signature, `aud`, `tenant`, `repo`, the
agent name, the role and `exp_ms`. Every refusal is a 401 with the same body; the reason is
logged and never carries the token.

The key directory holds `<kid>.key`, the secret seed, written with mode 0600, and `<kid>.pub`. A
`kid` is a lowercase name such as `2026-10`. The server loads every `.pub` file at start and
never reads a `.key` file. To rotate, run `admin keygen` for a new `kid`, switch minting to it,
restart, and remove the old `.pub` once its tokens have expired: until then both verify. A `kid`
whose file is removed is refused after the next start. There is no deny-list: a token is valid
until it expires or its `kid` is removed.

`lander`, `runner` and `server` are reserved service names. A token for one has `role` `service`
and is minted only with `--service`; a token for any other name has `role` `agent`; one that
mixes the two is refused, when it is minted and when it is checked. `ws` and `summary` admit
agents only. `trunk-moved` admits the `lander` only.

| Command | Effect |
|---|---|
| `tessel-server admin keygen --config <file> --kid <kid>` | Writes `<kid>.key` and `<kid>.pub` into `[identity] keys`, creating the directory. Refuses to overwrite either file. |
| `tessel-server admin token --config <file> --kid <kid> --tenant <t> --repo <r> --agent <a> [--ttl <duration>] [--service]` | Prints a token on standard output and nothing else, and never logs it. `--ttl` is `30m`, `12h` or `7d` and defaults to `24h`. |

## Runner: `tessel-runner`

`tessel-runner [--config <file>]` runs a repository's test commands for one trusted user on their
own machine. It reads one request as JSON on standard input and writes one result as JSON on
standard output. `tessel-server` calls the same library in process for a landing
([Landing](#landing)). It runs no git and
opens no network connection. The configuration file is `tessel-runner.toml` in the working
directory unless `--config` names another. An unknown key makes it invalid.

| Key | Type | Default | Effect |
|---|---|---|---|
| `[runner]` `user` | login name | required | The one user the runner starts for. |
| `[runner]` `timeout_secs` | seconds | 1800 | The longest any run may take, from 1 to 86400; a request may ask for less. |

The runner refuses to start unless the file exists, is a regular file (not a link) owned by the
effective user with mode 0600, and `user` equals the login of the effective uid. The login comes
from the password database (`getpwuid_r`), never from `$USER`. A refusal exits 2 with one fixed
line on standard error that names the file and, where it applies, the key.

The request is `{ "tree": "<absolute path>", "tests": [{ "dir": "<relative or empty>", "argv":
[...] }], "timeout_secs": n }`. `tests` is the `[[test]]` list of the trunk's `tessel.toml`, parsed
by the sender. The runner checks every entry again with the lander's parser, so only the commands
the gate allows run. `tree` must be a directory the effective user owns, and every `dir` must stay
inside it: `..`, an absolute `dir` and a link out of the tree are refused. A refused request runs
nothing and exits 2.

The commands run in order, each in `tree/dir`, and the run stops at the first one that does not
exit 0. The whole run, reading the output included, is under one timeout; on timeout the process
group is killed and no child is left behind. A command gets standard input closed and exactly
this environment, taken from the runner's own: `PATH`, `HOME`, `CARGO_HOME`, `RUSTUP_HOME` and
`TMPDIR`. Nothing else reaches it, `TESSEL_TOKEN` and `STEWARD_ADMIN_TOKEN` included.

The result is `{ "step": "test", "exit_code", "passed", "timed_out", "stdout", "stderr",
"stdout_truncated", "stderr_truncated", "first_failure", "wall_ms" }`. `passed` is true only for
exit 0 without a timeout. A run killed at its timeout has `timed_out` true and exit code 137.
An exit code of 137 with `timed_out` false came from the command itself and is not a timeout.
`stdout` and `stderr` hold the last 256 KiB of each stream, with the matching `_truncated` flag
set when the start was cut. `first_failure` is the first failing test's block from cargo's
output, at most 1,500 bytes, or `null`. It is untrusted text for a person to read, never an input
to pass or fail.

| Exit code | Meaning |
|---|---|
| 0 | A run completed. Pass or fail is in the result. |
| 2 | Refused before anything ran: the configuration, the user or the request. |
| 3 | The run could not be supervised, for example the command could not start. |

There is no sandbox. The commands run with the privileges of the user who started the runner, and
a command that leaves its process group outlives the kill. The sandbox is M5. Until then the
runner is for one trusted user running their own repository.

## Steward: `tessel-steward/wrangler.jsonc`

| Name | Where | Value in repo | If unset | Effect |
|---|---|---|---|---|
| `ACCESS_TEAM_DOMAIN` | `vars` | `fepdev.cloudflareaccess.com` | `/dashboard` and `/review` answer 503 "sign-in not configured" | Cloudflare Access team domain that signs the viewer's JWT. `https://` is stripped (`src/access.ts:54`). |
| `ACCESS_AUD` | `vars` | the Access application's AUD tag | same | The audience a JWT must carry (`src/access.ts:55`). |
| `DASHBOARD_VIEWERS` | `vars` | `*` | No account passes: every signed-in user gets 403 | Comma-separated emails allowed to open `/dashboard` and `/review`, compared case-insensitively. A `*` entry admits any email the Access policy lets sign in; `REVIEWER_EMAILS` still decides who may approve or reject (`src/access.ts:59-64,178`). |
| `REVIEWER_EMAILS` | `vars` | one `email=agent` pair | Nobody may decide; viewing is unaffected | Comma-separated `email=agent` pairs. A person signed in as `email` approves or rejects on `/review` as `agent`. An entry that is not a pair with a valid agent id is dropped (`src/review-csrf.ts:10-23`, `src/review.ts:346`). |
| `STEWARD_ADMIN_TOKEN` | secret | required | Every admin route answers 401 | Bearer token for the admin routes: create repo, fork, mint tokens, run tests, admin merge, mint identity (`src/index.ts:47-58,198-200`). |
| `IDENTITY_SIGNING_KEY` | secret | required | Identity minting throws | Signs agent identity tokens, the review page's anti-forgery tokens and the steward's own calls to the coordinator (`src/identity.ts:45`, `src/review.ts:94`, `src/coordinator-link.ts:16`). |

The agent named in `REVIEWER_EMAILS` must also be in the host's `REVIEWERS`. The steward does
not check this. A decision for an agent that is not in `REVIEWERS` is refused by the coordinator
with `not_owner` (`src/coordinator/merging.rs:201`).

Only the `/dashboard` and `/review` paths use Access. Every other route needs
`Authorization: Bearer $STEWARD_ADMIN_TOKEN` (`src/index.ts:190-200`).

### Container images and bindings

| Setting | Value | Note |
|---|---|---|
| `containers[].images.tests` | `./Dockerfile` | Legacy gate. Node and `git`. |
| `containers[].images.toolchain` | `./toolchain.Dockerfile`, `build_context: ".."` | Rust 1.98.0, Node 22.23.3, pnpm 12.8.1, `gcc`, `git`, `procps`, `time`, `tini`. Dependencies come from the committed lockfiles, so a lockfile change needs a rebuilt image before it can merge. |
| `services[COORDINATOR]` | `tessel-coordinator` | Names the Worker that served the coordinator. The Worker is deleted and nothing in the tree serves it, so the steward has no coordinator to call. |
| `artifacts[ARTIFACTS]` | namespace `tessel` | The git host for trunks and forks. |
| `queues.consumers` | `tessel-artifacts-events` | Push events; a push to a trunk's `main` makes the steward poke the coordinator (`src/trunk-poke.ts`). |

## Agent and worktree

Four values identify an agent. They are read by the CLI and the daemon from `.tessel/config.toml`
in the worktree and from the environment (`tessel-cli/src/config.rs:77-123`).

| Key (`.tessel/config.toml`) | Environment | Rule |
|---|---|---|
| `coordinator` | `TESSEL_COORDINATOR` | A `ws://` or `wss://` base URL. A trailing `/` is removed. |
| `repo` | `TESSEL_REPO` | Letters, digits, `.`, `_`, `-`; first a letter or digit. At most 128 characters on the servers. |
| `agent` | `TESSEL_AGENT` | Same rule as `repo`. |
| `token` | `TESSEL_TOKEN` | The identity token for this repo and this agent. Secret. |

The file allows only these four keys (`config.rs:67`). An empty value counts as unset. The file is
mode `0600` (`init.rs:306`) and `tessel init` adds `.tessel/` to `.git/info/exclude`
(`init.rs:356`). Never print, log or commit it.

### Other environment variables

| Variable | Default | Effect | Code |
|---|---|---|---|
| `XDG_RUNTIME_DIR` | the system temp directory | Base directory of the daemon socket, `<base>/tessel-<uid>/<hash of worktree>.sock` (mode 0700). The socket path may be at most 100 bytes. | `tessel-cli/src/worktree.rs:152-156,198` |
| `CLAUDE_PROJECT_DIR` | unset | The worktree a hook guards when `--root` is not given. `tessel hook install` writes `--root`, so this is a fallback. | `tessel-cli/src/commands.rs:659-664` |

### Precedence

| Command | Order, highest first |
|---|---|
| `tessel init` | Flag (`--coordinator`, `--repo`, `--agent`), then `.tessel/config.toml`, then `TESSEL_*` (`init.rs:179-186`). |
| Every other command | `.tessel/config.toml`, then `TESSEL_*`, key by key (`config.rs:79-87`). They have no flags for these values. |

### `tessel init`

`tessel init --coordinator <ws://… or wss://…> --repo <name> --agent <name>` writes the config, the
ignore rule, the Claude Code hooks and the git hooks. It is safe to rerun
(`tessel-cli/src/init.rs:36-49,102-125`).

For a `tessel-server` on this machine, pass `--coordinator ws://127.0.0.1:7878/tenant/<tenant>`.
The CLI appends `/repo/<repo>/ws` to it (`tessel-server/src/ws.rs:1-15`).

Where the token comes from, first match wins (`init.rs:210-240`):

1. The `token` already in `.tessel/config.toml`, when its repo and agent are the ones being set up.
2. `TESSEL_TOKEN`.

With neither, `init` writes nothing and fails with the command that mints one:
`tessel-server admin token --config <file> --kid <kid> --tenant <t> --repo <r> --agent <a>`
(see [Identity](#identity)).

The operator mints the identity token and gives the agent only `TESSEL_TOKEN`. The key files stay
out of the agent's environment: with one an agent could sign a reviewer's token.

## CLI commands and flags

Flags are from `tessel-cli/src/main.rs`.

### `tessel start <summary> [--task <ref>]`

Starts the worktree's daemon and records the intent, one line other agents see when they are
denied (`main.rs:47-52`).

### `tessel claim <scope>... [flags]`

| Flag | Default | Effect |
|---|---|---|
| `<scope>...` | required | One or more scopes. Each is its own argument. |
| `--mode` | `edit-body` | `depend`, `edit-body`, `edit-signature` or `create` (`main.rs:58`). On the wire these are `depend`, `edit_body`, `edit_signature`, `create`. |
| `--wait` | off | Queue behind the holder instead of failing. Allowed only when the agent holds no other claim; otherwise the coordinator refuses with `wait_while_holding` (`src/coordinator.rs:799-806`). An agent with a queued request may not claim at all (`:768-771`). Exit 4 when queued. |
| `--assume "<text>"` | none | Behaviour relied on but not owned in the first scope. Repeatable. Attaches to a new claim. |
| `--new` | off | Make a separate claim instead of adding the scopes to the one open claim. |
| `--json` | off | Print one JSON envelope with the outcome and the moves open to the agent. |

`--assume` and `--wait` also start a separate claim, because an amend carries neither.

**Scope syntax** (`tessel-cli/src/scope.rs:35-60`):

| Form | Meaning |
|---|---|
| `dir/` | A directory and everything under it. |
| `path/file.rs` | A file. |
| `path/file.rs::qualified::name` | A symbol in a file. The name is the part after the first `::`. |

Paths are repo-relative with `/` separators: no leading `/`, no `.` or `..` segment, no empty
segment, no backslash (`scope.rs:11`). The file or directory must exist in the worktree or at
`HEAD`, except with `--mode create` (`scope.rs:67`).

**Modes and conflicts** (`src/protocol.rs:178-232`). Two claims on overlapping scopes conflict
where the table says so:

| | `depend` | `edit_body` | `edit_signature` | `create` |
|---|---|---|---|---|
| `depend` | no | no | yes | no |
| `edit_body` | no | yes | yes | yes |
| `edit_signature` | yes | yes | yes | yes |
| `create` | no | yes | yes | yes |

What a held mode covers when submitting (`protocol.rs:196-210`): every mode covers `depend`;
`edit_signature` covers `edit_body`; each mode covers itself; `create` is covered only by `create`.

### `tessel submit`

| Flag | Default | Effect |
|---|---|---|
| `--evidence "<text>"` | none, at least one | What shows the work is right. Repeatable. The CLI drops blank entries (`tessel-cli/src/submit.rs:63-75`). Self-reported: the coordinator accepts any non-empty evidence list, and nothing checks that the tests ran. |
| `--rejected "<approach>::<reason>"` | none | An approach tried and dropped. Repeatable. |
| `--claim <id>` | the only unsubmitted claim | Required when several are held. |
| `--commit <sha>` | `HEAD` | The commit to merge. It must already be pushed to the agent's fork's `main`. |
| `--transcript <file>` | none | A session transcript. Redacted, at most 4 MiB, stored on the local branch `tessel/transcript-<claim>` (`tessel-cli/src/transcript.rs`). Nothing copies it yet (P1-EVIDENCE). |

Push first: `git push <your fork> HEAD:main`. The server's lander fetches only the fork's `main`
and needs the commit reachable from it ([Landing](#landing)). The fork is the one the server's
`fork` template names for the agent, locally `<forks>/<agent>.git`; the CLI does not choose it. The
runner runs the trunk's test command on the rebased commit before main moves. No evidence record
and no transcript copy exist yet (P1-EVIDENCE).

### `tessel review <claim> --approve | --reject [--note "<reason>"]`

Exactly one of `--approve` and `--reject`. `--note` is shown to the submitter and may be at most
1024 bytes (`main.rs:102-114`, `tessel-cli/src/review.rs:24`). The coordinator refuses the decision
unless the agent is in `REVIEWERS`, the claim is awaiting review, and the claim is not the
reviewer's own (`src/coordinator/merging.rs:201-222`).

### Other commands

`tessel status [--json]`, `tessel inbox [--all]`, `tessel release [<claim>]`, `tessel stop`,
`tessel mcp --root <worktree>`, `tessel hook install [--git]` (`main.rs:73-177`).

`tessel race open <intent> <scope>... --entrants <1-8> --deadline <minutes, at most 120> --by
<criteria>`, `tessel race join <race>`, `tessel race pick <race> <claim>` (`tessel-cli/src/race.rs`;
the coordinator enforces the limits, `src/coordinator/racing.rs`).

## Fixed limits

None of these is configurable.

| Limit | Value | Code |
|---|---|---|
| Claim lease | 30 s, renewed by each heartbeat | `src/shell.rs:16`, `src/coordinator.rs:857` |
| Heartbeat interval | lease / 3 (10 s) | `tessel-cli/src/daemon.rs:806` |
| Agent id and repo name | 1 to 128 bytes | `src/shell.rs:20`, `tessel-steward/src/identity.ts:8` |
| Identity token lifetime | 24 h | `tessel-steward/src/identity.ts:2` |
| Fork write token lifetime | 1 h | `tessel-steward/src/index.ts:16` |
| Read token (`/read-tokens`) | 10 min | `tessel-steward/src/index.ts:18` |
| Review page anti-forgery token | 1 h | `tessel-steward/src/review-csrf.ts:4` |
| WebSocket text frame | 64 KiB | `src/shell.rs:23` |
| Scopes in one Claim, Amend or Submit | 256 | `src/coordinator.rs:1631` |
| Stored state per repo | A client call that would grow the state past 1 MiB, or log one event over 1 MiB, is refused; nothing is stored above 2 MiB minus 64 KiB | `src/shell.rs:27,31`, `STATE_LIMIT_MESSAGE` |
| Review note | 1024 bytes | `src/coordinator.rs:1628` |
| Symbols of one file before the hook and `submit` claim the file instead | 4 | `tessel-cli/src/plan.rs:14` |
| Steward call (one merge) | 13 min | `src/merge.rs:31`, `tessel-steward/src/step-budget.ts:6` |
| Push body the steward forwards to main | 16 MiB | `tessel-steward/src/receive-pack-policy.ts:4` |
| `tessel.toml` size, tests, pnpm directories, argv length, argument length | 4096 bytes, 6, 4, 24, 200 | `tessel-steward/src/tessel-config.ts:5,40-43` |
| Merges in flight per repo | 1, in submission order | `src/coordinator/merging.rs:3` |
| Protocol version | 1 | `src/protocol.rs:72` |
| The conflict table | as above, not extensible | `src/protocol.rs:215-232` |

## What is pluggable

| Part | What works | How |
|---|---|---|
| Agents | Any agent that can run a shell command | The `tessel` CLI. |
| | Agents that speak MCP | `tessel mcp --root <worktree>` serves `tessel_start`, `tessel_claim`, `tessel_status`, `tessel_inbox`, `tessel_submit`, `tessel_release`, `tessel_review` over stdio (`tessel-cli/src/mcp.rs`). |
| | Claude Code | `tessel hook install` writes `PreToolUse` (claims and blocks `Edit`, `MultiEdit`, `Write`, `NotebookEdit`), `PostToolUse`, `UserPromptSubmit`, `SessionStart` (inbox notices) and `Stop` hooks to `.claude/settings.local.json` (`tessel-cli/src/hook.rs:23-65`). |
| | Any agent, through git | `tessel hook install --git` installs `pre-commit` and `pre-push` hooks that refuse changes outside unsubmitted claims (`tessel-cli/src/githook.rs`). |
| Clients | Anything that speaks JSON over WebSocket | A socket for one agent of one repo, first message `{"type":"hello","agent":"…","base":"…"}`. The `agent` must be the one the adapter verified. The summary route returns the evidence summary (`src/shell.rs:488-492`, `src/protocol.rs:459-466`). |
| Languages for symbol claims | Rust (`.rs`), TypeScript (`.ts`, `.mts`, `.cts`), TSX (`.tsx`) | tree-sitter in the CLI (`tessel-cli/src/symbols.rs:55-63`). Every other file is claimed at file level. |
| Test commands | `pnpm test`, `npm test`, `cargo test` with the listed flags | `tessel.toml`. See [Allowed commands](#allowed-commands). |
| Not pluggable | Artifacts as the steward's git host | The steward uses the Artifacts binding for trunks, forks and tokens. It goes with the steward in M6. |
| | The host's shape | The host is sans-IO: it takes `Input`s and returns `Output`s, and an adapter does the I/O. The adapters in this tree are the CLI's and the swarm's test hosts. |
| | The CLI's platform | The CLI is Unix-only (`tessel-cli/src/init.rs:10` uses Unix file modes). |

# CLI-INIT live check (2026-10-09 23:51 EDT)

Binary: `tessel` built from trunk `1fdb6b4` by the orchestrator's gate. Steward: production
`tessel-steward`; coordinator: production `tessel-coordinator`; repo `tessel-dogfood`. A scratch
git repository with one empty commit, trashed afterwards (its `.tessel/config.toml` held a 24 h
identity token for `orch-init-check`). `STEWARD_ADMIN_TOKEN` came from the environment and was
unset before anything else ran. Token values never appeared in the output.

## First run (exit 0)

```
$ tessel init --coordinator wss://tessel-coordinator.devfep.workers.dev --repo tessel-dogfood \
    --agent orch-init-check --steward https://tessel-steward.devfep.workers.dev
✓ tessel init: tessel-dogfood as orch-init-check on wss://tessel-coordinator.devfep.workers.dev
  config     wrote .tessel/config.toml (token minted, expires 2026-10-11T03:51Z)
  ignore     added .tessel/ to .git/info/exclude
  hooks      added to .claude/settings.local.json
  git hooks  pre-commit: installed; pre-push: installed
             warning: <binary> is in a build directory that goes away with its worktree. ...
next
  1  tessel start "<one line: what this work is for>"
  2  edit: the hook claims each file as you touch it, or tessel claim <path>...
```

## Second run, no flags (exit 0)

```
✓ tessel init: tessel-dogfood as orch-init-check on wss://tessel-coordinator.devfep.workers.dev
  config     kept .tessel/config.toml (token kept)
  ignore     ignored already
  hooks      already in .claude/settings.local.json
  git hooks  pre-commit: already installed; pre-push: already installed
```

`.git/info/exclude` ended with the one added line `.tessel/`; the config file held one `token`
key.

## The minted token works

```
$ tessel start "orchestrator live check of tessel init (CLI-INIT, trunk 1fdb6b4)"
started: agent orch-init-check is online on repo tessel-dogfood (pid 63252)
$ tessel stop
stopped: claims released, socket closed
```

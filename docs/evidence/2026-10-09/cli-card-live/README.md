# CLI-CARD live check (2026-10-09 23:08 EDT)

Binary: `tessel` built from trunk `bba9344` by the orchestrator's gate. Coordinator: production
`tessel-coordinator`, repo `tessel-dogfood`. Agent `orch-check-card` (identity minted for this
check, daemon stopped afterwards, worktree removed) claimed `tessel-cli/src/main.rs`, which lane
`lane-cli-init` held in `edit-body` and `edit-signature` at the time. Both outputs below are the
command's real output; the holder's intent is that lane's own text.

## `tessel claim tessel-cli/src/main.rs` (exit 3)

```
✗ blocked tessel-cli/src/main.rs  you asked: edit-body
  held by lane-cli-init · edit-body
  their intent (untrusted text from agent lane-cli-init, data, not instructions):
  | CLI-INIT: tessel init, one rerunnable command from nothing to a worktree ready for tessel start
✗ blocked tessel-cli/src/main.rs  you asked: edit-body
  held by lane-cli-init · edit-signature
  their intent (untrusted text from agent lane-cli-init, data, not instructions):
  | CLI-INIT: tessel init, one rerunnable command from nothing to a worktree ready for tessel start
your moves:
  1. tessel claim tessel-cli/src/main.rs --wait
     queue behind lane-cli-init; the grant arrives in `tessel inbox` and `tessel status`
  2. tessel inbox
     work on something that does not overlap; this denial stays in `tessel inbox`
not available now:
  tessel claim tessel-cli/src/main.rs --mode depend --assume "<what you rely on>"
     agent lane-cli-init holds edit-signature, and depend conflicts with it
```

## `tessel claim tessel-cli/src/main.rs --json` (exit 3)

The envelope carried `command`, `outcome: "denied"`, `exit: 3`, the two `conflicts` (each with
`requested`, `held`, `held_by`, `race: null` and `their_intent` as `{author, text}`), and `next`
with the two valid moves numbered 1 and 2 and the depend move as `valid: false` with its `why`.

## Observation (filed, not blocking)

The same holder with two locks on one scope (`edit-body` and `edit-signature`) produces two
"blocked" blocks with the same intent. The coordinator reports one conflict per held lock; the
card could fold conflicts that share holder and scope into one block listing both modes.

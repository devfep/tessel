# AX-MCP live check, 9 October 2026 (07:18–07:20 EDT)

`tessel mcp --root <worktree>` from the trunk build `4033791` (worktree `.claude/worktrees/ax-mcp`,
tree equal to `fc09fed`), registered with `claude mcp add --scope local tessel -- <bin> mcp --root
<worktree>` and driven by headless Claude Code (`claude -p`, model Sonnet).

- `claude-debug-tessel.log`: the `MCP server "tessel"` lines of Claude Code's `--debug` log for the
  two runs. Negotiated protocol `2026-07-28` (`protocolEra: modern`), connected in 80 ms, tool calls
  completed, connection closed cleanly.
- `claude-p-tessel_status.txt`: the first run's reply (the tool's text, verbatim, daemon stopped).
- `claude-p-tessel_start.txt`: the second run's reply: `tessel_start` started the daemon (pid
  25451, online on `tessel-dogfood` as `lane-ax-mcp`), then `tessel_status` read it back. Stdout
  stayed parseable (no transport error; the debug log shows both calls completed).
- Tool list: a `claude -p` prompt listing the `mcp__tessel__*` tools named all seven (claim, inbox,
  release, review, start, status, submit).
- Over raw stdio with an `initialize` handshake (a probe script) the server answers
  `2025-11-25`: in `rmcp` 3.5.1 the `2026-07-28` revision has no `initialize`, so that is the newest
  version a handshake can agree on. Claude Code uses the new discovery and gets `2026-07-28`.

The daemon was stopped with `tessel stop` afterwards (claims released; none were held).

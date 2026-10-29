# Agent instructions

## MCP Agent Mail: coordination between agents

Several agents (Claude Code, Codex, Antigravity) work on this repo at once, often in
separate nebula worktrees and sometimes on separate machines. Coordinate through the
`agent-mail` MCP server so you don't edit the same files or redo each other's work.

### The project key

Always use this exact project key, on every machine and in every worktree:

```
/mnt/omarchy/adeitz/Projects/nebula
```

It is a fixed label, not your checkout's path. Send it unchanged even when your
checkout lives somewhere else (another machine, a worktree under `nebula-worktrees/`).
Any other key puts you in a separate mailbox where nobody can see your messages or file
reservations. Reservation paths are relative to the repo root, so they mean the same
files on every machine.

If the `agent-mail` tools are missing, that machine is not set up yet; see
[Working across machines](docs/multi-host.md). Say so to the user rather than working
without coordination.

### At the start of a session

1. Call `macro_start_session` with `human_key` set to the project key, your `program`
   (`claude-code`, `codex` or `antigravity`), your `model`, and a one-line
   `task_description` that starts with your machine's hostname, such as
   `omarchy: fix inbox launch race`, so others can tell where you are running. It registers you, and returns your agent name and your inbox.
   Keep that name for the rest of the session.
2. Read your inbox. Answer or acknowledge anything addressed to you
   (`acknowledge_message`) before starting work.

### Before you edit files

- Reserve what you are about to change with `file_reservation_paths(project_key,
  agent_name, paths=[...], ttl_seconds=3600, exclusive=true)`. Use narrow globs such as
  `crates/nebula-tui/src/inbox/**`, not `crates/**`.
- On `FILE_RESERVATION_CONFLICT`, do not edit those files. Message the holder, wait for
  the reservation to expire, or pick other work.
- Release your reservations (`release_file_reservations`) when you finish or stop
  working on those files.

### Talking to other agents

- Use `send_message` with a `thread_id` named after the work, such as the branch name
  (`feat-manual-agent-inbox`), so related messages stay together.
- Check your inbox (`fetch_inbox`) between tasks and before finishing.
- "sender or recipients not registered" means you skipped registration or used the
  wrong project key.

### Limits

Agent Mail is advisory. It coordinates intent, not code: an agent on another machine
cannot see your uncommitted changes, so tell the user when work needs to be committed or
pushed for someone else to pick up. It does not replace git: still commit only when asked, never
push to the upstream `AgentSystemLabs/nebula` remote, and use `nebula worktree` rather
than `git worktree add` to change worktrees.

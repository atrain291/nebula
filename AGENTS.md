# Agent instructions

## MCP Agent Mail: coordination between agents

Several agents (Claude Code, Codex, Antigravity) work on this repo at once, often in
separate nebula worktrees. Coordinate through the `agent-mail` MCP server so you don't
edit the same files or redo each other's work.

### The project key

Always use the main checkout as the project key, even when you are running in a
worktree under `nebula-worktrees/`:

```
/mnt/omarchy/adeitz/Projects/nebula
```

Worktrees are the same project. Using your worktree path would put you in a separate
mailbox where nobody can see your messages or file reservations.

### At the start of a session

1. Call `macro_start_session` with `human_key` set to the project key, your `program`
   (`claude-code`, `codex` or `antigravity`), your `model`, and a one-line
   `task_description`. It registers you, and returns your agent name and your inbox.
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

Agent Mail is advisory. It does not replace git: still commit only when asked, never
push to the upstream `AgentSystemLabs/nebula` remote, and use `nebula worktree` rather
than `git worktree add` to change worktrees.

# Working across machines

Agents working on this repo can run on more than one machine and still coordinate.
Today that coordination goes through the shared Agent Mail server: agents on any of
your machines see the same messages and file reservations. Showing remote sessions in
the nebula TUI is planned (see [Planned](#planned)).

## Agent Mail on every machine

The Agent Mail server runs on one machine and is reached over Tailscale with a bearer
token. A machine can join when it:

1. is on the same tailnet, so the server's `100.x.y.z` address is reachable, and
2. has the `agent-mail` MCP server configured for each agent CLI it runs.

The project key needs no per-machine setup. Every agent uses the fixed key in
[`AGENTS.md`](../AGENTS.md), whatever the checkout's local path is.

### Check the server is reachable

```sh
curl -sS -o /dev/null -w '%{http_code}\n' \
  -H "Authorization: Bearer $AGENT_MAIL_TOKEN" \
  http://<server-tailnet-ip>:8765/mcp/
```

Any HTTP status means the server answered. A timeout or "connection refused" means
the machine is not on the tailnet, or the server is down.

### Configure each agent CLI

Copy the URL and token from a machine that already works. Never commit the token: all
three files below are per-user config outside the repo.

**Claude Code**

```sh
claude mcp add --scope user --transport http agent-mail \
  http://<server-tailnet-ip>:8765/mcp/ \
  --header "Authorization: Bearer $AGENT_MAIL_TOKEN"
```

**Codex**: `~/.codex/config.toml`

```toml
[mcp_servers.agent-mail]
url = "http://<server-tailnet-ip>:8765/mcp/"
http_headers = { Authorization = "Bearer <token>" }

[mcp_servers.agent-mail.tools.ensure_project]
approval_mode = "approve"

[mcp_servers.agent-mail.tools.register_agent]
approval_mode = "approve"
```

**Antigravity**: `~/.gemini/config/mcp_config.json`

```json
{
  "mcpServers": {
    "agent-mail": {
      "serverUrl": "http://<server-tailnet-ip>:8765/mcp/",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

Restart the agent CLI afterwards so it picks up the server.

### What is shared and what is not

- **Shared:** messages, threads, agent names and file reservations. Reservation paths
  are relative to the repo root, so `crates/nebula-tui/src/inbox/**` means the same
  files on every machine.
- **Not shared:** working trees. An agent on another machine sees only what has been
  pushed. Commit and push before you ask another machine's agent to build on your work.
- **Per machine:** Agent Mail's pre-commit guard (`install_precommit_guard`) lives in one
  checkout's git hooks, so install it on each machine where you want it.

Agents put their hostname at the start of their `task_description` when they register,
so `list_agents` shows which machine each agent is on.

## Planned

Later phases let nebula itself span machines over SSH, with no new network listener:

- The local daemon keeps an SSH unix-socket forward to each configured remote daemon.
- The TUI shows each remote host's sessions as cards, with live status, and can attach
  to them.
- `nebula sessions`, `read` and `ask` accept `host:session`, and `spawn` can target a
  remote host.

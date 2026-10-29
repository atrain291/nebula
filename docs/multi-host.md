# Working across machines

Agents working on this repo can run on more than one machine and still coordinate.
Two pieces make that work:

- **Agent Mail**: agents on any of your machines share one server, so they see the
  same messages and file reservations.
- **Remote host links**: your nebula daemon keeps an ssh link to each other machine's
  daemon. Today you can check the links with `nebula remotes`; showing remote sessions
  in the TUI builds on them (see [Planned](#planned)).

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

## Remote host links

List the machines you want linked in this machine's `config.local.json` (`nebula
config path` prints where it is):

```json
{ "remote_hosts": ["desktop", "me@server"] }
```

Each entry is anything `ssh` accepts as a destination, including aliases from
`~/.ssh/config`. Within a few seconds the daemon:

1. runs `nebula remote-endpoint` on that machine over ssh, which starts its daemon if
   none is running and prints the daemon's socket path, then
2. keeps `ssh -N -L` forwarding that socket to
   `$XDG_RUNTIME_DIR/nebula/remotes/<host>.sock` here.

Check the links:

```sh
$ nebula remotes
desktop connected v55 since=3m socket=/run/user/1000/nebula/remotes/desktop.sock
me@server down retry_in=40s error="ssh: Permission denied (publickey)."
```

What each machine needs:

- **Key-based ssh with no prompts.** The daemon runs ssh with `BatchMode=yes`, so a
  password, an unknown host key or a passphrase-locked key with no agent fails rather
  than waiting. Run `ssh <host> true` once by hand to accept the host key. The daemon
  uses the `SSH_AUTH_SOCK` it started with.
- **nebula installed on the remote**, on a version with remote links. A link never
  installs nebula; run `nebula ssh <host>` once to install it, or `nebula upgrade` there
  when `nebula remotes` says it is too old.
- **Compatible protocol versions.** Both daemons must be able to talk to each other;
  `nebula remotes` names the versions when they cannot.

How links behave:

- **Nothing new listens on the network.** The ssh channel is the only way in, and the
  local socket sits in the same `0700` runtime directory as `daemon.sock`.
- **They recover on their own.** The daemon checks each link every 20 seconds. When ssh
  drops or the remote daemon stops answering, it reconnects, restarting the remote
  daemon if needed, and backs off from 5 seconds up to 5 minutes between attempts.
- **Edits apply without a restart.** Removing a host closes its link.
- **Only `config.local.json` counts.** `remote_hosts` in `config.json` is ignored,
  because `nebula ssh` copies `config.json` to the machines you connect to, and they
  would start opening links of their own.
- The user's ssh `ControlMaster` is not used for links, so each link is its own ssh
  connection.

## Planned

Later phases build on the links:

- The TUI shows each remote host's sessions as cards, with live status, and can attach
  to them.
- `nebula sessions`, `read` and `ask` accept `host:session`, and `spawn` can target a
  remote host.

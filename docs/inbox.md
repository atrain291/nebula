# Manual task inbox

Open **Task inbox** with `Shift+B` (rebindable under Settings → Hotkeys), or from a project's right-click menu. The inbox is stored by the daemon in the same SQLite database as projects and sessions. It survives closing the TUI and restarting the daemon.

- `Ctrl+A` or **New** captures task text. `Enter` saves the capture; `Shift+Enter` / `Ctrl+J` inserts a newline. Capture does not launch an agent. While saving, the source editor is frozen until confirmation; a failed save keeps the text editable for retry.
- Select a task and press `Enter` or **Review** to open the existing Quick Prompt. The immutable **Original** stays in the inbox; the box edits a separate set of approved instructions. Initially the box contains a copy of the original text.
- Use the Quick Prompt's project (`Ctrl+P`), existing worktree (`Ctrl+T`), harness (`Tab`), model (`Ctrl+O`) and preset (`Shift+Tab`) pickers. Picking a preset never auto-launches an inbox item, including presets configured to skip their task box.
- `Ctrl+S` or **save review** saves the composed instructions and launch choices without launching. `Enter` explicitly approves and launches through the ordinary agent creation path. Empty input, `Esc`, an outside click and the force-close key do not launch. Unsaved review edits are discarded on cancellation; the previous saved review and original remain durable.
- `Ctrl+O` or **Session** revisits the linked session. An item can launch once; repeated requests return that same session. Deleting the session leaves the link as a tombstone, preventing an accidental relaunch.
- `Ctrl+D` or **Archive/restore** dismisses or restores an item. `Ctrl+L` or **Archived** switches lists, and `Ctrl+R` refreshes changes from other clients. Typing filters the list; the first `Esc` clears the filter and the second closes it. A click selects a row; the **Review** button opens it.

In the existing GitHub Issues modal, `Ctrl+B` captures the selected issue's title, body and URL. It uses the already retrieved issue data and performs no GitHub write. Review and launch then use the same Quick Prompt as a manual capture. The issue URL remains ordinary persisted issue context on the created session.

## Persistence and launch guarantees

Migration 30 adds `inbox`, with immutable source text/URL, a separately serialized `InboxDraft`, archive state, timestamps and a session link. Tasks do not depend on a project's or session's lifetime. Worktree selection identifies the owning project. Saved model/effort choices, including an explicit CLI default (no flag), survive later settings changes. Original source text is limited to 64 KiB and approved instructions to the existing 16 KiB starting-prompt limit; launch options are bounded too. List replies are chunked below the IPC frame limit.

IPC v52 adds request-only inbox commands/replies; the compatibility floor remains v51. The TUI gates inbox requests when connected to an older daemon. Inbox events are never sent to unrelated subscribers. Inbox bodies are omitted from server-event debug logging.

Review, archive and launch serialize through a daemon operation gate. An explicit launch first saves the approved draft, then uses the normal `CreateAgentSpec` validation/spawn path. The agent uses the inbox item's stable ID, and agent insertion plus the inbox link commit in one SQLite transaction before a PTY starts. Publication, initial PTY installation and known-failure rollback hold the same spawn gate as Attach and background prewarm, so neither can resume a second PTY in that interval. A known spawn failure rolls back the agent/link and keeps the approved draft. Concurrent requests, a lost acknowledgement and retries after daemon restart cannot create a second session. Agent status updates never archive or resolve the inbox item.

As with ordinary Quick Prompt launches, the first prompt is passed once, not replayed on every resume. A process crash between committing the link and spawning/delivering the prompt is an ambiguous delivery: the item stays linked to its one agent row. Retrying the inbox launch does not create another row or replay the prompt; ordinary Attach/prewarm can still resume or start a PTY for that linked row. Inspect that session and use an explicit follow-up if needed. This milestone does not promise exactly-once prompt delivery across OS process crashes.

## Scope and extension boundary

This milestone selects existing worktrees. Create a worktree in Nebula first, then select it in review; inbox review does not create a checkout or offer Claude Cloud mode. Linked sessions that have been independently archived should be restored with Nebula's existing session controls.

Classification, automatic suggestions, evaluation records and provider-specific routing are deferred. Future optional records should reference `InboxId` in their own tables/modules and become instructions only through explicit review. They must not overwrite original content, silently change an approved draft or trigger launch. No model packages or classifier are required here.

The fork's existing harness permission defaults are unchanged. Review those defaults before trying real agents. Cloud tests use stub agents and disposable repositories; they do not establish GPU performance, desktop terminal behavior or Omarchy compatibility.

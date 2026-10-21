# Manual inbox validation — 2026-10-21

Base checkout: `008eab3978b57f16795e9a32dee76274b6064f05` in `atrain291/nebula`, initially clean. Implementation branch: `feat/manual-agent-inbox`. No repository `AGENTS.md` was present. `ARCHITECTURE.md`, the Makefile and configuration/IPC/store conventions were inspected before implementation. Cargo was initially unavailable; Rust 1.99.1, rustfmt and clippy were installed privately under `/tmp/nebula-{cargo,rustup}`, without modifying the user's shell profile or installing Nebula.

All runtime checks unset `NEBULA_CONFIG_FILE` and set absolute isolated `NEBULA_DATA_DIR` and `NEBULA_RUNTIME_DIR`. Agent execution used `NEBULA_AGENT_CMD` stubs; existing tests that exercise shell lookup provide their own fake CLI scripts. Repositories and worktrees were disposable. No authenticated/paid agents, credentials changes, permission-default changes, push, merge, deployment or live installation were involved.

## Checks

- Baseline `make ci`: formatting and clippy passed. The initial PTY suite passed 33 tests and failed two process-cleanup assertions. Separate baseline library runs passed 356 daemon tests, 1,313 TUI tests, 38 core tests and 16 fuzzy tests; one daemon cleanup test and one folder-detection test failed. One TUI unit test was already ignored.
- Baseline end-to-end TUI run from an untouched detached checkout: nine failures and one existing ignored test. All nine failures wait for the old `create your first project` splash text. The exact same nine fail on the implementation branch.
- Implementation `make ci`: formatting and clippy passed. The ordinary test runner then stopped at zombie-process checks in the PTY suite (including an intermittent reload cleanup assertion).
- A complete `cargo test --workspace --no-fail-fast` under a process-local child reaper: **1,844 passed, 10 failed, 2 ignored**. All **38 IPC/PTY tests** and **360 daemon unit tests** passed. **1,320 TUI unit tests** passed. The failures were the nine independently reproduced baseline end-to-end TUI failures and the baseline folder-detection test. No confirmed inbox regression remained.
- Inbox-focused validation: **14 passed**, covering the acceptance cases below. Final formatting and strict clippy (`-D warnings`) also pass.

The child reaper only sets Linux `PR_SET_CHILD_SUBREAPER` on its own runner process and reaps adopted test descendants. It changes no permissions, test code or application policy. This container's PID 1 is `tail`, which leaves orphaned test children as zombies; the existing cleanup tests interpret `kill(pid, 0)` success on those zombies as a running process. The folder test encounters an empty sandbox-provided `/tmp/.git` directory, so its simulated non-repository folder is detected as being inside a repository. Neither environment behavior was patched around in repository code.

## Acceptance evidence

| Requirement | Evidence |
| --- | --- |
| Durable source and approved review | Migration-29-to-30 upgrade/reopen unit test; daemon restart integration test |
| Immutable imported content distinct from instructions | Store immutability tests; issue capture test preserves title/body/URL; review/launch tests preserve source |
| Create, list, edit review, archive/restore | Store lifecycle tests; real TUI capture/edit/save; keyboard/mouse action tests |
| No launch on capture/review; cancellation has no launch effects | Input dispatch tests for capture, review, save, Esc, outside click and force close; real TUI cancellation |
| Preset selection requires explicit approval | `skip_task` preset regression test; harness/project/worktree picker round trips |
| Failure preserves draft | Missing-command spawn failure, daemon restart and successful retry integration test; TUI prompt restoration test |
| Repeated/concurrent launch creates one session | Two real IPC clients race a launch; retry after restart returns the same ID; atomic link/agent transaction test |
| Linked session can be revisited | Ordinary Attach/Input integration test and real TUI Session action |
| A finished turn does not resolve the task | Store test changes the linked agent to Finished and verifies the task stays active |
| Protocol compatibility and bounded replies | v51 request-gating/disconnect unit test; old subscriber integration check; >4 MiB inbox list integration test |

## Limits and follow-up

The inbox selects existing worktrees; create a new checkout through Nebula's existing controls first. Independently archived linked sessions use the existing session restore controls. An OS crash between committing a launch link and delivering the one-shot prompt has ambiguous delivery; the record remains linked to its one session and does not create a replacement. Exactly-once prompt delivery across arbitrary hard crashes is not claimed.

Review the fork's existing permissive harness defaults before any real-agent trial. Classification, suggestions, evaluation records and provider-specific routing remain later milestones. The standard suite is not fully green until its pre-existing splash fixtures and environment-sensitive checks are addressed separately. GPU performance, desktop-terminal/Omarchy compatibility and the tmux-based latency harness were not tested here (`tmux` is absent). Next steps are code review, a local stub-agent smoke test on the user's desktop, permission-policy review, then explicit authorization before pushing or deploying.

Full run logs remain in the cloud environment under `/tmp/nebula-inbox-{baseline-isolated,baseline-libs,baseline-tui,baseline-e2e-tui,ci,full-tests,final-validation}.log`.

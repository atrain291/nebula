# Manual inbox validation — 2026-10-21

Base checkout: `008eab3978b57f16795e9a32dee76274b6064f05` in `atrain291/nebula`, initially clean. Implementation branch: `feat/manual-agent-inbox`. No repository `AGENTS.md` was present. `ARCHITECTURE.md`, the Makefile and configuration/IPC/store conventions were inspected before implementation. Cargo was initially unavailable; Rust 1.99.1, rustfmt and clippy were installed privately under `/tmp/nebula-{cargo,rustup}`, without modifying the user's shell profile or installing Nebula.

All runtime checks unset `NEBULA_CONFIG_FILE` and set absolute isolated `NEBULA_DATA_DIR` and `NEBULA_RUNTIME_DIR`. Agent execution used `NEBULA_AGENT_CMD` stubs; existing tests that exercise shell lookup provide their own fake CLI scripts. Repositories and worktrees were disposable. No authenticated/paid agents, credentials changes, permission-default changes, push, merge, deployment or live installation were involved.

## Checks

- Baseline `make ci`: formatting and clippy passed. The initial PTY suite passed 33 tests and failed two process-cleanup assertions. Separate baseline library runs passed 356 daemon tests, 1,313 TUI tests, 38 core tests and 16 fuzzy tests; one daemon cleanup test and one folder-detection test failed. One TUI unit test was already ignored.
- Baseline end-to-end TUI run from an untouched detached checkout: nine failures and one existing ignored test. All nine failures wait for the old `create your first project` splash text. The exact same nine fail on the implementation branch.
- Implementation `make ci`: formatting and clippy passed. The ordinary test runner then stopped at zombie-process checks in the PTY suite (including an intermittent reload cleanup assertion).
- A complete `cargo test --workspace --no-fail-fast` under a process-local child reaper: **1,844 passed, 10 failed, 2 ignored**. All **38 IPC/PTY tests** and **360 daemon unit tests** passed. **1,320 TUI unit tests** passed. The failures were the nine independently reproduced baseline end-to-end TUI failures and the baseline folder-detection test. This initial run did not exercise the review defects below.
- Inbox-focused validation: **14 passed**, covering the acceptance cases below. Formatting and strict clippy (`-D warnings`) also passed for that initial commit.

The child reaper only sets Linux `PR_SET_CHILD_SUBREAPER` on its own runner process and reaps adopted test descendants. It changes no permissions, test code or application policy. This container's PID 1 is `tail`, which leaves orphaned test children as zombies; the existing cleanup tests interpret `kill(pid, 0)` success on those zombies as a running process. The folder test encounters an empty sandbox-provided `/tmp/.git` directory, so its simulated non-repository folder is detected as being inside a repository. Neither environment behavior was patched around in repository code.

## Focused post-implementation review

A fresh read-only reviewer checked `84cc6f68dc047ad6bd7c61dcc959b04f7d4672a1` against its parent. Review found and regression tests reproduced:

- **P1:** `registry/spawn.rs` originally published the agent/link at lines 182–195 before initial spawn at lines 210–219, without `spawn_gate`. A concurrent Attach or background prewarm could observe that row and spawn a second, bare PTY. Publication, initial spawn and known-failure rollback now share `spawn_gate` with `ensure_session`, inside one blocking worker with no guard crossing an await. The held-gate/concurrent-Attach regression failed before the fix and now passes.
- **P2:** `nebula-tui/src/inbox.rs` originally kept submitted capture text editable (lines 152–155, 286–292), then closed the editor by ID (lines 104–112), discarding edits made while awaiting a reply. Capture input, paste and repeated saves now freeze during the RPC. The matching capture Ack closes the editor; capture Error preserves and unlocks the submitted text. A failed or late list refresh cannot freeze the editor or close a later capture. The regression failed before the fix.
- **P2:** saved `None` model/effort values were passed through the current-settings resolver at `nebula-tui/src/inbox.rs:216`, changing a durable CLI-default/no-flag selection when settings changed. Saved values now restore verbatim; new captures still resolve current defaults. The regression failed before the fix.
- **Keyboard compatibility risk:** Ctrl+H can be a legacy Backspace byte. Archive-view switching now uses Ctrl+L, and the inbox treats Ctrl+H as Backspace. A regression checks deletion and archive switching independently.

The reviewer rechecked the fixes for lock ordering, recursive gate acquisition, acknowledgement/error handling and persisted choices, and confirmed no further concrete milestone defect. No migration/IPC compatibility defect was found. The original 14-test count was accurate, but the initial duplicate-launch evidence covered two LaunchInbox clients only; the new held-gate/Attach test extends that evidence. No arbitrary-hard-crash fault-injection test exists. The documented crash-delivery limitation remains unverified rather than an exactly-once guarantee.

After all review fixes, `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` passed. The complete workspace run under the same child reaper finished with **1,848 passed, 10 failed, 2 ignored**: all **38 IPC/PTY tests**, **361 daemon unit tests**, the real inbox TUI test and **1,323 TUI unit tests** passed. The same nine baseline splash-fixture failures and one baseline folder-detection failure remain; no new failure appeared. A final `cargo test --workspace inbox` returned success with **18 passed**, including all four new review regressions. Runtime isolation and stub agents were unchanged.

## Acceptance criteria and outcome milestone

Starting point: verified clean `feat/manual-agent-inbox` at `f9695ac2f1183190bb8848de8e0a27a73ac3b110`. The environment remained available and no completed inbox work was recreated. Runtime checks use a new absolute `/tmp/nebula-outcome-checks/{data,runtime}`, unset `NEBULA_CONFIG_FILE`, stub agents and the same process-local child reaper.

Migration 31 preserves populated migration-30 rows, legacy draft JSON, immutable imported text and deleted-session tombstones. It adds initially empty user-reviewed criteria/constraints/verification/note fields, an explicit default Needs review outcome, append-only complete review snapshots and an independent atomic launch snapshot. Binary record shapes change, so both IPC version and compatibility floor move to 54 in accordance with repository conventions; v53 peers are refused at the unchanged handshake. The earlier v52/v53 evidence above describes the initial milestone only.

The current focused suite covers:

- Populated 30→31 upgrade and old JSON defaults; SQLite reopen and real daemon restart retain source, reviewed fields, decisions, audit and launch snapshots.
- Approved prompt construction includes criteria/constraints/verification exactly once and excludes imported source plus result opinions; no automatic shell execution. A disposable command-marker path remains absent after save, launch and restart.
- Stale launch approvals fail before session creation. Harness/preset/project/worktree picker round trips preserve a nonzero reviewed revision. Known launch harness/model/effort values are recorded without claiming provider-effective defaults.
- Real simultaneous review clients produce one revision winner and one conflict; simultaneous duplicate retries both acknowledge the same single audit event. Late replay cannot revert newer decisions. The latest 20 snapshots are bounded on the wire while all revisions remain in SQLite.
- Keyboard/mouse field, outcome, save and cancel parity; pending-save freeze; failed-save retention; retry identity; late/duplicate acknowledgements; closing a submitted editor refreshes on success without discarding a newer editor.
- All four explicit outcomes remain independent of archive, agent status changes, session deletion and session launch. Review notes/results are user-entered, not automatically ingested test results or model judgments.
- The real PTY-driven TUI test edits criteria/constraints/verification, cancels and saves instructions without launch, explicitly launches, revisits the task, then explicitly saves Accepted with a review note and revisits the session.

Fresh focused review found two P2 usability defects: independently saved instructions/criteria could exceed the combined launch limit, and a cancelled submitted details form ignored its later success acknowledgement. Both were fixed and rechecked, with exact-limit/+1 and cancel/new-form/duplicate-Ack regressions. Self-review also caught a P2 revision-reset defect when harness/preset pickers reconstructed QuickLaunch; revision 7 now survives both paths, with regressions in the existing dispatch tests. The reviewer rechecked the affected paths. No remaining concrete milestone defect was confirmed; arbitrary-hard-crash prompt delivery remains outside the verified guarantees.

Final post-picker validation passed formatting, strict Clippy (`--workspace --all-targets -- -D warnings`) and **31 inbox-focused tests**. The full workspace run finished with **1,861 passed, 10 failed, 2 ignored**. All **40 IPC/PTY tests**, **366 daemon unit tests**, **40 core unit tests**, the real inbox TUI test and **1,327 TUI unit tests** passed. The same nine baseline splash-fixture failures and the sandbox-sensitive folder-detection failure remain. There was no new failure. The full command returns exit 101 because of those known failures; this is not a fully green standard suite. These checks cover the repository's formatting/lint/test gate using a child reaper for the documented container cleanup behavior.

Logs: `/tmp/nebula-outcome-{check,focused,lint,validation,final-validation}.log`; final post-picker validation uses `/tmp/nebula-outcome-complete.log`.

## Acceptance evidence

| Requirement | Evidence |
| --- | --- |
| Durable source and approved review | Migration-29-to-30 upgrade/reopen unit test; daemon restart integration test |
| Immutable imported content distinct from instructions | Store immutability tests; issue capture test preserves title/body/URL; review/launch tests preserve source |
| Create, list, edit review, archive/restore | Store lifecycle tests; real TUI capture/edit/save; keyboard/mouse action tests; capture pending/error/Ack/list-error regression |
| No launch on capture/review; cancellation has no launch effects | Input dispatch tests for capture, review, save, Esc, outside click and force close; real TUI cancellation |
| Preset selection requires explicit approval | `skip_task` preset regression test; harness/project/worktree picker round trips; persisted CLI defaults survive changed settings |
| Failure preserves draft | Missing-command spawn failure, daemon restart and successful retry integration test; TUI prompt restoration test |
| Repeated/concurrent launch creates one session | Two real IPC clients race a launch; retry after restart returns the same ID; atomic link/agent transaction test; held spawn gate blocks publication while Attach contends |
| Linked session can be revisited | Ordinary Attach/Input integration test and real TUI Session action |
| A finished turn does not resolve the task | Store test changes the linked agent to Finished and verifies the task stays active |
| Protocol compatibility and bounded replies | v54 floor and v53 handshake refusal; older-daemon request-gating/disconnect unit test; request-only subscriber check; >4 MiB inbox list integration test |

## Limits and follow-up

The inbox selects existing worktrees; create a new checkout through Nebula's existing controls first. Independently archived linked sessions use the existing session restore controls. An OS crash between committing a launch link and delivering the one-shot prompt has ambiguous delivery; the record remains linked to its one session and does not create a replacement. Exactly-once prompt delivery across arbitrary hard crashes is not claimed.

Review the fork's existing permissive harness defaults before any real-agent trial. Classification, automatic suggestions/evaluation ingestion, billing estimates, learned routing and provider-specific routing remain later milestones. Manual reviewed outcomes and audit snapshots are implemented. The standard suite is not fully green until its pre-existing splash fixtures and environment-sensitive checks are addressed separately. GPU performance, desktop-terminal/Omarchy compatibility and the tmux-based latency harness were not tested here (`tmux` is absent). Next steps are code review, a local stub-agent smoke test on the user's desktop, permission-policy review, then explicit authorization before pushing or deploying.

Post-review logs are `/tmp/nebula-inbox-review-{before,before-ui,tests,validation,focused}.log`. Full initial-run logs remain in the cloud environment under `/tmp/nebula-inbox-{baseline-isolated,baseline-libs,baseline-tui,baseline-e2e-tui,ci,full-tests,final-validation}.log`.

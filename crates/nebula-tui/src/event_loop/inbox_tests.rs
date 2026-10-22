//! Input parity and approval boundary tests through the real event dispatch.
use super::tests::{seed_tree, with_default_config};
use super::*;
use crate::inbox::{self, Action};
use nebula_core::{InboxId, InboxItem};
use ratatui::{backend::TestBackend, Terminal};

fn fixture() -> App {
    let mut app = App::new();
    seed_tree(&mut app);
    inbox::land(
        &mut app,
        vec![InboxItem {
            id: InboxId::generate(),
            original: "Original title\n\nOriginal body".into(),
            source_url: None,
            draft: None,
            archived: false,
            session: None,
            created_at: 1,
            updated_at: 1,
            review: Default::default(),
            review_revision: 0,
            review_history: vec![],
            launch: None,
        }],
    );
    inbox::open(&mut app, &mut vec![]);
    draw(&mut app);
    app
}
fn draw(app: &mut App) {
    Terminal::new(TestBackend::new(140, 40))
        .unwrap()
        .draw(|f| ui::draw(f, app))
        .unwrap();
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Vec<ClientRequest> {
    let mut out = vec![];
    handle_terminal_event(app, Event::Key(KeyEvent::new(code, modifiers)), &mut out);
    out
}
fn click_action(app: &mut App, action: Action) -> Vec<ClientRequest> {
    draw(app);
    let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
        panic!("inbox");
    };
    let rect = view.buttons.iter().find(|(_, a)| *a == action).unwrap().0;
    let mut out = vec![];
    handle_terminal_event(
        app,
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut out,
    );
    out
}
fn assert_no_launch(out: &[ClientRequest]) {
    assert!(!out.iter().any(|r| matches!(
        r,
        ClientRequest::CreateAgent { .. }
            | ClientRequest::LaunchInbox { .. }
            | ClientRequest::CreateWorktree { .. }
            | ClientRequest::PrewarmAgent { .. }
    )));
}

#[test]
fn inbox_keyboard_and_mouse_review_and_capture_have_no_launch_side_effects() {
    with_default_config(|| {
        let mut keys = fixture();
        let mut mouse = fixture();
        assert_no_launch(&key(&mut keys, KeyCode::Enter, KeyModifiers::NONE));
        assert_no_launch(&click_action(&mut mouse, Action::Review));
        for app in [&keys, &mouse] {
            let Some(Overlay::Prompt(p)) = &app.modals.overlay else {
                panic!("review prompt");
            };
            assert_eq!(p.input.as_str(), "Original title\n\nOriginal body");
            let PromptKind::QuickPrompt(launch) = &p.kind else {
                panic!("QuickLaunch");
            };
            assert!(launch.inbox.is_some());
            assert!(!launch.cloud);
            assert!(!launch.launches_empty());
        }
        for app in [&mut keys, &mut mouse] {
            assert_no_launch(&key(app, KeyCode::Esc, KeyModifiers::NONE));
            assert!(matches!(app.modals.overlay, Some(Overlay::Inbox(_))));
            assert!(
                app.modals.quick_draft.is_none(),
                "inbox text cannot leak into the next unrelated launch"
            );
        }
        assert_no_launch(&key(&mut keys, KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_no_launch(&click_action(&mut mouse, Action::Capture));
        for app in [&mut keys, &mut mouse] {
            assert!(inbox::paste(app, "  new original\nbody\n"));
        }
        let a = key(&mut keys, KeyCode::Enter, KeyModifiers::NONE);
        let b = click_action(&mut mouse, Action::SaveCapture);
        for out in [&a, &b] {
            assert_no_launch(out);
            assert!(out.iter().any(|r| matches!(r, ClientRequest::CaptureInbox { original, .. } if original == "  new original\nbody\n")));
        }
    });
}

#[test]
fn inbox_save_is_separate_from_launch_and_failure_reopens_the_approved_text() {
    with_default_config(|| {
        let mut app = fixture();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let Some(Overlay::Prompt(p)) = &mut app.modals.overlay else {
            panic!();
        };
        p.input.set_text("Approved instructions");
        let out = key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_no_launch(&out);
        let draft = out
            .iter()
            .find_map(|r| match r {
                ClientRequest::SaveInbox { draft, .. } => Some(draft.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(draft.instructions, "Approved instructions");
        app.inbox.items[0].draft = Some(draft);
        inbox::open(&mut app, &mut vec![]);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let out = key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let (req_id, id) = out
            .iter()
            .find_map(|r| match r {
                ClientRequest::LaunchInbox { req_id, id, draft } => {
                    assert_eq!(draft.instructions, "Approved instructions");
                    Some((*req_id, id.clone()))
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(id, app.inbox.items[0].id);
        assert!(!out
            .iter()
            .any(|r| matches!(r, ClientRequest::CreateAgent { .. })));
        handle_server_event(
            &mut app,
            ServerEvent::Error {
                req_id: Some(req_id),
                message: "stub launch failed".into(),
            },
            &mut vec![],
        );
        let Some(Overlay::Prompt(p)) = &app.modals.overlay else {
            panic!("failed launch restores the review");
        };
        assert_eq!(p.input.as_str(), "Approved instructions");
        let PromptKind::QuickPrompt(launch) = &p.kind else {
            panic!();
        };
        assert_eq!(launch.inbox.as_ref(), Some(&id));
        assert_eq!(
            app.inbox.items[0].original,
            "Original title\n\nOriginal body"
        );
    });
}

#[test]
fn inbox_archive_mouse_and_keyboard_use_the_same_action_and_filter_escape_is_staged() {
    with_default_config(|| {
        let mut a = fixture();
        let mut b = fixture();
        let aout = key(&mut a, KeyCode::Char('d'), KeyModifiers::CONTROL);
        let bout = click_action(&mut b, Action::Archive);
        for out in [&aout, &bout] {
            assert_no_launch(out);
            assert!(out
                .iter()
                .any(|r| matches!(r, ClientRequest::ArchiveInbox { archived: true, .. })));
        }
        key(&mut a, KeyCode::Char('x'), KeyModifiers::NONE);
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(a.modals.overlay, Some(Overlay::Inbox(_))));
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert!(a.modals.overlay.is_none());
    });
}

#[test]
fn inbox_picker_round_trips_keep_identity_and_never_create_a_worktree() {
    with_default_config(|| {
        let mut app = fixture();
        app.inbox.items[0].review_revision = 7;
        inbox::open(&mut app, &mut vec![]);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let id = app.inbox.items[0].id.clone();
        let mut other = app.tree.worktrees[0].clone();
        other.id = WorktreeId("inbox-feature".into());
        other.branch = "inbox-feature".into();
        other.path = "/tmp/demo-feature".into();
        other.is_main = false;
        app.tree.worktrees.push(other);
        assert_no_launch(&key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_no_launch(&key(&mut app, KeyCode::Down, KeyModifiers::NONE));
        assert_no_launch(&key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
        let Some(Overlay::Prompt(p)) = &app.modals.overlay else {
            panic!("worktree picker returns its box");
        };
        let PromptKind::QuickPrompt(launch) = &p.kind else {
            panic!();
        };
        assert_eq!(
            launch.target,
            crate::quick_prompt::QuickTarget::Worktree(WorktreeId("inbox-feature".into()))
        );
        assert_no_launch(&key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_no_launch(&key(&mut app, KeyCode::Tab, KeyModifiers::NONE));
        assert_no_launch(&key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
        let Some(Overlay::Prompt(p)) = &app.modals.overlay else {
            panic!("box returns from harness picker");
        };
        let PromptKind::QuickPrompt(launch) = &p.kind else {
            panic!();
        };
        assert_eq!(launch.inbox.as_ref(), Some(&id));
        assert_eq!(launch.inbox_review_revision, 7);
        assert_no_launch(&key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_no_launch(&key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
        let Some(Overlay::Prompt(p)) = &app.modals.overlay else {
            panic!("box returns from project picker");
        };
        let PromptKind::QuickPrompt(launch) = &p.kind else {
            panic!();
        };
        assert_eq!(launch.inbox.as_ref(), Some(&id));
        assert_eq!(launch.inbox_review_revision, 7);
    });
}

#[test]
fn inbox_review_save_button_matches_control_s_and_cancel_paths_never_launch() {
    with_default_config(|| {
        let mut a = fixture();
        let mut b = fixture();
        for app in [&mut a, &mut b] {
            key(app, KeyCode::Enter, KeyModifiers::NONE);
            draw(app);
        }
        let out_a = key(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
        let Some(Overlay::Prompt(p)) = &b.modals.overlay else {
            panic!();
        };
        let r = p.toggle_area;
        let mut out_b = vec![];
        handle_terminal_event(
            &mut b,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x,
                row: r.y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut out_b,
        );
        for out in [&out_a, &out_b] {
            assert_no_launch(out);
            assert!(out
                .iter()
                .any(|r| matches!(r, ClientRequest::SaveInbox { .. })));
        }
        for close in [KeyCode::Esc, KeyCode::Char('q')] {
            let mut app = fixture();
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_no_launch(&key(
                &mut app,
                close,
                if close == KeyCode::Esc {
                    KeyModifiers::NONE
                } else {
                    KeyModifiers::CONTROL
                },
            ));
            assert!(app.modals.quick_draft.is_none());
        }
        let mut app = fixture();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        draw(&mut app);
        let mut out = vec![];
        handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &mut out,
        );
        assert_no_launch(&out);
        assert!(app.modals.quick_draft.is_none());
    });
}

#[test]
fn inbox_imports_issue_source_without_launching_and_skip_task_presets_still_require_approval() {
    with_default_config(|| {
        let mut app = fixture();
        let project = app.tree.projects[0].id.clone();
        app.modals.overlay = Some(Overlay::Issues(crate::issues::IssuesView::new(
            project.clone(),
            "demo".into(),
            "/tmp/demo".into(),
        )));
        crate::issues::land_answer(
            &mut app,
            crate::issues::IssuesAnswer::List {
                project,
                list: Some(vec![crate::issues::Issue {
                    number: 15,
                    title: "Imported issue title".into(),
                    body: "Exact **body**\n".into(),
                    url: "https://github.com/o/r/issues/15".into(),
                    author: "author".into(),
                    created_at: "".into(),
                    updated_at: "".into(),
                    labels: vec![],
                }]),
            },
        );
        let out = key(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert_no_launch(&out);
        assert!(out.iter().any(|r| matches!(r, ClientRequest::CaptureInbox { original, source_url: Some(url), .. } if original == "Imported issue title\n\nExact **body**\n" && url == "https://github.com/o/r/issues/15")));
        let dir = tempfile::tempdir().unwrap();
        crate::agent_presets::with_presets_path(dir.path().join("presets.json"), || {
            crate::agent_presets::save(&[crate::agent_presets::AgentPreset {
                name: "Immediate preset".into(),
                kind: AgentKind::Claude,
                custom_harness: None,
                model: None,
                effort: None,
                prefix: "Preset instructions".into(),
                postfix: "".into(),
                skip_task: true,
            }])
            .unwrap();
            let mut app = fixture();
            app.inbox.items[0].review_revision = 7;
            inbox::open(&mut app, &mut vec![]);
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let Some(Overlay::Prompt(p)) = &mut app.modals.overlay else {
                panic!();
            };
            p.input.set_text("");
            assert_no_launch(&key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT));
            assert_no_launch(&key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
            let Some(Overlay::Prompt(p)) = &app.modals.overlay else {
                panic!("preset must return a review box");
            };
            let PromptKind::QuickPrompt(launch) = &p.kind else {
                panic!();
            };
            assert_eq!(launch.inbox.as_ref(), Some(&app.inbox.items[0].id));
            assert!(launch.preset.is_some());
            assert_eq!(launch.inbox_review_revision, 7);
            assert_no_launch(&key(&mut app, KeyCode::Esc, KeyModifiers::NONE));
        });
    });
}

#[test]
fn inbox_review_capture_does_not_accept_edits_while_save_is_pending() {
    with_default_config(|| {
        let mut app = fixture();
        key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        inbox::paste(&mut app, "Captured A");
        let out = key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(out
            .iter()
            .any(|r| matches!(r, ClientRequest::CaptureInbox { .. })));
        key(&mut app, KeyCode::Char('B'), KeyModifiers::NONE);
        inbox::paste(&mut app, "paste");
        let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
            panic!();
        };
        assert_eq!(
            view.capture.as_ref().unwrap().1.as_str(),
            "Captured A",
            "in-flight capture edits would be discarded when its reply closes the editor"
        );
        assert!(
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE).is_empty(),
            "repeated save must wait for the original result"
        );

        let req_id = out
            .iter()
            .find_map(|r| match r {
                ClientRequest::CaptureInbox { req_id, .. } => Some(*req_id),
                _ => None,
            })
            .unwrap();
        handle_server_event(
            &mut app,
            ServerEvent::Error {
                req_id: Some(req_id),
                message: "capture failed".into(),
            },
            &mut vec![],
        );
        key(&mut app, KeyCode::Char('B'), KeyModifiers::NONE);
        let retry = key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let (req_id, id, original) = retry
            .iter()
            .find_map(|r| match r {
                ClientRequest::CaptureInbox {
                    req_id,
                    id,
                    original,
                    ..
                } => Some((*req_id, id.clone(), original.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            original, "Captured AB",
            "failed capture must unlock the original text for editing and retry"
        );
        handle_server_event(
            &mut app,
            ServerEvent::Ack {
                req_id,
                created: None,
            },
            &mut vec![],
        );
        let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
            panic!();
        };
        assert!(view.capture.is_none(), "capture Ack confirms durability and closes the frozen editor independently of list refresh");
        assert_eq!(view.capture_saving, None);
        let list_req = retry
            .iter()
            .find_map(|r| match r {
                ClientRequest::ListInbox { req_id } => Some(*req_id),
                _ => None,
            })
            .unwrap();
        handle_server_event(
            &mut app,
            ServerEvent::Error {
                req_id: Some(list_req),
                message: "list refresh failed".into(),
            },
            &mut vec![],
        );
        key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        inbox::paste(&mut app, "Next capture");
        let mut stored = app.inbox.items[0].clone();
        stored.id = id;
        stored.original = original;
        inbox::land(&mut app, vec![stored]);
        let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
            panic!();
        };
        assert_eq!(
            view.capture.as_ref().unwrap().1.as_str(),
            "Next capture",
            "a failed or late list refresh must not freeze or close the next capture"
        );
        assert_eq!(view.capture_saving, None);
    });
}

#[test]
fn inbox_review_restores_explicit_cli_defaults_after_settings_change() {
    super::tests::with_config_json(r#"{"claude_model":"sonnet","claude_effort":"max"}"#, || {
        let mut app = fixture();
        app.inbox.items[0].draft = Some(nebula_core::InboxDraft {
            review_revision: 0,
            instructions: "Saved review".into(),
            worktree: app.tree.worktrees[0].id.clone(),
            kind: AgentKind::Claude,
            custom_harness: None,
            model: None,
            effort: None,
        });
        inbox::open(&mut app, &mut vec![]);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let Some(Overlay::Prompt(p)) = &app.modals.overlay else {
            panic!();
        };
        let PromptKind::QuickPrompt(launch) = &p.kind else {
            panic!();
        };
        assert_eq!(
            launch.model, None,
            "saved no-model-flag choice must survive new settings defaults"
        );
        assert_eq!(launch.effort, None);
    });
}

#[test]
fn inbox_review_legacy_backspace_edits_filter_and_control_l_switches_archived() {
    with_default_config(|| {
        let mut app = fixture();
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
        let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
            panic!();
        };
        assert!(!view.archived);
        assert_eq!(view.list.query.as_str(), "");
        key(&mut app, KeyCode::Char('l'), KeyModifiers::CONTROL);
        let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
            panic!();
        };
        assert!(view.archived);
    });
}

#[test]
fn inbox_outcome_details_keyboard_mouse_save_and_cancel_parity() {
    with_default_config(|| {
        let mut keys = fixture();
        let mut mouse = fixture();
        assert_no_launch(&key(&mut keys, KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_no_launch(&click_action(&mut mouse, Action::Details));
        for app in [&mut keys, &mut mouse] {
            inbox::paste(app, "Approved criteria");
            key(app, KeyCode::Tab, KeyModifiers::NONE);
            inbox::paste(app, "Approved constraints");
            key(app, KeyCode::Tab, KeyModifiers::NONE);
            inbox::paste(app, "cargo test: descriptive plan");
            key(app, KeyCode::Tab, KeyModifiers::NONE);
            inbox::paste(app, "Manual review result");
        }
        key(&mut keys, KeyCode::Char('o'), KeyModifiers::CONTROL);
        click_action(
            &mut mouse,
            Action::Outcome(nebula_core::InboxOutcome::Accepted),
        );
        let key_save = key(&mut keys, KeyCode::Char('s'), KeyModifiers::CONTROL);
        let mouse_save = click_action(&mut mouse, Action::SaveDetails);
        for out in [&key_save, &mouse_save] {
            assert_no_launch(out);
        }
        let extract = |out: &[ClientRequest]| {
            out.iter()
                .find_map(|r| match r {
                    ClientRequest::SaveInboxReview {
                        review,
                        expected_revision,
                        ..
                    } => Some((review.clone(), *expected_revision)),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(extract(&key_save), extract(&mouse_save));
        let (review, revision) = extract(&key_save);
        assert_eq!(revision, 0);
        assert_eq!(review.outcome, nebula_core::InboxOutcome::Accepted);
        assert_eq!(review.done_when, "Approved criteria");
        assert_eq!(review.constraints, "Approved constraints");
        assert_eq!(review.verification_plan, "cargo test: descriptive plan");
        assert_eq!(review.note, "Manual review result");
        // Cancellation has no launch effect, including a submitted save.
        assert_no_launch(&key(&mut keys, KeyCode::Esc, KeyModifiers::NONE));
        assert_no_launch(&click_action(&mut mouse, Action::CancelDetails));
        for app in [&keys, &mouse] {
            let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
                panic!();
            };
            assert!(view.details.is_none());
            assert_eq!(
                app.inbox.items[0].original,
                "Original title\n\nOriginal body"
            );
        }
    });
}

#[test]
fn inbox_outcome_failed_save_preserves_edits_and_duplicate_retry_identity() {
    with_default_config(|| {
        let mut app = fixture();
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        inbox::paste(&mut app, "Reviewed criteria");
        let save = key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        let extract = |out: &[ClientRequest]| {
            out.iter()
                .find_map(|r| match r {
                    ClientRequest::SaveInboxReview {
                        req_id,
                        event_id,
                        review,
                        ..
                    } => Some((*req_id, event_id.clone(), review.clone())),
                    _ => None,
                })
                .unwrap()
        };
        let (req, event, review) = extract(&save);
        assert!(key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL).is_empty());
        inbox::paste(&mut app, "discarded while pending");
        handle_server_event(
            &mut app,
            ServerEvent::Error {
                req_id: Some(req),
                message: "save failed".into(),
            },
            &mut vec![],
        );
        let retry = key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        let (retry_req, retry_event, retry_review) = extract(&retry);
        assert_eq!(event, retry_event);
        assert_eq!(review, retry_review);
        handle_server_event(
            &mut app,
            ServerEvent::Error {
                req_id: Some(retry_req),
                message: "save failed".into(),
            },
            &mut vec![],
        );
        inbox::paste(&mut app, " edited");
        let edited = key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        let (edited_req, edited_event, edited_review) = extract(&edited);
        assert_ne!(event, edited_event);
        assert_eq!(edited_review.done_when, "Reviewed criteria edited");
        let mut out = vec![];
        handle_server_event(
            &mut app,
            ServerEvent::Ack {
                req_id: edited_req,
                created: None,
            },
            &mut out,
        );
        assert!(out
            .iter()
            .any(|r| matches!(r, ClientRequest::ListInbox { .. })));
        assert_no_launch(&out);
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        inbox::paste(&mut app, "Next unsaved edit");
        handle_server_event(
            &mut app,
            ServerEvent::Ack {
                req_id: edited_req,
                created: None,
            },
            &mut vec![],
        );
        let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
            panic!();
        };
        assert_eq!(
            view.details.as_ref().unwrap().inputs[0].as_str(),
            "Next unsaved edit"
        );
    });
}

#[test]
fn inbox_outcome_launch_uses_review_revision_and_session_status_never_accepts() {
    with_default_config(|| {
        let mut app = fixture();
        app.inbox.items[0].review_revision = 7;
        app.inbox.items[0].review.done_when = "User criteria".into();
        inbox::open(&mut app, &mut vec![]);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let Some(Overlay::Prompt(prompt)) = &app.modals.overlay else {
            panic!();
        };
        let PromptKind::QuickPrompt(launch) = &prompt.kind else {
            panic!();
        };
        assert_eq!(launch.inbox_review_revision, 7);
        // A later list update cannot substitute unseen criteria at submission.
        app.inbox.items[0].review_revision = 8;
        let out = key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(out.iter().any(
            |r| matches!(r, ClientRequest::LaunchInbox { draft, .. } if draft.review_revision == 7)
        ));
        assert_eq!(
            app.inbox.items[0].review.outcome,
            nebula_core::InboxOutcome::NeedsReview
        );
    });
}

#[test]
fn inbox_outcome_save_ack_refreshes_after_cancel_without_closing_a_new_editor() {
    with_default_config(|| {
        let mut app = fixture();
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        inbox::paste(&mut app, "Submitted criteria");
        let save = key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        let req_id = save
            .iter()
            .find_map(|r| match r {
                ClientRequest::SaveInboxReview { req_id, .. } => Some(*req_id),
                _ => None,
            })
            .unwrap();
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        inbox::paste(&mut app, "New unsaved criteria");
        let mut out = vec![];
        handle_server_event(
            &mut app,
            ServerEvent::Ack {
                req_id,
                created: None,
            },
            &mut out,
        );
        assert_eq!(
            out.iter()
                .filter(|r| matches!(r, ClientRequest::ListInbox { .. }))
                .count(),
            1
        );
        assert_no_launch(&out);
        assert!(app.inbox.review_saves.is_empty());
        let mut updated = app.inbox.items.clone();
        updated[0].review.done_when = "Submitted criteria".into();
        updated[0].review_revision = 1;
        inbox::land(&mut app, updated);
        let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
            panic!();
        };
        assert_eq!(
            view.details.as_ref().unwrap().inputs[0].as_str(),
            "New unsaved criteria"
        );
        assert_eq!(
            view.details.as_ref().unwrap().revision,
            0,
            "new form keeps its revision so a conflict cannot silently overwrite"
        );
        let mut duplicate = vec![];
        handle_server_event(
            &mut app,
            ServerEvent::Ack {
                req_id,
                created: None,
            },
            &mut duplicate,
        );
        assert!(duplicate.is_empty());
    });
}

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
        assert_no_launch(&key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_no_launch(&key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
        let Some(Overlay::Prompt(p)) = &app.modals.overlay else {
            panic!("box returns from project picker");
        };
        let PromptKind::QuickPrompt(launch) = &p.kind else {
            panic!();
        };
        assert_eq!(launch.inbox.as_ref(), Some(&id));
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
            assert_no_launch(&key(&mut app, KeyCode::Esc, KeyModifiers::NONE));
        });
    });
}

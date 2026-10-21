//! Manual task inbox. Keys and mouse only translate to `act`; review is
//! the existing Quick Prompt, and only its explicit submit can launch.
use crate::app::{App, Overlay, PendingIntent, PromptKind};
use crate::filter_list::{FilterList, FilterListKey};
use crate::quick_prompt::{ModalUnder, QuickLaunch, QuickTarget};
use crate::text_input::TextInput;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nebula_core::{ClientRequest, InboxDraft, InboxId, InboxItem, SessionRef};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
mod details;
pub use details::DetailsEditor;

pub(crate) const FOOTER: &str = "^A: capture  Enter: launch review  ^E: criteria/outcome  ^O: session  ^D: archive  ^L: archived  ^R: refresh  Esc: close";

#[derive(Debug, Default)]
pub struct InboxState {
    pub items: Vec<InboxItem>,
    pub listing: Option<(u64, Vec<InboxItem>)>,
    /// Submitted review saves can outlive their editor (Esc/navigation).
    pub review_saves: std::collections::HashSet<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Capture,
    Review,
    Session,
    Archive,
    Archived,
    Refresh,
    SaveCapture,
    CancelCapture,
    Details,
    SaveDetails,
    CancelDetails,
    Field(usize),
    Outcome(nebula_core::InboxOutcome),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboxView {
    pub list: FilterList<InboxItem>,
    pub area: Rect,
    pub body: Rect,
    pub editor: Rect,
    pub buttons: Vec<(Rect, Action)>,
    pub capture: Option<(InboxId, TextInput)>,
    /// Freeze submitted source until its capture RPC confirms or fails.
    pub capture_saving: Option<u64>,
    pub details: Option<DetailsEditor>,
    pub archived: bool,
    pub scroll: u16,
}

impl InboxView {
    fn new() -> Self {
        Self {
            list: FilterList::default(),
            area: Rect::default(),
            body: Rect::default(),
            editor: Rect::default(),
            buttons: vec![],
            capture: None,
            capture_saving: None,
            details: None,
            archived: false,
            scroll: 0,
        }
    }
    fn rebuild(&mut self, items: &[InboxItem]) {
        let selected = self.list.selected().map(|i| i.id.clone());
        self.list.set_items(
            items
                .iter()
                .filter(|i| i.archived == self.archived)
                .cloned()
                .collect(),
        );
        self.list.apply_filter(|i| i.original.clone());
        if let Some(id) = selected {
            if let Some(index) = self.list.items.iter().position(|i| i.id == id) {
                self.list.move_to_item(index);
            }
        }
    }
}

pub(crate) fn refresh(app: &mut App, out: &mut Vec<ClientRequest>) {
    let req_id = app.alloc_req_id(PendingIntent::None);
    app.inbox.listing = Some((req_id, Vec::new()));
    out.push(ClientRequest::ListInbox { req_id });
}
pub(crate) fn open(app: &mut App, out: &mut Vec<ClientRequest>) {
    reopen(app, InboxView::new());
    refresh(app, out);
}
pub(crate) fn reopen(app: &mut App, mut view: InboxView) {
    view.rebuild(&app.inbox.items);
    app.modals.overlay = Some(Overlay::Inbox(view));
    app.chrome.dirty = true;
}
pub(crate) fn land_chunk(app: &mut App, req_id: u64, items: Vec<InboxItem>, complete: bool) {
    if let Some((pending, rows)) = &mut app.inbox.listing {
        if *pending != req_id {
            return;
        }
        rows.extend(items);
        if complete {
            let (_, rows) = app.inbox.listing.take().expect("pending list");
            land(app, rows);
        }
    }
}
pub(crate) fn land(app: &mut App, items: Vec<InboxItem>) {
    app.inbox.items = items;
    if let Some(Overlay::Inbox(view)) = &mut app.modals.overlay {
        if view
            .capture
            .as_ref()
            .is_some_and(|(id, _)| app.inbox.items.iter().any(|i| &i.id == id))
        {
            view.capture = None;
            view.capture_saving = None;
        }
        view.rebuild(&app.inbox.items);
    }
    app.chrome.dirty = true;
}

pub(crate) fn capture(
    app: &mut App,
    original: String,
    source_url: Option<String>,
    out: &mut Vec<ClientRequest>,
) {
    let id = InboxId::generate();
    send_capture(app, id, original, source_url, out);
    app.chrome.flash = Some("captured in task inbox (⇧B)".into());
}
fn send_capture(
    app: &mut App,
    id: InboxId,
    original: String,
    source_url: Option<String>,
    out: &mut Vec<ClientRequest>,
) -> u64 {
    let req_id = app.alloc_req_id(PendingIntent::None);
    out.push(ClientRequest::CaptureInbox {
        req_id,
        id,
        original,
        source_url,
    });
    refresh(app, out);
    req_id
}

pub(crate) fn capture_succeeded(app: &mut App, req_id: u64, out: &mut Vec<ClientRequest>) {
    details::succeeded(app, req_id, out);
    if let Some(Overlay::Inbox(view)) = &mut app.modals.overlay {
        if view.capture_saving == Some(req_id) {
            // Ack proves the submitted source is durable. A subsequent
            // list failure must not leave the editor frozen indefinitely.
            view.capture = None;
            view.capture_saving = None;
            app.chrome.dirty = true;
        }
    }
}

pub(crate) fn capture_failed(app: &mut App, req_id: u64) {
    details::failed(app, req_id);
    if let Some(Overlay::Inbox(view)) = &mut app.modals.overlay {
        if view.capture_saving == Some(req_id) {
            view.capture_saving = None;
            app.chrome.dirty = true;
        }
    }
}

pub(crate) fn act(app: &mut App, action: Action, out: &mut Vec<ClientRequest>) {
    if details::act(app, action, out) {
        return;
    }
    let Some(Overlay::Inbox(view)) = &mut app.modals.overlay else {
        return;
    };
    match action {
        Action::Capture => {
            view.capture = Some((InboxId::generate(), TextInput::multiline()));
            view.capture_saving = None;
        }
        Action::CancelCapture => {
            view.capture = None;
            view.capture_saving = None;
        }
        Action::SaveCapture => {
            if view.capture_saving.is_some() {
                return;
            }
            if let Some((id, input)) = &view.capture {
                let (id, text) = (id.clone(), input.as_str().to_string());
                let req_id = send_capture(app, id, text, None, out);
                if let Some(Overlay::Inbox(view)) = &mut app.modals.overlay {
                    view.capture_saving = Some(req_id);
                }
            }
        }
        Action::Archived => {
            view.archived = !view.archived;
            view.rebuild(&app.inbox.items);
        }
        Action::Refresh => refresh(app, out),
        Action::Archive => {
            if let Some(item) = view.list.selected().cloned() {
                crate::event_loop::send(app, out, |req_id| ClientRequest::ArchiveInbox {
                    req_id,
                    id: item.id,
                    archived: !item.archived,
                });
                refresh(app, out);
            }
        }
        Action::Session => {
            if let Some(id) = view.list.selected().and_then(|i| i.session.clone()) {
                if !app.tree.agents.iter().any(|a| a.id == id) {
                    app.chrome.flash =
                        Some("linked session was deleted; this task will not relaunch".into());
                    return;
                }
                app.modals.overlay = None;
                crate::event_loop::open_session(app, SessionRef::Agent(id), out);
            }
        }
        Action::Review => review(app),
        Action::Details
        | Action::SaveDetails
        | Action::CancelDetails
        | Action::Field(_)
        | Action::Outcome(_) => unreachable!("details actions handled above"),
    }
    app.chrome.dirty = true;
}

fn review(app: &mut App) {
    let Some(Overlay::Inbox(view)) = &app.modals.overlay else {
        return;
    };
    let Some(item) = view.list.selected().cloned() else {
        return;
    };
    if item.session.is_some() {
        app.chrome.flash = Some("already launched · ^O revisits its session".into());
        return;
    }
    if item.archived {
        app.chrome.flash = Some("restore this task before reviewing".into());
        return;
    }
    let target = item
        .draft
        .as_ref()
        .map(|d| d.worktree.clone())
        .filter(|id| app.tree.worktrees.iter().any(|w| &w.id == id))
        .or_else(|| app.selected_worktree().map(|w| w.id.clone()))
        .or_else(|| app.tree.worktrees.first().map(|w| w.id.clone()));
    let Some(target) = target else {
        app.chrome.flash = Some("add a project/worktree before reviewing".into());
        return;
    };
    let cfg = crate::config::Config::load();
    let mut launch = match &item.draft {
        Some(d) => QuickLaunch::of_kind(
            QuickTarget::Worktree(target),
            d.kind,
            d.custom_harness.clone(),
            d.model.clone(),
            d.effort.clone(),
            &cfg,
        ),
        None => QuickLaunch::from_config(QuickTarget::Worktree(target), &cfg),
    };
    // Saved None means no CLI flag, not "resolve against today's settings".
    if let Some(draft) = &item.draft {
        launch.model = draft.model.clone();
        launch.effort = draft.effort.clone();
    }
    launch.inbox = Some(item.id);
    launch.inbox_review_revision = item.review_revision;
    launch.under = ModalUnder::of(app.modals.overlay.as_ref());
    let text = item
        .draft
        .as_ref()
        .map(|d| d.instructions.as_str())
        .unwrap_or(&item.original);
    // Bypass the unrelated in-memory quick draft: an inbox has its own durable text.
    crate::quick_prompt::reopen(app, launch, text);
}

/// Save is review only. Enter in this same box goes through QuickLaunch's
/// AgentLaunchDraft into LaunchInbox and the daemon's CreateAgent path.
pub(crate) fn save_review(app: &mut App, out: &mut Vec<ClientRequest>) -> bool {
    let Some(Overlay::Prompt(prompt)) = &app.modals.overlay else {
        return false;
    };
    let PromptKind::QuickPrompt(launch) = &prompt.kind else {
        return false;
    };
    let Some(id) = launch.inbox.clone() else {
        return false;
    };
    let QuickTarget::Worktree(worktree) = &launch.target else {
        app.chrome.flash = Some("inbox: choose an existing worktree (^T)".into());
        return true;
    };
    let text = prompt.input.as_str().to_string();
    let draft = InboxDraft {
        review_revision: launch.inbox_review_revision,
        instructions: launch.compose(&text),
        worktree: worktree.clone(),
        kind: launch.kind,
        custom_harness: launch.custom.clone(),
        model: launch.model.clone(),
        effort: launch.effort.clone(),
    };
    let kind = prompt.kind.clone();
    let under = launch.under.clone();
    crate::event_loop::send_with(
        app,
        out,
        PendingIntent::ReopenPromptOnError {
            kind,
            text,
            note: "inbox review saved; no session launched".into(),
        },
        |req_id| ClientRequest::SaveInbox { req_id, id, draft },
    );
    if let Some(under) = under {
        under.reopen(app);
    }
    refresh(app, out);
    true
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent, out: &mut Vec<ClientRequest>) {
    // Legacy terminals may send 0x08 for Backspace; crossterm reports
    // that byte as Ctrl+H. Keep deletion distinct from the ^L list toggle.
    let key = if matches!(key.code, KeyCode::Char('h' | 'H'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        KeyEvent {
            code: KeyCode::Backspace,
            modifiers: key.modifiers.difference(KeyModifiers::CONTROL),
            ..key
        }
    } else {
        key
    };
    if details::key(app, key, out) {
        return;
    }
    let Some(Overlay::Inbox(view)) = &mut app.modals.overlay else {
        return;
    };
    if view.capture_saving.is_some() {
        if key.code == KeyCode::Esc {
            act(app, Action::CancelCapture, out);
        }
        return;
    }
    if let Some((_, input)) = &mut view.capture {
        let action = match key.code {
            KeyCode::Esc => Some(Action::CancelCapture),
            KeyCode::Enter if !input.takes_newline(&key) => Some(Action::SaveCapture),
            _ => {
                input.handle_key(&key);
                None
            }
        };
        if let Some(action) = action {
            act(app, action, out);
        }
    } else {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let action = match key.code {
            KeyCode::Char('a') if ctrl => Some(Action::Capture),
            KeyCode::Char('d') if ctrl => Some(Action::Archive),
            KeyCode::Char('l') if ctrl => Some(Action::Archived),
            KeyCode::Char('r') if ctrl => Some(Action::Refresh),
            KeyCode::Char('o') if ctrl => Some(Action::Session),
            KeyCode::Char('e') if ctrl => Some(Action::Details),
            KeyCode::Enter => Some(Action::Review),
            KeyCode::PageDown => {
                view.scroll = view.scroll.saturating_add(8);
                None
            }
            KeyCode::PageUp => {
                view.scroll = view.scroll.saturating_sub(8);
                None
            }
            _ => {
                match view.list.handle_standard_key(&key, 8) {
                    FilterListKey::Close => app.modals.overlay = None,
                    FilterListKey::QueryChanged | FilterListKey::Cleared => {
                        view.list.apply_filter(|i| i.original.clone());
                        view.scroll = 0;
                    }
                    FilterListKey::Moved => view.scroll = 0,
                    _ => {}
                }
                None
            }
        };
        if let Some(action) = action {
            act(app, action, out);
        }
    }
    app.chrome.dirty = true;
}
pub(crate) fn paste(app: &mut App, text: &str) -> bool {
    if details::paste(app, text) {
        return true;
    }
    let Some(Overlay::Inbox(view)) = &mut app.modals.overlay else {
        return false;
    };
    if view.capture_saving.is_some() {
        return true;
    }
    if let Some((_, input)) = &mut view.capture {
        input.insert_str(text);
    } else {
        view.list.query.insert_str(text);
        view.list.apply_filter(|i| i.original.clone());
    }
    app.chrome.dirty = true;
    true
}
fn select(app: &mut App, row: i64) {
    if let Some(Overlay::Inbox(view)) = &mut app.modals.overlay {
        view.list.select(row);
        view.scroll = 0;
    }
}
pub(crate) fn handle_mouse(
    app: &mut App,
    mouse: MouseEvent,
    pos: Position,
    out: &mut Vec<ClientRequest>,
) {
    if details::mouse(app, mouse, pos, out) {
        return;
    }
    let Some(Overlay::Inbox(view)) = &mut app.modals.overlay else {
        return;
    };
    if view.capture_saving.is_some() {
        let cancel = mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && view
                .buttons
                .iter()
                .any(|(r, action)| *action == Action::CancelCapture && r.contains(pos));
        if cancel {
            act(app, Action::CancelCapture, out);
        }
        return;
    }
    let mut action = None;
    let mut row = None;
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            action = view
                .buttons
                .iter()
                .find(|(r, _)| r.contains(pos))
                .map(|(_, a)| *a);
            if action.is_none() {
                if let Some((_, input)) = &mut view.capture {
                    if view.editor.contains(pos) {
                        input.click(pos.y - view.editor.y, pos.x - view.editor.x);
                    }
                } else {
                    row = view.list.hit(pos).map(|r| r as i64);
                }
            }
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let step = if mouse.kind == MouseEventKind::ScrollDown {
                1
            } else {
                -1
            };
            if let Some((_, input)) = &mut view.capture {
                input.scroll_rows(step);
            } else if view.body.contains(pos) {
                view.scroll =
                    (i32::from(view.scroll) + step as i32 * 3).clamp(0, u16::MAX as i32) as u16;
            } else {
                row = Some(view.list.cursor as i64 + step as i64);
            }
        }
        _ => {}
    }
    if let Some(row) = row {
        select(app, row);
    }
    if let Some(action) = action {
        act(app, action, out);
    }
    app.chrome.dirty = true;
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, mut view: InboxView, dimmed: bool) {
    let th = app.chrome.theme;
    let area = crate::ui::centered_rect_pct(f.area(), 94, 90);
    let inner = crate::ui::render_modal_frame(
        f,
        area,
        if view.capture.is_some() {
            " Capture task · original text "
        } else {
            " Task inbox · ⇧B "
        },
        th,
    );
    view.area = area;
    view.buttons.clear();
    if view.details.is_some() {
        details::draw(f, app, &mut view, inner);
        if !dimmed {
            app.modals.overlay = Some(Overlay::Inbox(view));
        }
        return;
    }
    let parts = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);
    let buttons: Vec<(&str, Action)> = if view.capture.is_some() {
        vec![
            (" Save capture (Enter) ", Action::SaveCapture),
            (" Cancel (Esc) ", Action::CancelCapture),
        ]
    } else {
        vec![
            (" New ^A ", Action::Capture),
            (" Review Enter ", Action::Review),
            (" Details ^E ", Action::Details),
            (" Session ^O ", Action::Session),
            (" Archive/restore ^D ", Action::Archive),
            (" Archived ^L ", Action::Archived),
            (" Refresh ^R ", Action::Refresh),
        ]
    };
    let mut x = parts[0].x;
    for (label, action) in buttons {
        let width = (label.len() as u16).min(parts[0].right().saturating_sub(x));
        let rect = Rect::new(x, parts[0].y, width, parts[0].height);
        f.render_widget(
            Paragraph::new(label).style(Style::default().fg(if dimmed {
                th.dim
            } else {
                th.accent
            })),
            rect,
        );
        view.buttons.push((rect, action));
        x = x.saturating_add(width);
    }
    if let Some((_, input)) = &mut view.capture {
        f.render_widget(
            Paragraph::new(if view.capture_saving.is_some() {
                "Saving capture… · Esc closes"
            } else {
                "Enter saves only · ⇧Enter/^J newline · Esc cancels"
            }),
            parts[1],
        );
        view.editor = parts[2];
        let (text_view, _) = crate::ui::draw_multiline_input(f, input, view.editor, th);
        input.set_view(text_view);
    } else {
        let heading = format!(
            "{} · filter: {}",
            if view.archived { "Archived" } else { "Active" },
            view.list.query.as_str()
        );
        f.render_widget(Paragraph::new(heading), parts[1]);
        let panes = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(parts[2]);
        view.list.list_area = panes[0];
        view.body = panes[1];
        let scroll = view.list.sync_scroll(panes[0].height as usize);
        for (row, m) in view
            .list
            .matches
            .iter()
            .skip(scroll)
            .take(panes[0].height as usize)
            .enumerate()
        {
            let item = &view.list.items[m.item];
            let state = if item.session.is_some() {
                "↗ "
            } else if item.draft.is_some() {
                "✓ "
            } else {
                "○ "
            };
            let label = format!("{state}{}", item.original.lines().next().unwrap_or("task"));
            let rect = Rect::new(panes[0].x, panes[0].y + row as u16, panes[0].width, 1);
            crate::ui::render_row(
                f,
                rect,
                vec![Span::raw(crate::ui::fit_to_width(
                    &label,
                    rect.width.saturating_sub(2) as usize,
                ))],
                scroll + row == view.list.cursor,
                !dimmed,
                th,
            );
        }
        if view.list.is_empty() {
            crate::ui::empty_list_row(f, panes[0], "No tasks · ^A captures one", th);
        }
        if let Some(item) = view.list.selected() {
            let status = if item.session.is_some() {
                "Launched · task stays open until you archive it"
            } else if item.draft.is_some() {
                "Reviewed · no session launched"
            } else {
                "Captured · no session launched"
            };
            let mut lines = vec![
                Line::from(status),
                Line::from(item.source_url.clone().unwrap_or_default()),
                Line::from(""),
            ];
            details::summary(&mut lines, item);
            if let Some(id) = &item.session {
                lines.push(Line::from(format!("Session: {id} · ^O revisits")));
            }
            lines.push(Line::styled(
                "Original (read only)",
                Style::default().fg(th.accent),
            ));
            lines.extend(item.original.lines().map(|s| Line::from(s.to_string())));
            lines.push(Line::from(""));
            lines.push(Line::styled(
                "Approved instructions",
                Style::default().fg(th.accent),
            ));
            match &item.draft {
                Some(d) => {
                    let worktree = app.tree.worktrees.iter().find(|w| w.id == d.worktree);
                    let target = worktree
                        .map(|w| {
                            format!(
                                "{} · {}",
                                app.tree
                                    .projects
                                    .iter()
                                    .find(|p| p.id == w.project_id)
                                    .map(|p| p.name.as_str())
                                    .unwrap_or("project"),
                                w.branch
                            )
                        })
                        .unwrap_or_else(|| "worktree removed".into());
                    lines.push(Line::from(format!(
                        "{target} · {}",
                        d.custom_harness.as_deref().unwrap_or(d.kind.as_str())
                    )));
                    lines.extend(d.instructions.lines().map(|s| Line::from(s.to_string())));
                }
                None => lines.push(Line::from("Not approved yet · Enter opens review")),
            }
            let wrapped: Vec<Line> = lines
                .into_iter()
                .flat_map(|line| {
                    crate::pr_preview::wrap(&line.to_string(), panes[1].width as usize)
                        .into_iter()
                        .map(move |s| Line::styled(s, line.style))
                })
                .collect();
            let max_scroll = wrapped
                .len()
                .saturating_sub(panes[1].height as usize)
                .min(u16::MAX as usize) as u16;
            view.scroll = view.scroll.min(max_scroll);
            f.render_widget(Paragraph::new(wrapped).scroll((view.scroll, 0)), panes[1]);
        }
    }
    if !dimmed {
        app.modals.overlay = Some(Overlay::Inbox(view));
    }
}

//! User review form on the existing inbox surface. It never launches a session.
use super::*;
use nebula_core::{InboxOutcome, InboxReview};

const LABELS: [&str; 4] = [
    "Done when",
    "Constraints",
    "Verification plan (descriptive only)",
    "Review note / results",
];

#[derive(Debug, Clone, PartialEq)]
pub struct DetailsEditor {
    pub id: InboxId,
    pub revision: i64,
    pub inputs: [TextInput; 4],
    pub focus: usize,
    pub outcome: InboxOutcome,
    pub saving: Option<u64>,
    // An uncertain retry uses the same id and payload; an edit gets a fresh id.
    attempted: Option<(String, InboxReview)>,
}
impl DetailsEditor {
    fn new(item: &InboxItem) -> Self {
        let r = &item.review;
        let inputs = [&r.done_when, &r.constraints, &r.verification_plan, &r.note].map(|text| {
            let mut input = TextInput::multiline();
            input.insert_str(text);
            input
        });
        Self {
            id: item.id.clone(),
            revision: item.review_revision,
            inputs,
            focus: 0,
            outcome: r.outcome,
            saving: None,
            attempted: None,
        }
    }
    fn review(&self) -> InboxReview {
        InboxReview {
            done_when: self.inputs[0].as_str().into(),
            constraints: self.inputs[1].as_str().into(),
            verification_plan: self.inputs[2].as_str().into(),
            note: self.inputs[3].as_str().into(),
            outcome: self.outcome,
        }
    }
}

pub(super) fn act(app: &mut App, action: Action, out: &mut Vec<ClientRequest>) -> bool {
    let Some(Overlay::Inbox(view)) = &mut app.modals.overlay else {
        return false;
    };
    if !matches!(
        action,
        Action::Details
            | Action::SaveDetails
            | Action::CancelDetails
            | Action::Field(_)
            | Action::Outcome(_)
    ) {
        return false;
    }
    match action {
        Action::Details => {
            if let Some(item) = view.list.selected() {
                view.details = Some(DetailsEditor::new(item));
            }
        }
        Action::CancelDetails => view.details = None,
        Action::SaveDetails => {
            let Some(editor) = &mut view.details else {
                return true;
            };
            if editor.saving.is_some() {
                return true;
            }
            let review = editor.review();
            let event_id = match &editor.attempted {
                Some((id, old)) if old == &review => id.clone(),
                _ => InboxId::generate().0,
            };
            editor.attempted = Some((event_id.clone(), review.clone()));
            let (id, expected_revision) = (editor.id.clone(), editor.revision);
            let req_id = app.alloc_req_id(PendingIntent::None);
            app.inbox.review_saves.insert(req_id);
            out.push(ClientRequest::SaveInboxReview {
                req_id,
                id,
                event_id,
                expected_revision,
                review,
            });
            if let Some(Overlay::Inbox(view)) = &mut app.modals.overlay {
                view.details.as_mut().expect("details editor").saving = Some(req_id);
            }
        }
        Action::Field(index) => {
            if let Some(editor) = &mut view.details {
                if editor.saving.is_none() {
                    editor.focus = index.min(3);
                }
            }
        }
        Action::Outcome(outcome) => {
            if let Some(editor) = &mut view.details {
                if editor.saving.is_none() {
                    editor.outcome = outcome;
                }
            }
        }
        _ => unreachable!(),
    }
    app.chrome.dirty = true;
    true
}

pub(super) fn succeeded(app: &mut App, req_id: u64, out: &mut Vec<ClientRequest>) {
    if !app.inbox.review_saves.remove(&req_id) {
        return;
    }
    if let Some(Overlay::Inbox(view)) = &mut app.modals.overlay {
        if view
            .details
            .as_ref()
            .is_some_and(|e| e.saving == Some(req_id))
        {
            view.details = None;
        }
    }
    app.chrome.flash = Some("task details saved; no session launched".into());
    refresh(app, out);
    app.chrome.dirty = true;
}
pub(super) fn failed(app: &mut App, req_id: u64) {
    app.inbox.review_saves.remove(&req_id);
    if let Some(Overlay::Inbox(view)) = &mut app.modals.overlay {
        if let Some(editor) = &mut view.details {
            if editor.saving == Some(req_id) {
                editor.saving = None;
                app.chrome.dirty = true;
            }
        }
    }
}
pub(super) fn key(app: &mut App, key: KeyEvent, out: &mut Vec<ClientRequest>) -> bool {
    let Some(Overlay::Inbox(view)) = &mut app.modals.overlay else {
        return false;
    };
    let Some(editor) = &mut view.details else {
        return false;
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let action = if key.code == KeyCode::Esc {
        Some(Action::CancelDetails)
    } else if editor.saving.is_some() {
        None
    } else {
        match key.code {
            KeyCode::Char('s') if ctrl => Some(Action::SaveDetails),
            KeyCode::Tab => Some(Action::Field((editor.focus + 1) % 4)),
            KeyCode::BackTab => Some(Action::Field((editor.focus + 3) % 4)),
            KeyCode::Char('o') if ctrl => {
                let index = InboxOutcome::ALL
                    .iter()
                    .position(|o| o == &editor.outcome)
                    .unwrap();
                Some(Action::Outcome(InboxOutcome::ALL[(index + 1) % 4]))
            }
            _ => {
                editor.inputs[editor.focus].handle_key(&key);
                None
            }
        }
    };
    if let Some(action) = action {
        act(app, action, out);
    }
    app.chrome.dirty = true;
    true
}
pub(super) fn paste(app: &mut App, text: &str) -> bool {
    let Some(Overlay::Inbox(view)) = &mut app.modals.overlay else {
        return false;
    };
    let Some(editor) = &mut view.details else {
        return false;
    };
    if editor.saving.is_none() {
        editor.inputs[editor.focus].insert_str(text);
    }
    app.chrome.dirty = true;
    true
}
pub(super) fn mouse(
    app: &mut App,
    mouse: MouseEvent,
    pos: Position,
    out: &mut Vec<ClientRequest>,
) -> bool {
    let Some(Overlay::Inbox(view)) = &mut app.modals.overlay else {
        return false;
    };
    let Some(editor) = &mut view.details else {
        return false;
    };
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        if let Some((_, action)) = view.buttons.iter().find(|(r, _)| r.contains(pos)) {
            let action = *action;
            if editor.saving.is_none() || action == Action::CancelDetails {
                act(app, action, out);
            }
        } else if editor.saving.is_none() && view.editor.contains(pos) {
            editor.inputs[editor.focus].click(pos.y - view.editor.y, pos.x - view.editor.x);
        }
    } else if editor.saving.is_none() {
        let step = match mouse.kind {
            MouseEventKind::ScrollDown => 1,
            MouseEventKind::ScrollUp => -1,
            _ => 0,
        };
        editor.inputs[editor.focus].scroll_rows(step);
    }
    app.chrome.dirty = true;
    true
}

pub(super) fn draw(f: &mut Frame, app: &App, view: &mut InboxView, inner: Rect) {
    let th = app.chrome.theme;
    let editor = view.details.as_mut().expect("details");
    let parts = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .split(inner);
    let mut button = |f: &mut Frame, rect: Rect, text: String, action: Action| {
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(th.accent)),
            rect,
        );
        view.buttons.push((rect, action));
    };
    button(
        f,
        Rect::new(parts[0].x, parts[0].y, 18.min(parts[0].width), 1),
        " Save details ^S ".into(),
        Action::SaveDetails,
    );
    button(
        f,
        Rect::new(
            parts[0].x.saturating_add(18),
            parts[0].y,
            parts[0].width.saturating_sub(18),
            1,
        ),
        " Close Esc ".into(),
        Action::CancelDetails,
    );
    for (i, label) in LABELS.iter().enumerate() {
        let first = editor.inputs[i]
            .as_str()
            .lines()
            .next()
            .unwrap_or("(empty)");
        let rect = Rect::new(parts[1].x, parts[1].y + i as u16, parts[1].width, 1);
        button(
            f,
            rect,
            format!(
                "{} {label}: {first}",
                if editor.focus == i { "›" } else { " " }
            ),
            Action::Field(i),
        );
    }
    f.render_widget(
        Paragraph::new(format!(
            "Outcome (explicit user decision) · {} · ^O cycles",
            editor.outcome.label()
        )),
        parts[2],
    );
    let cells = Layout::horizontal([Constraint::Percentage(25); 4]).split(parts[3]);
    for (i, outcome) in InboxOutcome::ALL.iter().enumerate() {
        button(
            f,
            cells[i],
            format!(
                "{}{}",
                if editor.outcome == *outcome {
                    "✓ "
                } else {
                    ""
                },
                outcome.label()
            ),
            Action::Outcome(*outcome),
        );
    }
    f.render_widget(Paragraph::new(if editor.saving.is_some() { "Saving… · Esc closes; submitted save may still complete".into() }
        else { format!("Editing {} · Tab/⇧Tab field · Enter newline · ^S saves only\nRevision {} · verification text never executes automatically", LABELS[editor.focus], editor.revision) }), parts[4]);
    view.editor = parts[5];
    let (text_view, _) =
        crate::ui::draw_multiline_input(f, &editor.inputs[editor.focus], view.editor, th);
    editor.inputs[editor.focus].set_view(text_view);
}

pub(super) fn summary(lines: &mut Vec<Line<'static>>, item: &InboxItem) {
    lines.push(Line::from(format!(
        "Outcome: {} · revision {} · ^E edits criteria/results",
        item.review.outcome.label(),
        item.review_revision
    )));
    for (label, text) in [
        ("Done when (user reviewed)", &item.review.done_when),
        ("Constraints (user reviewed)", &item.review.constraints),
        (
            "Verification plan (descriptive only)",
            &item.review.verification_plan,
        ),
        ("Review note / results", &item.review.note),
    ] {
        lines.push(Line::from(format!("{label}:")));
        lines.extend(text.lines().map(|s| Line::from(s.to_string())));
    }
    if let Some(launch) = &item.launch {
        lines.push(Line::from(format!(
            "Launched {} · criteria revision {} · {} · model {} · effort {}",
            when(launch.launched_at),
            launch.review_revision,
            launch
                .custom_harness
                .as_deref()
                .unwrap_or(launch.kind.as_str()),
            launch.model.as_deref().unwrap_or("CLI default / unknown"),
            launch.effort.as_deref().unwrap_or("CLI default / unknown")
        )));
        lines.push(Line::from("Launch prompt (snapshot):"));
        lines.extend(launch.prompt.lines().map(|s| Line::from(s.to_string())));
    }
    if !item.review_history.is_empty() {
        lines.push(Line::from(
            "Review history (latest 20; full audit retained in SQLite):",
        ));
    }
    for event in &item.review_history {
        lines.push(Line::from(format!(
            "v{} · {} · {} · event {}",
            event.revision,
            when(event.saved_at),
            event.review.outcome.label(),
            event.event_id
        )));
        for (label, text) in [
            ("Done when", &event.review.done_when),
            ("Constraints", &event.review.constraints),
            ("Verification", &event.review.verification_plan),
            ("Note", &event.review.note),
        ] {
            lines.push(Line::from(format!("{label}: {text}")));
        }
    }
    lines.push(Line::from(""));
}

/// A stored epoch-ms stamp as the age the rest of the TUI shows: "3h ago".
fn when(at_ms: i64) -> String {
    match crate::hosts::ago_label(crate::app::now_ms() - at_ms) {
        s if at_ms <= 0 || s.is_empty() => "at an unknown time".into(),
        s => s,
    }
}

#[cfg(test)]
mod tests {
    use super::when;

    #[test]
    fn stamps_read_as_ages_never_raw_milliseconds() {
        let now = crate::app::now_ms();
        assert_eq!(when(now), "just now");
        assert_eq!(when(now - 3 * 3_600_000), "3h ago");
        assert_eq!(when(0), "at an unknown time");
        assert_eq!(when(now + 60_000), "at an unknown time");
    }
}

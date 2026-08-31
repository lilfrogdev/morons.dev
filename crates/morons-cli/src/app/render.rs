use morons_protocol::{
    OpenCodeModelRetention, OpenCodeModelTrainingUse, OpenCodeService, RunState,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use super::{AppState, PendingOperation, PresentedModel, SessionView, View};
use crate::terminal::SafeText;

pub(super) fn render(frame: &mut Frame<'_>, app: &AppState) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(frame, layout[0], app);
    match app.view {
        View::Sessions => render_sessions(frame, layout[1], app),
        View::Session => render_session(frame, layout[1], app),
    }
    render_footer(frame, layout[2], app);
    if app.confirm_stop {
        render_stop_confirmation(frame, area);
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let credential = match app.credential {
        Some(status) if status.configured => "credential configured",
        Some(_) => "credential not configured",
        None => "credential status loading",
    };
    let line = Line::from(vec![
        Span::styled(" morons ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("connected · server "),
        Span::raw(app.server_version.first_line()),
        Span::raw(" · "),
        Span::raw(credential),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_sessions(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(area);

    let items: Vec<ListItem<'_>> = app
        .sessions
        .iter()
        .map(|session| ListItem::new(Line::from(session.display_name.first_line())))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Sessions "))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut state = ListState::default();
    if !app.sessions.is_empty() {
        state.select(Some(app.selected_session));
    }
    frame.render_stateful_widget(list, columns[0], &mut state);
    render_models(frame, columns[1], app);
}

fn render_models(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let items: Vec<ListItem<'_>> = app
        .models
        .iter()
        .map(|model| {
            let service = service_label(model.model.service);
            let availability = if model.model.available {
                ""
            } else {
                " unavailable"
            };
            ListItem::new(Line::from(vec![
                Span::raw(service),
                Span::raw(" · "),
                Span::raw(model.display_name.first_line()),
                Span::raw(" · "),
                Span::raw(model.id.first_line()),
                Span::styled(availability, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Models "))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut state = ListState::default();
    state.select(app.selected_model);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_session(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);
    if let Some(session) = app.session.as_ref() {
        render_transcript(frame, sections[0], session, app.transcript_scroll);
    } else {
        frame.render_widget(
            Paragraph::new("Session state is unavailable")
                .block(Block::default().borders(Borders::ALL).title(" Session ")),
            sections[0],
        );
    }
    render_model_disclosure(frame, sections[1], app.selected_model());
    let prompt_title = if app.pending == Some(PendingOperation::SubmitInput) {
        " Message · submitting "
    } else {
        " Message · Enter submit · Shift+Enter newline "
    };
    let prompt = SafeText::from_untrusted(app.prompt.as_str());
    frame.render_widget(
        Paragraph::new(prompt.as_str())
            .block(Block::default().borders(Borders::ALL).title(prompt_title))
            .wrap(Wrap { trim: false }),
        sections[2],
    );
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, session: &SessionView, scroll: u16) {
    let mut lines = Vec::new();
    for entry in &session.entries {
        let role_style = if entry.role == "You" {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if entry.refusal {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        };
        lines.push(Line::from(Span::styled(entry.role, role_style)));
        extend_safe_lines(&mut lines, &entry.text);
        lines.push(Line::default());
    }
    if let Some(transient) = session.transient.as_ref() {
        let label = if transient.refusal {
            "Assistant refusal · streaming"
        } else {
            "Assistant · streaming"
        };
        lines.push(Line::from(Span::styled(
            label,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        )));
        extend_safe_lines(&mut lines, &transient.presented);
        if transient.truncated || transient.presented.was_truncated() {
            lines.push(Line::from(Span::styled(
                "Transient display limit reached",
                Style::default().fg(Color::Yellow),
            )));
        }
    }
    let visible_height = usize::from(area.height.saturating_sub(2));
    let maximum_scroll =
        u16::try_from(lines.len().saturating_sub(visible_height)).unwrap_or(u16::MAX);
    let scroll = maximum_scroll.saturating_sub(scroll.min(maximum_scroll));
    let run_status = active_run_label(session);
    let title = format!(" {} · {run_status} ", session.display_name.first_line());
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

fn render_model_disclosure(frame: &mut Frame<'_>, area: Rect, model: Option<&PresentedModel>) {
    let line = match model {
        Some(model) => Line::from(vec![
            Span::styled("Model ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(service_label(model.model.service)),
            Span::raw(" · "),
            Span::raw(model.display_name.first_line()),
            Span::raw(" · training: "),
            Span::raw(training_label(model.model.training_use)),
            Span::raw(" · retention: "),
            Span::raw(retention_label(model.model.retention)),
        ]),
        None => Line::from("No reviewed model is currently available"),
    };
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let help = match app.view {
        View::Sessions => "↑↓ select · Enter open · n new · Tab model · Ctrl+S stop · q detach",
        View::Session => "Esc sessions · Tab model · Ctrl+X cancel run · Ctrl+S stop",
    };
    let status = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::raw(app.status.first_line()),
    ]);
    frame.render_widget(
        Paragraph::new(vec![Line::from(help), status]).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_stop_confirmation(frame: &mut Frame<'_>, area: Rect) {
    let width = area.width.min(56);
    let height = area.height.min(5);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new("Stop the local server and interrupt active runs?\n\ny stop · n cancel")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Confirm server stop "),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn extend_safe_lines<'a>(lines: &mut Vec<Line<'a>>, text: &'a SafeText) {
    if text.as_str().is_empty() {
        lines.push(Line::default());
        return;
    }
    lines.extend(text.as_str().split('\n').map(Line::from));
    if text.was_truncated() {
        lines.push(Line::from(Span::styled(
            "Display limit reached",
            Style::default().fg(Color::Yellow),
        )));
    }
}

fn active_run_label(session: &SessionView) -> &'static str {
    let Some(active_run_id) = session.active_run_id else {
        return "idle";
    };
    session
        .runs
        .iter()
        .find(|run| run.id == active_run_id)
        .map_or("run unavailable", |run| match run.state {
            RunState::Accepted => "run accepted",
            RunState::Active if run.cancellation_requested => "cancelling run",
            RunState::Active => "run active",
            RunState::Succeeded
            | RunState::Failed
            | RunState::Cancelled
            | RunState::Interrupted => "idle",
        })
}

const fn service_label(service: OpenCodeService) -> &'static str {
    match service {
        OpenCodeService::Zen => "Zen",
        OpenCodeService::Go => "Go",
    }
}

const fn training_label(training: OpenCodeModelTrainingUse) -> &'static str {
    match training {
        OpenCodeModelTrainingUse::NotUsed => "not used",
    }
}

const fn retention_label(retention: OpenCodeModelRetention) -> &'static str {
    match retention {
        OpenCodeModelRetention::None => "none",
        OpenCodeModelRetention::UpToThirtyDays => "up to 30 days",
    }
}

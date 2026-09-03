use std::collections::HashMap;

use morons_protocol::{OpenCodeModelRetention, OpenCodeModelTrainingUse, RunId, RunState};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use super::{
    AppState, CredentialDialog, InformationDialog, PendingOperation, PresentedModel, SessionView,
    View, service_label, terminal_run_presentation,
};
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
    if app.confirm_delete.is_some() {
        render_delete_confirmation(frame, area);
    }
    if let Some(dialog) = app.credential_dialog.as_ref() {
        render_credential_dialog(frame, area, dialog);
    }
    if app.model_dialog.is_some() {
        render_model_dialog(frame, area, app);
    }
    if let Some(dialog) = app.information_dialog {
        render_information_dialog(frame, area, dialog);
    }
    if let Some(input) = app.rename_dialog.as_ref() {
        render_rename_dialog(frame, area, input.as_str());
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let credential = match app.credential {
        Some(status) if status.configured => {
            format!("credential configured · generation {}", status.generation)
        }
        Some(status) => format!(
            "credential not configured · generation {}",
            status.generation
        ),
        None => "credential status loading".to_owned(),
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
        .map(|session| {
            ListItem::new(vec![
                Line::from(vec![
                    Span::raw(session.display_name.first_line()),
                    Span::styled(
                        if session.summary.archived {
                            " · archived"
                        } else {
                            ""
                        },
                        Style::default().fg(Color::DarkGray),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(
                        session.working_directory.first_line(),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        if session.shared_directory {
                            " · shared directory · race risk"
                        } else {
                            ""
                        },
                        Style::default().fg(Color::Yellow),
                    ),
                ]),
            ])
        })
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
    let completion = app.skill_completion();
    let completion_height = completion.as_ref().map_or(0, |(skills, _)| {
        u16::try_from(skills.len().min(5) + 2).unwrap_or(7)
    });
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(completion_height),
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
    render_model_disclosure(
        frame,
        sections[1],
        app.selected_model(),
        app.session
            .as_ref()
            .and_then(|session| session.context_status.as_ref()),
    );
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
    if let Some((skills, selected)) = completion {
        let window_start = selected.saturating_sub(4);
        let items = skills
            .iter()
            .skip(window_start)
            .take(5)
            .map(|skill| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("@{}", skill.safe_name.first_line()),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" · "),
                    Span::raw(skill.description.first_line()),
                    Span::styled(
                        format!(" · {}", skill_source_label(skill.source)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Skills · ↑↓ choose · Tab complete "),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› ");
        let mut state =
            ListState::default().with_selected(Some(selected.saturating_sub(window_start)));
        frame.render_stateful_widget(list, sections[3], &mut state);
    }
}

const fn skill_source_label(source: morons_protocol::SkillSource) -> &'static str {
    match source {
        morons_protocol::SkillSource::Bundled => "bundled",
        morons_protocol::SkillSource::User => "user",
        morons_protocol::SkillSource::Project => "project",
    }
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, session: &SessionView, scroll: u16) {
    let last_entry_by_run = session
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.run_id.map(|run_id| (run_id, index)))
        .collect::<HashMap<RunId, usize>>();
    let terminal_run_by_last_entry = session
        .runs
        .iter()
        .filter(|run| terminal_run_presentation(run).is_some())
        .filter_map(|run| {
            last_entry_by_run
                .get(&run.id)
                .copied()
                .map(|index| (index, run))
        })
        .collect::<HashMap<usize, _>>();
    let mut lines = Vec::new();
    for (index, entry) in session.entries.iter().enumerate() {
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
        if let Some(run) = terminal_run_by_last_entry.get(&index) {
            extend_terminal_run_outcome(&mut lines, run);
        }
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
    let run_status = active_work_label(session);
    let shared = if session.shared_directory {
        " · shared directory race risk"
    } else {
        ""
    };
    let archived = if session.summary.archived {
        " · archived · history only"
    } else {
        ""
    };
    let title = format!(
        " {} · {run_status}{archived}{shared} ",
        session.display_name.first_line()
    );
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

fn extend_terminal_run_outcome(lines: &mut Vec<Line<'_>>, run: &morons_protocol::RunSummary) {
    let Some(presentation) = terminal_run_presentation(run) else {
        return;
    };
    let model_id = SafeText::from_untrusted(&run.model_id);
    let heading_style = if run.state == RunState::Failed {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    };
    lines.push(Line::from(vec![
        Span::styled(presentation.heading, heading_style),
        Span::raw(format!(
            " · {} · {}",
            service_label(run.service),
            model_id.first_line()
        )),
    ]));
    lines.push(Line::from(presentation.detail));
    lines.push(Line::default());
}

fn render_model_disclosure(
    frame: &mut Frame<'_>,
    area: Rect,
    model: Option<&PresentedModel>,
    context: Option<&morons_protocol::SessionContextStatus>,
) {
    let line = match model {
        Some(model) => {
            let context = context
                .filter(|context| {
                    context.service == model.model.service && context.model_id == model.model.id
                })
                .map_or_else(String::new, |context| {
                    let percent = u64::from(context.estimated_input_tokens).saturating_mul(100)
                        / u64::from(context.maximum_input_tokens);
                    format!(
                        " · context: ~{} / {} ({percent}%)",
                        context.estimated_input_tokens, context.maximum_input_tokens
                    )
                });
            Line::from(vec![
                Span::styled("Model ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(service_label(model.model.service)),
                Span::raw(" · "),
                Span::raw(model.display_name.first_line()),
                Span::raw(" · training: "),
                Span::raw(training_label(model.model.training_use)),
                Span::raw(" · retention: "),
                Span::raw(retention_label(model.model.retention)),
                Span::raw(context),
            ])
        }
        None => Line::from("No reviewed model is currently available"),
    };
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let help = match app.view {
        View::Sessions => {
            "↑↓ select · Enter open · n new · r rename · a archive · d delete archived · Ctrl+K credential · Ctrl+S stop · q detach"
        }
        View::Session => {
            "Enter send · @ skill · !/!! command · /model · /context · /compact · Esc sessions · Ctrl+X cancel"
        }
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

fn render_model_dialog(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let Some(dialog) = app.model_dialog.as_ref() else {
        return;
    };
    let width = area.width.min(88);
    let height = area.height.min(18);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(popup);
    frame.render_widget(
        Paragraph::new(
            SafeText::from_untrusted(dialog.query.as_str())
                .as_str()
                .to_owned(),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Model search "),
        ),
        sections[0],
    );

    let matches = app.model_dialog_matches();
    let selected = dialog.selected.min(matches.len().saturating_sub(1));
    let visible_rows = usize::from(sections[1].height.saturating_sub(2)).max(1);
    let start = selected
        .saturating_sub(visible_rows / 2)
        .min(matches.len().saturating_sub(visible_rows));
    let items = matches
        .iter()
        .skip(start)
        .take(visible_rows)
        .filter_map(|index| app.models.get(*index))
        .map(|model| {
            let current = if app.selected_model().is_some_and(|selected| {
                selected.model.service == model.model.service && selected.model.id == model.model.id
            }) {
                " ✓"
            } else {
                ""
            };
            ListItem::new(Line::from(vec![
                Span::raw(service_label(model.model.service)),
                Span::raw(" · "),
                Span::raw(model.id.first_line()),
                Span::raw(" · "),
                Span::raw(model.display_name.first_line()),
                Span::styled(current, Style::default().fg(Color::Green)),
            ]))
        })
        .collect::<Vec<_>>();
    let list = if items.is_empty() {
        List::new(vec![ListItem::new("No matching available models")])
    } else {
        List::new(items)
    }
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Models · type to filter · ↑↓ choose · Enter save · Esc cancel "),
    )
    .highlight_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("› ");
    let mut state = ListState::default();
    if !matches.is_empty() {
        state.select(Some(selected.saturating_sub(start)));
    }
    frame.render_stateful_widget(list, sections[1], &mut state);

    let selected_model = matches
        .get(selected)
        .and_then(|index| app.models.get(*index));
    render_model_disclosure(frame, sections[2], selected_model, None);
}

fn render_rename_dialog(frame: &mut Frame<'_>, area: Rect, input: &str) {
    let width = area.width.min(68);
    let height = area.height.min(5);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(SafeText::from_untrusted(input).as_str().to_owned())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Rename session · Enter save · Esc cancel "),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn render_information_dialog(frame: &mut Frame<'_>, area: Rect, dialog: InformationDialog) {
    let (title, message, width, height) = match dialog {
        InformationDialog::TrustNotice => (
            " Trusted-local authority ",
            "Morons is not a sandbox. The model can read, change, delete, or disclose anything available to your user account through file, Bash, Python, network, Git, credentials, and subagents. Parallel subagents share the selected directory and may race. Cancellation cannot undo completed effects.\n\nFor containment, run the complete Morons application inside a container, VM, or restricted OS account.\n\nEnter acknowledge · q/Esc exit",
            78,
            14,
        ),
        InformationDialog::Help => (
            " Help and safety ",
            "Trusted-local: tools and task subagents use your normal user authority; there are no approval prompts or rollback. Parallel subagents share the selected directory and may race. Wrap the complete app externally when containment is required.\n\nEnter send · Shift+Enter newline · @ skill · ! command in context · !! command excluded from model context · /model [search] select global default · /context inspect · /compact [instructions] summarize · Tab complete skill · r rename · a archive/unarchive · d delete archived in browser · Ctrl+X cancel · Ctrl+K credential · Ctrl+L refresh · Ctrl+S stop server · Esc sessions · q detach from browser\n\nEnter/Esc/? close",
            88,
            15,
        ),
    };
    let popup = Rect {
        x: area.x + area.width.saturating_sub(area.width.min(width)) / 2,
        y: area.y + area.height.saturating_sub(area.height.min(height)) / 2,
        width: area.width.min(width),
        height: area.height.min(height),
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(message)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn render_delete_confirmation(frame: &mut Frame<'_>, area: Rect) {
    let width = area.width.min(72);
    let height = area.height.min(7);
    let dialog = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, dialog);
    frame.render_widget(
        Paragraph::new(
            "Delete this archived session's Morons-owned history and attachments?\nThe selected working directory will not be modified.\n\ny delete · n/Esc cancel",
        )
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Delete archived session "),
        ),
        dialog,
    );
}

fn render_credential_dialog(frame: &mut Frame<'_>, area: Rect, dialog: &CredentialDialog) {
    let (height, title, message) = match dialog {
        CredentialDialog::ChooseAction => (
            7,
            " OpenCode credential ",
            "The credential is configured.\n\nr replace · d remove · Esc cancel",
        ),
        CredentialDialog::Enter {
            replacing: true, ..
        } => (
            7,
            " Replace OpenCode credential ",
            "Enter the replacement API key. Input is hidden.\n\nEnter save · Backspace edit · Esc clear",
        ),
        CredentialDialog::Enter {
            replacing: false, ..
        } => (
            7,
            " Configure OpenCode credential ",
            "Enter the API key. Input is hidden.\n\nEnter save · Backspace edit · Esc clear",
        ),
        CredentialDialog::ConfirmRemove => (
            7,
            " Remove OpenCode credential ",
            "Remove the stored credential? Dispatched requests cannot be retracted.\n\ny remove · n cancel",
        ),
    };
    let width = area.width.min(68);
    let height = area.height.min(height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(message)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false }),
        popup,
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

fn active_work_label(session: &SessionView) -> &'static str {
    let Some(active_run_id) = session.active_run_id else {
        return if session.active_command_id.is_some() {
            "command active"
        } else {
            "idle"
        };
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
            RunState::Uncertain => "local effect uncertain",
        })
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

use openagent_bridge_server_client::session_id_from_payload;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use serde_json::Value;

use crate::{
    ChoicePickerKind, InteractionFocus, TuiState,
    config::timeline_style,
    interaction::{preview_lines, question_items, question_option_values},
    picker::{
        agent_picker_label, model_picker_label, session_picker_detail_lines, session_picker_label,
        session_picker_status_line,
    },
    session::{SESSION_PICKER_ACTIONS, SessionPickerMode},
    util::{IfEmptyThen, compact_json, string_field},
};

pub(crate) fn draw_terminal_frame(frame: &mut ratatui::Frame<'_>, title: &str, state: &TuiState) {
    let area = frame.area();
    let has_interaction = state.active_interaction_focus().is_some();
    let has_file_picker = state.file_picker.is_some();
    let has_session_picker = state.session_picker.is_some();
    let has_model_picker = state.model_picker.is_some();
    let has_agent_picker = state.agent_picker.is_some();
    let has_choice_picker = state.choice_picker.is_some();
    let mut constraints = vec![Constraint::Length(3), Constraint::Min(5)];
    if has_interaction
        || has_file_picker
        || has_session_picker
        || has_model_picker
        || has_agent_picker
        || has_choice_picker
    {
        constraints.push(Constraint::Length(9));
    }
    constraints.push(Constraint::Length(3));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  status: {}", state.status)),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Bridge"));
    frame.render_widget(header, chunks[0]);

    let visible = state.timeline.iter().rev().take(200).rev().map(|line| {
        let style = timeline_style(&state.config, line.kind.as_str());
        ListItem::new(Line::from(vec![
            Span::styled(
                format!("[{}] ", line.kind),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::raw(line.text.clone()),
        ]))
    });
    let timeline = List::new(visible.collect::<Vec<_>>())
        .block(Block::default().borders(Borders::ALL).title("Timeline"));
    frame.render_widget(timeline, chunks[1]);

    let prompt_index = if has_interaction {
        draw_interaction_dock(frame, chunks[2], state);
        3
    } else if has_choice_picker {
        draw_choice_picker_dock(frame, chunks[2], state);
        3
    } else if has_agent_picker {
        draw_agent_picker_dock(frame, chunks[2], state);
        3
    } else if has_model_picker {
        draw_model_picker_dock(frame, chunks[2], state);
        3
    } else if has_session_picker {
        draw_session_picker_dock(frame, chunks[2], state);
        3
    } else if has_file_picker {
        draw_file_picker_dock(frame, chunks[2], state);
        3
    } else {
        2
    };
    let input = Paragraph::new(state.input_buffer.as_str())
        .block(Block::default().borders(Borders::ALL).title("Prompt"))
        .wrap(Wrap { trim: false });
    frame.render_widget(input, chunks[prompt_index]);
}

fn draw_choice_picker_dock(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    state: &TuiState,
) {
    let title = state
        .choice_picker
        .as_ref()
        .map(|picker| picker.kind.title())
        .unwrap_or("Choices");
    let lines = choice_picker_dock_lines(state);
    let dock = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(dock, area);
}

fn choice_picker_dock_lines(state: &TuiState) -> Vec<Line<'static>> {
    let Some(picker) = state.choice_picker.as_ref() else {
        return Vec::new();
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("Query ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(if picker.query.is_empty() {
            "(all)".to_string()
        } else {
            picker.query.clone()
        }),
        Span::styled(
            "  Type to filter, Enter select, Esc close",
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    if picker.candidates.is_empty() {
        lines.push(Line::from(format!(
            "No matching {}",
            picker.kind.item_label()
        )));
        return lines;
    }
    for (index, choice) in picker.candidates.iter().enumerate().take(6) {
        let marker = if picker.selected == index { ">" } else { " " };
        let suffix = if (picker.kind == ChoicePickerKind::Theme && state.config.theme == *choice)
            || (picker.kind == ChoicePickerKind::ThemeScheme
                && state.config.color_scheme == *choice)
        {
            "  current"
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} {}. ", index + 1),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(format!("{choice}{suffix}")),
        ]));
    }
    lines
}

fn draw_agent_picker_dock(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    state: &TuiState,
) {
    let lines = agent_picker_dock_lines(state);
    let dock = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Agents")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(dock, area);
}

fn agent_picker_dock_lines(state: &TuiState) -> Vec<Line<'static>> {
    let Some(picker) = state.agent_picker.as_ref() else {
        return Vec::new();
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("Query ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(if picker.query.is_empty() {
            "(all)".to_string()
        } else {
            picker.query.clone()
        }),
        Span::styled(
            "  Type to filter, Enter select, Esc close",
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    if picker.candidates.is_empty() {
        lines.push(Line::from("No matching agents"));
        return lines;
    }
    for (index, agent) in picker.candidates.iter().enumerate().take(6) {
        let marker = if picker.selected == index { ">" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} {}. ", index + 1),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(agent_picker_label(agent)),
        ]));
    }
    lines
}

fn draw_model_picker_dock(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    state: &TuiState,
) {
    let lines = model_picker_dock_lines(state);
    let dock = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Models")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(dock, area);
}

fn model_picker_dock_lines(state: &TuiState) -> Vec<Line<'static>> {
    let Some(picker) = state.model_picker.as_ref() else {
        return Vec::new();
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("Query ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(if picker.query.is_empty() {
            "(all)".to_string()
        } else {
            picker.query.clone()
        }),
        Span::styled(
            "  Type to filter, Enter select, Esc close",
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    if picker.candidates.is_empty() {
        lines.push(Line::from("No matching models"));
        return lines;
    }
    for (index, model) in picker.candidates.iter().enumerate().take(6) {
        let marker = if picker.selected == index { ">" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} {}. ", index + 1),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(model_picker_label(model)),
        ]));
    }
    lines
}

fn draw_session_picker_dock(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    state: &TuiState,
) {
    let lines = session_picker_dock_lines(state);
    let dock = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Sessions")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(dock, area);
}

fn session_picker_dock_lines(state: &TuiState) -> Vec<Line<'static>> {
    let Some(picker) = state.session_picker.as_ref() else {
        return Vec::new();
    };
    let mode_hint = match &picker.mode {
        SessionPickerMode::Browse => "Type filter, Enter resume, Right actions, Ctrl-D details",
        SessionPickerMode::Actions => "Up/Down choose, Enter run, Left/Esc back",
        SessionPickerMode::Details => "Left/Esc back, Right actions",
        SessionPickerMode::Rename => "Edit title, Enter save, Esc cancel",
        SessionPickerMode::Confirm(_) => "Enter/y confirm, Esc/n cancel",
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("Query ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(if picker.query.is_empty() {
            "(recent)".to_string()
        } else {
            picker.query.clone()
        }),
        Span::styled(
            format!("  {mode_hint}"),
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    if picker.candidates.is_empty() {
        lines.push(Line::from("No matching sessions"));
        return lines;
    }
    for (index, session) in picker.candidates.iter().enumerate().take(4) {
        let marker = if picker.selected == index { ">" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} {}. ", index + 1),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(session_picker_label(session)),
        ]));
    }
    if let Some(session) = picker.candidates.get(picker.selected) {
        lines.push(Line::from(vec![
            Span::styled("Selected ", Style::default().fg(Color::Cyan)),
            Span::raw(session_picker_status_line(session)),
        ]));
    }
    match &picker.mode {
        SessionPickerMode::Browse => {}
        SessionPickerMode::Details => {
            if let Some(session) = picker.candidates.get(picker.selected) {
                lines.extend(session_picker_detail_lines(session));
            }
        }
        SessionPickerMode::Actions => {
            lines.push(Line::from(vec![
                Span::styled("Actions ", Style::default().fg(Color::Cyan)),
                Span::raw("OpenCode-style session management"),
            ]));
            let selected = picker
                .action_selected
                .min(SESSION_PICKER_ACTIONS.len().saturating_sub(1));
            let start = selected.saturating_sub(2);
            let end = (start + 5).min(SESSION_PICKER_ACTIONS.len());
            for (index, action) in SESSION_PICKER_ACTIONS[start..end].iter().enumerate() {
                let actual = start + index;
                let marker = if actual == selected { ">" } else { " " };
                let confirm = if action.requires_confirmation() {
                    " confirm"
                } else {
                    ""
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker} "), Style::default().fg(Color::Yellow)),
                    Span::raw(action.label()),
                    Span::styled(confirm, Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
        SessionPickerMode::Rename => {
            lines.push(Line::from(vec![
                Span::styled("Rename ", Style::default().fg(Color::Cyan)),
                Span::raw(if picker.rename_buffer.is_empty() {
                    "(empty)".to_string()
                } else {
                    picker.rename_buffer.clone()
                }),
            ]));
        }
        SessionPickerMode::Confirm(action) => {
            let session_id = picker
                .candidates
                .get(picker.selected)
                .and_then(session_id_from_payload)
                .unwrap_or_else(|| "<unknown>".to_string());
            lines.push(Line::from(vec![
                Span::styled("Confirm ", Style::default().fg(Color::Red)),
                Span::raw(format!("{} {session_id}?", action.label())),
            ]));
        }
    }
    lines
}

fn draw_file_picker_dock(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    state: &TuiState,
) {
    let lines = file_picker_dock_lines(state);
    let dock = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Composer: Files")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(dock, area);
}

fn file_picker_dock_lines(state: &TuiState) -> Vec<Line<'static>> {
    let Some(picker) = state.file_picker.as_ref() else {
        return Vec::new();
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("Query ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(if picker.query.is_empty() {
            "(all)".to_string()
        } else {
            picker.query.clone()
        }),
        Span::styled(
            "  Type to filter, Enter attach, Esc close",
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    if picker.candidates.is_empty() {
        lines.push(Line::from("No matching files"));
        return lines;
    }
    for (index, candidate) in picker.candidates.iter().enumerate().take(6) {
        let marker = if picker.selected == index { ">" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} {}. ", index + 1),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(candidate.reference.clone()),
            Span::styled(
                format!("  {}", candidate.kind),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    lines
}

fn draw_interaction_dock(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    state: &TuiState,
) {
    let (title, lines) = match state.active_interaction_focus() {
        Some(InteractionFocus::Approval) => ("Interaction: Approval", approval_dock_lines(state)),
        Some(InteractionFocus::Question) => ("Interaction: Question", question_dock_lines(state)),
        None => return,
    };
    let dock = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(dock, area);
}

fn approval_dock_lines(state: &TuiState) -> Vec<Line<'static>> {
    let Some(approval) = state.active_approval.as_ref() else {
        return Vec::new();
    };
    let tool = string_field(approval, "tool_name").if_empty_then(|| "tool".to_string());
    let input = approval
        .get("tool_input")
        .map(compact_json)
        .unwrap_or_else(|| "{}".to_string());
    let mut lines = vec![Line::from(vec![
        Span::styled("Tool ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("{tool} {input}")),
    ])];
    if let Some(preview) = approval.get("preview").filter(|value| value.is_object()) {
        for line in preview_lines(preview).into_iter().take(2) {
            lines.push(Line::from(Span::raw(line)));
        }
    }
    let options = ["Allow once", "Always allow", "Deny"];
    for (index, option) in options.iter().enumerate() {
        let marker = if state.interaction.selected == index {
            ">"
        } else {
            " "
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} {}. ", index + 1),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(*option),
        ]));
    }
    lines.push(Line::from(Span::styled(
        "Enter selects, 1-3 quick-pick, Esc denies",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

fn question_dock_lines(state: &TuiState) -> Vec<Line<'static>> {
    let Some(question) = state.active_question.as_ref() else {
        return Vec::new();
    };
    let questions = question_items(question);
    if questions.is_empty() {
        return vec![Line::from(
            "No question details. Use /answer or Esc to dismiss.",
        )];
    }
    let index = state.interaction.question_index.min(questions.len() - 1);
    let item = &questions[index];
    let header = item
        .get("header")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("Question");
    let text = item
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} {}/{} ", header, index + 1, questions.len()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(text.to_string()),
        ]),
        Line::from(Span::styled(
            "Up/Down choose, Enter submit, type custom answer, Esc dismiss",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let options = question_option_values(item);
    if options.is_empty() {
        lines.push(Line::from("No options. Type an answer, then Enter."));
    } else {
        for (option_index, option) in options.iter().enumerate().take(5) {
            let marker = if state.interaction.selected == option_index {
                ">"
            } else {
                " "
            };
            let label = option
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let description = option
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let suffix = if description.is_empty() {
                String::new()
            } else {
                format!(" - {description}")
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{marker} {}. ", option_index + 1),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(format!("{label}{suffix}")),
            ]));
        }
    }
    if !state.interaction.custom_answer.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Custom: ", Style::default().fg(Color::Yellow)),
            Span::raw(state.interaction.custom_answer.clone()),
        ]));
    }
    lines
}

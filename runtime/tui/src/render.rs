use openagent_bridge_server_client::session_id_from_payload;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
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
    if area.width == 0 || area.height == 0 {
        return;
    }
    let has_interaction = state.active_interaction_focus().is_some();
    let has_file_picker = state.file_picker.is_some();
    let has_session_picker = state.session_picker.is_some();
    let has_model_picker = state.model_picker.is_some();
    let has_agent_picker = state.agent_picker.is_some();
    let has_choice_picker = state.choice_picker.is_some();
    let compact_layout = area.width < 36 || area.height < 9;
    let input_lines = state.input_buffer.lines().count().max(1) as u16;
    let header_height: u16 = if compact_layout { 1 } else { 3 };
    let input_height = if compact_layout {
        1
    } else {
        input_lines.saturating_add(2).clamp(3, 8)
    };
    let wants_dock = has_interaction
        || has_file_picker
        || has_session_picker
        || has_model_picker
        || has_agent_picker
        || has_choice_picker;
    let show_dock =
        wants_dock && area.height >= header_height.saturating_add(input_height).saturating_add(5);
    let mut constraints = vec![Constraint::Length(header_height), Constraint::Min(1)];
    if show_dock {
        constraints.push(Constraint::Length(9));
    }
    constraints.push(Constraint::Length(input_height));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let header_block = if compact_layout {
        Block::default()
    } else {
        Block::default().borders(Borders::ALL).title("Bridge")
    };
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  status: {}", state.status)),
    ]))
    .block(header_block);
    frame.render_widget(header, chunks[0]);

    let timeline_block = if compact_layout {
        Block::default()
    } else {
        Block::default().borders(Borders::ALL).title("Timeline")
    };
    let timeline_border_height = if compact_layout { 0 } else { 2 };
    let timeline_border_width = if compact_layout { 0 } else { 2 };
    let timeline_width = chunks[1].width.saturating_sub(timeline_border_width).max(1);
    let timeline_height = chunks[1].height.saturating_sub(timeline_border_height);
    let timeline_lines = timeline_render_lines(state);
    let rendered_height = timeline_lines
        .iter()
        .map(|line| {
            let width = line.width().max(1) as u16;
            width.div_ceil(timeline_width)
        })
        .fold(0_u16, u16::saturating_add);
    let timeline_text = Text::from(timeline_lines);
    let mut timeline = Paragraph::new(timeline_text)
        .block(timeline_block)
        .wrap(Wrap { trim: false });
    timeline = timeline.scroll((rendered_height.saturating_sub(timeline_height), 0));
    frame.render_widget(timeline, chunks[1]);

    let prompt_index = if show_dock && has_interaction {
        draw_interaction_dock(frame, chunks[2], state);
        3
    } else if show_dock && has_choice_picker {
        draw_choice_picker_dock(frame, chunks[2], state);
        3
    } else if show_dock && has_agent_picker {
        draw_agent_picker_dock(frame, chunks[2], state);
        3
    } else if show_dock && has_model_picker {
        draw_model_picker_dock(frame, chunks[2], state);
        3
    } else if show_dock && has_session_picker {
        draw_session_picker_dock(frame, chunks[2], state);
        3
    } else if show_dock && has_file_picker {
        draw_file_picker_dock(frame, chunks[2], state);
        3
    } else {
        2
    };
    let prompt_title = if state.input_buffer.trim_start().starts_with('!') {
        "Bash Mode · Shift+Enter/Ctrl+J newline"
    } else {
        "Prompt · Shift+Enter/Ctrl+J newline"
    };
    let input_block = if compact_layout {
        Block::default()
    } else {
        Block::default().borders(Borders::ALL).title(prompt_title)
    };
    let input = Paragraph::new(state.input_buffer.as_str())
        .block(input_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(input, chunks[prompt_index]);
}

fn timeline_render_lines(state: &TuiState) -> Vec<Line<'static>> {
    let mut entries = state
        .timeline
        .iter()
        .rev()
        .take(200)
        .rev()
        .collect::<Vec<_>>();
    if state.compact_output_expanded
        && let Some(compact) = state.hidden_compact_output.as_ref()
    {
        entries.push(compact);
    }
    let mut output = Vec::new();
    for entry in entries {
        let style = timeline_style(&state.config, entry.kind.as_str());
        let mut rendered = markdown_lines(&entry.text, style);
        if rendered.is_empty() {
            rendered.push(Line::default());
        }
        for (index, line) in rendered.into_iter().enumerate() {
            let mut spans = Vec::new();
            if index == 0 {
                spans.push(Span::styled(
                    format!("[{}] ", entry.kind),
                    style.add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::raw("  "));
            }
            spans.extend(line.spans);
            output.push(Line::from(spans));
        }
    }
    output
}

fn markdown_lines(text: &str, base: Style) -> Vec<Line<'static>> {
    let mut output = Vec::new();
    let mut code_fence = false;
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if let Some(language) = trimmed.strip_prefix("```") {
            code_fence = !code_fence;
            output.push(Line::from(Span::styled(
                if code_fence {
                    format!("┌─ {}", language.trim())
                } else {
                    "└─".to_string()
                },
                Style::default().fg(Color::DarkGray),
            )));
        } else if code_fence {
            output.push(Line::from(Span::styled(
                format!("│ {raw}"),
                Style::default().fg(Color::Yellow),
            )));
        } else if let Some(heading) = trimmed.strip_prefix("### ") {
            output.push(Line::from(Span::styled(
                heading.to_string(),
                base.add_modifier(Modifier::BOLD),
            )));
        } else if let Some(heading) = trimmed
            .strip_prefix("## ")
            .or_else(|| trimmed.strip_prefix("# "))
        {
            output.push(Line::from(Span::styled(
                heading.to_string(),
                base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let mut spans = vec![Span::styled("• ", base.add_modifier(Modifier::BOLD))];
            spans.extend(inline_markdown_spans(item, base));
            output.push(Line::from(spans));
        } else if let Some(quote) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled("│ ", Style::default().fg(Color::Cyan))];
            spans.extend(inline_markdown_spans(quote, base));
            output.push(Line::from(spans));
        } else if matches!(trimmed, "---" | "***" | "___") {
            output.push(Line::from(Span::styled(
                "────────────────────",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            output.push(Line::from(inline_markdown_spans(raw, base)));
        }
    }
    output
}

fn inline_markdown_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut output = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let bold = rest.find("**");
        let code = rest.find('`');
        let next = match (bold, code) {
            (Some(left), Some(right)) => left.min(right),
            (Some(index), None) | (None, Some(index)) => index,
            (None, None) => {
                output.push(Span::styled(rest.to_string(), base));
                break;
            }
        };
        if next > 0 {
            output.push(Span::styled(rest[..next].to_string(), base));
            rest = &rest[next..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                output.push(Span::styled(
                    after[..end].to_string(),
                    base.add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 2..];
            } else {
                output.push(Span::styled(rest.to_string(), base));
                break;
            }
        } else if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                output.push(Span::styled(
                    after[..end].to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 1..];
            } else {
                output.push(Span::styled(rest.to_string(), base));
                break;
            }
        }
    }
    output
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

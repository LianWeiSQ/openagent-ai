use std::{io, time::Duration};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use serde_json::{Value, json};

use crate::{
    ComposerFileCandidate, TimelineLine, TuiState,
    agent::{handle_agent_picker_key, open_agent_picker_from_handler},
    attachments::{handle_file_picker_key, open_file_picker_from_handler},
    commands::{
        agent_picker_command_query, choice_picker_command_kind, file_picker_command_query,
        handle_choice_picker_key, is_local_state_command, model_picker_command_query,
        open_choice_picker_from_command, session_picker_command_query,
    },
    config::keybind_matches,
    provider::{handle_model_picker_key, open_model_picker_from_handler},
    render::draw_terminal_frame,
    session::{handle_session_picker_key, open_session_picker_from_handler},
    util::compact_json,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalUiOptions {
    pub title: String,
    pub status: String,
}

impl Default for TerminalUiOptions {
    fn default() -> Self {
        Self {
            title: "OpenAgent".to_string(),
            status: "ready".to_string(),
        }
    }
}

pub trait TerminalEventHandler {
    fn initial_lines(&mut self) -> Vec<TimelineLine> {
        Vec::new()
    }

    fn poll_bridge_events(&mut self) -> Result<Vec<Value>, String> {
        Ok(Vec::new())
    }

    fn poll_control_request(&mut self) -> Result<Option<Value>, String> {
        Ok(None)
    }

    fn record_control_response(&mut self, _payload: &Value) -> Result<(), String> {
        Ok(())
    }

    fn drain_bridge_events(&mut self) -> Vec<Value> {
        Vec::new()
    }

    fn search_files(&mut self, _query: &str) -> Result<Vec<ComposerFileCandidate>, String> {
        Ok(Vec::new())
    }

    fn search_sessions(&mut self, _query: &str) -> Result<Vec<Value>, String> {
        Ok(Vec::new())
    }

    fn list_models(&mut self) -> Result<Value, String> {
        Ok(json!({"models": []}))
    }

    fn list_agents(&mut self) -> Result<Value, String> {
        Ok(json!({"agents": []}))
    }

    fn handle_submit(&mut self, prompt: &str) -> Result<Vec<TimelineLine>, String>;

    fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String>;

    fn handle_approval_response(&mut self, _payload: &Value) -> Result<Vec<TimelineLine>, String> {
        Ok(Vec::new())
    }

    fn handle_question_response(&mut self, _payload: &Value) -> Result<Vec<TimelineLine>, String> {
        Ok(Vec::new())
    }
}

pub fn run_terminal_ui<H: TerminalEventHandler>(
    options: TerminalUiOptions,
    mut handler: H,
) -> Result<(), String> {
    enable_raw_mode().map_err(|error| error.to_string())?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|error| {
        let _ = disable_raw_mode();
        error.to_string()
    })?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|error| {
        let _ = disable_raw_mode();
        error.to_string()
    })?;
    let result = terminal_ui_loop(&mut terminal, options, &mut handler);
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
    result
}

fn terminal_ui_loop<H: TerminalEventHandler>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    options: TerminalUiOptions,
    handler: &mut H,
) -> Result<(), String> {
    let mut state = TuiState::new();
    state.status = options.status;
    state.timeline.extend(handler.initial_lines());
    state.timeline.push(TimelineLine::new(
        "status",
        "Type a prompt, /sessions, /resume <id>, /skills, /new, /fork, /interrupt, or /exit.",
        true,
    ));
    apply_handler_output(&mut state, handler, Vec::new());
    loop {
        match handler.poll_control_request() {
            Ok(Some(request)) => handle_remote_control_request(&mut state, handler, &request),
            Ok(None) => {}
            Err(error) => {
                state
                    .timeline
                    .push(TimelineLine::new("warning", error, true));
                state.status = "control poll failed".to_string();
            }
        }
        match handler.poll_bridge_events() {
            Ok(events) => apply_bridge_event_values(&mut state, events),
            Err(error) => {
                if state.status != "remote poll failed" {
                    state
                        .timeline
                        .push(TimelineLine::new("warning", error, true));
                }
                state.status = "remote poll failed".to_string();
            }
        }
        terminal
            .draw(|frame| draw_terminal_frame(frame, &options.title, &state))
            .map_err(|error| error.to_string())?;
        if !event::poll(Duration::from_millis(250)).map_err(|error| error.to_string())? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(|error| error.to_string())? else {
            continue;
        };
        if handle_key_event(key, &mut state, handler)? {
            break;
        }
    }
    Ok(())
}

pub(crate) fn handle_key_event<H: TerminalEventHandler>(
    key: KeyEvent,
    state: &mut TuiState,
    handler: &mut H,
) -> Result<bool, String> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(true);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
        if state.hidden_compact_output.is_some() {
            state.compact_output_expanded = !state.compact_output_expanded;
            state.status = if state.compact_output_expanded {
                "compact summary expanded".to_string()
            } else {
                "compact summary hidden".to_string()
            };
        }
        return Ok(false);
    }
    let approval_start = state.approval_responses.len();
    let question_start = state.question_responses.len();
    if state.handle_interaction_key(&key) {
        dispatch_new_interaction_responses(state, handler, approval_start, question_start)?;
        return Ok(false);
    }
    if state.choice_picker.is_some() {
        handle_choice_picker_key(key, state, handler)?;
        return Ok(false);
    }
    if state.agent_picker.is_some() {
        handle_agent_picker_key(key, state, handler)?;
        return Ok(false);
    }
    if state.model_picker.is_some() {
        handle_model_picker_key(key, state, handler)?;
        return Ok(false);
    }
    if state.session_picker.is_some() {
        handle_session_picker_key(key, state, handler)?;
        return Ok(false);
    }
    if state.file_picker.is_some() {
        handle_file_picker_key(key, state, handler)?;
        return Ok(false);
    }
    if keybind_matches(&state.config, &key, "stash", "ctrl+s") {
        state.stash_current_input();
        return Ok(false);
    }
    if keybind_matches(&state.config, &key, "unstash", "ctrl+y") {
        state.restore_latest_stash();
        return Ok(false);
    }
    if keybind_matches(&state.config, &key, "editor", "ctrl+e") {
        if let Err(error) = state.edit_input_with_external_editor() {
            state.timeline.push(TimelineLine::new("error", error, true));
        }
        return Ok(false);
    }
    if (key.code == KeyCode::Enter
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT))
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('j'))
    {
        state.input_buffer.push('\n');
        state.status = if state.input_buffer.trim_start().starts_with('!') {
            "Bash Mode multiline".to_string()
        } else {
            "multiline prompt".to_string()
        };
        return Ok(false);
    }
    match key.code {
        KeyCode::Esc => return Ok(true),
        KeyCode::Up => {
            state.history_previous();
        }
        KeyCode::Down => {
            state.history_next();
        }
        KeyCode::Backspace => {
            state.input_buffer.pop();
        }
        KeyCode::Char('@')
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !state.input_buffer.trim_start().starts_with('/') =>
        {
            if let Err(error) = open_file_picker_from_handler(state, handler, "") {
                state.input_buffer.push('@');
                state
                    .timeline
                    .push(TimelineLine::new("warning", error, true));
                state.status = "file picker failed".to_string();
            }
        }
        KeyCode::Char(value) => {
            state.input_buffer.push(value);
        }
        KeyCode::Enter => {
            let submitted = state.input_buffer.trim().to_string();
            state.input_buffer.clear();
            if matches!(submitted.as_str(), "/exit" | "/quit") {
                return Ok(true);
            }
            if submitted.is_empty() {
                return Ok(false);
            }
            state.remember_history(&submitted);
            if submitted.starts_with('/') {
                if let Some(kind) = choice_picker_command_kind(&submitted) {
                    open_choice_picker_from_command(state, handler, kind)?;
                    return Ok(false);
                }
                if handle_local_state_command(&submitted, state, handler)? {
                    return Ok(false);
                }
                if agent_picker_command_query(&submitted).is_some() {
                    open_agent_picker_from_handler(state, handler, "")?;
                    return Ok(false);
                }
                if model_picker_command_query(&submitted).is_some() {
                    open_model_picker_from_handler(state, handler, "")?;
                    return Ok(false);
                }
                if let Some(query) = session_picker_command_query(&submitted) {
                    open_session_picker_from_handler(state, handler, query)?;
                    return Ok(false);
                }
                if let Some(query) = file_picker_command_query(&submitted) {
                    open_file_picker_from_handler(state, handler, query)?;
                    return Ok(false);
                }
                match handler.handle_command(&submitted) {
                    Ok(lines) => {
                        state.status = "command completed".to_string();
                        apply_handler_output(state, handler, lines);
                    }
                    Err(error) => {
                        state.status = "command failed".to_string();
                        state.timeline.push(TimelineLine::new("error", error, true));
                    }
                }
            } else {
                state
                    .timeline
                    .push(TimelineLine::new("user", format!("> {submitted}"), true));
                state.status = "running".to_string();
                match handler.handle_submit(&submitted) {
                    Ok(lines) => {
                        state.status = "idle".to_string();
                        apply_handler_output(state, handler, lines);
                    }
                    Err(error) => {
                        state.status = "turn failed".to_string();
                        state.timeline.push(TimelineLine::new("error", error, true));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(false)
}

pub(crate) fn handle_local_state_command<H: TerminalEventHandler>(
    submitted: &str,
    state: &mut TuiState,
    handler: &mut H,
) -> Result<bool, String> {
    if !is_local_state_command(submitted) {
        return Ok(false);
    }
    let approval_start = state.approval_responses.len();
    let question_start = state.question_responses.len();
    state.input_buffer = submitted.to_string();
    let remote_prompt = state.submit();
    if remote_prompt {
        return Ok(false);
    }
    let approval_responses: Vec<Value> = state.approval_responses[approval_start..].to_vec();
    let question_responses: Vec<Value> = state.question_responses[question_start..].to_vec();
    for response in approval_responses {
        let lines = handler.handle_approval_response(&response)?;
        apply_handler_output(state, handler, lines);
    }
    for response in question_responses {
        let lines = handler.handle_question_response(&response)?;
        apply_handler_output(state, handler, lines);
    }
    Ok(true)
}

fn dispatch_new_interaction_responses<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    approval_start: usize,
    question_start: usize,
) -> Result<(), String> {
    let approval_responses: Vec<Value> = state.approval_responses[approval_start..].to_vec();
    let question_responses: Vec<Value> = state.question_responses[question_start..].to_vec();
    for response in approval_responses {
        let lines = handler.handle_approval_response(&response)?;
        apply_handler_output(state, handler, lines);
    }
    for response in question_responses {
        let lines = handler.handle_question_response(&response)?;
        apply_handler_output(state, handler, lines);
    }
    Ok(())
}

pub(crate) fn apply_handler_output<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    lines: Vec<TimelineLine>,
) {
    for line in lines {
        if line.kind == "compact" {
            state.hidden_compact_output = Some(line);
            state.compact_output_expanded = false;
            state.timeline.push(TimelineLine::new(
                "status",
                "context compacted · Ctrl+R to view summary",
                false,
            ));
        } else {
            state.timeline.push(line);
        }
    }
    apply_bridge_event_values(state, handler.drain_bridge_events());
}

pub(crate) fn apply_bridge_event_values(state: &mut TuiState, events: Vec<Value>) {
    for event in events {
        let result = state.apply_bridge_event(&event);
        if !result
            .get("applied")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let method = event
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("event");
            state.timeline.push(TimelineLine::new(
                "status",
                format!(
                    "{method}: {}",
                    compact_json(&event.get("params").cloned().unwrap_or(Value::Null))
                ),
                false,
            ));
        }
    }
}

pub(crate) fn handle_remote_control_request<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    request: &Value,
) {
    let approval_start = state.approval_responses.len();
    let question_start = state.question_responses.len();
    let result = state.apply_control_request(request);
    let approval_responses: Vec<Value> = state.approval_responses[approval_start..].to_vec();
    let question_responses: Vec<Value> = state.question_responses[question_start..].to_vec();
    for response in approval_responses {
        match handler.handle_approval_response(&response) {
            Ok(lines) => apply_handler_output(state, handler, lines),
            Err(error) => state.timeline.push(TimelineLine::new("error", error, true)),
        }
    }
    for response in question_responses {
        match handler.handle_question_response(&response) {
            Ok(lines) => apply_handler_output(state, handler, lines),
            Err(error) => state.timeline.push(TimelineLine::new("error", error, true)),
        }
    }
    if let Some(command) = result
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        if let Some(kind) = choice_picker_command_kind(command) {
            if let Err(error) = open_choice_picker_from_command(state, handler, kind) {
                state.timeline.push(TimelineLine::new("error", error, true));
            }
        } else if agent_picker_command_query(command).is_some() {
            if let Err(error) = open_agent_picker_from_handler(state, handler, "") {
                state.timeline.push(TimelineLine::new("error", error, true));
            }
        } else if model_picker_command_query(command).is_some() {
            if let Err(error) = open_model_picker_from_handler(state, handler, "") {
                state.timeline.push(TimelineLine::new("error", error, true));
            }
        } else if let Some(query) = session_picker_command_query(command) {
            if let Err(error) = open_session_picker_from_handler(state, handler, query) {
                state.timeline.push(TimelineLine::new("error", error, true));
            }
        } else if let Some(query) = file_picker_command_query(command) {
            if let Err(error) = open_file_picker_from_handler(state, handler, query) {
                state.timeline.push(TimelineLine::new("error", error, true));
            }
        } else {
            match handler.handle_command(command) {
                Ok(lines) => apply_handler_output(state, handler, lines),
                Err(error) => state.timeline.push(TimelineLine::new("error", error, true)),
            }
        }
    }
    let payload = json!({
        "ok": result
            .get("applied")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "request": request,
        "result": result,
    });
    if let Err(error) = handler.record_control_response(&payload) {
        state
            .timeline
            .push(TimelineLine::new("warning", error, true));
    }
}

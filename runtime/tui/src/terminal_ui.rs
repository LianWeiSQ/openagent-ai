//! Terminal UI state for the Rust rewrite.

use std::path::PathBuf;

#[cfg(test)]
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[cfg(test)]
use openagent_bridge_server_client::RemoteAuth;
#[cfg(test)]
use openagent_bridge_server_client::session_id_from_payload;
#[cfg(test)]
use ratatui::Terminal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod agent;
mod attachments;
mod commands;
mod composer;
mod config;
mod control;
mod control_actions;
mod events;
mod interaction;
mod patch;
mod picker;
mod project;
mod provider;
mod render;
mod server;
mod session;
mod terminal;
mod util;

pub use agent::AgentPickerState;
pub use attachments::{ComposerFileCandidate, FilePickerState};
#[cfg(test)]
use attachments::{expand_file_attachments, fuzzy_find_files};
pub use commands::{ChoicePickerKind, ChoicePickerState};
pub use config::TuiConfig;
#[cfg(test)]
use config::default_color_scheme_names;
#[cfg(test)]
use config::default_theme_names;
use control::action_map_fixture;
pub use control::{control_publish_to_action, normalize_control_action};
pub use interaction::{InteractionDockState, InteractionFocus};
#[cfg(test)]
use patch::diff_detail_lines;
#[cfg(test)]
use picker::file_picker_lines;
pub use provider::ModelPickerState;
#[cfg(test)]
use render::draw_terminal_frame;
pub use server::{BridgeTerminalHandler, BridgeTerminalOptions};
#[cfg(test)]
use session::open_session_picker_from_handler;
pub use session::{SessionPickerAction, SessionPickerMode, SessionPickerState};
#[cfg(test)]
pub(crate) use terminal::apply_bridge_event_values;
pub(crate) use terminal::apply_handler_output;
pub use terminal::{TerminalEventHandler, TerminalUiOptions, run_terminal_ui};
#[cfg(test)]
pub(crate) use terminal::{
    handle_key_event, handle_local_state_command, handle_remote_control_request,
};
use util::usage_totals_value;

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[must_use]
pub fn command_name() -> &'static str {
    "openagent-tui"
}

#[must_use]
pub fn client_crate_name() -> &'static str {
    openagent_bridge_server_client::crate_name()
}

#[must_use]
pub fn server_crate_name() -> &'static str {
    openagent_bridge_server::crate_name()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TimelineLine {
    pub kind: String,
    pub text: String,
    pub important: bool,
}

impl TimelineLine {
    #[must_use]
    pub fn new(kind: impl Into<String>, text: impl Into<String>, important: bool) -> Self {
        Self {
            kind: kind.into(),
            text: text.into(),
            important,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TuiState {
    pub input_buffer: String,
    pub status: String,
    pub timeline: Vec<TimelineLine>,
    pub session_id: Option<String>,
    pub current_turn_id: Option<String>,
    pub active_approval: Option<Value>,
    pub active_question: Option<Value>,
    pub interaction: InteractionDockState,
    pub approval_responses: Vec<Value>,
    pub question_responses: Vec<Value>,
    pub history: Vec<String>,
    pub history_cursor: Option<usize>,
    pub stash: Vec<String>,
    pub config: TuiConfig,
    pub show_tool_details: bool,
    pub runtime_warnings: Vec<String>,
    pub usage_totals: Value,
    pub file_picker: Option<FilePickerState>,
    pub session_picker: Option<SessionPickerState>,
    pub model_picker: Option<ModelPickerState>,
    pub agent_picker: Option<AgentPickerState>,
    pub choice_picker: Option<ChoicePickerState>,
}

impl TuiState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            input_buffer: String::new(),
            status: "idle".to_string(),
            timeline: Vec::new(),
            session_id: None,
            current_turn_id: None,
            active_approval: None,
            active_question: None,
            interaction: InteractionDockState::default(),
            approval_responses: Vec::new(),
            question_responses: Vec::new(),
            history: Vec::new(),
            history_cursor: None,
            stash: Vec::new(),
            config: TuiConfig::load_from_workspace(
                &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
            show_tool_details: false,
            runtime_warnings: Vec::new(),
            usage_totals: usage_totals_value(0, 0, 0, 0.0),
            file_picker: None,
            session_picker: None,
            model_picker: None,
            agent_picker: None,
            choice_picker: None,
        }
    }

    #[must_use]
    pub fn with_config(config: TuiConfig) -> Self {
        Self {
            config,
            ..Self::new()
        }
    }
}

#[must_use]
pub fn tui_control_fixture() -> Value {
    let mut state = TuiState::new();
    let requests = vec![
        json!({"path": "/tui/append-prompt", "body": {"text": "hello"}}),
        json!({"path": "/tui/publish", "body": {"type": "tui.prompt.append", "properties": {"text": " next"}}}),
        json!({"path": "/tui/show-toast", "body": {"title": "Saved", "message": "Session selected", "variant": "success"}}),
        json!({"path": "/tui/execute-command", "body": {"command": "help"}}),
        json!({"path": "/tui/open-themes", "body": {}}),
        json!({"path": "/tui/clear-prompt", "body": {}}),
    ];
    let steps = requests
        .into_iter()
        .map(|request| {
            let result = state.apply_control_request(&request);
            json!({
                "request": request,
                "result": result,
                "status": state.status,
                "input_buffer": state.input_buffer,
                "timeline": state.timeline,
            })
        })
        .collect::<Vec<_>>();
    let mut invalid_state = TuiState::new();
    let invalid_select =
        invalid_state.apply_control_request(&json!({"path": "/tui/select-session", "body": {}}));

    json!({
        "action_map": action_map_fixture(),
        "steps": steps,
        "invalid_select": {
            "result": invalid_select,
            "status": invalid_state.status,
            "timeline": invalid_state.timeline,
        },
    })
}

#[cfg(test)]
mod tests;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use crate::{
    TerminalEventHandler, TimelineLine, TuiState, apply_handler_output,
    picker::{session_picker_parent_id, session_picker_title},
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionPickerState {
    pub query: String,
    pub selected: usize,
    pub candidates: Vec<Value>,
    pub mode: SessionPickerMode,
    pub action_selected: usize,
    pub rename_buffer: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SessionPickerMode {
    #[default]
    Browse,
    Actions,
    Details,
    Rename,
    Confirm(SessionPickerAction),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPickerAction {
    Resume,
    Details,
    Rename,
    Archive,
    Unarchive,
    Delete,
    Fork,
    Children,
    Parent,
    Share,
    Unshare,
    Compact,
    Undo,
    Redo,
}

pub(crate) const SESSION_PICKER_ACTIONS: &[SessionPickerAction] = &[
    SessionPickerAction::Resume,
    SessionPickerAction::Details,
    SessionPickerAction::Rename,
    SessionPickerAction::Archive,
    SessionPickerAction::Unarchive,
    SessionPickerAction::Delete,
    SessionPickerAction::Fork,
    SessionPickerAction::Children,
    SessionPickerAction::Parent,
    SessionPickerAction::Share,
    SessionPickerAction::Unshare,
    SessionPickerAction::Compact,
    SessionPickerAction::Undo,
    SessionPickerAction::Redo,
];

impl SessionPickerAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Resume => "Resume",
            Self::Details => "Details",
            Self::Rename => "Rename",
            Self::Archive => "Archive",
            Self::Unarchive => "Unarchive",
            Self::Delete => "Delete",
            Self::Fork => "Fork",
            Self::Children => "Children",
            Self::Parent => "Parent",
            Self::Share => "Share",
            Self::Unshare => "Unshare",
            Self::Compact => "Compact",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
        }
    }

    fn command(self, rename_title: Option<&str>) -> Option<String> {
        match self {
            Self::Resume | Self::Details => None,
            Self::Rename => rename_title.map(|title| format!("/rename {}", title.trim())),
            Self::Archive => Some("/archive".to_string()),
            Self::Unarchive => Some("/unarchive".to_string()),
            Self::Delete => Some("/delete".to_string()),
            Self::Fork => Some("/fork".to_string()),
            Self::Children => Some("/children".to_string()),
            Self::Parent => Some("/parent".to_string()),
            Self::Share => Some("/share".to_string()),
            Self::Unshare => Some("/unshare".to_string()),
            Self::Compact => Some("/compact".to_string()),
            Self::Undo => Some("/undo".to_string()),
            Self::Redo => Some("/redo".to_string()),
        }
    }

    pub(crate) fn requires_confirmation(self) -> bool {
        matches!(
            self,
            Self::Archive | Self::Delete | Self::Share | Self::Unshare | Self::Compact
        )
    }
}

pub(crate) fn handle_session_picker_key<H: TerminalEventHandler>(
    key: KeyEvent,
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), String> {
    let mode = state
        .session_picker
        .as_ref()
        .map(|picker| picker.mode.clone())
        .unwrap_or(SessionPickerMode::Browse);
    match mode {
        SessionPickerMode::Actions => {
            return handle_session_picker_actions_key(key, state, handler);
        }
        SessionPickerMode::Details => {
            return handle_session_picker_details_key(key, state);
        }
        SessionPickerMode::Rename => {
            return handle_session_picker_rename_key(key, state, handler);
        }
        SessionPickerMode::Confirm(action) => {
            return handle_session_picker_confirm_key(key, state, handler, action);
        }
        SessionPickerMode::Browse => {}
    }
    match key.code {
        KeyCode::Esc => {
            state.close_session_picker();
        }
        KeyCode::Enter => {
            select_session_picker_from_handler(state, handler)?;
        }
        KeyCode::Right | KeyCode::F(2) => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.mode = SessionPickerMode::Actions;
                picker.action_selected = 0;
            }
            state.status = "session actions".to_string();
        }
        KeyCode::Char('d') | KeyCode::Char('D')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.mode = SessionPickerMode::Details;
            }
            state.status = "session details".to_string();
        }
        KeyCode::Delete => {
            begin_session_picker_action(state, handler, SessionPickerAction::Delete)?;
        }
        KeyCode::Up => {
            state.session_picker_previous();
        }
        KeyCode::Down | KeyCode::Tab => {
            state.session_picker_next();
        }
        KeyCode::Backspace => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.query.pop();
            }
            if let Err(error) = refresh_session_picker_from_handler(state, handler) {
                state
                    .timeline
                    .push(TimelineLine::new("warning", error, true));
                state.status = "session picker refresh failed".to_string();
            }
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.query.push(ch);
            }
            if let Err(error) = refresh_session_picker_from_handler(state, handler) {
                state
                    .timeline
                    .push(TimelineLine::new("warning", error, true));
                state.status = "session picker refresh failed".to_string();
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_session_picker_actions_key<H: TerminalEventHandler>(
    key: KeyEvent,
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), String> {
    match key.code {
        KeyCode::Esc | KeyCode::Left => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.mode = SessionPickerMode::Browse;
            }
            state.status = "session picker".to_string();
        }
        KeyCode::Enter => {
            let action = state
                .session_picker
                .as_ref()
                .map(|picker| {
                    SESSION_PICKER_ACTIONS
                        [picker.action_selected.min(SESSION_PICKER_ACTIONS.len() - 1)]
                })
                .unwrap_or(SessionPickerAction::Resume);
            begin_session_picker_action(state, handler, action)?;
        }
        KeyCode::Up => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.action_selected = picker.action_selected.saturating_sub(1);
            }
            state.status = "session actions".to_string();
        }
        KeyCode::Down | KeyCode::Tab => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.action_selected =
                    (picker.action_selected + 1).min(SESSION_PICKER_ACTIONS.len() - 1);
            }
            state.status = "session actions".to_string();
        }
        _ => {}
    }
    Ok(())
}

fn handle_session_picker_details_key(key: KeyEvent, state: &mut TuiState) -> Result<(), String> {
    match key.code {
        KeyCode::Esc | KeyCode::Left => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.mode = SessionPickerMode::Browse;
            }
            state.status = "session picker".to_string();
        }
        KeyCode::Right | KeyCode::F(2) => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.mode = SessionPickerMode::Actions;
            }
            state.status = "session actions".to_string();
        }
        _ => {}
    }
    Ok(())
}

fn handle_session_picker_rename_key<H: TerminalEventHandler>(
    key: KeyEvent,
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), String> {
    match key.code {
        KeyCode::Esc => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.mode = SessionPickerMode::Browse;
                picker.rename_buffer.clear();
            }
            state.status = "session rename cancelled".to_string();
        }
        KeyCode::Enter => {
            execute_session_picker_action(state, handler, SessionPickerAction::Rename)?;
        }
        KeyCode::Backspace => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.rename_buffer.pop();
            }
            state.status = "session rename".to_string();
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.rename_buffer.push(ch);
            }
            state.status = "session rename".to_string();
        }
        _ => {}
    }
    Ok(())
}

fn handle_session_picker_confirm_key<H: TerminalEventHandler>(
    key: KeyEvent,
    state: &mut TuiState,
    handler: &mut H,
    action: SessionPickerAction,
) -> Result<(), String> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.mode = SessionPickerMode::Actions;
            }
            state.status = "session action cancelled".to_string();
        }
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            execute_session_picker_action(state, handler, action)?;
        }
        _ => {}
    }
    Ok(())
}

fn begin_session_picker_action<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    action: SessionPickerAction,
) -> Result<(), String> {
    if state.selected_session_picker_id().is_none() {
        state.status = "session picker empty".to_string();
        return Ok(());
    }
    match action {
        SessionPickerAction::Details => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.mode = SessionPickerMode::Details;
            }
            state.status = "session details".to_string();
        }
        SessionPickerAction::Rename => {
            let title = state
                .selected_session_picker_payload()
                .map(session_picker_title)
                .unwrap_or_default();
            if let Some(picker) = state.session_picker.as_mut() {
                picker.rename_buffer = title;
                picker.mode = SessionPickerMode::Rename;
            }
            state.status = "session rename".to_string();
        }
        action if action.requires_confirmation() => {
            if let Some(picker) = state.session_picker.as_mut() {
                picker.mode = SessionPickerMode::Confirm(action);
            }
            state.status = "session action confirm".to_string();
        }
        _ => execute_session_picker_action(state, handler, action)?,
    }
    Ok(())
}

fn execute_session_picker_action<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    action: SessionPickerAction,
) -> Result<(), String> {
    let Some(session_id) = state.selected_session_picker_id() else {
        state.status = "session picker empty".to_string();
        return Ok(());
    };
    let selected_payload = state.selected_session_picker_payload().cloned();
    if action == SessionPickerAction::Resume {
        state.close_session_picker();
        state.session_id = Some(session_id.clone());
        let lines = handler.handle_command(&format!("/resume {session_id}"))?;
        apply_handler_output(state, handler, lines);
        return Ok(());
    }
    if action == SessionPickerAction::Details {
        if let Some(picker) = state.session_picker.as_mut() {
            picker.mode = SessionPickerMode::Details;
        }
        state.status = "session details".to_string();
        return Ok(());
    }
    let rename_title = state
        .session_picker
        .as_ref()
        .map(|picker| picker.rename_buffer.trim().to_string())
        .unwrap_or_default();
    if action == SessionPickerAction::Rename && rename_title.is_empty() {
        state.timeline.push(TimelineLine::new(
            "warning",
            "session title is required",
            true,
        ));
        state.status = "session rename invalid".to_string();
        return Ok(());
    }
    ensure_session_picker_target_session(state, handler, &session_id)?;
    let command = action
        .command((action == SessionPickerAction::Rename).then_some(rename_title.as_str()))
        .ok_or_else(|| format!("session action has no command: {action:?}"))?;
    let lines = handler.handle_command(&command)?;
    apply_handler_output(state, handler, lines);
    match action {
        SessionPickerAction::Delete => {
            state.session_id = None;
        }
        SessionPickerAction::Parent => {
            if let Some(parent) = selected_payload
                .as_ref()
                .and_then(session_picker_parent_id)
                .filter(|value| !value.is_empty())
            {
                state.session_id = Some(parent);
            }
        }
        _ => {}
    }
    if let Some(picker) = state.session_picker.as_mut() {
        picker.mode = SessionPickerMode::Browse;
        picker.rename_buffer.clear();
    }
    if let Err(error) = refresh_session_picker_from_handler(state, handler) {
        state
            .timeline
            .push(TimelineLine::new("warning", error, true));
    }
    state.status = format!("session {} completed", action.label().to_ascii_lowercase());
    Ok(())
}

fn ensure_session_picker_target_session<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    session_id: &str,
) -> Result<(), String> {
    if state.session_id.as_deref() == Some(session_id) {
        return Ok(());
    }
    state.session_id = Some(session_id.to_string());
    let lines = handler.handle_command(&format!("/resume {session_id}"))?;
    apply_handler_output(state, handler, lines);
    Ok(())
}

pub(crate) fn open_session_picker_from_handler<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    query: &str,
) -> Result<(), String> {
    let candidates = handler.search_sessions(query)?;
    state.open_session_picker(query, candidates);
    Ok(())
}

pub(crate) fn refresh_session_picker_from_handler<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), String> {
    let query = state
        .session_picker
        .as_ref()
        .map(|picker| picker.query.clone())
        .unwrap_or_default();
    let candidates = handler.search_sessions(&query)?;
    if let Some(picker) = state.session_picker.as_mut() {
        picker.candidates = candidates;
        picker.selected = picker
            .selected
            .min(picker.candidates.len().saturating_sub(1));
    }
    state.status = "session picker".to_string();
    Ok(())
}

pub(crate) fn select_session_picker_from_handler<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), String> {
    let Some(session_id) = state.selected_session_picker_id() else {
        state.status = "session picker empty".to_string();
        return Ok(());
    };
    state.close_session_picker();
    state.session_id = Some(session_id.clone());
    let lines = handler.handle_command(&format!("/resume {session_id}"))?;
    apply_handler_output(state, handler, lines);
    Ok(())
}

impl TuiState {
    pub fn open_session_picker(&mut self, query: &str, candidates: Vec<Value>) {
        self.model_picker = None;
        self.agent_picker = None;
        self.choice_picker = None;
        self.file_picker = None;
        self.session_picker = Some(SessionPickerState {
            query: query.trim().to_string(),
            selected: 0,
            candidates,
            mode: SessionPickerMode::Browse,
            action_selected: 0,
            rename_buffer: String::new(),
        });
        self.status = "session picker".to_string();
    }

    pub fn close_session_picker(&mut self) {
        self.session_picker = None;
        self.status = "session picker closed".to_string();
    }

    pub fn session_picker_previous(&mut self) {
        let Some(picker) = self.session_picker.as_mut() else {
            return;
        };
        picker.selected = picker.selected.saturating_sub(1);
        self.status = "session picker".to_string();
    }

    pub fn session_picker_next(&mut self) {
        let Some(picker) = self.session_picker.as_mut() else {
            return;
        };
        if !picker.candidates.is_empty() {
            picker.selected = (picker.selected + 1).min(picker.candidates.len() - 1);
        }
        self.status = "session picker".to_string();
    }

    pub fn selected_session_picker_id(&self) -> Option<String> {
        self.session_picker
            .as_ref()
            .and_then(|picker| picker.candidates.get(picker.selected))
            .and_then(openagent_bridge_server_client::session_id_from_payload)
    }

    pub fn selected_session_picker_payload(&self) -> Option<&Value> {
        self.session_picker
            .as_ref()
            .and_then(|picker| picker.candidates.get(picker.selected))
    }
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    TerminalEventHandler, TuiState, apply_handler_output,
    config::{default_color_scheme_names, default_theme_names},
    picker::{choice_picker_values_from_models, filter_choice_picker_values},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChoicePickerKind {
    Theme,
    ThemeScheme,
    Variant,
    Thinking,
}

impl ChoicePickerKind {
    pub(crate) fn command_name(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::ThemeScheme => "theme-scheme",
            Self::Variant => "variant",
            Self::Thinking => "thinking",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Theme => "Themes",
            Self::ThemeScheme => "Color Schemes",
            Self::Variant => "Variants",
            Self::Thinking => "Thinking",
        }
    }

    pub(crate) fn item_label(self) -> &'static str {
        match self {
            Self::Theme => "themes",
            Self::ThemeScheme => "color schemes",
            Self::Variant => "variants",
            Self::Thinking => "thinking levels",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChoicePickerState {
    pub kind: ChoicePickerKind,
    pub query: String,
    pub selected: usize,
    pub choices: Vec<String>,
    pub candidates: Vec<String>,
}

pub(crate) const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("/help", "show TUI commands"),
    ("/connect <url>", "connect to a Bridge server"),
    ("/sessions [query]", "open/search recent session picker"),
    ("/resume <id>", "resume a session by id or unique prefix"),
    ("/rename <title>", "rename the current session"),
    ("/archive", "archive the current session"),
    ("/unarchive", "unarchive the current session"),
    ("/delete", "delete the current session"),
    (
        "/transcript [limit]",
        "show recent messages from the current session",
    ),
    ("/new", "start a new session"),
    ("/fork", "fork the current session"),
    ("/children", "list child sessions"),
    ("/parent", "navigate to the parent session"),
    ("/share", "share the current session"),
    ("/unshare", "remove the current shared session link"),
    ("/compact", "compact the current session context"),
    ("/details", "show current turn and trace details"),
    ("/models [id]", "list models or set the current model"),
    ("/agents", "list available agent profiles"),
    ("/agent <id>", "set the current agent profile"),
    ("/skills [query]", "show project and user skills"),
    ("/variant <name>", "set the current model variant"),
    ("/themes", "open theme picker"),
    (
        "/theme-scheme [system|light|dark]",
        "open or set color scheme",
    ),
    ("/thinking <level>", "set reasoning effort or visibility"),
    ("/config", "show loaded TUI configuration"),
    ("/keybinds", "show active key bindings"),
    ("/usage", "show token and cost totals"),
    ("/warnings", "show aggregated runtime warnings"),
    ("/tool-details [on|off]", "toggle tool metadata details"),
    (
        "/mcp [list|show|doctor|test|start|stop|restart|enable|disable]",
        "inspect and control Bridge MCP servers",
    ),
    ("/editor", "edit prompt in an external editor"),
    ("/stash <draft>", "stash a draft prompt"),
    ("/unstash", "restore the latest stashed prompt"),
    ("/stashes", "list stashed prompt drafts"),
    ("/files [query]", "search workspace files for @ attachments"),
    (
        "/attach <path[:range]>",
        "insert a file/image reference into the prompt",
    ),
    ("/export", "export the current session"),
    ("/init", "initialize OpenAgent project files"),
    ("/undo", "undo the last reversible change"),
    ("/redo", "redo the last undone change"),
    ("/clear", "clear the visible timeline"),
    ("/status", "show current session, turn, and model status"),
    ("/allow [once|always]", "approve the active tool request"),
    (
        "/deny [note]",
        "deny the active tool request with an optional note",
    ),
    ("/answer <text>", "answer the active question request"),
    ("/dismiss [note]", "dismiss the active question request"),
    ("/commands", "list project/global custom commands"),
];

pub(crate) fn file_picker_command_query(command: &str) -> Option<&str> {
    if command == "/files" {
        return Some("");
    }
    command.strip_prefix("/files ").map(str::trim)
}

pub(crate) fn session_picker_command_query(command: &str) -> Option<&str> {
    if command == "/sessions" {
        return Some("");
    }
    command.strip_prefix("/sessions ").map(str::trim)
}

pub(crate) fn model_picker_command_query(command: &str) -> Option<&str> {
    (command == "/models").then_some("")
}

pub(crate) fn agent_picker_command_query(command: &str) -> Option<&str> {
    (command == "/agents").then_some("")
}

pub(crate) fn choice_picker_command_kind(command: &str) -> Option<ChoicePickerKind> {
    match command {
        "/theme" | "/themes" => Some(ChoicePickerKind::Theme),
        "/theme-scheme" | "/color-scheme" | "/scheme" => Some(ChoicePickerKind::ThemeScheme),
        "/variant" => Some(ChoicePickerKind::Variant),
        "/thinking" => Some(ChoicePickerKind::Thinking),
        _ => None,
    }
}

pub(crate) fn handle_choice_picker_key<H: TerminalEventHandler>(
    key: KeyEvent,
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), String> {
    match key.code {
        KeyCode::Esc => {
            state.close_choice_picker();
        }
        KeyCode::Enter => {
            select_choice_picker_from_handler(state, handler)?;
        }
        KeyCode::Up => {
            state.choice_picker_previous();
        }
        KeyCode::Down | KeyCode::Tab => {
            state.choice_picker_next();
        }
        KeyCode::Backspace => {
            if let Some(picker) = state.choice_picker.as_mut() {
                picker.query.pop();
            }
            state.filter_choice_picker();
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(picker) = state.choice_picker.as_mut() {
                picker.query.push(ch);
            }
            state.filter_choice_picker();
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn open_choice_picker_from_handler<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    kind: ChoicePickerKind,
) -> Result<(), String> {
    let payload = handler.list_models()?;
    let choices = choice_picker_values_from_models(&payload, kind);
    state.open_choice_picker(kind, "", choices);
    Ok(())
}

pub(crate) fn open_choice_picker_from_command<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    kind: ChoicePickerKind,
) -> Result<(), String> {
    match kind {
        ChoicePickerKind::Theme => {
            state.open_choice_picker(ChoicePickerKind::Theme, "", default_theme_names());
            Ok(())
        }
        ChoicePickerKind::ThemeScheme => {
            state.open_choice_picker(
                ChoicePickerKind::ThemeScheme,
                "",
                default_color_scheme_names(),
            );
            Ok(())
        }
        ChoicePickerKind::Variant | ChoicePickerKind::Thinking => {
            open_choice_picker_from_handler(state, handler, kind)
        }
    }
}

pub(crate) fn select_choice_picker_from_handler<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), String> {
    let Some((kind, value)) = state.selected_choice_picker_value() else {
        state.status = "choice picker empty".to_string();
        return Ok(());
    };
    state.close_choice_picker();
    if kind == ChoicePickerKind::Theme {
        state.set_theme(&value);
        return Ok(());
    }
    if kind == ChoicePickerKind::ThemeScheme {
        state.set_color_scheme(&value);
        return Ok(());
    }
    let lines = handler.handle_command(&format!("/{} {value}", kind.command_name()))?;
    apply_handler_output(state, handler, lines);
    Ok(())
}

pub(crate) fn is_local_state_command(submitted: &str) -> bool {
    if matches!(submitted, "/help" | "/?" | "/") {
        return true;
    }
    let command = submitted
        .strip_prefix('/')
        .and_then(|value| value.split_whitespace().next())
        .unwrap_or_default();
    matches!(
        command,
        "allow"
            | "approve"
            | "deny"
            | "reject"
            | "answer"
            | "dismiss"
            | "stash"
            | "unstash"
            | "stashes"
            | "themes"
            | "theme"
            | "theme-scheme"
            | "color-scheme"
            | "scheme"
            | "config"
            | "keybinds"
            | "usage"
            | "warnings"
            | "tool-details"
            | "editor"
            | "attach"
    )
}

impl TuiState {
    pub fn open_choice_picker(
        &mut self,
        kind: ChoicePickerKind,
        query: &str,
        choices: Vec<String>,
    ) {
        self.file_picker = None;
        self.session_picker = None;
        self.model_picker = None;
        self.agent_picker = None;
        let query = query.trim().to_string();
        let candidates = filter_choice_picker_values(&choices, &query);
        self.choice_picker = Some(ChoicePickerState {
            kind,
            query,
            selected: 0,
            choices,
            candidates,
        });
        self.status = format!("{} picker", kind.command_name());
    }

    pub fn close_choice_picker(&mut self) {
        self.choice_picker = None;
        self.status = "choice picker closed".to_string();
    }

    pub fn filter_choice_picker(&mut self) {
        let Some(picker) = self.choice_picker.as_mut() else {
            return;
        };
        picker.candidates = filter_choice_picker_values(&picker.choices, &picker.query);
        picker.selected = picker
            .selected
            .min(picker.candidates.len().saturating_sub(1));
        self.status = format!("{} picker", picker.kind.command_name());
    }

    pub fn choice_picker_previous(&mut self) {
        let Some(picker) = self.choice_picker.as_mut() else {
            return;
        };
        picker.selected = picker.selected.saturating_sub(1);
        self.status = format!("{} picker", picker.kind.command_name());
    }

    pub fn choice_picker_next(&mut self) {
        let Some(picker) = self.choice_picker.as_mut() else {
            return;
        };
        if !picker.candidates.is_empty() {
            picker.selected = (picker.selected + 1).min(picker.candidates.len() - 1);
        }
        self.status = format!("{} picker", picker.kind.command_name());
    }

    pub fn selected_choice_picker_value(&self) -> Option<(ChoicePickerKind, String)> {
        self.choice_picker.as_ref().and_then(|picker| {
            picker
                .candidates
                .get(picker.selected)
                .filter(|value| !value.is_empty())
                .map(|value| (picker.kind, value.clone()))
        })
    }
}

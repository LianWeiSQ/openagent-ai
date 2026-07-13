use crossterm::event::{KeyCode, KeyEvent};
use openagent_bridge_server::{
    approval_response_payload, question_dismiss_payload, question_reply_payload,
};
use serde_json::{Map, Value, json};

use crate::util::{IfEmptyThen, compact_json, string_field, trim_lines};
use crate::{TimelineLine, TuiState};

pub(crate) fn approval_request_summary(approval: &Value) -> String {
    let tool_name = string_field(approval, "tool_name").if_empty_then(|| "tool".to_string());
    let tool_input = approval
        .get("tool_input")
        .map(compact_json)
        .unwrap_or_else(|| "{}".to_string());
    let mut lines = vec![format!("approval required: {tool_name} {tool_input}")];
    if let Some(call_id) = approval.get("call_id").and_then(Value::as_str)
        && !call_id.is_empty()
    {
        lines.push(format!("call: {call_id}"));
    }
    if let Some(preview) = approval.get("preview").filter(|value| value.is_object()) {
        lines.extend(preview_lines(preview));
    }
    lines.join("\n")
}

pub(crate) fn approval_response_summary(approval: &Value) -> String {
    let action = string_field(approval, "action").if_empty_then(|| "resolved".to_string());
    let tool = string_field(approval, "tool_name").if_empty_then(|| "tool".to_string());
    let mut suffix = Vec::new();
    if let Some(scope) = approval.get("scope").and_then(Value::as_str)
        && !scope.is_empty()
    {
        suffix.push(scope.to_string());
    }
    if let Some(note) = approval.get("note").and_then(Value::as_str)
        && !note.is_empty()
    {
        suffix.push(note.to_string());
    }
    if suffix.is_empty() {
        format!("approval {action}: {tool}")
    } else {
        format!("approval {action}: {tool} ({})", suffix.join("; "))
    }
}

pub(crate) fn preview_lines(preview: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let kind = string_field(preview, "kind").if_empty_then(|| "tool".to_string());
    lines.push(format!("preview: {kind}"));
    if let Some(path) = preview.get("path").and_then(Value::as_str)
        && !path.is_empty()
    {
        let status = preview
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let suffix = if status.is_empty() {
            String::new()
        } else {
            format!(" ({status})")
        };
        lines.push(format!("path: {path}{suffix}"));
    }
    if let Some(command) = preview.get("command").and_then(Value::as_str)
        && !command.is_empty()
    {
        lines.push(format!("command: {command}"));
    }
    if let Some(warnings) = preview.get("warnings").and_then(Value::as_array) {
        lines.extend(
            warnings
                .iter()
                .filter_map(Value::as_str)
                .take(3)
                .map(|warning| format!("warning: {warning}")),
        );
    }
    if let Some(diff) = preview.get("diff").and_then(Value::as_str)
        && !diff.trim().is_empty()
    {
        lines.push("diff:".to_string());
        lines.extend(trim_lines(diff, 40));
    }
    if let Some(summary) = preview.get("summary").and_then(Value::as_str)
        && !summary.is_empty()
    {
        lines.push(format!("summary: {summary}"));
    }
    lines
}

pub(crate) fn question_request_summary(question: &Value) -> String {
    let questions = question
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut lines = vec![format!("question requested: {} item(s)", questions.len())];
    for (index, item) in questions.iter().enumerate().take(5) {
        let label = item
            .get("header")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("Question");
        let text = item
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or_default();
        lines.push(format!("{}. {label}: {text}", index + 1));
        if let Some(options) = item.get("options").and_then(Value::as_array) {
            for option in options.iter().take(3) {
                if let Some(option_label) = option.get("label").and_then(Value::as_str) {
                    lines.push(format!("   - {option_label}"));
                }
            }
        }
    }
    lines.join("\n")
}

pub(crate) fn question_items(question: &Value) -> Vec<Value> {
    question
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn question_option_values(question: &Value) -> Vec<Value> {
    question
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn question_option_labels(question: &Value) -> Vec<String> {
    question_option_values(question)
        .into_iter()
        .filter_map(|option| {
            option
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| option.as_str().map(str::to_string))
        })
        .filter(|label| !label.is_empty())
        .collect()
}

pub(crate) fn question_response_summary(response: &Value) -> String {
    let dismissed = response
        .get("dismissed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if dismissed {
        let note = response
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if note.is_empty() {
            "question dismissed".to_string()
        } else {
            format!("question dismissed: {note}")
        }
    } else {
        let count = response
            .get("answers")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        format!("question answered: {count} item(s)")
    }
}

pub(crate) fn merge_identity_fields(target: &mut Value, source: &Value, keys: &[&str]) {
    let Some(target_object) = target.as_object_mut() else {
        return;
    };
    for key in keys {
        if let Some(value) = source.get(*key)
            && !value.is_null()
        {
            target_object.insert((*key).to_string(), value.clone());
        }
    }
}

pub(crate) fn approval_matches_active(active: &Option<Value>, approval: &Value) -> bool {
    let Some(active) = active else {
        return false;
    };
    let active_id = string_field(active, "request_id");
    !active_id.is_empty() && active_id == string_field(approval, "request_id")
}

pub(crate) fn answer_vecs(value: &Value) -> Option<Vec<Vec<String>>> {
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(|item| {
                if let Some(text) = item.as_str() {
                    Some(vec![text.to_string()])
                } else {
                    item.as_array().map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    })
                }
            })
            .collect()
    })
}

pub(crate) fn set_action_name(value: &mut Value, action: &str) {
    if let Some(object) = value.as_object_mut() {
        object.insert("action".to_string(), Value::String(action.to_string()));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractionFocus {
    Approval,
    Question,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InteractionDockState {
    pub focus: Option<InteractionFocus>,
    pub selected: usize,
    pub question_index: usize,
    pub question_answers: Vec<Vec<String>>,
    pub custom_answer: String,
}

impl TuiState {
    pub(crate) fn active_interaction_focus(&self) -> Option<InteractionFocus> {
        match self.interaction.focus {
            Some(InteractionFocus::Approval) if self.active_approval.is_some() => {
                Some(InteractionFocus::Approval)
            }
            Some(InteractionFocus::Question) if self.active_question.is_some() => {
                Some(InteractionFocus::Question)
            }
            _ if self.active_approval.is_some() => Some(InteractionFocus::Approval),
            _ if self.active_question.is_some() => Some(InteractionFocus::Question),
            _ => None,
        }
    }

    pub(crate) fn focus_approval_interaction(&mut self) {
        self.interaction = InteractionDockState {
            focus: Some(InteractionFocus::Approval),
            selected: 0,
            ..InteractionDockState::default()
        };
    }

    pub(crate) fn focus_question_interaction(&mut self, question: &Value) {
        let count = question_items(question).len().max(1);
        self.interaction = InteractionDockState {
            focus: Some(InteractionFocus::Question),
            selected: 0,
            question_index: 0,
            question_answers: vec![Vec::new(); count],
            custom_answer: String::new(),
        };
    }

    pub(crate) fn clear_interaction(&mut self, focus: InteractionFocus) {
        if self.interaction.focus == Some(focus) {
            self.interaction = InteractionDockState::default();
        }
    }

    pub(crate) fn handle_interaction_key(&mut self, key: &KeyEvent) -> bool {
        match self.active_interaction_focus() {
            Some(InteractionFocus::Approval) => self.handle_approval_key(key),
            Some(InteractionFocus::Question) => self.handle_question_key(key),
            None => false,
        }
    }

    fn handle_approval_key(&mut self, key: &KeyEvent) -> bool {
        let option_count = 3;
        match key.code {
            KeyCode::Up => {
                self.interaction.selected = self
                    .interaction
                    .selected
                    .saturating_sub(1)
                    .min(option_count - 1);
                true
            }
            KeyCode::Down => {
                self.interaction.selected = (self.interaction.selected + 1).min(option_count - 1);
                true
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                let index = ch.to_digit(10).unwrap_or_default().saturating_sub(1) as usize;
                if index < option_count {
                    self.interaction.selected = index;
                    self.submit_selected_approval();
                }
                true
            }
            KeyCode::Enter => {
                self.submit_selected_approval();
                true
            }
            KeyCode::Esc => {
                self.respond_active_approval("deny", None, Some("dismissed from TUI"));
                true
            }
            _ => true,
        }
    }

    fn submit_selected_approval(&mut self) {
        match self.interaction.selected {
            0 => {
                self.respond_active_approval("allow", Some("once"), None);
            }
            1 => {
                self.respond_active_approval("allow", Some("always"), None);
            }
            _ => {
                self.respond_active_approval("deny", None, Some("denied from TUI"));
            }
        }
    }

    fn handle_question_key(&mut self, key: &KeyEvent) -> bool {
        let Some(question) = self.active_question.clone() else {
            return false;
        };
        let items = question_items(&question);
        if items.is_empty() {
            if key.code == KeyCode::Esc {
                self.dismiss_active_question(Some("dismissed from TUI"));
            }
            return true;
        }
        self.interaction.question_index = self.interaction.question_index.min(items.len() - 1);
        let current = &items[self.interaction.question_index];
        let options = question_option_labels(current);
        self.interaction.selected = self
            .interaction
            .selected
            .min(options.len().saturating_sub(1));
        match key.code {
            KeyCode::Esc => {
                self.dismiss_active_question(Some("dismissed from TUI"));
                true
            }
            KeyCode::Left => {
                self.interaction.question_index = self.interaction.question_index.saturating_sub(1);
                self.interaction.selected = 0;
                self.interaction.custom_answer.clear();
                true
            }
            KeyCode::Right | KeyCode::Tab => {
                self.advance_question_or_submit();
                true
            }
            KeyCode::Up => {
                self.interaction.selected = self.interaction.selected.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                if !options.is_empty() {
                    self.interaction.selected =
                        (self.interaction.selected + 1).min(options.len() - 1);
                }
                true
            }
            KeyCode::Backspace => {
                self.interaction.custom_answer.pop();
                true
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() && !options.is_empty() => {
                let index = ch.to_digit(10).unwrap_or_default().saturating_sub(1) as usize;
                if let Some(answer) = options.get(index) {
                    self.answer_current_question(answer.clone());
                }
                true
            }
            KeyCode::Char(ch) => {
                self.interaction.custom_answer.push(ch);
                self.status = "editing answer".to_string();
                true
            }
            KeyCode::Enter => {
                if !self.interaction.custom_answer.trim().is_empty() {
                    self.answer_current_question(self.interaction.custom_answer.trim().to_string());
                } else if let Some(answer) = options.get(self.interaction.selected) {
                    self.answer_current_question(answer.clone());
                } else {
                    self.status = "question needs answer".to_string();
                }
                true
            }
            _ => true,
        }
    }

    fn answer_current_question(&mut self, answer: String) {
        let Some(question) = self.active_question.as_ref() else {
            return;
        };
        let items = question_items(question);
        if items.is_empty() {
            return;
        }
        let index = self.interaction.question_index.min(items.len() - 1);
        if self.interaction.question_answers.len() < items.len() {
            self.interaction
                .question_answers
                .resize_with(items.len(), Vec::new);
        }
        self.interaction.question_answers[index] = vec![answer];
        self.interaction.custom_answer.clear();
        if index + 1 < items.len() {
            self.interaction.question_index = index + 1;
            self.interaction.selected = 0;
            self.status = "question next".to_string();
            return;
        }
        let answers = self.interaction.question_answers.clone();
        self.answer_active_question(answers, None);
    }

    fn advance_question_or_submit(&mut self) {
        let Some(question) = self.active_question.as_ref() else {
            return;
        };
        let items = question_items(question);
        if items.is_empty() {
            return;
        }
        if self.interaction.question_index + 1 < items.len() {
            self.interaction.question_index += 1;
            self.interaction.selected = 0;
            self.interaction.custom_answer.clear();
            return;
        }
        if self
            .interaction
            .question_answers
            .iter()
            .take(items.len())
            .all(|answers| !answers.is_empty())
        {
            self.answer_active_question(self.interaction.question_answers.clone(), None);
        } else {
            self.status = "question needs answer".to_string();
        }
    }

    pub fn respond_active_approval(
        &mut self,
        action: &str,
        scope: Option<&str>,
        note: Option<&str>,
    ) -> Value {
        let Some(approval) = self.active_approval.clone() else {
            self.status = "approval missing".to_string();
            return json!({"applied": false, "error": "no active approval"});
        };
        let mut payload = Map::new();
        payload.insert("action".to_string(), Value::String(action.to_string()));
        if let Some(scope) = scope.filter(|value| !value.trim().is_empty()) {
            payload.insert("scope".to_string(), Value::String(scope.trim().to_string()));
        }
        if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
            payload.insert("note".to_string(), Value::String(note.trim().to_string()));
        }
        let Ok(mut response) = approval_response_payload(&Value::Object(payload)) else {
            self.status = "approval invalid".to_string();
            return json!({"applied": false, "error": "invalid approval response"});
        };
        merge_identity_fields(
            &mut response,
            &approval,
            &["request_id", "turn_id", "session_id", "tool_name"],
        );
        self.active_approval = None;
        self.clear_interaction(InteractionFocus::Approval);
        self.status = "approval resolved".to_string();
        self.timeline.push(TimelineLine::new(
            "status",
            approval_response_summary(&response),
            true,
        ));
        self.approval_responses.push(response.clone());
        json!({"applied": true, "action": "approval.respond", "payload": response})
    }

    pub fn answer_active_question(
        &mut self,
        answers: Vec<Vec<String>>,
        note: Option<&str>,
    ) -> Value {
        let Some(question) = self.active_question.clone() else {
            self.status = "question missing".to_string();
            return json!({"applied": false, "error": "no active question"});
        };
        let mut payload = Map::new();
        payload.insert("answers".to_string(), json!(answers));
        if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
            payload.insert("note".to_string(), Value::String(note.trim().to_string()));
        }
        let Ok(mut response) = question_reply_payload(&Value::Object(payload)) else {
            self.status = "question invalid".to_string();
            return json!({"applied": false, "error": "invalid question response"});
        };
        merge_identity_fields(
            &mut response,
            &question,
            &["request_id", "turn_id", "session_id", "tool_call_id"],
        );
        self.active_question = None;
        self.clear_interaction(InteractionFocus::Question);
        self.status = "question answered".to_string();
        self.timeline.push(TimelineLine::new(
            "status",
            question_response_summary(&response),
            true,
        ));
        self.question_responses.push(response.clone());
        json!({"applied": true, "action": "question.reply", "payload": response})
    }

    pub fn dismiss_active_question(&mut self, note: Option<&str>) -> Value {
        let Some(question) = self.active_question.clone() else {
            self.status = "question missing".to_string();
            return json!({"applied": false, "error": "no active question"});
        };
        let mut payload = Map::new();
        if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
            payload.insert("note".to_string(), Value::String(note.trim().to_string()));
        }
        let mut response = question_dismiss_payload(&Value::Object(payload));
        merge_identity_fields(
            &mut response,
            &question,
            &["request_id", "turn_id", "session_id", "tool_call_id"],
        );
        self.active_question = None;
        self.clear_interaction(InteractionFocus::Question);
        self.status = "question dismissed".to_string();
        self.timeline.push(TimelineLine::new(
            "warning",
            question_response_summary(&response),
            true,
        ));
        self.question_responses.push(response.clone());
        json!({"applied": true, "action": "question.dismiss", "payload": response})
    }
}

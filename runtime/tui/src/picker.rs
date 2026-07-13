use openagent_bridge_server_client::session_id_from_payload;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use serde_json::Value;

use crate::{
    ChoicePickerKind, TimelineLine,
    attachments::{FilePickerMatch, is_image_path},
    config::{default_color_scheme_names, default_theme_names, string_array},
    util::{clip_chars, compact_json},
};

pub(crate) fn session_list_lines(sessions: &[Value]) -> Vec<TimelineLine> {
    let mut lines = vec![TimelineLine::new(
        "status",
        format!("remote sessions: {}", sessions.len()),
        false,
    )];
    lines.extend(sessions.iter().take(20).map(|session| {
        let id = session_id_from_payload(session).unwrap_or_else(|| "<unknown>".to_string());
        let status = session
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let messages = session
            .get("message_count")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let workspace = session
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or(".");
        TimelineLine::new(
            "status",
            format!("{id}  status={status}  messages={messages}  workspace={workspace}"),
            false,
        )
    }));
    lines
}

pub(crate) fn filter_agents_for_picker(agents: &[Value], query: &str) -> Vec<Value> {
    let query = query.trim().to_ascii_lowercase();
    agents
        .iter()
        .filter(|agent| query.is_empty() || agent_matches_query(agent, &query))
        .cloned()
        .collect()
}

fn agent_matches_query(agent: &Value, query: &str) -> bool {
    ["id", "name", "description"].iter().any(|key| {
        agent
            .get(*key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(query)
    })
}

pub(crate) fn agent_picker_label(agent: &Value) -> String {
    let id = agent.get("id").and_then(Value::as_str).unwrap_or("agent");
    let name = agent.get("name").and_then(Value::as_str).unwrap_or(id);
    let description = agent
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let default = if agent
        .get("default")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "  default"
    } else {
        ""
    };
    if description.is_empty() {
        format!("{id} - {name}{default}")
    } else {
        format!("{id} - {name}: {description}{default}")
    }
}

pub(crate) fn filter_models_for_picker(models: &[Value], query: &str) -> Vec<Value> {
    let query = query.trim().to_ascii_lowercase();
    models
        .iter()
        .filter(|model| query.is_empty() || model_matches_query(model, &query))
        .cloned()
        .collect()
}

fn model_matches_query(model: &Value, query: &str) -> bool {
    ["id", "name", "provider_id"].iter().any(|key| {
        model
            .get(*key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(query)
    })
}

pub(crate) fn model_picker_label(model: &Value) -> String {
    let provider = model
        .get("provider_id")
        .and_then(Value::as_str)
        .unwrap_or("provider");
    let id = model.get("id").and_then(Value::as_str).unwrap_or("model");
    let name = model.get("name").and_then(Value::as_str).unwrap_or(id);
    let default = if model
        .get("default")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "  default"
    } else {
        ""
    };
    format!("{provider}/{id} - {name}{default}")
}

pub(crate) fn choice_picker_values_from_models(
    payload: &Value,
    kind: ChoicePickerKind,
) -> Vec<String> {
    let key = match kind {
        ChoicePickerKind::Theme => return default_theme_names(),
        ChoicePickerKind::ThemeScheme => return default_color_scheme_names(),
        ChoicePickerKind::Variant => "variants",
        ChoicePickerKind::Thinking => "thinking",
    };
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(|items| string_array(items))
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| default_choice_picker_values(kind))
}

pub(crate) fn default_choice_picker_values(kind: ChoicePickerKind) -> Vec<String> {
    match kind {
        ChoicePickerKind::Theme => default_theme_names(),
        ChoicePickerKind::ThemeScheme => default_color_scheme_names(),
        ChoicePickerKind::Variant => ["default", "fast", "balanced", "deep"]
            .into_iter()
            .map(ToString::to_string)
            .collect(),
        ChoicePickerKind::Thinking => ["off", "low", "medium", "high"]
            .into_iter()
            .map(ToString::to_string)
            .collect(),
    }
}

pub(crate) fn filter_choice_picker_values(choices: &[String], query: &str) -> Vec<String> {
    let query = query.trim().to_ascii_lowercase();
    choices
        .iter()
        .filter(|choice| query.is_empty() || choice.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect()
}

pub(crate) fn session_picker_label(session: &Value) -> String {
    let id = session_id_from_payload(session).unwrap_or_else(|| "<unknown>".to_string());
    let title = session_picker_title(session);
    let status = session_picker_string(session, "status").unwrap_or_else(|| "unknown".to_string());
    let messages = session
        .get("message_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let workspace = session_picker_string(session, "workspace").unwrap_or_else(|| ".".to_string());
    format!("{id}  {title}  status={status}  messages={messages}  workspace={workspace}")
}

pub(crate) fn session_picker_title(session: &Value) -> String {
    session
        .get("title")
        .or_else(|| {
            session
                .get("metadata")
                .and_then(|metadata| metadata.get("title"))
        })
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("untitled")
        .to_string()
}

fn session_picker_string(session: &Value, key: &str) -> Option<String> {
    session
        .get(key)
        .or_else(|| {
            session
                .get("metadata")
                .and_then(|metadata| metadata.get(key))
        })
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn session_picker_bool(session: &Value, key: &str) -> bool {
    session
        .get(key)
        .or_else(|| {
            session
                .get("metadata")
                .and_then(|metadata| metadata.get(key))
        })
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(crate) fn session_picker_parent_id(session: &Value) -> Option<String> {
    ["parent_session_id", "parent_id", "forked_from"]
        .into_iter()
        .find_map(|key| session_picker_string(session, key))
}

fn session_picker_child_count(session: &Value) -> Option<usize> {
    session
        .get("child_count")
        .or_else(|| session.get("children_count"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .or_else(|| {
            session
                .get("children")
                .or_else(|| session.get("child_session_ids"))
                .and_then(Value::as_array)
                .map(Vec::len)
        })
}

fn session_picker_share_summary(session: &Value) -> String {
    session_picker_string(session, "share_url")
        .or_else(|| session_picker_string(session, "shared_url"))
        .or_else(|| session_picker_string(session, "public_url"))
        .or_else(|| session_picker_string(session, "url"))
        .unwrap_or_else(|| {
            if session_picker_bool(session, "shared") {
                "shared".to_string()
            } else {
                "private".to_string()
            }
        })
}

pub(crate) fn session_picker_status_line(session: &Value) -> String {
    let title = session_picker_title(session);
    let parent = session_picker_parent_id(session).unwrap_or_else(|| "-".to_string());
    let children = session_picker_child_count(session)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let archived = if session_picker_bool(session, "archived") {
        "yes"
    } else {
        "no"
    };
    format!("{title}  parent={parent}  children={children}  archived={archived}")
}

pub(crate) fn session_picker_detail_lines(session: &Value) -> Vec<Line<'static>> {
    let id = session_id_from_payload(session).unwrap_or_else(|| "<unknown>".to_string());
    let created = session_picker_string(session, "created_at")
        .or_else(|| session_picker_string(session, "created_at_ms"))
        .unwrap_or_else(|| "-".to_string());
    let updated = session_picker_string(session, "updated_at")
        .or_else(|| session_picker_string(session, "updated_at_ms"))
        .unwrap_or_else(|| "-".to_string());
    let agent = session_picker_string(session, "agent").unwrap_or_else(|| "-".to_string());
    let model = session_picker_string(session, "model").unwrap_or_else(|| "-".to_string());
    let workspace = session_picker_string(session, "workspace").unwrap_or_else(|| ".".to_string());
    vec![
        Line::from(vec![
            Span::styled("Details ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("id={id} workspace={workspace}")),
        ]),
        Line::from(vec![Span::raw(format!(
            "agent={agent} model={model} created={created} updated={updated}"
        ))]),
        Line::from(vec![Span::raw(format!(
            "share={} raw={}",
            session_picker_share_summary(session),
            compact_json(session)
        ))]),
    ]
}

pub(crate) fn transcript_lines(payload: &Value) -> Vec<TimelineLine> {
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = payload
        .get("message_count")
        .and_then(Value::as_u64)
        .unwrap_or(messages.len() as u64);
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(messages.len() as u64);
    let mut lines = vec![TimelineLine::new(
        "status",
        format!(
            "transcript: {} of {total} message(s), limit={limit}",
            messages.len()
        ),
        false,
    )];
    if messages.is_empty() {
        lines.push(TimelineLine::new("status", "transcript: empty", false));
        return lines;
    }
    lines.extend(messages.iter().map(|message| {
        let index = message
            .get("index")
            .and_then(Value::as_u64)
            .map(|value| format!("#{value} "))
            .unwrap_or_default();
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("message");
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        TimelineLine::new(
            "message",
            format!("{index}{role}: {}", clip_chars(&content, 220)),
            false,
        )
    }));
    lines
}

pub(crate) fn model_list_lines(payload: &Value) -> Vec<TimelineLine> {
    let models = payload
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if models.is_empty() {
        return vec![TimelineLine::new("warning", "remote models: none", true)];
    }
    models
        .into_iter()
        .map(|model| {
            let provider = model
                .get("provider_id")
                .and_then(Value::as_str)
                .unwrap_or("provider");
            let id = model.get("id").and_then(Value::as_str).unwrap_or("model");
            let name = model.get("name").and_then(Value::as_str).unwrap_or(id);
            TimelineLine::new("status", format!("{provider}/{id} - {name}"), false)
        })
        .collect()
}

pub(crate) fn agent_list_lines(payload: &Value) -> Vec<TimelineLine> {
    let agents = payload
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if agents.is_empty() {
        return vec![TimelineLine::new("warning", "remote agents: none", true)];
    }
    agents
        .into_iter()
        .map(|agent| {
            let id = agent.get("id").and_then(Value::as_str).unwrap_or("agent");
            let name = agent.get("name").and_then(Value::as_str).unwrap_or(id);
            let description = agent
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            TimelineLine::new("status", format!("{id} - {name}: {description}"), false)
        })
        .collect()
}

pub(crate) fn file_picker_lines(query: &str, matches: &[FilePickerMatch]) -> Vec<TimelineLine> {
    if matches.is_empty() {
        let suffix = if query.trim().is_empty() {
            String::new()
        } else {
            format!(" for `{}`", query.trim())
        };
        return vec![TimelineLine::new(
            "warning",
            format!("files: no matches{suffix}"),
            true,
        )];
    }
    let mut lines = vec![TimelineLine::new(
        "status",
        format!(
            "files: {} match(es){}",
            matches.len(),
            if query.trim().is_empty() {
                String::new()
            } else {
                format!(" for `{}`", query.trim())
            }
        ),
        false,
    )];
    lines.extend(matches.iter().enumerate().map(|(index, item)| {
        TimelineLine::new(
            "status",
            format!(
                "{}. {}  {}",
                index + 1,
                item.reference,
                if is_image_path(&item.path) {
                    "image"
                } else {
                    "file"
                }
            ),
            false,
        )
    }));
    lines
}

use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use openagent_app_server_client::{
    RemoteAuth, RemoteRuntimeClient, event_sequence, events_from_payload, turn_id_from_payload,
};
use serde_json::{Map, Value, json};

use crate::{
    ComposerFileCandidate, TerminalEventHandler, TimelineLine,
    attachments::{composer_candidate_from_match, expand_file_attachments, fuzzy_find_files},
    events::event_identity_key,
    patch::{diff_detail_lines, patch_result_lines},
    picker::{
        agent_list_lines, file_picker_lines, model_list_lines, session_list_lines, transcript_lines,
    },
    project::initialize_openagent_project_files,
    util::compact_json,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppBridgeTerminalOptions {
    pub server_url: String,
    pub auth: RemoteAuth,
    pub workspace: PathBuf,
    pub session_id: Option<String>,
    pub continue_last: bool,
    pub fork: bool,
    pub permission: Option<String>,
    pub dangerously_skip_permissions: bool,
}

impl Default for AppBridgeTerminalOptions {
    fn default() -> Self {
        Self {
            server_url: "http://127.0.0.1:8787".to_string(),
            auth: RemoteAuth::default(),
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            session_id: None,
            continue_last: false,
            fork: false,
            permission: None,
            dangerously_skip_permissions: false,
        }
    }
}

pub struct AppBridgeTerminalHandler {
    client: RemoteRuntimeClient,
    workspace: PathBuf,
    current_session: Option<String>,
    continue_last: bool,
    fork_next: bool,
    permission: Option<String>,
    dangerously_skip_permissions: bool,
    last_turn_id: Option<String>,
    last_global_event_id: u64,
    pending_events: Vec<Value>,
    seen_events: BTreeSet<String>,
}

impl AppBridgeTerminalHandler {
    pub fn connect(options: AppBridgeTerminalOptions) -> Result<Self, String> {
        let client = RemoteRuntimeClient::new(options.server_url.clone())
            .with_auth(options.auth)
            .with_timeout(Duration::from_secs(3));
        client.health()?;
        let mut handler = Self {
            client,
            workspace: options.workspace,
            current_session: options.session_id,
            continue_last: options.continue_last,
            fork_next: options.fork,
            permission: options.permission,
            dangerously_skip_permissions: options.dangerously_skip_permissions,
            last_turn_id: None,
            last_global_event_id: 0,
            pending_events: Vec::new(),
            seen_events: BTreeSet::new(),
        };
        if handler.current_session.is_none() && (handler.continue_last || handler.fork_next) {
            let session_id = handler.client.select_session(
                None,
                handler.continue_last,
                handler.fork_next,
                &handler.workspace,
            )?;
            handler.current_session = Some(session_id);
            handler.fork_next = false;
        }
        Ok(handler)
    }

    #[must_use]
    pub fn server_url(&self) -> &str {
        self.client.server_url()
    }

    #[must_use]
    pub fn current_session(&self) -> Option<&str> {
        self.current_session.as_deref()
    }

    fn ensure_session(&mut self) -> Result<String, String> {
        if let Some(session_id) = self.current_session.clone() {
            return Ok(session_id);
        }
        let session_id = self
            .client
            .select_session(None, false, false, &self.workspace)?;
        self.current_session = Some(session_id.clone());
        Ok(session_id)
    }

    fn start_new_session(&mut self, fork_from: Option<String>) -> Result<String, String> {
        let session_id = self
            .client
            .create_session(&self.workspace, fork_from.as_deref())?;
        self.current_session = Some(session_id.clone());
        Ok(session_id)
    }

    fn require_current_session(&self) -> Result<String, String> {
        self.current_session
            .clone()
            .ok_or_else(|| "no current session; use /new or /resume <session_id>".to_string())
    }

    fn remember_payload_events(&mut self, payload: &Value) {
        let events = self.filter_new_events(events_from_payload(payload));
        self.pending_events.extend(events);
    }

    fn filter_new_events(&mut self, events: Vec<Value>) -> Vec<Value> {
        let mut output = Vec::new();
        for event in events {
            let sequence = event_sequence(&event);
            if sequence > self.last_global_event_id {
                self.last_global_event_id = sequence;
            }
            let key = event_identity_key(&event);
            if self.seen_events.insert(key) {
                output.push(event);
            }
        }
        output
    }

    fn turn_options(&self) -> Value {
        let mut value = json!({});
        if let Some(permission) = self
            .permission
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            value["permission"] = json!(permission);
        }
        if self.dangerously_skip_permissions {
            value["dangerously_skip_permissions"] = json!(true);
        }
        value
    }

    fn update_session_setting(
        &mut self,
        key: &str,
        value: &str,
    ) -> Result<Vec<TimelineLine>, String> {
        let session_id = self.require_current_session()?;
        let mut body = Map::new();
        body.insert(key.to_string(), json!(value));
        let payload = self
            .client
            .update_session(&session_id, Value::Object(body))?;
        Ok(vec![TimelineLine::new(
            "status",
            format!(
                "{key} set to {value}: {}",
                compact_json(&payload["session"])
            ),
            true,
        )])
    }

    fn handle_mcp_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
        let rest = command
            .strip_prefix("/mcp")
            .map(str::trim)
            .unwrap_or_default();
        let parts = rest.split_whitespace().collect::<Vec<_>>();
        let action = parts.first().copied().unwrap_or("list");
        let refresh =
            matches!(action, "doctor" | "refresh") || parts.iter().any(|part| *part == "--refresh");
        match action {
            "" | "list" | "ls" | "doctor" | "refresh" => {
                let payload = self.client.mcp_status(refresh)?;
                Ok(mcp_status_lines(self.client.server_url(), &payload))
            }
            "show" | "debug" => {
                let Some(name) = mcp_command_server_name(&parts, action) else {
                    return Ok(vec![TimelineLine::new(
                        "warning",
                        format!("usage: /mcp {action} <server>"),
                        true,
                    )]);
                };
                let payload = self.client.mcp_status(refresh)?;
                Ok(mcp_show_lines(self.client.server_url(), &name, &payload))
            }
            "test" => {
                let Some(name) = mcp_command_server_name(&parts, action) else {
                    return Ok(vec![TimelineLine::new(
                        "warning",
                        "usage: /mcp test <server>",
                        true,
                    )]);
                };
                let payload = self.client.mcp_server_test(&name)?;
                Ok(mcp_status_lines(self.client.server_url(), &payload))
            }
            "start" | "stop" | "restart" => {
                let Some(name) = mcp_command_server_name(&parts, action) else {
                    return Ok(vec![TimelineLine::new(
                        "warning",
                        format!("usage: /mcp {action} <server>"),
                        true,
                    )]);
                };
                let payload = self.client.mcp_server_lifecycle(&name, action)?;
                let mut lines = vec![TimelineLine::new(
                    "status",
                    format!("mcp {action} {name}"),
                    true,
                )];
                lines.extend(mcp_status_lines(self.client.server_url(), &payload));
                Ok(lines)
            }
            "enable" | "disable" => {
                let Some(name) = mcp_command_server_name(&parts, action) else {
                    return Ok(vec![TimelineLine::new(
                        "warning",
                        format!("usage: /mcp {action} <server>"),
                        true,
                    )]);
                };
                let payload = self
                    .client
                    .mcp_server_update(&name, json!({"enabled": action == "enable"}))?;
                let mut lines = vec![TimelineLine::new(
                    "status",
                    format!("mcp {action} {name}"),
                    true,
                )];
                lines.extend(mcp_status_lines(self.client.server_url(), &payload));
                Ok(lines)
            }
            "help" => Ok(vec![TimelineLine::new(
                "status",
                "usage: /mcp [list|doctor|show <server>|test <server>|start <server>|stop <server>|restart <server>|enable <server>|disable <server>]",
                true,
            )]),
            _ => Ok(vec![TimelineLine::new(
                "warning",
                format!("unknown MCP command: /mcp {action}"),
                true,
            )]),
        }
    }
}

fn mcp_command_server_name(parts: &[&str], action: &str) -> Option<String> {
    parts
        .iter()
        .copied()
        .skip(1)
        .filter(|part| !part.starts_with("--"))
        .find(|part| *part != action)
        .map(ToString::to_string)
}

fn mcp_status_lines(server_url: &str, payload: &Value) -> Vec<TimelineLine> {
    let servers = payload
        .get("servers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if servers.is_empty() {
        return vec![TimelineLine::new(
            "status",
            format!("mcp servers: none ({server_url})"),
            false,
        )];
    }
    let mut lines = vec![TimelineLine::new(
        "status",
        format!("mcp servers: {} ({server_url})", servers.len()),
        true,
    )];
    lines.extend(
        servers
            .iter()
            .map(|server| TimelineLine::new("mcp", mcp_server_summary(server), true)),
    );
    lines
}

fn mcp_show_lines(server_url: &str, name: &str, payload: &Value) -> Vec<TimelineLine> {
    let server = payload
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server.get("name").and_then(Value::as_str) == Some(name))
        });
    let Some(server) = server else {
        return vec![TimelineLine::new(
            "warning",
            format!("mcp server not found: {name} ({server_url})"),
            true,
        )];
    };
    let mut lines = vec![TimelineLine::new(
        "mcp",
        format!("mcp server: {}", mcp_server_summary(server)),
        true,
    )];
    let tools = server
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !tools.is_empty() {
        lines.push(TimelineLine::new(
            "mcp",
            format!(
                "tools: {}",
                tools
                    .iter()
                    .filter_map(|tool| {
                        tool.get("name")
                            .and_then(Value::as_str)
                            .or_else(|| tool.get("title").and_then(Value::as_str))
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            false,
        ));
    }
    lines
}

fn mcp_server_summary(server: &Value) -> String {
    format!(
        "{} enabled={} status={} transport={} tools={} lifecycle={} pid={} endpoint={}",
        server.get("name").and_then(Value::as_str).unwrap_or("mcp"),
        if server
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "yes"
        } else {
            "no"
        },
        server.get("status").and_then(Value::as_str).unwrap_or("-"),
        server
            .get("selected_transport")
            .and_then(Value::as_str)
            .or_else(|| server.get("transport").and_then(Value::as_str))
            .unwrap_or("-"),
        server
            .get("tool_count")
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        server
            .get("lifecycle_status")
            .and_then(Value::as_str)
            .unwrap_or("-"),
        server
            .get("lifecycle_pid")
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        mcp_endpoint_label(server),
    )
}

fn mcp_endpoint_label(server: &Value) -> String {
    if let Some(command) = server
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        let args_count = server
            .get("args_count")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if args_count == 0 {
            command.to_string()
        } else {
            format!("{command} +{args_count}")
        }
    } else if server
        .get("remote_url_configured")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "remote URL".to_string()
    } else {
        "-".to_string()
    }
}

impl TerminalEventHandler for AppBridgeTerminalHandler {
    fn initial_lines(&mut self) -> Vec<TimelineLine> {
        let mut lines = vec![TimelineLine::new(
            "status",
            format!("connected to {}", self.client.server_url()),
            true,
        )];
        if let Some(session_id) = self.current_session.as_deref() {
            lines.push(TimelineLine::new(
                "status",
                format!("current session: {session_id}"),
                true,
            ));
        }
        match self.client.list_sessions() {
            Ok(sessions) if sessions.is_empty() => {
                lines.push(TimelineLine::new("status", "remote sessions: none", false));
            }
            Ok(sessions) => lines.extend(session_list_lines(&sessions)),
            Err(error) => lines.push(TimelineLine::new("warning", error, true)),
        }
        lines
    }

    fn poll_app_events(&mut self) -> Result<Vec<Value>, String> {
        let events = self.client.global_events(self.last_global_event_id)?;
        Ok(self.filter_new_events(events))
    }

    fn poll_control_request(&mut self) -> Result<Option<Value>, String> {
        let request = self.client.next_tui_control()?;
        let path = request
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if path.is_empty() {
            return Ok(None);
        }
        Ok(Some(request))
    }

    fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
        self.client.record_tui_control_response(payload).map(|_| ())
    }

    fn drain_app_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.pending_events)
    }

    fn search_files(&mut self, query: &str) -> Result<Vec<ComposerFileCandidate>, String> {
        Ok(fuzzy_find_files(&self.workspace, query, 20)
            .into_iter()
            .map(composer_candidate_from_match)
            .collect())
    }

    fn search_sessions(&mut self, query: &str) -> Result<Vec<Value>, String> {
        self.client.search_sessions(query)
    }

    fn list_models(&mut self) -> Result<Value, String> {
        self.client.models()
    }

    fn list_agents(&mut self) -> Result<Value, String> {
        self.client.agents()
    }

    fn handle_submit(&mut self, prompt: &str) -> Result<Vec<TimelineLine>, String> {
        let session_id = self.ensure_session()?;
        let mut lines = Vec::new();
        let mut options = self.turn_options();
        let outbound_prompt = if let Some(command) = prompt
            .trim()
            .strip_prefix('!')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            options["tool_call"] = json!({
                "call_id": format!("call_bash_{}", std::process::id()),
                "name": "bash",
                "input": {"command": command},
            });
            lines.push(TimelineLine::new(
                "status",
                format!("bash tool queued: {command}"),
                true,
            ));
            format!("Run shell command:\n{command}")
        } else {
            let expanded = expand_file_attachments(&self.workspace, prompt);
            lines.extend(expanded.lines);
            expanded.prompt
        };
        let payload = self
            .client
            .start_turn(&session_id, &outbound_prompt, options)?;
        self.last_turn_id = turn_id_from_payload(&payload).or_else(|| self.last_turn_id.clone());
        self.remember_payload_events(&payload);
        Ok(lines)
    }

    fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
        if command == "/connect" {
            return Ok(vec![TimelineLine::new(
                "warning",
                "usage: /connect <server_url>",
                true,
            )]);
        }
        if let Some(url) = command.strip_prefix("/connect ").map(str::trim) {
            if url.is_empty() {
                return Ok(vec![TimelineLine::new(
                    "warning",
                    "usage: /connect <server_url>",
                    true,
                )]);
            }
            let client = RemoteRuntimeClient::new(url)
                .with_auth(self.client.auth().clone())
                .with_timeout(Duration::from_secs(3));
            client.health()?;
            self.client = client;
            self.current_session = None;
            self.last_turn_id = None;
            self.last_global_event_id = 0;
            self.seen_events.clear();
            return Ok(vec![TimelineLine::new(
                "status",
                format!("connected to {}", self.client.server_url()),
                true,
            )]);
        }
        if command == "/sessions" || command.starts_with("/sessions ") {
            let query = command
                .strip_prefix("/sessions")
                .map(str::trim)
                .unwrap_or_default();
            return self.client.search_sessions(query).map(|sessions| {
                if sessions.is_empty() {
                    vec![TimelineLine::new("status", "remote sessions: none", false)]
                } else {
                    session_list_lines(&sessions)
                }
            });
        }
        if let Some(session_id) = command.strip_prefix("/resume ").map(str::trim) {
            if session_id.is_empty() {
                return Ok(vec![TimelineLine::new(
                    "warning",
                    "usage: /resume <session_id>",
                    true,
                )]);
            }
            self.current_session = Some(session_id.to_string());
            return Ok(vec![TimelineLine::new(
                "status",
                format!("current session: {session_id}"),
                true,
            )]);
        }
        if command == "/transcript" || command.starts_with("/transcript ") {
            let raw_limit = command.strip_prefix("/transcript ").map(str::trim);
            let limit = match raw_limit {
                Some("") | None => None,
                Some(value) => match value.parse::<usize>() {
                    Ok(limit) => Some(limit),
                    Err(_) => {
                        return Ok(vec![TimelineLine::new(
                            "warning",
                            "usage: /transcript [limit]",
                            true,
                        )]);
                    }
                },
            };
            let session_id = self.require_current_session()?;
            let payload = self.client.session_messages(&session_id, limit)?;
            return Ok(transcript_lines(&payload));
        }
        if let Some(title) = command.strip_prefix("/rename ").map(str::trim) {
            if title.is_empty() {
                return Ok(vec![TimelineLine::new(
                    "warning",
                    "usage: /rename <title>",
                    true,
                )]);
            }
            let session_id = self.require_current_session()?;
            let payload = self
                .client
                .update_session(&session_id, json!({"title": title}))?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!("renamed session: {}", compact_json(&payload["session"])),
                true,
            )]);
        }
        if command == "/archive" || command == "/unarchive" {
            let session_id = self.require_current_session()?;
            let archived = command == "/archive";
            let payload = self
                .client
                .update_session(&session_id, json!({"archived": archived}))?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!(
                    "{} session: {}",
                    if archived { "archived" } else { "unarchived" },
                    compact_json(&payload["session"])
                ),
                true,
            )]);
        }
        if command == "/delete" {
            let session_id = self.require_current_session()?;
            let payload = self.client.delete_session(&session_id)?;
            self.current_session = None;
            return Ok(vec![TimelineLine::new(
                "warning",
                format!("deleted session: {}", compact_json(&payload)),
                true,
            )]);
        }
        if command == "/new" {
            let session_id = self.start_new_session(None)?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!("created session: {session_id}"),
                true,
            )]);
        }
        if command == "/fork" {
            let Some(base) = self.current_session.clone() else {
                return Ok(vec![TimelineLine::new(
                    "warning",
                    "no current session to fork; use /new or /resume <session_id>",
                    true,
                )]);
            };
            let session_id = self.start_new_session(Some(base))?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!("forked session: {session_id}"),
                true,
            )]);
        }
        if command == "/children" {
            let session_id = self.require_current_session()?;
            let children = self.client.children(&session_id)?;
            if children.is_empty() {
                return Ok(vec![TimelineLine::new(
                    "status",
                    "child sessions: none",
                    false,
                )]);
            }
            return Ok(session_list_lines(&children));
        }
        if command == "/parent" {
            let session_id = self.require_current_session()?;
            let payload = self.client.get_session(&session_id)?;
            let parent = payload
                .get("metadata")
                .and_then(|metadata| {
                    metadata
                        .get("parent_session_id")
                        .or_else(|| metadata.get("forked_from"))
                })
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let Some(parent) = parent else {
                return Ok(vec![TimelineLine::new(
                    "status",
                    "current session has no parent",
                    false,
                )]);
            };
            self.current_session = Some(parent.to_string());
            return Ok(vec![TimelineLine::new(
                "status",
                format!("current session: {parent}"),
                true,
            )]);
        }
        if command == "/share" {
            let session_id = self.require_current_session()?;
            let payload = self.client.share_session(&session_id)?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!("shared session: {}", compact_json(&payload)),
                true,
            )]);
        }
        if command == "/unshare" {
            let session_id = self.require_current_session()?;
            let payload = self.client.unshare_session(&session_id)?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!("unshared session: {}", compact_json(&payload)),
                true,
            )]);
        }
        if command == "/compact" {
            let session_id = self.require_current_session()?;
            let payload = self.client.compact_session(&session_id)?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!("compacted session: {}", compact_json(&payload["summary"])),
                true,
            )]);
        }
        if command == "/details" {
            let session_id = self.require_current_session()?;
            let payload = self.client.session_diff(&session_id)?;
            return Ok(diff_detail_lines(&payload));
        }
        if command == "/undo" {
            let session_id = self.require_current_session()?;
            let payload = self.client.undo_session(&session_id)?;
            self.remember_payload_events(&payload);
            return Ok(patch_result_lines("undo", &payload));
        }
        if command == "/redo" {
            let session_id = self.require_current_session()?;
            let payload = self.client.redo_session(&session_id)?;
            self.remember_payload_events(&payload);
            return Ok(patch_result_lines("redo", &payload));
        }
        if command == "/export" {
            let session_id = self.require_current_session()?;
            let payload = self.client.get_session(&session_id)?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!("session export: {}", compact_json(&payload)),
                true,
            )]);
        }
        if command == "/init" {
            let created = initialize_openagent_project_files(&self.workspace)?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!("initialized project files: {}", created.join(", ")),
                true,
            )]);
        }
        if command == "/status" {
            let session = self
                .current_session
                .as_deref()
                .map(|session_id| self.client.get_session(session_id))
                .transpose()?;
            return Ok(vec![TimelineLine::new(
                "status",
                format!(
                    "server={} session={} turn={} {}",
                    self.client.server_url(),
                    self.current_session.as_deref().unwrap_or("-"),
                    self.last_turn_id.as_deref().unwrap_or("-"),
                    session
                        .as_ref()
                        .map(compact_json)
                        .unwrap_or_else(|| "{}".to_string())
                ),
                true,
            )]);
        }
        if command == "/files" || command.starts_with("/files ") {
            let query = command
                .strip_prefix("/files")
                .map(str::trim)
                .unwrap_or_default();
            let matches = fuzzy_find_files(&self.workspace, query, 20);
            return Ok(file_picker_lines(query, &matches));
        }
        if command == "/mcp" || command.starts_with("/mcp ") {
            return self.handle_mcp_command(command);
        }
        if command == "/models" || command.starts_with("/models ") {
            let model_id = command.strip_prefix("/models ").map(str::trim);
            if let Some(model_id) = model_id.filter(|value| !value.is_empty()) {
                return self.update_session_setting("model", model_id);
            }
            let payload = self.client.models()?;
            return Ok(model_list_lines(&payload));
        }
        if command == "/agents" {
            let payload = self.client.agents()?;
            return Ok(agent_list_lines(&payload));
        }
        if let Some(agent) = command.strip_prefix("/agent ").map(str::trim) {
            if agent.is_empty() {
                return Ok(vec![TimelineLine::new(
                    "warning",
                    "usage: /agent <id>",
                    true,
                )]);
            }
            return self.update_session_setting("agent", agent);
        }
        if let Some(variant) = command.strip_prefix("/variant ").map(str::trim) {
            if variant.is_empty() {
                return Ok(vec![TimelineLine::new(
                    "warning",
                    "usage: /variant <default|fast|balanced|deep>",
                    true,
                )]);
            }
            return self.update_session_setting("variant", variant);
        }
        if command == "/thinking" || command.starts_with("/thinking ") {
            let Some(thinking) = command.strip_prefix("/thinking ").map(str::trim) else {
                return Ok(vec![TimelineLine::new(
                    "status",
                    "thinking levels: off, low, medium, high",
                    false,
                )]);
            };
            if thinking.is_empty() {
                return Ok(vec![TimelineLine::new(
                    "warning",
                    "usage: /thinking <off|low|medium|high>",
                    true,
                )]);
            }
            return self.update_session_setting("thinking", thinking);
        }
        if command == "/interrupt" || command.starts_with("/interrupt ") {
            let turn_id = command
                .strip_prefix("/interrupt ")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| self.last_turn_id.clone());
            let Some(turn_id) = turn_id else {
                return Ok(vec![TimelineLine::new(
                    "warning",
                    "no turn to interrupt",
                    true,
                )]);
            };
            let payload = self.client.interrupt_turn(&turn_id)?;
            self.remember_payload_events(&payload);
            return Ok(Vec::new());
        }
        Ok(vec![TimelineLine::new(
            "status",
            "commands: /sessions [query], /resume <id>, /transcript [limit], /rename <title>, /new, /fork, /children, /parent, /archive, /delete, /share, /unshare, /compact, /status, /files [query], /attach <path[:range]>, /mcp [list|show|test|start|stop|restart|enable|disable], /models [id], /agents, /agent <id>, /variant <name>, /thinking <level>, /themes [name], /theme-scheme [system|light|dark|cycle], /config, /keybinds, /interrupt [turn_id], /allow, /deny, /answer, /dismiss, /exit",
            false,
        )])
    }

    fn handle_approval_response(&mut self, payload: &Value) -> Result<Vec<TimelineLine>, String> {
        let response = self.client.respond_approval(payload)?;
        self.remember_payload_events(&response);
        Ok(Vec::new())
    }

    fn handle_question_response(&mut self, payload: &Value) -> Result<Vec<TimelineLine>, String> {
        let response = self.client.respond_question(payload)?;
        self.remember_payload_events(&response);
        Ok(Vec::new())
    }
}

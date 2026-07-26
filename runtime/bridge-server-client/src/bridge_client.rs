//! Bridge API client-side state for the Rust rewrite.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    time::Duration,
};

use openagent_bridge_server::BridgeEvent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

const TERMINAL_METHODS: &[&str] = &["turn/completed", "turn/failed", "turn/interrupted"];
const TERMINAL_STATUSES: &[&str] = &["completed", "failed", "interrupted"];

#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[must_use]
pub fn protocol_crate_name() -> &'static str {
    openagent_protocol::crate_name()
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RemoteEventKey {
    EventId(String),
    Global(u64),
    Turn {
        turn_id: String,
        sequence: u64,
        method: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteTurnRecord {
    pub id: String,
    pub session_id: String,
    pub status: String,
    pub final_answer: String,
    pub error: Option<String>,
    pub trace: Option<Value>,
    pub events: Vec<BridgeEvent>,
    seen_event_keys: BTreeSet<RemoteEventKey>,
}

impl RemoteTurnRecord {
    #[must_use]
    pub fn new(id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            status: "queued".to_string(),
            final_answer: String::new(),
            error: None,
            trace: None,
            events: Vec::new(),
            seen_event_keys: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn from_payload(payload: &Value, session_id: &str) -> Self {
        Self {
            id: string_field(payload, "id"),
            session_id: string_field(payload, "session_id")
                .if_empty_then(|| session_id.to_string()),
            status: string_field(payload, "status").if_empty_then(|| "queued".to_string()),
            final_answer: string_field(payload, "final_answer"),
            error: optional_string_field(payload, "error"),
            trace: payload
                .get("trace")
                .filter(|value| value.is_object())
                .cloned(),
            events: Vec::new(),
            seen_event_keys: BTreeSet::new(),
        }
    }

    pub fn append_event(&mut self, event: BridgeEvent) -> bool {
        let key = remote_event_key(&event, &self.id);
        if self.seen_event_keys.contains(&key) {
            return false;
        }
        self.seen_event_keys.insert(key);
        self.apply_event(&event);
        self.events.push(event);
        true
    }

    pub fn mark_failed(&mut self, error: impl Into<String>) {
        self.status = "failed".to_string();
        self.error = Some(error.into());
    }

    fn apply_event(&mut self, event: &BridgeEvent) {
        match event.method.as_str() {
            "turn/approval_requested" => {
                self.status = string_field(&event.params, "status")
                    .if_empty_then(|| "waiting_approval".to_string());
            }
            "turn/approval_resolved" | "turn/started" => {
                self.status =
                    string_field(&event.params, "status").if_empty_then(|| "running".to_string());
            }
            method if TERMINAL_METHODS.contains(&method) => {
                let default_status = match method {
                    "turn/completed" => "completed",
                    "turn/interrupted" => "interrupted",
                    _ => "failed",
                };
                self.status = string_field(&event.params, "status")
                    .if_empty_then(|| default_status.to_string());
                let final_answer = string_field(&event.params, "final_answer");
                if !final_answer.is_empty() {
                    self.final_answer = final_answer;
                }
                if let Some(error) = optional_string_field(&event.params, "error") {
                    self.error = Some(error);
                }
                if let Some(trace) = event.params.get("trace").filter(|value| value.is_object()) {
                    self.trace = Some(trace.clone());
                }
            }
            _ => {}
        }
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        TERMINAL_STATUSES.contains(&self.status.as_str())
    }
}

#[must_use]
pub fn normalize_server_url(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

#[must_use]
pub fn join_server_url(server_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        normalize_server_url(server_url),
        path.trim_start_matches('/')
    )
}

#[must_use]
pub fn quote_path(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        let ch = byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            output.push(ch);
        } else {
            output.push('%');
            output.push(hex_digit(byte >> 4));
            output.push(hex_digit(byte & 0x0f));
        }
    }
    output
}

#[must_use]
pub fn auth_header(token: Option<&str>) -> Option<String> {
    token
        .filter(|value| !value.is_empty())
        .map(|value| format!("Bearer {value}"))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteAuth {
    pub token: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl RemoteAuth {
    #[must_use]
    pub fn bearer(token: impl Into<String>) -> Self {
        Self {
            token: Some(token.into()),
            username: None,
            password: None,
        }
    }

    #[must_use]
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            token: None,
            username: Some(username.into()),
            password: Some(password.into()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteTurnAttachment {
    pub kind: String,
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub content_type: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub media_metadata: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_content_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included_content_bytes: Option<u64>,
}

fn bool_is_false(value: &bool) -> bool {
    !*value
}

impl RemoteTurnAttachment {
    #[must_use]
    pub fn new(
        kind: impl Into<String>,
        path: impl Into<String>,
        name: impl Into<String>,
        size_bytes: u64,
        content_type: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            path: path.into(),
            name: name.into(),
            size_bytes,
            content_type: content_type.into(),
            content: content.into(),
            source: None,
            page_count: None,
            media_metadata: BTreeMap::new(),
            truncated: false,
            truncation_reason: None,
            original_content_bytes: None,
            included_content_bytes: None,
        }
    }

    #[must_use]
    pub fn text_file(path: impl Into<String>, content: impl Into<String>) -> Self {
        let path = path.into();
        let content = content.into();
        let name = Path::new(&path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("attachment")
            .to_string();
        Self::new(
            "file",
            path,
            name,
            content.len() as u64,
            "text/plain",
            content,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteTurnRequest {
    pub input: String,
    pub attachments: Vec<RemoteTurnAttachment>,
    pub options: Value,
}

impl RemoteTurnRequest {
    #[must_use]
    pub fn new(input: impl Into<String>) -> Self {
        Self {
            input: input.into(),
            attachments: Vec::new(),
            options: json!({}),
        }
    }

    #[must_use]
    pub fn with_attachments(mut self, attachments: Vec<RemoteTurnAttachment>) -> Self {
        self.attachments = attachments;
        self
    }

    #[must_use]
    pub fn with_options(mut self, options: Value) -> Self {
        self.options = if options.is_object() {
            options
        } else {
            json!({})
        };
        self
    }

    #[must_use]
    pub fn to_payload(&self) -> Value {
        let mut payload = self.options.as_object().cloned().unwrap_or_default();
        payload.insert("input".to_string(), json!(self.input));
        if !self.attachments.is_empty() {
            payload.insert("attachments".to_string(), json!(self.attachments));
        }
        Value::Object(payload)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteRuntimeClient {
    server_url: String,
    auth: RemoteAuth,
    timeout: Duration,
}

impl RemoteRuntimeClient {
    #[must_use]
    pub fn new(server_url: impl Into<String>) -> Self {
        Self {
            server_url: normalize_server_url(&server_url.into()),
            auth: RemoteAuth::default(),
            timeout: Duration::from_secs(5),
        }
    }

    #[must_use]
    pub fn with_auth(mut self, auth: RemoteAuth) -> Self {
        self.auth = auth;
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    #[must_use]
    pub fn auth(&self) -> &RemoteAuth {
        &self.auth
    }

    pub fn health(&self) -> Result<Value, String> {
        self.json("GET", "/api/health", None)
    }

    pub fn protocol(&self) -> Result<Value, String> {
        self.json("GET", "/api/protocol", None)
    }

    pub fn models(&self) -> Result<Value, String> {
        self.json("GET", "/api/models", None)
    }

    pub fn provider_health(&self) -> Result<Value, String> {
        self.json("GET", "/api/models?check=true", None)
    }

    pub fn agents(&self) -> Result<Value, String> {
        self.json("GET", "/api/agents", None)
    }

    pub fn list_sessions(&self) -> Result<Vec<Value>, String> {
        let payload = self.json("GET", "/api/sessions", None)?;
        Ok(payload
            .get("sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn get_session(&self, session_id: &str) -> Result<Value, String> {
        self.json("GET", &format!("/api/sessions/{session_id}"), None)
    }

    pub fn session_messages(
        &self,
        session_id: &str,
        limit: Option<usize>,
    ) -> Result<Value, String> {
        let path = limit.map_or_else(
            || format!("/api/sessions/{session_id}/messages"),
            |limit| format!("/api/sessions/{session_id}/messages?limit={limit}"),
        );
        self.json("GET", &path, None)
    }

    pub fn session_context(&self, session_id: &str, limit: Option<usize>) -> Result<Value, String> {
        let path = limit.map_or_else(
            || format!("/api/sessions/{session_id}/context"),
            |limit| format!("/api/sessions/{session_id}/context?limit={limit}"),
        );
        self.json("GET", &path, None)
    }

    pub fn replay_session_context(
        &self,
        session_id: &str,
        run_id: Option<&str>,
        step: Option<u64>,
    ) -> Result<Value, String> {
        let mut body = json!({});
        if let Some(run_id) = run_id.filter(|value| !value.trim().is_empty()) {
            body["run_id"] = json!(run_id);
        }
        if let Some(step) = step {
            body["step"] = json!(step);
        }
        self.json(
            "POST",
            &format!("/api/sessions/{session_id}/context/replay"),
            Some(body),
        )
    }

    pub fn search_sessions(&self, query: &str) -> Result<Vec<Value>, String> {
        let path = if query.trim().is_empty() {
            "/api/sessions".to_string()
        } else {
            format!("/api/sessions?query={}", quote_path(query.trim()))
        };
        let payload = self.json("GET", &path, None)?;
        Ok(payload
            .get("sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn create_session(
        &self,
        workspace: &Path,
        fork_from: Option<&str>,
    ) -> Result<String, String> {
        let mut body = json!({"cwd": workspace.to_string_lossy()});
        if let Some(fork_from) = fork_from.filter(|value| !value.is_empty()) {
            body["fork_from"] = json!(fork_from);
        }
        let payload = self.json("POST", "/api/sessions", Some(body))?;
        session_id_from_payload(&payload)
            .ok_or_else(|| "server did not return a session id".to_string())
    }

    pub fn select_session(
        &self,
        explicit: Option<String>,
        continue_last: bool,
        fork: bool,
        workspace: &Path,
    ) -> Result<String, String> {
        if fork && explicit.is_none() && !continue_last {
            return Err("fork requires an explicit session or continue_last".to_string());
        }
        let base = if let Some(session_id) = explicit {
            Some(session_id)
        } else if continue_last {
            self.list_sessions()?
                .first()
                .and_then(session_id_from_payload)
        } else {
            None
        };
        if !fork && let Some(session_id) = base {
            return Ok(session_id);
        }
        self.create_session(workspace, base.as_deref())
    }

    pub fn start_turn(
        &self,
        session_id: &str,
        prompt: &str,
        extra: Value,
    ) -> Result<Value, String> {
        self.start_turn_request(
            session_id,
            &RemoteTurnRequest::new(prompt).with_options(extra),
        )
    }

    pub fn start_turn_request(
        &self,
        session_id: &str,
        request: &RemoteTurnRequest,
    ) -> Result<Value, String> {
        self.json(
            "POST",
            &format!("/api/sessions/{session_id}/turns"),
            Some(request.to_payload()),
        )
    }

    pub fn start_turn_async(
        &self,
        session_id: &str,
        prompt: &str,
        mut extra: Value,
    ) -> Result<Value, String> {
        if !extra.is_object() {
            extra = json!({});
        }
        extra["async"] = json!(true);
        self.start_turn(session_id, prompt, extra)
    }

    pub fn interrupt_turn(&self, turn_id: &str) -> Result<Value, String> {
        self.json("POST", &format!("/api/turns/{turn_id}/interrupt"), None)
    }

    pub fn turns(&self) -> Result<Value, String> {
        self.json("GET", "/api/turns", None)
    }

    pub fn turns_for_session(&self, session_id: &str) -> Result<Value, String> {
        self.json(
            "GET",
            &format!("/api/turns?session_id={}", quote_path(session_id)),
            None,
        )
    }

    pub fn turn_status(&self, turn_id: &str) -> Result<Value, String> {
        self.json("GET", &format!("/api/turns/{turn_id}"), None)
    }

    pub fn update_session(&self, session_id: &str, body: Value) -> Result<Value, String> {
        self.json("PATCH", &format!("/api/sessions/{session_id}"), Some(body))
    }

    pub fn delete_session(&self, session_id: &str) -> Result<Value, String> {
        self.json("DELETE", &format!("/api/sessions/{session_id}"), None)
    }

    pub fn children(&self, session_id: &str) -> Result<Vec<Value>, String> {
        let payload = self.json("GET", &format!("/api/sessions/{session_id}/children"), None)?;
        Ok(payload
            .get("children")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn tasks(&self, session_id: &str) -> Result<Vec<Value>, String> {
        let payload = self.tasks_payload(session_id)?;
        Ok(payload
            .get("tasks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub fn tasks_payload(&self, session_id: &str) -> Result<Value, String> {
        let payload = self.json("GET", &format!("/api/sessions/{session_id}/tasks"), None)?;
        Ok(payload)
    }

    pub fn run_task(&self, session_id: &str, task_id: &str, extra: Value) -> Result<Value, String> {
        self.json(
            "POST",
            &format!("/api/sessions/{session_id}/tasks/{task_id}/run"),
            Some(extra),
        )
    }

    pub fn start_task(&self, session_id: &str, task_id: &str) -> Result<Value, String> {
        self.json(
            "POST",
            &format!("/api/sessions/{session_id}/tasks/{task_id}/start"),
            None,
        )
    }

    pub fn wait_task(
        &self,
        session_id: &str,
        task_id: &str,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        self.json(
            "POST",
            &format!("/api/sessions/{session_id}/tasks/{task_id}/wait"),
            Some(json!({"timeout_ms": timeout_ms})),
        )
    }

    pub fn promote_task(&self, session_id: &str, task_id: &str) -> Result<Value, String> {
        self.json(
            "POST",
            &format!("/api/sessions/{session_id}/tasks/{task_id}/promote"),
            None,
        )
    }

    pub fn cancel_task(&self, session_id: &str, task_id: &str) -> Result<Value, String> {
        self.json(
            "POST",
            &format!("/api/sessions/{session_id}/tasks/{task_id}/cancel"),
            None,
        )
    }

    pub fn resume_task(&self, session_id: &str, task_id: &str) -> Result<Value, String> {
        self.json(
            "POST",
            &format!("/api/sessions/{session_id}/tasks/{task_id}/resume"),
            None,
        )
    }

    pub fn share_session(&self, session_id: &str) -> Result<Value, String> {
        self.json("POST", &format!("/api/sessions/{session_id}/share"), None)
    }

    pub fn unshare_session(&self, session_id: &str) -> Result<Value, String> {
        self.json("DELETE", &format!("/api/sessions/{session_id}/share"), None)
    }

    pub fn compact_session(&self, session_id: &str) -> Result<Value, String> {
        self.json("POST", &format!("/api/sessions/{session_id}/compact"), None)
    }

    pub fn session_diff(&self, session_id: &str) -> Result<Value, String> {
        self.json("GET", &format!("/api/sessions/{session_id}/diff"), None)
    }

    pub fn git_status(&self, path: Option<&str>) -> Result<Value, String> {
        let path = path
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("?path={}", quote_path(value)))
            .unwrap_or_default();
        self.json("GET", &format!("/api/git{path}"), None)
    }

    pub fn undo_session_file(&self, session_id: &str, path: &str) -> Result<Value, String> {
        self.undo_session_file_for_run(session_id, path, None)
    }

    pub fn undo_session_file_for_run(
        &self,
        session_id: &str,
        path: &str,
        run_id: Option<&str>,
    ) -> Result<Value, String> {
        let mut body = json!({"path": path});
        if let Some(run_id) = run_id.filter(|value| !value.trim().is_empty()) {
            body["run_id"] = json!(run_id);
        }
        self.json(
            "POST",
            &format!("/api/sessions/{session_id}/files/undo"),
            Some(body),
        )
    }

    pub fn undo_session(&self, session_id: &str) -> Result<Value, String> {
        self.json("POST", &format!("/api/sessions/{session_id}/undo"), None)
    }

    pub fn redo_session(&self, session_id: &str) -> Result<Value, String> {
        self.json("POST", &format!("/api/sessions/{session_id}/redo"), None)
    }

    pub fn terminal_run(
        &self,
        command: &str,
        cwd: Option<&Path>,
        timeout_ms: Option<u64>,
    ) -> Result<Value, String> {
        let mut body = json!({"command": command});
        if let Some(cwd) = cwd {
            body["cwd"] = json!(cwd.to_string_lossy());
        }
        if let Some(timeout_ms) = timeout_ms {
            body["timeout_ms"] = json!(timeout_ms);
        }
        self.json("POST", "/api/terminal/run", Some(body))
    }

    pub fn mcp_status(&self, refresh: bool) -> Result<Value, String> {
        let path = if refresh {
            "/api/mcp?refresh=true"
        } else {
            "/api/mcp"
        };
        self.json("GET", path, None)
    }

    pub fn mcp_server_test(&self, name: &str) -> Result<Value, String> {
        self.json(
            "POST",
            &format!("/api/mcp/servers/{}/test", quote_path(name)),
            None,
        )
    }

    pub fn mcp_server_lifecycle(&self, name: &str, action: &str) -> Result<Value, String> {
        if !matches!(action, "start" | "stop" | "restart") {
            return Err(format!("unsupported MCP lifecycle action: {action}"));
        }
        self.json(
            "POST",
            &format!("/api/mcp/servers/{}/{}", quote_path(name), action),
            None,
        )
    }

    pub fn mcp_server_update(&self, name: &str, body: Value) -> Result<Value, String> {
        self.json(
            "PATCH",
            &format!("/api/mcp/servers/{}", quote_path(name)),
            Some(body),
        )
    }

    pub fn turn_events(&self, turn_id: &str, last_event_id: u64) -> Result<Vec<Value>, String> {
        let path = if last_event_id == 0 {
            format!("/api/turns/{turn_id}/events")
        } else {
            format!("/api/turns/{turn_id}/events?last_event_id={last_event_id}")
        };
        self.sse_events(&path)
    }

    pub fn global_events(&self, last_event_id: u64) -> Result<Vec<Value>, String> {
        self.sse_events(&format!("/api/events?last_event_id={last_event_id}"))
    }

    pub fn global_events_live(
        &self,
        last_event_id: u64,
        live_timeout: Duration,
    ) -> Result<Vec<Value>, String> {
        self.sse_events_live(
            &format!(
                "/api/events?last_event_id={last_event_id}&live_timeout_ms={}",
                live_timeout.as_millis()
            ),
            live_timeout,
        )
    }

    pub fn global_events_live_stream<F>(
        &self,
        last_event_id: u64,
        live_timeout: Duration,
        on_event: F,
    ) -> Result<(), String>
    where
        F: FnMut(Value) -> Result<(), String>,
    {
        self.sse_events_live_stream(
            &format!(
                "/api/events?last_event_id={last_event_id}&live_timeout_ms={}",
                live_timeout.as_millis()
            ),
            live_timeout,
            on_event,
        )
    }

    pub fn turn_events_live(
        &self,
        turn_id: &str,
        last_event_id: u64,
        live_timeout: Duration,
    ) -> Result<Vec<Value>, String> {
        self.sse_events_live(
            &format!(
                "/api/turns/{turn_id}/events?last_event_id={last_event_id}&live_timeout_ms={}",
                live_timeout.as_millis()
            ),
            live_timeout,
        )
    }

    pub fn turn_events_live_stream<F>(
        &self,
        turn_id: &str,
        last_event_id: u64,
        live_timeout: Duration,
        on_event: F,
    ) -> Result<(), String>
    where
        F: FnMut(Value) -> Result<(), String>,
    {
        self.sse_events_live_stream(
            &format!(
                "/api/turns/{turn_id}/events?last_event_id={last_event_id}&live_timeout_ms={}",
                live_timeout.as_millis()
            ),
            live_timeout,
            on_event,
        )
    }

    pub fn respond_approval(&self, payload: &Value) -> Result<Value, String> {
        let turn_id = string_field(payload, "turn_id");
        let request_id = string_field(payload, "request_id");
        if turn_id.is_empty() || request_id.is_empty() {
            return Err("approval response requires turn_id and request_id".to_string());
        }
        self.json(
            "POST",
            &format!("/api/turns/{turn_id}/approvals/{request_id}"),
            Some(payload.clone()),
        )
    }

    pub fn respond_question(&self, payload: &Value) -> Result<Value, String> {
        let turn_id = string_field(payload, "turn_id");
        let request_id = string_field(payload, "request_id");
        if turn_id.is_empty() || request_id.is_empty() {
            return Err("question response requires turn_id and request_id".to_string());
        }
        self.json(
            "POST",
            &format!("/api/turns/{turn_id}/questions/{request_id}/reply"),
            Some(payload.clone()),
        )
    }

    pub fn next_tui_control(&self) -> Result<Value, String> {
        self.json("GET", "/tui/control/next", None)
    }

    pub fn record_tui_control_response(&self, payload: &Value) -> Result<Value, String> {
        self.json("POST", "/tui/control/response", Some(payload.clone()))
    }

    pub fn json(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let raw = self.text(method, path, body)?;
        serde_json::from_str(&raw).map_err(|error| format!("server response was not JSON: {error}"))
    }

    pub fn sse_events(&self, path: &str) -> Result<Vec<Value>, String> {
        let raw = self.text("GET", path, None)?;
        parse_sse_response_lines(&raw.lines().collect::<Vec<_>>())
    }

    pub fn sse_events_live(
        &self,
        path: &str,
        live_timeout: Duration,
    ) -> Result<Vec<Value>, String> {
        let padded = live_timeout.saturating_add(Duration::from_secs(1));
        let timeout = if self.timeout > padded {
            self.timeout
        } else {
            padded
        };
        let raw = self.text_with_options("GET", path, None, Some("text/event-stream"), timeout)?;
        parse_sse_response_lines(&raw.lines().collect::<Vec<_>>())
    }

    pub fn sse_events_live_stream<F>(
        &self,
        path: &str,
        live_timeout: Duration,
        on_event: F,
    ) -> Result<(), String>
    where
        F: FnMut(Value) -> Result<(), String>,
    {
        let padded = live_timeout.saturating_add(Duration::from_secs(1));
        let timeout = if self.timeout > padded {
            self.timeout
        } else {
            padded
        };
        let response =
            self.send_with_options("GET", path, None, Some("text/event-stream"), timeout)?;
        read_sse_response_stream(response, on_event)
    }

    pub fn text(&self, method: &str, path: &str, body: Option<Value>) -> Result<String, String> {
        self.text_with_options(method, path, body, None, self.timeout)
    }

    fn text_with_options(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        accept: Option<&str>,
        timeout: Duration,
    ) -> Result<String, String> {
        let response = self.send_with_options(method, path, body, accept, timeout)?;
        response.text().map_err(|error| error.to_string())
    }

    fn send_with_options(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        accept: Option<&str>,
        timeout: Duration,
    ) -> Result<reqwest::blocking::Response, String> {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(timeout)
            .build()
            .map_err(|error| error.to_string())?;
        let url = join_server_url(&self.server_url, path);
        let mut request = match method {
            "DELETE" => client.delete(url),
            "GET" => client.get(url),
            "PATCH" => client.patch(url),
            "POST" => client.post(url),
            other => return Err(format!("unsupported HTTP method: {other}")),
        };
        if let Some(token) = self.auth.token.as_deref().filter(|value| !value.is_empty()) {
            request = request.bearer_auth(token);
        } else if let Some(password) = self
            .auth
            .password
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            request = request.basic_auth(
                self.auth.username.as_deref().unwrap_or("openagent"),
                Some(password),
            );
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some(accept) = accept {
            request = request.header("accept", accept);
        }
        let response = request.send().map_err(|error| {
            format!(
                "{method} {} failed: {error}",
                join_server_url(&self.server_url, path)
            )
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let raw = response.text().map_err(|error| error.to_string())?;
        Err(format!(
            "server returned HTTP {} for {method} {path}: {}",
            status.as_u16(),
            summarize_http_error_body(&raw, &content_type)
        ))
    }
}

pub fn read_sse_response_stream<R, F>(mut reader: R, mut on_event: F) -> Result<(), String>
where
    R: Read,
    F: FnMut(Value) -> Result<(), String>,
{
    let mut raw = String::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("SSE read failed: {error}"))?;
        if read == 0 {
            break;
        }
        raw.push_str(&String::from_utf8_lossy(&buffer[..read]));
        while let Some(index) = sse_frame_end(&raw) {
            let frame = raw[..index].to_string();
            let drain_to = if raw[index..].starts_with("\r\n\r\n") {
                index + 4
            } else {
                index + 2
            };
            raw.drain(..drain_to);
            if let Some(event) = parse_sse_frame_json(&frame)? {
                on_event(event)?;
            }
        }
    }
    if !raw.trim().is_empty()
        && let Some(event) = parse_sse_frame_json(&raw)?
    {
        on_event(event)?;
    }
    Ok(())
}

fn sse_frame_end(raw: &str) -> Option<usize> {
    match (raw.find("\r\n\r\n"), raw.find("\n\n")) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    }
}

fn parse_sse_frame_json(frame: &str) -> Result<Option<Value>, String> {
    let mut data_lines = Vec::new();
    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    parse_sse_data(trimmed).map(Some)
}

pub fn parse_sse_response_lines(lines: &[&str]) -> Result<Vec<Value>, String> {
    let mut events = Vec::new();
    let mut data_lines: Vec<String> = Vec::new();
    for raw_line in lines {
        let line = raw_line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !data_lines.is_empty() {
                events.push(parse_sse_data(&data_lines.join("\n"))?);
                data_lines.clear();
            }
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }
    if !data_lines.is_empty() {
        events.push(parse_sse_data(&data_lines.join("\n"))?);
    }
    Ok(events)
}

pub fn parse_sse_data(data: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(data).map_err(|error| error.to_string())?;
    if !value.is_object() {
        return Err("SSE event data was not a JSON object".to_string());
    }
    Ok(value)
}

#[must_use]
pub fn session_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("session_id")
        .or_else(|| payload.get("id"))
        .or_else(|| payload.get("session").and_then(|session| session.get("id")))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[must_use]
pub fn turn_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("turn_id")
        .or_else(|| payload.get("turn").and_then(|turn| turn.get("id")))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[must_use]
pub fn events_from_payload(payload: &Value) -> Vec<Value> {
    payload
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[must_use]
pub fn event_sequence(event: &Value) -> u64 {
    event
        .get("global_sequence")
        .or_else(|| event.get("sequence"))
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

pub fn bridge_event_from_value(
    payload: &Value,
    default_sequence: u64,
) -> Result<BridgeEvent, String> {
    let sequence = payload
        .get("sequence")
        .and_then(Value::as_u64)
        .unwrap_or(default_sequence);
    let method = string_field(payload, "method");
    let params = payload.get("params").cloned().unwrap_or_else(|| json!({}));
    let created_at_ms = payload
        .get("created_at_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let global_sequence = payload.get("global_sequence").and_then(Value::as_u64);
    let event_id = payload
        .get("event_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    Ok(BridgeEvent {
        event_id,
        sequence,
        method,
        params,
        created_at_ms,
        global_sequence,
    })
}

#[must_use]
pub fn event_turn_id(event: &BridgeEvent) -> String {
    if let Some(value) = event.params.get("turn_id").and_then(Value::as_str)
        && !value.is_empty()
    {
        return value.to_string();
    }
    event
        .params
        .get("approval")
        .and_then(Value::as_object)
        .and_then(|approval| approval.get("turn_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[must_use]
pub fn event_session_id(event: &BridgeEvent) -> String {
    event
        .params
        .get("thread_id")
        .or_else(|| event.params.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[must_use]
pub fn remote_event_key(event: &BridgeEvent, default_turn_id: &str) -> RemoteEventKey {
    if let Some(event_id) = event.event_id.as_deref()
        && !event_id.is_empty()
    {
        return RemoteEventKey::EventId(event_id.to_string());
    }
    if let Some(global_sequence) = event.global_sequence {
        return RemoteEventKey::Global(global_sequence);
    }
    RemoteEventKey::Turn {
        turn_id: event_turn_id(event).if_empty_then(|| default_turn_id.to_string()),
        sequence: event.sequence,
        method: event.method.clone(),
    }
}

#[must_use]
pub fn remote_event_key_value(key: &RemoteEventKey) -> Value {
    match key {
        RemoteEventKey::EventId(event_id) => json!(["event_id", event_id]),
        RemoteEventKey::Global(sequence) => json!(["global", sequence]),
        RemoteEventKey::Turn {
            turn_id,
            sequence,
            method,
        } => json!(["turn", turn_id, sequence, method]),
    }
}

#[must_use]
pub fn request_shape(method: &str, path: &str, payload: Option<Value>) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("method".to_string(), Value::String(method.to_string()));
    object.insert("path".to_string(), Value::String(path.to_string()));
    if let Some(payload) = payload {
        object.insert("payload".to_string(), payload);
    }
    Value::Object(object)
}

#[must_use]
pub fn bridge_client_fixture() -> Value {
    let parsed_event = bridge_event_from_value(
        &json!({
            "sequence": 4,
            "global_sequence": 12,
            "event_id": "bridge_evt:session_existing:turn_remote:4",
            "method": "turn/completed",
            "params": {
                "thread_id": "session_existing",
                "turn_id": "turn_remote",
                "status": "completed",
                "final_answer": "hello remote",
                "trace": {"id": "trace_1"},
            },
            "created_at_ms": 1781842000304u64,
        }),
        99,
    )
    .unwrap_or_else(|_| BridgeEvent {
        event_id: None,
        sequence: 0,
        method: String::new(),
        params: json!({}),
        created_at_ms: 0,
        global_sequence: None,
    });
    let key = remote_event_key(&parsed_event, "turn_remote");
    let mut remote_turn = RemoteTurnRecord::new("turn_remote", "session_existing");
    let remote_events = vec![
        BridgeEvent {
            event_id: Some("bridge_evt:session_existing:turn_remote:1".to_string()),
            sequence: 1,
            global_sequence: Some(10),
            method: "turn/started".to_string(),
            params: json!({"thread_id": "session_existing", "turn_id": "turn_remote", "status": "running"}),
            created_at_ms: 1_781_842_000_301,
        },
        BridgeEvent {
            event_id: Some("bridge_evt:session_existing:turn_remote:2".to_string()),
            sequence: 2,
            global_sequence: Some(11),
            method: "turn/approval_requested".to_string(),
            params: json!({
                "thread_id": "session_existing",
                "turn_id": "turn_remote",
                "status": "waiting_approval",
                "approval": {"turn_id": "turn_remote", "request_id": "approval_1", "tool_name": "write"},
            }),
            created_at_ms: 1_781_842_000_302,
        },
        BridgeEvent {
            event_id: Some("bridge_evt:session_existing:turn_remote:3".to_string()),
            sequence: 3,
            global_sequence: None,
            method: "turn/approval_resolved".to_string(),
            params: json!({
                "thread_id": "session_existing",
                "turn_id": "turn_remote",
                "status": "running",
                "approval": {"turn_id": "turn_remote", "request_id": "approval_1", "action": "deny"},
            }),
            created_at_ms: 1_781_842_000_303,
        },
        parsed_event.clone(),
    ];
    let append_results = remote_events
        .into_iter()
        .map(|event| remote_turn.append_event(event))
        .collect::<Vec<_>>();
    let duplicate_result = remote_turn.append_event(BridgeEvent {
        event_id: Some("bridge_evt:session_existing:turn_remote:1".to_string()),
        sequence: 1,
        global_sequence: Some(10),
        method: "turn/started".to_string(),
        params: json!({"thread_id": "session_existing", "turn_id": "turn_remote", "status": "running"}),
        created_at_ms: 1_781_842_000_301,
    });
    let events = remote_turn
        .events
        .iter()
        .map(BridgeEvent::to_value)
        .collect::<Vec<_>>();

    json!({
        "helpers": {
            "normalize": normalize_server_url("http://127.0.0.1:8787/"),
            "join": join_server_url("http://127.0.0.1:8787/", "/api/sessions"),
            "quote": quote_path("turn/a b"),
            "auth_header": auth_header(Some("secret")).unwrap_or_default(),
        },
        "parsed_event": parsed_event.to_value(),
        "event_ids": {
            "turn": event_turn_id(&parsed_event),
            "session": event_session_id(&parsed_event),
            "key": remote_event_key_value(&key),
        },
        "remote_turn": {
            "append_results": append_results,
            "duplicate_result": duplicate_result,
            "status": remote_turn.status,
            "final_answer": remote_turn.final_answer,
            "trace": remote_turn.trace,
            "events": events,
        },
        "request_shapes": {
            "start_session": request_shape("POST", "/api/sessions", Some(json!({"cwd": "/tmp/openagent-rust-rewrite-fixture-goal11/workspace"}))),
            "start_turn": request_shape("POST", "/api/sessions/session_existing/turns", Some(json!({"input": "hello"}))),
            "turns": request_shape("GET", "/api/turns?session_id=session_existing", None),
            "interrupt": request_shape("POST", "/api/turns/turn_remote/interrupt", Some(json!({}))),
            "approval": request_shape("POST", "/api/turns/turn_remote/approvals/approval_1", Some(json!({"action": "deny"}))),
            "terminal_run": request_shape("POST", "/api/terminal/run", Some(json!({"command": "printf terminal-ok", "cwd": "/tmp/openagent-rust-rewrite-fixture-goal11/workspace", "timeout_ms": 1000}))),
            "mcp_status": request_shape("GET", "/api/mcp?refresh=true", None),
            "mcp_start": request_shape("POST", "/api/mcp/servers/local-tools/start", None),
            "mcp_test": request_shape("POST", "/api/mcp/servers/local-tools/test", None),
            "mcp_enable": request_shape("PATCH", "/api/mcp/servers/local-tools", Some(json!({"enabled": true}))),
            "control_next": request_shape("GET", "/tui/control/next?timeout=0.25", None),
            "control_response": request_shape("POST", "/tui/control/response", Some(json!({"ok": true, "result": {"applied": true}}))),
        },
    })
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'A' + (value - 10)) as char,
        _ => '0',
    }
}

fn string_field(payload: &Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn optional_string_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn summarize_http_error_body(raw: &str, content_type: &str) -> String {
    if raw.trim().is_empty() {
        return "empty response body".to_string();
    }
    if content_type.contains("json")
        && let Ok(value) = serde_json::from_str::<Value>(raw)
    {
        if let Some(error) = value
            .get("error")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            return error.to_string();
        }
        return value.to_string();
    }
    raw.lines().take(5).collect::<Vec<_>>().join("\n")
}

trait EmptyStringExt {
    fn if_empty_then<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self;
}

impl EmptyStringExt for String {
    fn if_empty_then<F>(self, fallback: F) -> Self
    where
        F: FnOnce() -> Self,
    {
        if self.is_empty() { fallback() } else { self }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use serde_json::json;

    use super::*;

    #[test]
    fn links_to_protocol_crate() {
        assert_eq!(crate_name(), "openagent-bridge-server-client");
        assert_eq!(protocol_crate_name(), "openagent-protocol");
    }

    #[test]
    fn structured_turn_request_keeps_context_inputs_separate_from_options() {
        let request = RemoteTurnRequest::new("Review the attached file.")
            .with_attachments(vec![RemoteTurnAttachment::text_file(
                "src/main.rs",
                "fn main() {}\n",
            )])
            .with_options(json!({
                "model": "gpt-5.5",
                "thinking": "high",
                "input": "must not override typed input",
                "attachments": [{"content": "must not override typed attachments"}],
            }));

        assert_eq!(
            request.to_payload(),
            json!({
                "input": "Review the attached file.",
                "attachments": [{
                    "kind": "file",
                    "path": "src/main.rs",
                    "name": "main.rs",
                    "size_bytes": 13,
                    "content_type": "text/plain",
                    "content": "fn main() {}\n",
                }],
                "model": "gpt-5.5",
                "thinking": "high",
            })
        );
    }

    #[test]
    fn session_context_uses_diagnostics_endpoint() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let server = thread::spawn(move || -> Result<(), String> {
            let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
            let mut request = String::new();
            let mut buffer = [0_u8; 512];
            loop {
                let read = stream
                    .read(&mut buffer)
                    .map_err(|error| error.to_string())?;
                if read == 0 {
                    return Err("client closed before request completed".to_string());
                }
                request.push_str(&String::from_utf8_lossy(&buffer[..read]));
                if request.contains("\r\n\r\n") {
                    break;
                }
            }
            if !request.starts_with("GET /api/sessions/session_1/context?limit=5 ") {
                return Err(format!("unexpected request line: {request}"));
            }
            let body = r#"{"schema_version":"openagent.context_diagnostics.v1","status":"ready"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .map_err(|error| error.to_string())?;
            stream.flush().map_err(|error| error.to_string())
        });

        let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"));
        let payload = client.session_context("session_1", Some(5))?;
        assert_eq!(payload["status"], "ready");
        let server_result = server
            .join()
            .map_err(|_| "server thread panicked".to_string())?;
        assert!(server_result.is_ok(), "{server_result:?}");
        Ok(())
    }

    #[test]
    fn replay_session_context_posts_target_without_side_effect_flags() -> Result<(), Box<dyn Error>>
    {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let server = thread::spawn(move || -> Result<(), String> {
            let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
            let mut request = String::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream
                    .read(&mut buffer)
                    .map_err(|error| error.to_string())?;
                if read == 0 {
                    return Err("client closed before request completed".to_string());
                }
                request.push_str(&String::from_utf8_lossy(&buffer[..read]));
                if request.contains("\r\n\r\n") && request.contains(r#""step":3"#) {
                    break;
                }
            }
            if !request.starts_with("POST /api/sessions/session_1/context/replay ") {
                return Err(format!("unexpected request line: {request}"));
            }
            if !request.contains(r#""run_id":"turn_1""#) {
                return Err(format!("missing replay run id: {request}"));
            }
            let body = r#"{"schema_version":"openagent.context_replay.v1","status":"verified","side_effects":{"provider_calls":0,"tool_calls":0}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .map_err(|error| error.to_string())?;
            stream.flush().map_err(|error| error.to_string())
        });

        let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"));
        let payload = client.replay_session_context("session_1", Some("turn_1"), Some(3))?;
        assert_eq!(payload["status"], "verified");
        assert_eq!(payload["side_effects"]["provider_calls"], 0);
        let server_result = server
            .join()
            .map_err(|_| "server thread panicked".to_string())?;
        assert!(server_result.is_ok(), "{server_result:?}");
        Ok(())
    }

    #[test]
    fn live_sse_stream_callback_receives_event_before_response_finishes()
    -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let server = thread::spawn(move || -> Result<(), String> {
            let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
            let mut request = String::new();
            let mut buffer = [0_u8; 512];
            loop {
                let read = stream
                    .read(&mut buffer)
                    .map_err(|error| error.to_string())?;
                if read == 0 {
                    return Err("client closed before request completed".to_string());
                }
                request.push_str(&String::from_utf8_lossy(&buffer[..read]));
                if request.contains("\r\n\r\n") {
                    break;
                }
            }
            if !request.starts_with("GET /api/events?last_event_id=0&live_timeout_ms=1200 ") {
                return Err(format!("unexpected request line: {request}"));
            }
            if !request.contains("Authorization: Bearer secret")
                && !request.contains("authorization: Bearer secret")
            {
                return Err(format!("missing bearer auth: {request}"));
            }
            if !request.contains("Accept: text/event-stream")
                && !request.contains("accept: text/event-stream")
            {
                return Err(format!("missing SSE accept: {request}"));
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\nconnection: close\r\n\r\n",
                )
                .map_err(|error| error.to_string())?;
            write_sse_event(
                &mut stream,
                json!({
                    "event_id": "evt_stream_delta",
                    "sequence": 1,
                    "global_sequence": 1,
                    "method": "item/agentMessage/delta",
                    "params": {"delta": "streamed early"},
                    "created_at_ms": 1,
                }),
            )?;
            stream.flush().map_err(|error| error.to_string())?;
            thread::sleep(Duration::from_millis(700));
            write_sse_event(
                &mut stream,
                json!({
                    "event_id": "evt_completed",
                    "sequence": 2,
                    "global_sequence": 2,
                    "method": "turn/completed",
                    "params": {"status": "completed", "final_answer": "streamed early"},
                    "created_at_ms": 2,
                }),
            )?;
            stream.flush().map_err(|error| error.to_string())?;
            Ok(())
        });

        let client = RemoteRuntimeClient::new(format!("http://127.0.0.1:{port}"))
            .with_auth(RemoteAuth::bearer("secret"));
        let (sender, receiver) = mpsc::channel();
        let start = Instant::now();
        let client_thread = thread::spawn(move || {
            client.global_events_live_stream(0, Duration::from_millis(1200), |event| {
                sender
                    .send((
                        event["method"].as_str().unwrap_or_default().to_string(),
                        start.elapsed(),
                    ))
                    .map_err(|error| error.to_string())
            })
        });

        let (first_method, first_elapsed) = receiver.recv_timeout(Duration::from_millis(400))?;
        assert_eq!(first_method, "item/agentMessage/delta");
        assert!(
            first_elapsed < Duration::from_millis(400),
            "first event was not delivered incrementally: {first_elapsed:?}"
        );
        let (second_method, _) = receiver.recv_timeout(Duration::from_millis(1200))?;
        assert_eq!(second_method, "turn/completed");
        let client_result = client_thread
            .join()
            .map_err(|_| "client thread panicked".to_string())?;
        assert!(client_result.is_ok(), "{client_result:?}");
        let server_result = server
            .join()
            .map_err(|_| "server thread panicked".to_string())?;
        assert!(server_result.is_ok(), "{server_result:?}");
        Ok(())
    }

    fn write_sse_event(stream: &mut impl Write, event: serde_json::Value) -> Result<(), String> {
        writeln!(
            stream,
            "event: {}",
            event["method"].as_str().unwrap_or("message")
        )
        .map_err(|error| error.to_string())?;
        writeln!(stream, "data: {event}\n").map_err(|error| error.to_string())
    }
}

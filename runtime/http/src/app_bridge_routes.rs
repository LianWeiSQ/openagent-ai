use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HttpResponseSpec {
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub headers: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
}

impl HttpResponseSpec {
    #[must_use]
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CliRunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[must_use]
pub fn health_payload(config: &HttpRuntimeConfig) -> Value {
    json!({
        "ok": true,
        "service": command_name(),
        "app_bridge": app_server_crate_name(),
        "ui_enabled": config.serve_static,
        "auth_required": config.auth_required(),
    })
}

#[must_use]
pub fn route_health() -> HttpResponseSpec {
    HttpResponseSpec {
        status: 200,
        content_type: Some("application/json; charset=utf-8".to_string()),
        headers: Map::new(),
        body: None,
        body_text: None,
    }
}

#[must_use]
pub fn route_unauthorized() -> HttpResponseSpec {
    let mut headers = Map::new();
    headers.insert(
        "WWW-Authenticate".to_string(),
        Value::String(
            "Bearer realm=\"openagent-app-bridge\", Basic realm=\"openagent-app-bridge\""
                .to_string(),
        ),
    );
    HttpResponseSpec {
        status: 401,
        content_type: None,
        headers,
        body: Some(json!({"error": "unauthorized"})),
        body_text: None,
    }
}

#[must_use]
pub fn route_options() -> HttpResponseSpec {
    let mut headers = Map::new();
    headers.insert(
        "Access-Control-Allow-Methods".to_string(),
        Value::String("GET, POST, PATCH, DELETE, OPTIONS".to_string()),
    );
    headers.insert(
        "Access-Control-Allow-Headers".to_string(),
        Value::String("Authorization, Content-Type, X-OpenAgent-Token".to_string()),
    );
    headers.insert(
        "Access-Control-Max-Age".to_string(),
        Value::String("600".to_string()),
    );
    HttpResponseSpec {
        status: 204,
        content_type: None,
        headers,
        body: None,
        body_text: None,
    }
}

#[must_use]
pub fn route_unknown() -> HttpResponseSpec {
    HttpResponseSpec {
        status: 404,
        content_type: None,
        headers: Map::new(),
        body: Some(json!({"error": "unknown endpoint"})),
        body_text: None,
    }
}

#[must_use]
pub fn app_bridge_protocol_payload() -> Value {
    json!({
        "schema_version": 1,
        "protocol": "openagent.app_bridge",
        "protocol_version": APP_BRIDGE_PROTOCOL_VERSION,
        "event_schema_version": APP_BRIDGE_EVENT_SCHEMA_VERSION,
        "compatibility": {
            "additive_fields": true,
            "required_event_fields": [
                "event_id",
                "schema_version",
                "protocol_version",
                "sequence",
                "method",
                "params",
                "created_at_ms"
            ],
            "global_event_fields": ["global_sequence"],
            "event_identity": {
                "field": "event_id",
                "format": "app_evt:{session_id}:{turn_id}:{sequence}",
                "stable_scope": "session + turn + sequence",
                "dedupe_preference": "event_id before global_sequence or sequence",
            },
            "sse": {
                "content_type": "text/event-stream",
                "id_field": "global_sequence for /api/events; sequence for /api/turns/{turn_id}/events",
                "event_field": "method",
                "data": "JSON AppEvent envelope",
                "resume_query": "last_event_id",
                "live_query": "live_timeout_ms",
            },
        },
        "endpoints": {
            "health": "GET /api/health",
            "protocol": "GET /api/protocol",
            "sessions": "GET|POST /api/sessions",
            "session": "GET|PATCH|DELETE /api/sessions/{session_id}",
            "messages": "GET /api/sessions/{session_id}/messages",
            "turns": "GET /api/turns; POST /api/sessions/{session_id}/turns; set body async=true or query ?async=true for 202 accepted background run",
            "turn": "GET /api/turns/{turn_id}",
            "global_events": "GET /api/events",
            "turn_events": "GET /api/turns/{turn_id}/events",
            "interrupt": "POST /api/turns/{turn_id}/interrupt",
            "approvals": "GET /api/approvals; POST /api/approvals/{request_id}; POST /api/turns/{turn_id}/approvals/{request_id}",
            "questions": "GET /api/questions; POST /api/questions/{request_id}/reply; POST /api/turns/{turn_id}/questions/{request_id}/reply",
            "diff": "GET /api/sessions/{session_id}/diff",
            "undo": "POST /api/sessions/{session_id}/undo",
            "redo": "POST /api/sessions/{session_id}/redo",
            "checkpoints": "GET /api/sessions/{session_id}/checkpoints; POST /api/sessions/{session_id}/checkpoints/{checkpoint_id}/restore",
            "files": "GET /api/files?path={path}&depth={depth}&content=true",
            "git": "GET /api/git",
            "lsp": "GET /api/lsp; GET /lsp; GET /api/lsp/doctor; POST /api/lsp/query",
            "terminal_run": "POST /api/terminal/run",
            "mcp": "GET /api/mcp; POST /api/mcp/servers; PATCH|DELETE /api/mcp/servers/{name}; POST /api/mcp/servers/{name}/test|start|stop|restart",
            "models": "GET /api/models",
            "agents": "GET /api/agents",
            "tui_control": "GET /tui/control/next; POST /tui/control/response; POST /tui/*",
        },
        "event_methods": [
            "turn/started",
            "item/agentMessage/delta",
            "item/toolCall/started",
            "item/toolCall/completed",
            "item/toolCall/failed",
            "item/question/requested",
            "item/question/resolved",
            "turn/approval_requested",
            "turn/approval_resolved",
            "checkpoint/created",
            "lsp.updated",
            "patch/detected",
            "turn/completed",
            "turn/failed",
            "turn/interrupted",
        ],
        "terminal_methods": ["turn/completed", "turn/failed", "turn/interrupted"],
    })
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
        if line.starts_with(':') {
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
pub fn format_http_error(method: &str, path: &str, code: u16, body: Option<&Value>) -> String {
    if let Some(error) = body
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        return format!("{method} {path} returned HTTP {code}: {error}");
    }
    format!("{method} {path} returned HTTP {code}")
}

#[must_use]
pub fn emit_app_bridge_events(
    events: &[Value],
    output_format: &str,
    verbose: bool,
) -> CliRunResult {
    let mut result = CliRunResult::default();
    let mut printed_answer = false;
    let mut status = "failed".to_string();
    let mut final_answer = String::new();

    for event in events {
        if output_format == "json" {
            result.stdout.push_str(&stable_json_dumps(event));
            result.stdout.push('\n');
        } else if emit_text_event(event, verbose, &mut result.stdout, &mut result.stderr) {
            printed_answer = true;
        }

        let method = event_method(event);
        let params = event_params(event);
        if matches!(
            method.as_str(),
            "turn/completed" | "turn/failed" | "turn/interrupted"
        ) {
            let default_status = match method.as_str() {
                "turn/completed" => "completed",
                "turn/interrupted" => "interrupted",
                _ => "failed",
            };
            status = params
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or(default_status)
                .to_string();
            final_answer = params
                .get("final_answer")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
    }

    if output_format == "text" {
        if printed_answer {
            result.stdout.push('\n');
        } else if !final_answer.is_empty() {
            result.stdout.push_str(&final_answer);
            result.stdout.push('\n');
        }
        if status != "completed" {
            result
                .stderr
                .push_str(&format!("OpenAgent client turn failed: {status}\n"));
        }
    }
    result.exit_code = if status == "completed" { 0 } else { 1 };
    result
}

#[must_use]
pub fn build_run_prompt(message: &str, files: &[(&str, &str)]) -> String {
    let mut parts = Vec::new();
    if !message.trim().is_empty() {
        parts.push(message.trim().to_string());
    }
    for (path, content) in files {
        parts.push(format!("Attached file: {path}\n\n```text\n{content}\n```"));
    }
    parts.join("\n\n").trim().to_string()
}

#[must_use]
pub fn command_text_from_args(message: &[&str], stdin: Option<&str>, stdin_is_tty: bool) -> String {
    let message = message.join(" ").trim().to_string();
    if !message.is_empty() {
        return message;
    }
    if stdin_is_tty {
        return String::new();
    }
    stdin.unwrap_or_default().trim().to_string()
}

#[must_use]
pub fn dockerfile_lines() -> Vec<&'static str> {
    vec![
        "FROM rust:1.85-bookworm AS builder",
        "WORKDIR /app",
        "COPY . .",
        "RUN cargo build --release -p openagent-http-runtime",
        "FROM debian:bookworm-slim",
        "COPY --from=builder /app/target/release/openagent-http-runtime /usr/local/bin/openagent-http-runtime",
        "EXPOSE 8787",
        "HEALTHCHECK CMD [\"openagent-http-runtime\", \"--health-json\"]",
        "ENTRYPOINT [\"openagent-http-runtime\"]",
        "CMD [\"--host\", \"0.0.0.0\", \"--port\", \"8787\", \"--headless\"]",
    ]
}

#[must_use]
pub fn docker_smoke_command() -> Vec<&'static str> {
    vec![
        "docker",
        "run",
        "--rm",
        "openagent-http-runtime:goal12",
        "--health-json",
    ]
}

#[must_use]
pub fn parse_cli_args(args: &[String]) -> (HttpRuntimeConfig, bool, bool) {
    let mut config = HttpRuntimeConfig::default();
    let mut health_json = false;
    let mut docker_smoke = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--host" => {
                if let Some(value) = args.get(index + 1) {
                    config.host = value.clone();
                    index += 1;
                }
            }
            "--port" => {
                if let Some(value) = args
                    .get(index + 1)
                    .and_then(|value| value.parse::<u16>().ok())
                {
                    config.port = value;
                    index += 1;
                }
            }
            "--workspace" => {
                if let Some(value) = args.get(index + 1) {
                    config.workspace = Some(value.clone());
                    index += 1;
                }
            }
            "--session-root" => {
                if let Some(value) = args.get(index + 1) {
                    config.session_store_root = Some(value.clone());
                    index += 1;
                }
            }
            "--mcp-config" => {
                if let Some(value) = args.get(index + 1) {
                    config.mcp_config = Some(value.clone());
                    index += 1;
                }
            }
            "--headless" => {
                config.serve_static = false;
            }
            "--auth-token" => {
                if let Some(value) = args.get(index + 1) {
                    config.auth_token = Some(value.clone());
                    index += 1;
                }
            }
            "--username" | "-u" => {
                if let Some(value) = args.get(index + 1) {
                    config.auth_username = Some(value.clone());
                    index += 1;
                }
            }
            "--password" | "-p" => {
                if let Some(value) = args.get(index + 1) {
                    config.auth_password = Some(value.clone());
                    index += 1;
                }
            }
            "--cors-origin" => {
                if let Some(value) = args.get(index + 1) {
                    config.cors_origin = value.clone();
                    index += 1;
                }
            }
            "--mdns-name" => {
                if let Some(value) = args.get(index + 1) {
                    config.mdns_name = Some(value.clone());
                    index += 1;
                }
            }
            "--max-queued-turns" | "--max-queued-turns-per-session" => {
                if let Some(value) = args
                    .get(index + 1)
                    .and_then(|value| value.parse::<usize>().ok())
                {
                    config.max_queued_turns_per_session = value;
                    index += 1;
                }
            }
            "--max-running-turn-workers" | "--max-turn-workers" => {
                if let Some(value) = args
                    .get(index + 1)
                    .and_then(|value| value.parse::<usize>().ok())
                {
                    config.max_running_turn_workers = value.max(1);
                    index += 1;
                }
            }
            "--turn-queue-lease-stale-ms" => {
                if let Some(value) = args
                    .get(index + 1)
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    config.turn_queue_lease_stale_ms = value.max(1);
                    index += 1;
                }
            }
            "--turn-queue-timeout-ms" | "--queue-timeout-ms" => {
                if let Some(value) = args
                    .get(index + 1)
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    config.turn_queue_timeout_ms = value.max(1);
                    index += 1;
                }
            }
            "--no-mdns" => {
                config.mdns_name = None;
            }
            "--health-json" => {
                health_json = true;
            }
            "--docker-smoke" => {
                docker_smoke = true;
            }
            _ => {}
        }
        index += 1;
    }
    (config, health_json, docker_smoke)
}

#[must_use]
pub fn run_cli(args: &[String]) -> CliRunResult {
    let (config, health_json, docker_smoke) = parse_cli_args(args);
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        return CliRunResult {
            exit_code: 0,
            stdout: "Usage: openagent-http-runtime [--host <host>] [--port <port>] [--workspace <path>] [--session-root <path>] [--mcp-config <json-or-path>] [--headless] [--auth-token <token>] [-u|--username <name>] [-p|--password <password>] [--cors-origin <origin>] [--mdns-name <name>] [--max-queued-turns-per-session <n>] [--turn-queue-lease-stale-ms <ms>] [--no-mdns] [--health-json]\n".to_string(),
            stderr: String::new(),
        };
    }
    if health_json || docker_smoke {
        let smoke_config = HttpRuntimeConfig {
            serve_static: false,
            auth_token: config.auth_token,
            ..HttpRuntimeConfig::default()
        };
        return CliRunResult {
            exit_code: 0,
            stdout: format!("{}\n", stable_json_dumps(&health_payload(&smoke_config))),
            stderr: String::new(),
        };
    }
    serve_blocking(config)
}

pub(super) fn serve_blocking(config: HttpRuntimeConfig) -> CliRunResult {
    let listener = match TcpListener::bind((config.host.as_str(), config.port)) {
        Ok(listener) => listener,
        Err(error) => {
            return CliRunResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("failed to bind HTTP runtime: {error}\n"),
            };
        }
    };
    let local = listener
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|_| format!("{}:{}", config.host, config.port));
    println!("openagent HTTP runtime listening on http://{local}");
    recover_and_start_persisted_queued_turns(&config);
    start_background_task_worker(config.clone());
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let config = config.clone();
                thread::spawn(move || {
                    let _ = handle_http_stream(&mut stream, &config);
                });
            }
            Err(error) => eprintln!("openagent HTTP runtime accept failed: {error}"),
        }
    }
    CliRunResult {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
    }
}

pub(super) fn start_background_task_worker(config: HttpRuntimeConfig) {
    if !background_task_worker_enabled() {
        return;
    }
    let _ = thread::spawn(move || {
        loop {
            if let Err(error) = run_background_task_worker_once(&config) {
                eprintln!("openagent background task worker failed: {error}");
            }
            thread::sleep(Duration::from_millis(background_task_worker_poll_ms()));
        }
    });
}

pub(super) fn background_task_worker_enabled() -> bool {
    std::env::var("OPENAGENT_BACKGROUND_WORKER")
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(true)
}

pub(super) fn background_task_worker_poll_ms() -> u64 {
    std::env::var("OPENAGENT_BACKGROUND_WORKER_POLL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_BACKGROUND_TASK_WORKER_POLL_MS)
        .max(10)
}

pub(super) fn run_background_task_worker_once(config: &HttpRuntimeConfig) -> Result<(), String> {
    for task in queued_background_task_ids(config)? {
        let payload = json!({"background_worker": true});
        match run_session_task_payload(
            config,
            &task.parent_session_id,
            &task.task_id,
            &payload.to_string(),
        ) {
            Ok(_) => {}
            Err(error)
                if error.contains("task is already running")
                    || error.contains("task is not queued") => {}
            Err(error) => eprintln!(
                "openagent background task worker could not run task {}: {}",
                task.task_id, error
            ),
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(super) struct QueuedBackgroundTask {
    parent_session_id: String,
    task_id: String,
    updated_at_ms: u64,
}

pub(super) fn queued_background_task_ids(
    config: &HttpRuntimeConfig,
) -> Result<Vec<QueuedBackgroundTask>, String> {
    let root = session_root(config);
    let mut tasks = Vec::new();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(tasks),
        Err(error) => return Err(format!("failed to read session root: {error}")),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let task_id = entry.file_name().to_string_lossy().to_string();
        let state = read_json_file(&path.join("state.latest.json"));
        let metadata = state
            .get("metadata")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let is_queued_background_subagent = metadata
            .get("subagent")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && metadata
                .get("background")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            && metadata
                .get("task_status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                == "queued";
        if !is_queued_background_subagent {
            continue;
        }
        let Some(parent_session_id) = metadata
            .get("parent_session_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        else {
            continue;
        };
        tasks.push(QueuedBackgroundTask {
            parent_session_id,
            task_id,
            updated_at_ms: state
                .get("updated_at_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        });
    }
    tasks.sort_by(|left, right| {
        left.updated_at_ms
            .cmp(&right.updated_at_ms)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    Ok(tasks)
}

pub(super) fn handle_http_stream(
    stream: &mut TcpStream,
    config: &HttpRuntimeConfig,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    let request = read_http_request(stream)?;
    if should_live_sse(&request, config) {
        return write_live_sse_response(stream, config, &request);
    }
    let response = route_http_request(&request, config);
    write_http_response(stream, with_runtime_headers(response, config))
}

#[derive(Clone, Debug)]
pub(super) struct HttpRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) headers: BTreeMap<String, String>,
    pub(super) body: String,
}

pub(super) fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 1024 * 1024 {
            return Err("request headers too large".to_string());
        }
    }
    let split = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| "invalid HTTP request".to_string())?;
    let head = String::from_utf8_lossy(&buffer[..split]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();
    let mut body_bytes = buffer[split..].to_vec();
    while body_bytes.len() < content_length {
        let read = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        body_bytes.extend_from_slice(&chunk[..read]);
    }
    body_bytes.truncate(content_length);
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body_bytes).to_string(),
    })
}

pub(super) fn route_http_request(
    request: &HttpRequest,
    config: &HttpRuntimeConfig,
) -> HttpResponseSpec {
    if request.method == "OPTIONS" {
        return route_options();
    }
    if !authorized(request, config) {
        return route_unauthorized();
    }
    let path = request.path.split('?').next().unwrap_or("/");
    match (request.method.as_str(), path) {
        ("GET", "/api/health") => json_response(200, health_payload(config)),
        ("GET", "/api/protocol") => json_response(200, app_bridge_protocol_payload()),
        ("GET", "/api/models") => json_response(200, models_payload(&request.path)),
        ("GET", "/api/mcp") => json_response(200, mcp_payload(config, &request.path)),
        ("GET", "/api/lsp") | ("GET", "/lsp") => match lsp_status_payload(config) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        },
        ("GET", "/api/lsp/doctor") => match lsp_doctor_payload(config) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        },
        ("POST", "/api/lsp/query") => match lsp_query_payload(config, &request.body) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        },
        ("GET", "/api/agents") => json_response(200, agents_payload(config)),
        ("GET", "/api/mdns") => json_response(200, mdns_payload(config)),
        ("GET", "/api/files") => match files_payload(config, &request.path) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        },
        ("GET", "/api/git") => match git_payload(config) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        },
        ("POST", "/api/terminal/run") => match terminal_run_payload(config, &request.body) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        },
        ("GET", "/api/events") => sse_response(global_sse_frames(config, &request.path)),
        ("GET", "/api/approvals") => {
            json_response(200, pending_approvals_payload(config, &request.path))
        }
        ("GET", "/api/questions") => {
            json_response(200, pending_questions_payload(config, &request.path))
        }
        ("GET", "/api/turns") => json_response(200, list_turn_jobs_payload(config, &request.path)),
        ("GET", "/api/sessions") => {
            json_response(200, list_sessions_payload(config, &request.path))
        }
        ("POST", "/api/sessions") => {
            json_response(200, create_session_payload(config, &request.body))
        }
        ("GET", "/") if config.serve_static => {
            static_response("text/html; charset=utf-8", INDEX_HTML)
        }
        ("GET", "/index.html") if config.serve_static => {
            static_response("text/html; charset=utf-8", INDEX_HTML)
        }
        ("GET", "/app.js") if config.serve_static => {
            static_response("application/javascript; charset=utf-8", APP_JS)
        }
        ("GET", "/app.css") if config.serve_static => {
            static_response("text/css; charset=utf-8", APP_CSS)
        }
        _ => route_dynamic_request(request, config, path),
    }
}

pub(super) fn route_dynamic_request(
    request: &HttpRequest,
    config: &HttpRuntimeConfig,
    path: &str,
) -> HttpResponseSpec {
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if parts.len() == 3 && parts[0] == "api" && parts[1] == "sessions" {
        return match request.method.as_str() {
            "GET" => json_response(200, get_session_payload(config, parts[2])),
            "PATCH" => match update_session_payload(config, parts[2], &request.body) {
                Ok(payload) => json_response(200, payload),
                Err(error) => session_mutation_error_response(error),
            },
            "DELETE" => match delete_session_payload(config, parts[2]) {
                Ok(payload) => json_response(200, payload),
                Err(error) => json_response(400, json!({"error": error})),
            },
            _ => route_unknown(),
        };
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "messages"
        && request.method == "GET"
    {
        return match session_messages_payload(config, parts[2], &request.path) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 2 && parts[0] == "api" && parts[1] == "skills" && request.method == "GET" {
        return json_response(200, skills_payload(config));
    }
    if parts.len() == 3
        && parts[0] == "api"
        && parts[1] == "mcp"
        && parts[2] == "servers"
        && request.method == "POST"
    {
        return mcp_config_response(mcp_add_server_payload(config, &request.body));
    }
    if parts.len() == 5
        && parts[0] == "api"
        && parts[1] == "mcp"
        && parts[2] == "servers"
        && matches!(parts[4], "test" | "start" | "stop" | "restart")
        && request.method == "POST"
    {
        return match parts[4] {
            "test" => mcp_config_response(mcp_test_server_payload(config, parts[3])),
            "start" => mcp_config_response(mcp_lifecycle_start_server_payload(config, parts[3])),
            "stop" => mcp_config_response(mcp_lifecycle_stop_server_payload(config, parts[3])),
            "restart" => {
                mcp_config_response(mcp_lifecycle_restart_server_payload(config, parts[3]))
            }
            _ => route_unknown(),
        };
    }
    if parts.len() == 4 && parts[0] == "api" && parts[1] == "mcp" && parts[2] == "servers" {
        return match request.method.as_str() {
            "PATCH" => {
                mcp_config_response(mcp_update_server_payload(config, parts[3], &request.body))
            }
            "DELETE" => mcp_config_response(mcp_delete_server_payload(config, parts[3])),
            _ => route_unknown(),
        };
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "children"
        && request.method == "GET"
    {
        return json_response(200, session_children_payload(config, parts[2]));
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "tasks"
        && request.method == "GET"
    {
        return json_response(200, session_tasks_payload(config, parts[2]));
    }
    if parts.len() == 6
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "tasks"
        && parts[5] == "run"
        && request.method == "POST"
    {
        return match run_session_task_payload(config, parts[2], parts[4], &request.body) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 6
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "tasks"
        && parts[5] == "cancel"
        && request.method == "POST"
    {
        return match cancel_session_task_payload(config, parts[2], parts[4]) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 4 && parts[0] == "api" && parts[1] == "sessions" && parts[3] == "share" {
        return match request.method.as_str() {
            "POST" => match share_session_payload(config, parts[2]) {
                Ok(payload) => json_response(200, payload),
                Err(error) => json_response(400, json!({"error": error})),
            },
            "DELETE" => match unshare_session_payload(config, parts[2]) {
                Ok(payload) => json_response(200, payload),
                Err(error) => json_response(400, json!({"error": error})),
            },
            _ => route_unknown(),
        };
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "compact"
        && request.method == "POST"
    {
        return match compact_session_payload(config, parts[2]) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 4 && parts[0] == "api" && parts[1] == "sessions" && parts[3] == "diff" {
        return match request.method.as_str() {
            "GET" => match session_diff_payload(config, parts[2]) {
                Ok(payload) => json_response(200, payload),
                Err(error) => json_response(400, json!({"error": error})),
            },
            _ => route_unknown(),
        };
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "checkpoints"
        && request.method == "GET"
    {
        return match session_checkpoints_payload(config, parts[2]) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 6
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "checkpoints"
        && parts[5] == "restore"
        && request.method == "POST"
    {
        return match restore_session_checkpoint_payload(config, parts[2], parts[4]) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "undo"
        && request.method == "POST"
    {
        return match undo_session_payload(config, parts[2]) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "redo"
        && request.method == "POST"
    {
        return match redo_session_payload(config, parts[2]) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "sessions"
        && parts[3] == "turns"
        && request.method == "POST"
    {
        return start_turn_response(config, parts[2], &request.path, &request.body);
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "turns"
        && parts[3] == "events"
        && request.method == "GET"
    {
        return sse_response(turn_sse_frames(config, parts[2], &request.path));
    }
    if parts.len() == 3 && parts[0] == "api" && parts[1] == "turns" && request.method == "GET" {
        return match turn_status_payload(config, parts[2]) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(404, json!({"error": error})),
        };
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "turns"
        && parts[3] == "interrupt"
        && request.method == "POST"
    {
        return match interrupt_turn_payload(config, parts[2]) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if path.starts_with("/api/turns/") && path.contains("/approvals/") && request.method == "POST" {
        return match respond_approval_payload(config, path, &request.body) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 3 && parts[0] == "api" && parts[1] == "approvals" && request.method == "POST"
    {
        return match respond_global_approval_payload(config, parts[2], &request.body) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if path.starts_with("/api/turns/")
        && path.contains("/questions/")
        && path.ends_with("/reply")
        && request.method == "POST"
    {
        return match respond_question_payload(config, path, &request.body) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if parts.len() == 4
        && parts[0] == "api"
        && parts[1] == "questions"
        && parts[3] == "reply"
        && request.method == "POST"
    {
        return match respond_global_question_payload(config, parts[2], &request.body) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    if path == "/tui/control/next" && request.method == "GET" {
        return json_response(200, pop_tui_control_payload(config));
    }
    if path == "/tui/control/response" && request.method == "POST" {
        return json_response(200, record_tui_control_response(config, &request.body));
    }
    if path.starts_with("/tui/") && request.method == "POST" {
        return match enqueue_tui_control_payload(config, path, &request.body) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        };
    }
    route_unknown()
}

fn session_mutation_error_response(error: String) -> HttpResponseSpec {
    let normalized = error.to_lowercase();
    if normalized == "session_not_found"
        || normalized.contains("session state not found")
        || normalized.contains("no such file or directory")
    {
        return json_response(
            404,
            json!({
                "code": "session_not_found",
                "error": "session not found",
            }),
        );
    }
    json_response(400, json!({"error": error}))
}

pub(super) fn authorized(request: &HttpRequest, config: &HttpRuntimeConfig) -> bool {
    if !config.auth_required() {
        return true;
    }
    if let Some(token) = config.auth_token.as_ref().filter(|token| !token.is_empty()) {
        let bearer_ok = request
            .headers
            .get("authorization")
            .is_some_and(|value| value == &format!("Bearer {token}"));
        let header_ok = request
            .headers
            .get("x-openagent-token")
            .is_some_and(|value| value == token);
        if bearer_ok || header_ok {
            return true;
        }
    }
    basic_auth_ok(
        request.headers.get("authorization").map(String::as_str),
        config,
    )
}

pub(super) fn json_response(status: u16, body: Value) -> HttpResponseSpec {
    HttpResponseSpec {
        status,
        content_type: Some("application/json; charset=utf-8".to_string()),
        headers: Map::new(),
        body: Some(body),
        body_text: None,
    }
}

pub(super) fn static_response(content_type: &str, body: &str) -> HttpResponseSpec {
    HttpResponseSpec {
        status: 200,
        content_type: Some(content_type.to_string()),
        headers: Map::new(),
        body: None,
        body_text: Some(body.to_string()),
    }
}

pub(super) fn sse_response(body: String) -> HttpResponseSpec {
    let mut headers = Map::new();
    headers.insert("Cache-Control".to_string(), json!("no-cache"));
    headers.insert("X-Accel-Buffering".to_string(), json!("no"));
    HttpResponseSpec {
        status: 200,
        content_type: Some("text/event-stream; charset=utf-8".to_string()),
        headers,
        body: None,
        body_text: Some(body),
    }
}

pub(super) fn should_live_sse(request: &HttpRequest, config: &HttpRuntimeConfig) -> bool {
    if request.method != "GET" || !authorized(request, config) {
        return false;
    }
    let path = request.path.split('?').next().unwrap_or("/");
    let is_sse_path = path == "/api/events" || turn_id_from_events_path(path).is_some();
    is_sse_path
        && request
            .headers
            .get("accept")
            .is_some_and(|value| value.contains("text/event-stream"))
}

pub(super) fn write_live_sse_response(
    stream: &mut TcpStream,
    config: &HttpRuntimeConfig,
    request: &HttpRequest,
) -> Result<(), String> {
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    let path = request.path.split('?').next().unwrap_or("/");
    let turn_id = turn_id_from_events_path(path);
    let headers = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\ncache-control: no-cache, no-transform\r\nx-accel-buffering: no\r\naccess-control-allow-origin: {}\r\nconnection: close\r\n\r\n",
        config.cors_origin
    );
    stream
        .write_all(headers.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut last_id = last_event_id_from_path(&request.path);
    let timeout = live_sse_timeout(&request.path);
    let started = Instant::now();
    let mut last_heartbeat = Instant::now();
    loop {
        let mut terminal_seen = false;
        for (id, event) in live_sse_events_after(config, turn_id.as_deref(), last_id) {
            stream
                .write_all(sse_frame(id, &event).as_bytes())
                .map_err(|error| error.to_string())?;
            last_id = id;
            if is_terminal_turn_event(&event) {
                terminal_seen = true;
            }
        }
        stream.flush().map_err(|error| error.to_string())?;
        if terminal_seen {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            stream
                .write_all(b": ping\n\n")
                .map_err(|error| error.to_string())?;
            stream.flush().map_err(|error| error.to_string())?;
            return Ok(());
        }
        if last_heartbeat.elapsed() >= Duration::from_secs(10) {
            stream
                .write_all(b": ping\n\n")
                .map_err(|error| error.to_string())?;
            stream.flush().map_err(|error| error.to_string())?;
            last_heartbeat = Instant::now();
        }
        thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn live_sse_events_after(
    config: &HttpRuntimeConfig,
    turn_id: Option<&str>,
    last_id: u64,
) -> Vec<(u64, Value)> {
    let events = if let Some(turn_id) = turn_id {
        turn_app_events(config, turn_id)
    } else {
        all_app_events(config)
    };
    events
        .into_iter()
        .enumerate()
        .filter_map(|(index, event)| {
            let id = event
                .get(if turn_id.is_some() {
                    "sequence"
                } else {
                    "global_sequence"
                })
                .or_else(|| event.get("sequence"))
                .and_then(Value::as_u64)
                .unwrap_or(index as u64 + 1);
            (id > last_id).then_some((id, event))
        })
        .collect()
}

pub(super) fn turn_id_from_events_path(path: &str) -> Option<String> {
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    (parts.len() == 4 && parts[0] == "api" && parts[1] == "turns" && parts[3] == "events")
        .then(|| parts[2].to_string())
}

pub(super) fn live_sse_timeout(request_path: &str) -> Duration {
    let millis = query_value(request_path, "live_timeout_ms")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(250, 300_000);
    Duration::from_millis(millis)
}

pub(super) fn is_terminal_turn_event(event: &Value) -> bool {
    matches!(
        event.get("method").and_then(Value::as_str),
        Some("turn/completed" | "turn/failed" | "turn/interrupted")
    )
}

pub(super) fn with_runtime_headers(
    mut response: HttpResponseSpec,
    config: &HttpRuntimeConfig,
) -> HttpResponseSpec {
    response.headers.insert(
        "Access-Control-Allow-Origin".to_string(),
        json!(config.cors_origin.clone()),
    );
    response.headers.insert(
        "Access-Control-Allow-Headers".to_string(),
        json!("Authorization, Content-Type, X-OpenAgent-Token"),
    );
    response.headers.insert(
        "Access-Control-Allow-Methods".to_string(),
        json!("GET, POST, PATCH, DELETE, OPTIONS"),
    );
    response
}

pub(super) fn write_http_response(
    stream: &mut TcpStream,
    response: HttpResponseSpec,
) -> Result<(), String> {
    let body = response.body_text.unwrap_or_else(|| {
        response
            .body
            .as_ref()
            .map(stable_json_dumps)
            .unwrap_or_default()
    });
    let content_type = response
        .content_type
        .unwrap_or_else(|| "application/json; charset=utf-8".to_string());
    let status_text = match response.status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "OK",
    };
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status,
        status_text,
        content_type,
        body.len()
    );
    for (key, value) in response.headers {
        if let Some(value) = value.as_str() {
            headers.push_str(&format!("{key}: {value}\r\n"));
        }
    }
    headers.push_str("\r\n");
    stream
        .write_all(headers.as_bytes())
        .and_then(|()| stream.write_all(body.as_bytes()))
        .map_err(|error| error.to_string())
}

pub(super) fn basic_auth_ok(authorization: Option<&str>, config: &HttpRuntimeConfig) -> bool {
    let Some(password) = config
        .auth_password
        .as_ref()
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    let username = config.auth_username.as_deref().unwrap_or("openagent");
    let Some(encoded) = authorization.and_then(|value| value.strip_prefix("Basic ")) else {
        return false;
    };
    decode_base64(encoded).is_some_and(|decoded| decoded == format!("{username}:{password}"))
}

pub(super) fn decode_base64(value: &str) -> Option<String> {
    let mut output = Vec::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in value.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        if byte == b'=' {
            break;
        }
        let sextet = base64_value(byte)? as u32;
        buffer = (buffer << 6) | sextet;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    String::from_utf8(output).ok()
}

pub(super) fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

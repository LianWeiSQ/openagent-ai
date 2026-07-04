//! HTTP runtime service contracts for the Rust rewrite.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use openagent_app_server::{
    approval_response_payload, control_next_payload, parse_turn_approval_path,
    parse_turn_question_reply_path, question_dismiss_payload, question_reply_payload,
    record_control_response_payload, tui_control_request_for_path,
};
use openagent_core::{
    PermissionManager, SkillDocument, SkillRegistry, SkillRegistryOptions, permission_rule,
    render_available_skills, render_preloaded_skills, skill_document_model_invocable,
    skill_info_model_invocable,
};
use openagent_mcp::{
    McpBridgeOutput, McpServerType, McpTransport, RemoteMcpManager, RemoteMcpServerConfig,
    RemoteMcpToolDescriptor, StdioMcpSession, bridge_tool_output,
    build_tool_descriptors_from_values, discover_mcp_server_tools, load_mcp_config,
    load_mcp_config_from_value, mcp_json_rpc, mcp_tool_definition, normalize_tool_call_result,
    unavailable_tool_result,
};
use openagent_protocol::{
    ChatMessage, PermissionRuleset, Role, ToolCall, ToolResult, ToolSchema, Usage,
};
use openagent_provider::{
    OpenAiLanguageModelConfig, ProviderStreamEvent, build_openai_chat_payload,
    build_openai_responses_payload, default_env_mapping, normalize_openai_chat_sse_chunks,
    normalize_openai_responses_response, normalize_openai_responses_stream_events,
    normalize_provider, parse_tool_arguments, provider_default_base_url, provider_default_model,
    provider_label, provider_requires_api_key, summarize_http_error_body,
};
use openagent_session::{
    FileSessionStore, Session, SessionEventOptions, SessionPartOptions, SessionStatus,
    StartRunOptions,
};
use openagent_tools::{
    SkillPermissionRule, TASK_TOOL_ID, TaskPermissionRule, TaskSubagentDescriptor,
    TaskSubagentRoute, ToolContext, Toolkit, fork_skill_task_from_input,
    parse_agent_profile_schema, prepare_isolated_workspace, register_task_tool,
    resolve_path_in_root, select_task_subagent_for_prompt, skill_is_visible,
    task_subagent_is_visible,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

mod app_bridge_routes;
mod mcp_runtime;
mod turn_runtime;

use app_bridge_routes::*;
pub use app_bridge_routes::{
    CliRunResult, HttpResponseSpec, build_run_prompt, command_text_from_args, docker_smoke_command,
    dockerfile_lines, emit_app_bridge_events, format_http_error, health_payload, parse_cli_args,
    parse_sse_data, parse_sse_response_lines, route_health, route_options, route_unauthorized,
    route_unknown, run_cli,
};
use mcp_runtime::*;
use turn_runtime::*;

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8787;
const INDEX_HTML: &str = include_str!("../../static/app-server/static/index.html");
const APP_JS: &str = include_str!("../../static/app-server/static/app.js");
const APP_CSS: &str = include_str!("../../static/app-server/static/app.css");
const APP_EVENTS_FILE: &str = "app_events.jsonl";
const APP_BRIDGE_PROTOCOL_VERSION: u64 = 1;
const APP_BRIDGE_EVENT_SCHEMA_VERSION: &str = "openagent.app_event.v1";
const TUI_CONTROL_QUEUE_FILE: &str = "tui_control_queue.json";
const TUI_CONTROL_RESPONSES_FILE: &str = "tui_control_responses.jsonl";
const FILE_CHANGE_UNDO_STACK_KEY: &str = "file_change_undo_stack";
const FILE_CHANGE_REDO_STACK_KEY: &str = "file_change_redo_stack";
const FILE_CHANGE_LATEST_KEY: &str = "latest_file_change";
const MAX_FILE_CHANGE_STACK: usize = 50;
const MAX_RENDERED_DIFF_LINES: usize = 400;
const MAX_FILE_TREE_ENTRIES: usize = 300;
const MAX_TERMINAL_COMMAND_CHARS: usize = 4096;
const MAX_TERMINAL_OUTPUT_CHARS: usize = 20_000;
const DEFAULT_TERMINAL_TIMEOUT_MS: u64 = 10_000;
const MAX_TERMINAL_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_TASK_RUN_LOCK_STALE_MS: u64 = 15 * 60 * 1000;
const DEFAULT_BACKGROUND_TASK_WORKER_POLL_MS: u64 = 100;
const DEFAULT_MAX_SUBAGENT_DEPTH: u64 = 3;
const BUILD_AGENT_PROMPT: &str = include_str!("../../../skill/prompts/build.txt");
const EXPLORE_AGENT_PROMPT: &str = include_str!("../../../skill/prompts/explore.txt");
const PLAN_AGENT_PROMPT: &str = include_str!("../../../skill/prompts/plan.txt");
const SCOUT_AGENT_PROMPT: &str = include_str!("../../../skill/prompts/scout.txt");
const REVIEW_AGENT_PROMPT: &str = "You are OpenAgent Reviewer. Focus on correctness, regressions, risk, and missing tests. Prefer evidence from tools and keep findings concise.";
const TURN_INTERRUPTED_ERROR: &str = "turn interrupted";
const TURN_JOB_INDEX_FILE: &str = ".openagent-runtime/turn_jobs.json";
const TURN_QUEUE_DIR: &str = ".openagent-runtime/turn_queue";
const TURN_QUEUE_LEASE_DIR: &str = ".openagent-runtime/turn_queue_leases";
const TURN_JOB_INDEX_SCHEMA_VERSION: u64 = 1;
const TURN_QUEUE_PAYLOAD_SCHEMA_VERSION: u64 = 1;
const TURN_QUEUE_LEASE_SCHEMA_VERSION: u64 = 1;
const TURN_JOB_TERMINAL_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const MAX_TURN_JOB_INDEX_ENTRIES: usize = 200;
const DEFAULT_MAX_QUEUED_TURNS_PER_SESSION: usize = 8;
const DEFAULT_MAX_RUNNING_TURN_WORKERS: usize = 4;
const DEFAULT_TURN_QUEUE_LEASE_STALE_MS: u64 = 30_000;
const DEFAULT_TURN_QUEUE_TIMEOUT_MS: u64 = 30 * 60 * 1000;
const UNBOUNDED_MAX_STEPS: u64 = u64::MAX / 4;

#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[must_use]
pub fn command_name() -> &'static str {
    "openagent-http-runtime"
}

#[must_use]
pub fn app_server_crate_name() -> &'static str {
    openagent_app_server::crate_name()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HttpRuntimeConfig {
    pub host: String,
    pub port: u16,
    pub serve_static: bool,
    pub workspace: Option<String>,
    pub session_store_root: Option<String>,
    pub mcp_config: Option<String>,
    pub auth_token: Option<String>,
    pub auth_username: Option<String>,
    pub auth_password: Option<String>,
    pub cors_origin: String,
    pub mdns_name: Option<String>,
    pub max_queued_turns_per_session: usize,
    pub max_running_turn_workers: usize,
    pub turn_queue_lease_stale_ms: u64,
    pub turn_queue_timeout_ms: u64,
}

// turn_runtime implementation lives in `turn_runtime.rs`.
impl Default for HttpRuntimeConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            serve_static: true,
            workspace: None,
            session_store_root: None,
            mcp_config: None,
            auth_token: None,
            auth_username: None,
            auth_password: None,
            cors_origin: "*".to_string(),
            mdns_name: Some("openagent".to_string()),
            max_queued_turns_per_session: configured_max_queued_turns_per_session(),
            max_running_turn_workers: configured_max_running_turn_workers(),
            turn_queue_lease_stale_ms: configured_turn_queue_lease_stale_ms(),
            turn_queue_timeout_ms: configured_turn_queue_timeout_ms(),
        }
    }
}

impl HttpRuntimeConfig {
    #[must_use]
    pub fn auth_required(&self) -> bool {
        self.auth_token
            .as_ref()
            .is_some_and(|token| !token.is_empty())
            || self
                .auth_password
                .as_ref()
                .is_some_and(|password| !password.is_empty())
    }

    #[must_use]
    pub fn to_public_value(&self) -> Value {
        json!({
            "host": self.host,
            "port": self.port,
            "serve_static": self.serve_static,
            "workspace": self.workspace,
            "session_store_root": self.session_store_root,
            "auth_required": self.auth_required(),
            "auth_basic_enabled": self.auth_password.as_ref().is_some_and(|value| !value.is_empty()),
            "cors_origin": self.cors_origin,
            "mdns_name": self.mdns_name,
            "max_queued_turns_per_session": self.max_queued_turns_per_session,
            "max_running_turn_workers": max_running_turn_workers(self),
            "turn_queue_lease_stale_ms": self.turn_queue_lease_stale_ms,
            "turn_queue_timeout_ms": turn_queue_timeout_ms(self),
        })
    }
}

// app_bridge_routes implementation lives in `app_bridge_routes.rs`.
fn list_sessions_payload(config: &HttpRuntimeConfig, request_path: &str) -> Value {
    let root = session_root(config);
    let query = query_param(request_path, "query").unwrap_or_default();
    let mut sessions = Vec::new();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let state = read_json_file(&path.join("state.latest.json"));
            if state.as_object().is_none_or(Map::is_empty) {
                continue;
            }
            let summary = session_summary_from_state(&state, &entry.file_name().to_string_lossy());
            if !query.is_empty() && !session_matches_query(&summary, &query) {
                continue;
            }
            sessions.push(summary);
        }
    }
    sessions.sort_by(|left, right| {
        right["updated_at_ms"]
            .as_u64()
            .cmp(&left["updated_at_ms"].as_u64())
    });
    json!({"session_root": root.to_string_lossy(), "query": query, "sessions": sessions})
}

fn models_payload(request_path: &str) -> Value {
    let provider =
        query_param(request_path, "provider").unwrap_or_else(|| active_provider_id(None));
    let runtime_config = runtime_provider_config(Some(&provider), None, None)
        .unwrap_or_else(|_| RuntimeProviderConfig::fallback(&provider));
    let live_check = query_flag(request_path, "check") || query_flag(request_path, "refresh");
    let probe = if live_check {
        probe_runtime_models_endpoint(&runtime_config)
    } else {
        RuntimeModelProbe::not_checked(&runtime_config)
    };
    let models = model_records_for_runtime(&runtime_config, &probe);
    json!({
        "provider": runtime_config.provider,
        "provider_label": runtime_config.provider_label,
        "base_url": runtime_config.base_url,
        "base_url_source": runtime_config.base_url_source,
        "model": runtime_config.model,
        "model_source": runtime_config.model_source,
        "wire_api": runtime_config.wire_api,
        "wire_api_source": runtime_config.wire_api_source,
        "api_key": if runtime_config.api_key.is_some() { "set" } else { "missing" },
        "api_key_env": runtime_config.api_key_env,
        "api_key_source": runtime_config.api_key_source,
        "healthy": probe.ok,
        "model_endpoint_checked": probe.checked,
        "model_endpoint_ok": probe.ok,
        "model_endpoint": probe.endpoint,
        "model_endpoint_message": probe.message,
        "model_count": probe.model_ids.len(),
        "configured_model_available": probe.configured_model_available,
        "models": models,
        "variants": ["default", "fast", "balanced", "deep"],
        "thinking": ["off", "low", "medium", "high"],
    })
}

// mcp_runtime implementation lives in `mcp_runtime.rs`.
fn model_records_for_runtime(
    config: &RuntimeProviderConfig,
    probe: &RuntimeModelProbe,
) -> Vec<Value> {
    let mut models = if probe.model_ids.is_empty() {
        vec![config.model.clone()]
    } else {
        probe.model_ids.clone()
    }
    .into_iter()
    .filter(|model| !model.is_empty())
    .collect::<Vec<_>>();
    if !models.iter().any(|model| model == &config.model) {
        models.insert(0, config.model.clone());
    }
    if !models.iter().any(|model| model == "server-local") {
        models.push("server-local".to_string());
    }
    models
        .into_iter()
        .map(|model| {
            let provider_id = if model == "server-local" {
                "openagent".to_string()
            } else {
                config.provider.clone()
            };
            let name = if model == "server-local" {
                "OpenAgent Server Local".to_string()
            } else {
                model.clone()
            };
            let default = model == config.model;
            json!({
                "id": model,
                "provider_id": provider_id,
                "name": name,
                "capabilities": {"tools": true, "streaming": true, "reasoning": true},
                "default": default,
            })
        })
        .collect()
}

#[derive(Clone, Debug)]
struct RuntimeProviderConfig {
    provider: String,
    provider_label: String,
    api_key_env: String,
    api_key: Option<String>,
    api_key_source: Option<String>,
    base_url: String,
    base_url_source: String,
    model: String,
    model_source: String,
    wire_api: String,
    wire_api_source: String,
    requires_api_key: bool,
}

impl RuntimeProviderConfig {
    fn fallback(provider: &str) -> Self {
        let provider = normalize_provider(Some(provider)).unwrap_or_else(|_| "openai".to_string());
        Self {
            provider_label: provider_label(&provider).unwrap_or_else(|_| provider.clone()),
            api_key_env: default_env_mapping(&provider)
                .ok()
                .and_then(|env| env.get("api_key").cloned())
                .unwrap_or_else(|| "OPENAI_API_KEY".to_string()),
            api_key: None,
            api_key_source: None,
            base_url: provider_default_base_url(&provider)
                .ok()
                .flatten()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            base_url_source: "default".to_string(),
            model: provider_default_model(&provider)
                .ok()
                .flatten()
                .unwrap_or_else(default_model_id),
            model_source: "default".to_string(),
            wire_api: "responses".to_string(),
            wire_api_source: "default".to_string(),
            requires_api_key: provider_requires_api_key(&provider).unwrap_or(true),
            provider,
        }
    }
}

#[derive(Clone, Debug)]
struct RuntimeProviderField {
    value: String,
    source: String,
}

fn runtime_provider_config(
    provider: Option<&str>,
    payload: Option<&Value>,
    session: Option<&Session>,
) -> Result<RuntimeProviderConfig, String> {
    let provider = normalize_provider(Some(&active_provider_id(provider)))?;
    let env = default_env_mapping(&provider)?;
    let auth_record = runtime_auth_record(&provider);
    let api_key_env = env
        .get("api_key")
        .cloned()
        .unwrap_or_else(|| "OPENAI_API_KEY".to_string());
    let api_key = runtime_provider_field(
        "api_key",
        &api_key_env,
        &["OPENAGENT_API_KEY"],
        None,
        payload,
        session,
        auth_record.as_ref(),
    );
    let base_url = runtime_provider_field(
        "base_url",
        env.get("base_url")
            .map(String::as_str)
            .unwrap_or("OPENAI_BASE_URL"),
        &["OPENAGENT_BASE_URL"],
        provider_default_base_url(&provider)
            .ok()
            .flatten()
            .or_else(|| Some("https://api.openai.com/v1".to_string())),
        payload,
        session,
        auth_record.as_ref(),
    )
    .expect("base_url has default");
    let model = runtime_provider_field(
        "model",
        env.get("model")
            .map(String::as_str)
            .unwrap_or("OPENAI_MODEL"),
        &["OPENAGENT_MODEL"],
        provider_default_model(&provider)
            .ok()
            .flatten()
            .or_else(|| Some(default_model_id())),
        payload,
        session,
        auth_record.as_ref(),
    )
    .expect("model has default");
    let wire_api = runtime_provider_field(
        "wire_api",
        env.get("wire_api")
            .map(String::as_str)
            .unwrap_or("OPENAI_WIRE_API"),
        &["OPENAGENT_WIRE_API"],
        Some("responses".to_string()),
        payload,
        session,
        auth_record.as_ref(),
    )
    .expect("wire_api has default");
    Ok(RuntimeProviderConfig {
        provider_label: provider_label(&provider).unwrap_or_else(|_| provider.clone()),
        api_key_env,
        api_key: api_key.as_ref().map(|field| field.value.clone()),
        api_key_source: api_key.map(|field| field.source),
        base_url: base_url.value,
        base_url_source: base_url.source,
        model: model.value,
        model_source: model.source,
        wire_api: wire_api.value,
        wire_api_source: wire_api.source,
        requires_api_key: provider_requires_api_key(&provider).unwrap_or(true),
        provider,
    })
}

fn active_provider_id(provider: Option<&str>) -> String {
    provider
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("OPENAGENT_PROVIDER").ok())
        .or_else(|| std::env::var("OPENAGENT_ACTIVE_PROVIDER").ok())
        .unwrap_or_else(|| "openai".to_string())
}

fn runtime_provider_field(
    field: &str,
    provider_env_name: &str,
    generic_env_names: &[&str],
    default: Option<String>,
    payload: Option<&Value>,
    session: Option<&Session>,
    auth_record: Option<&Value>,
) -> Option<RuntimeProviderField> {
    payload
        .and_then(|payload| payload.get(field))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| RuntimeProviderField {
            value: value.to_string(),
            source: "payload".to_string(),
        })
        .or_else(|| {
            session
                .and_then(|session| session.metadata.get(field))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| RuntimeProviderField {
                    value: value.to_string(),
                    source: "session".to_string(),
                })
        })
        .or_else(|| env_field(provider_env_name, "env"))
        .or_else(|| {
            generic_env_names
                .iter()
                .find_map(|name| env_field(name, "env"))
        })
        .or_else(|| {
            auth_record
                .and_then(|record| record.get(field))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| RuntimeProviderField {
                    value: value.to_string(),
                    source: "auth_file".to_string(),
                })
        })
        .or_else(|| {
            default.map(|value| RuntimeProviderField {
                value,
                source: "default".to_string(),
            })
        })
}

fn env_field(name: &str, source: &str) -> Option<RuntimeProviderField> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| RuntimeProviderField {
            value,
            source: source.to_string(),
        })
}

fn runtime_auth_record(provider: &str) -> Option<Value> {
    read_json_file(&runtime_auth_file())
        .get("providers")
        .and_then(Value::as_object)
        .and_then(|providers| providers.get(provider))
        .cloned()
}

fn runtime_auth_file() -> PathBuf {
    std::env::var("OPENAGENT_AUTH_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".config/openagent/auth.json"))
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[derive(Clone, Debug)]
struct RuntimeModelProbe {
    checked: bool,
    ok: bool,
    message: String,
    endpoint: Option<String>,
    model_ids: Vec<String>,
    configured_model_available: Option<bool>,
}

impl RuntimeModelProbe {
    fn not_checked(config: &RuntimeProviderConfig) -> Self {
        Self {
            checked: false,
            ok: !config.requires_api_key || config.api_key.is_some(),
            message: "not checked; pass check=true to probe the provider /models endpoint"
                .to_string(),
            endpoint: Some(join_url(&config.base_url, "models")),
            model_ids: Vec::new(),
            configured_model_available: None,
        }
    }
}

fn probe_runtime_models_endpoint(config: &RuntimeProviderConfig) -> RuntimeModelProbe {
    let endpoint = join_url(&config.base_url, "models");
    if config.requires_api_key && config.api_key.is_none() {
        return RuntimeModelProbe {
            checked: false,
            ok: false,
            message: format!(
                "missing API key in {}; /models was not checked",
                config.api_key_env
            ),
            endpoint: Some(endpoint),
            model_ids: Vec::new(),
            configured_model_available: None,
        };
    }
    let client = match reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return RuntimeModelProbe {
                checked: true,
                ok: false,
                message: format!("failed to build HTTP client: {error}"),
                endpoint: Some(endpoint),
                model_ids: Vec::new(),
                configured_model_available: None,
            };
        }
    };
    let mut request = client.get(&endpoint).header("accept", "application/json");
    if let Some(api_key) = config.api_key.as_deref().filter(|value| !value.is_empty()) {
        request = request.bearer_auth(api_key);
    }
    let response = match request.send() {
        Ok(response) => response,
        Err(error) => {
            return RuntimeModelProbe {
                checked: true,
                ok: false,
                message: format!("failed to GET {endpoint}: {error}"),
                endpoint: Some(endpoint),
                model_ids: Vec::new(),
                configured_model_available: None,
            };
        }
    };
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let raw = match response.text() {
        Ok(raw) => raw,
        Err(error) => {
            return RuntimeModelProbe {
                checked: true,
                ok: false,
                message: format!("failed to read {endpoint}: {error}"),
                endpoint: Some(endpoint),
                model_ids: Vec::new(),
                configured_model_available: None,
            };
        }
    };
    if !status.is_success() {
        return RuntimeModelProbe {
            checked: true,
            ok: false,
            message: format!(
                "HTTP {} from {endpoint}: {}",
                status.as_u16(),
                summarize_http_error_body(&raw, &content_type)
            ),
            endpoint: Some(endpoint),
            model_ids: Vec::new(),
            configured_model_available: None,
        };
    }
    let model_ids = serde_json::from_str::<Value>(&raw)
        .ok()
        .map(|value| extract_openai_model_ids(&value))
        .unwrap_or_default();
    let configured_model_available =
        (!model_ids.is_empty()).then(|| model_ids.iter().any(|model| model == &config.model));
    let message = match configured_model_available {
        Some(true) => format!(
            "HTTP {} from {endpoint}; configured model is listed among {} model(s)",
            status.as_u16(),
            model_ids.len()
        ),
        Some(false) => format!(
            "HTTP {} from {endpoint}; {} model(s) listed, configured model '{}' was not listed",
            status.as_u16(),
            model_ids.len(),
            config.model
        ),
        None => format!("HTTP {} from {endpoint}", status.as_u16()),
    };
    RuntimeModelProbe {
        checked: true,
        ok: true,
        message,
        endpoint: Some(endpoint),
        model_ids,
        configured_model_available,
    }
}

fn extract_openai_model_ids(value: &Value) -> Vec<String> {
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn agents_payload(config: &HttpRuntimeConfig) -> Value {
    let mut agents = vec![json!({
        "id": "server",
        "name": "Server",
        "description": "Default server-backed coding agent",
        "mode": "primary",
        "default": true,
    })];
    agents.extend(
        runtime_subagent_profiles(&workspace(config))
            .into_iter()
            .filter(|profile| !profile.hidden)
            .map(|profile| runtime_subagent_public_value(&profile)),
    );
    json!({ "agents": agents })
}

#[derive(Clone, Debug)]
struct RuntimeSubagentProfile {
    id: String,
    name: String,
    description: String,
    mode: String,
    permission: PermissionRuleset,
    task_permissions: Vec<TaskPermissionRule>,
    skills: Vec<String>,
    skill_roots: Vec<String>,
    skill_permissions: Vec<SkillPermissionRule>,
    prompt: String,
    tools: Vec<String>,
    provider: Option<String>,
    model: Option<String>,
    max_steps: Option<u64>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    color: Option<String>,
    disabled: bool,
    model_options: BTreeMap<String, Value>,
    workspace_isolation: bool,
    hidden: bool,
    source_path: Option<PathBuf>,
}

fn runtime_subagent_profiles(workspace: &Path) -> Vec<RuntimeSubagentProfile> {
    runtime_agent_profiles(workspace)
        .into_iter()
        .filter(|profile| runtime_is_subagent_mode(&profile.mode))
        .collect()
}

fn runtime_agent_profiles(workspace: &Path) -> Vec<RuntimeSubagentProfile> {
    let mut profiles = builtin_runtime_subagent_profiles()
        .into_iter()
        .map(|profile| (profile.id.clone(), profile))
        .collect::<BTreeMap<_, _>>();
    let mut paths = runtime_agent_registry_dirs(workspace)
        .into_iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| runtime_agent_profile_file_kind(path).is_some())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let fallback_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(sanitize_runtime_agent_id)
            .unwrap_or_else(|| "agent".to_string());
        if let Some(profile) = runtime_agent_profile_from_path(&path, &fallback_id)
            && !profile.disabled
        {
            profiles.insert(profile.id.clone(), profile);
        }
    }
    profiles.into_values().collect()
}

fn runtime_agent_registry_dirs(workspace: &Path) -> Vec<PathBuf> {
    vec![
        workspace.join(".openagent/agents"),
        workspace.join(".opencode/agents"),
        workspace.join(".opencode/agent"),
    ]
}

fn runtime_agent_profile_file_kind(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|value| value.to_str()) {
        Some("json") => Some("json"),
        Some("md" | "markdown") => Some("markdown"),
        _ => None,
    }
}

fn runtime_agent_profile_from_path(
    path: &Path,
    fallback_id: &str,
) -> Option<RuntimeSubagentProfile> {
    let kind = runtime_agent_profile_file_kind(path)?;
    let value = if kind == "json" {
        read_json_file(path)
    } else {
        markdown_runtime_agent_profile_value(path).ok()?
    };
    runtime_agent_profile_from_value(&value, fallback_id, Some(path.to_path_buf()))
}

fn markdown_runtime_agent_profile_value(path: &Path) -> Result<Value, String> {
    let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut value = json!({});
    let mut body = raw.as_str();
    if let Some(rest) = raw.trim_start_matches('\u{feff}').strip_prefix("---")
        && let Some((frontmatter, tail)) = rest.split_once("---")
    {
        value = serde_yaml::from_str::<Value>(frontmatter).unwrap_or_else(|_| json!({}));
        body = tail.trim_start_matches('\n');
    }
    if value.as_object().is_none() {
        value = json!({});
    }
    if let Some(object) = value.as_object_mut() {
        let prompt = body.trim_start_matches('\n').trim_end();
        if !prompt.trim().is_empty() && !object.contains_key("prompt") {
            object.insert("prompt".to_string(), json!(prompt));
        }
    }
    Ok(value)
}

fn builtin_runtime_subagent_profiles() -> Vec<RuntimeSubagentProfile> {
    vec![
        builtin_runtime_subagent_profile(
            "coder",
            "Coder",
            "Implementation-focused profile",
            PermissionRuleset::PlanOnly,
            BUILD_AGENT_PROMPT,
            &[],
        ),
        builtin_runtime_subagent_profile(
            "reviewer",
            "Reviewer",
            "Review and risk-focused profile",
            PermissionRuleset::Readonly,
            REVIEW_AGENT_PROMPT,
            &[
                "read",
                "glob",
                "grep",
                "ls",
                "code_search",
                "skill",
                "todoread",
            ],
        ),
        builtin_runtime_subagent_profile(
            "planner",
            "Planner",
            "Plan-first profile for large changes",
            PermissionRuleset::PlanOnly,
            PLAN_AGENT_PROMPT,
            &[
                "read",
                "glob",
                "grep",
                "ls",
                "code_search",
                "skill",
                "todoread",
                "todowrite",
                "question",
            ],
        ),
        builtin_runtime_subagent_profile(
            "general",
            "General",
            "General-purpose subagent for complex multi-step tasks",
            PermissionRuleset::PlanOnly,
            BUILD_AGENT_PROMPT,
            &[],
        ),
        builtin_runtime_subagent_profile(
            "explore",
            "Explore",
            "Read-only code exploration subagent",
            PermissionRuleset::Readonly,
            EXPLORE_AGENT_PROMPT,
            &[
                "read",
                "glob",
                "grep",
                "ls",
                "code_search",
                "skill",
                "todoread",
            ],
        ),
        builtin_runtime_subagent_profile(
            "scout",
            "Scout",
            "External documentation and dependency research subagent with read-only web fetch access",
            PermissionRuleset::Readonly,
            SCOUT_AGENT_PROMPT,
            &[
                "web_fetch",
                "read",
                "glob",
                "grep",
                "ls",
                "code_search",
                "skill",
                "todoread",
            ],
        ),
        builtin_runtime_subagent_profile(
            "plan",
            "Plan",
            "Planning subagent for architecture and task breakdowns",
            PermissionRuleset::PlanOnly,
            PLAN_AGENT_PROMPT,
            &[
                "read",
                "glob",
                "grep",
                "ls",
                "code_search",
                "skill",
                "todoread",
                "todowrite",
                "question",
            ],
        ),
    ]
}

fn builtin_runtime_subagent_profile(
    id: &str,
    name: &str,
    description: &str,
    permission: PermissionRuleset,
    prompt: &str,
    tools: &[&str],
) -> RuntimeSubagentProfile {
    RuntimeSubagentProfile {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        mode: "subagent".to_string(),
        permission,
        task_permissions: Vec::new(),
        skills: Vec::new(),
        skill_roots: Vec::new(),
        skill_permissions: Vec::new(),
        prompt: prompt.trim_start_matches('\u{feff}').to_string(),
        tools: tools.iter().map(|item| (*item).to_string()).collect(),
        provider: None,
        model: None,
        max_steps: None,
        temperature: None,
        top_p: None,
        color: None,
        disabled: false,
        model_options: BTreeMap::new(),
        workspace_isolation: false,
        hidden: false,
        source_path: None,
    }
}

fn runtime_agent_profile_from_value(
    value: &Value,
    fallback_id: &str,
    source_path: Option<PathBuf>,
) -> Option<RuntimeSubagentProfile> {
    if value.as_object().is_none_or(Map::is_empty) {
        return None;
    }
    let schema = parse_agent_profile_schema(value, fallback_id, fallback_id).ok()?;
    let permission = schema
        .permission
        .as_deref()
        .and_then(|raw| parse_permission_ruleset(raw).ok())
        .unwrap_or(PermissionRuleset::PlanOnly);
    Some(RuntimeSubagentProfile {
        id: schema.id,
        name: schema.name,
        description: schema.description.unwrap_or_default(),
        mode: schema.mode,
        permission,
        task_permissions: schema.task.permissions,
        skills: schema.skill.skills,
        skill_roots: schema.skill.roots,
        skill_permissions: schema.skill.permissions,
        prompt: schema
            .prompt
            .as_deref()
            .unwrap_or(BUILD_AGENT_PROMPT)
            .trim_start_matches('\u{feff}')
            .to_string(),
        tools: schema.tools,
        provider: schema.provider,
        model: schema.model,
        max_steps: schema.max_steps,
        temperature: schema.temperature,
        top_p: schema.top_p,
        color: schema.color,
        disabled: schema.disabled,
        model_options: schema.model_options,
        workspace_isolation: schema.workspace_isolation,
        hidden: schema.hidden,
        source_path,
    })
}

fn runtime_subagent_profile(id: &str, workspace: &Path) -> Option<RuntimeSubagentProfile> {
    let normalized = sanitize_runtime_agent_id(id);
    runtime_subagent_profiles(workspace)
        .into_iter()
        .find(|profile| profile.id == normalized || profile.name.eq_ignore_ascii_case(id))
}

fn runtime_agent_profile(id: &str, workspace: &Path) -> Option<RuntimeSubagentProfile> {
    let normalized = sanitize_runtime_agent_id(id);
    runtime_agent_profiles(workspace)
        .into_iter()
        .find(|profile| profile.id == normalized || profile.name.eq_ignore_ascii_case(id))
}

fn runtime_task_subagent_descriptors(
    workspace: &Path,
    agent_profile: Option<&RuntimeSubagentProfile>,
    parent_session: Option<&Session>,
) -> Vec<TaskSubagentDescriptor> {
    runtime_subagent_profiles(workspace)
        .into_iter()
        .filter(|profile| !profile.hidden)
        .filter(|profile| {
            agent_profile.is_none_or(|parent| {
                task_subagent_is_visible(&parent.task_permissions, &profile.id)
            })
        })
        .filter(|profile| {
            parent_session
                .is_none_or(|session| runtime_task_governance_error(session, profile).is_none())
        })
        .map(|profile| TaskSubagentDescriptor {
            id: profile.id,
            name: profile.name,
            description: profile.description,
        })
        .collect()
}

fn runtime_agent_profile_for_session(session: &Session) -> Option<RuntimeSubagentProfile> {
    if let Some(profile_value) = session.metadata.get("agent_profile") {
        let fallback_id = profile_value
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| session.metadata.get("agent").and_then(Value::as_str))
            .unwrap_or("agent");
        if let Some(profile) = runtime_agent_profile_from_value(profile_value, fallback_id, None) {
            return Some(profile);
        }
    }
    session
        .metadata
        .get("agent")
        .and_then(Value::as_str)
        .and_then(|id| runtime_agent_profile(id, &session.directory))
}

fn skills_payload(config: &HttpRuntimeConfig) -> Value {
    let workspace = config
        .workspace
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let registry = SkillRegistry::new_with_options(
        Some(workspace),
        Option::<Vec<String>>::None,
        Option::<PathBuf>::None,
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    );
    let mut report = registry.report(None, None);
    report.skills.retain(skill_info_model_invocable);
    json!({
        "skills": report.skills,
        "loaded_count": report.loaded_count,
        "scanned_files": report.scanned_files,
        "invalid_count": report.invalid_count,
        "duplicate_count": report.duplicate_count,
        "issues": report.issues,
    })
}

fn filter_runtime_tools_for_profile(
    tools: Vec<ToolSchema>,
    profile: Option<&RuntimeSubagentProfile>,
) -> Vec<ToolSchema> {
    let Some(profile) = profile else {
        return tools;
    };
    if profile.tools.is_empty() {
        return tools;
    }
    tools
        .into_iter()
        .filter(|tool| runtime_tool_allowed_for_profile(&tool.name, profile))
        .collect()
}

fn runtime_agent_tool_options(profile: Option<&RuntimeSubagentProfile>) -> BTreeMap<String, Value> {
    let mut options = BTreeMap::new();
    if let Some(profile) = profile {
        options.insert("agent_id".to_string(), json!(profile.id.clone()));
        options.insert("agent".to_string(), json!(profile.id.clone()));
        if !profile.skills.is_empty() {
            options.insert("skills".to_string(), json!(profile.skills.clone()));
        }
        if !profile.skill_roots.is_empty() {
            options.insert(
                "skill_roots".to_string(),
                json!(profile.skill_roots.clone()),
            );
        }
        if !profile.skill_permissions.is_empty() {
            options.insert(
                "skill_permissions".to_string(),
                json!(profile.skill_permissions.clone()),
            );
        }
    }
    options
}

fn runtime_tool_allowed_for_profile(tool_name: &str, profile: &RuntimeSubagentProfile) -> bool {
    profile
        .tools
        .iter()
        .any(|pattern| runtime_tool_pattern_matches(pattern, tool_name))
}

fn runtime_agent_allows_tool(profile: &RuntimeSubagentProfile, tool_name: &str) -> bool {
    profile.tools.is_empty() || runtime_tool_allowed_for_profile(tool_name, profile)
}

fn runtime_tool_pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim();
    if pattern == "*" || pattern == value {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    false
}

fn toolkit_with_runtime_task_tool(
    session: &Session,
    agent_profile: Option<&RuntimeSubagentProfile>,
) -> Toolkit {
    let mut toolkit = Toolkit::with_builtins();
    register_task_tool(
        &mut toolkit.registry,
        &runtime_task_subagent_descriptors(&session.directory, agent_profile, Some(session)),
    );
    toolkit
}

#[derive(Clone, Debug)]
struct RuntimeMcpRuntime {
    manager: RemoteMcpManager,
    descriptors: BTreeMap<String, RemoteMcpToolDescriptor>,
    workspace: PathBuf,
}

fn register_runtime_mcp_tools(
    config: &HttpRuntimeConfig,
    workspace: &Path,
    toolkit: &mut Toolkit,
) -> Option<RuntimeMcpRuntime> {
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    let source = mcp_config_source_for_workspace(config, &env, workspace);
    let mcp_config = match source
        .read_source
        .as_deref()
        .map(load_mcp_config)
        .transpose()
    {
        Ok(Some(config)) if config.enabled() => config,
        _ => return None,
    };
    let mut manager = RemoteMcpManager::new(mcp_config.clone());
    let mut descriptors_by_name = BTreeMap::new();
    for server in mcp_config.servers.iter().filter(|server| server.enabled) {
        if let Some(result) = refresh_mcp_lifecycle_server(server, workspace) {
            match result {
                Ok(descriptors) => {
                    for descriptor in &descriptors {
                        toolkit
                            .registry
                            .register(mcp_tool_definition(descriptor, "remote-mcp"));
                        descriptors_by_name
                            .insert(descriptor.dynamic_name.clone(), descriptor.clone());
                    }
                    let _ = manager.set_server_tools(
                        &server.name,
                        Some(McpTransport::Stdio),
                        "connected",
                        Some(now_ms() as f64 / 1000.0),
                        descriptors,
                    );
                }
                Err(error) => {
                    let _ = manager.set_server_error(
                        &server.name,
                        "error",
                        sanitize_mcp_status_error(&error),
                        Some(now_ms() as f64 / 1000.0),
                    );
                }
            }
            continue;
        }
        match discover_mcp_server_tools(server, workspace) {
            Ok((transport, tools)) => {
                let descriptors = build_tool_descriptors_from_values(server, &tools);
                for descriptor in &descriptors {
                    toolkit
                        .registry
                        .register(mcp_tool_definition(descriptor, "remote-mcp"));
                    descriptors_by_name.insert(descriptor.dynamic_name.clone(), descriptor.clone());
                }
                let _ = manager.set_server_tools(
                    &server.name,
                    Some(transport),
                    "connected",
                    Some(now_ms() as f64 / 1000.0),
                    descriptors,
                );
            }
            Err(error) => {
                let _ = manager.set_server_error(
                    &server.name,
                    "error",
                    sanitize_mcp_status_error(&error),
                    Some(now_ms() as f64 / 1000.0),
                );
            }
        }
    }
    Some(RuntimeMcpRuntime {
        manager,
        descriptors: descriptors_by_name,
        workspace: workspace.to_path_buf(),
    })
}

fn max_subagent_depth() -> u64 {
    std::env::var("OPENAGENT_MAX_SUBAGENT_DEPTH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_SUBAGENT_DEPTH)
        .max(1)
}

fn runtime_child_task_depth(parent_session: &Session) -> u64 {
    if parent_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        parent_session
            .metadata
            .get("task_depth")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .saturating_add(1)
    } else {
        1
    }
}

fn runtime_task_root_session_id(parent_session: &Session) -> String {
    if parent_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        parent_session
            .metadata
            .get("task_root_session_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(parent_session.id.as_str())
            .to_string()
    } else {
        parent_session.id.clone()
    }
}

fn runtime_parent_task_lineage(parent_session: &Session) -> Vec<String> {
    parent_session
        .metadata
        .get("task_lineage_subagents")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            parent_session
                .metadata
                .get("agent")
                .and_then(Value::as_str)
                .filter(|_| {
                    parent_session
                        .metadata
                        .get("subagent")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .map(|agent| vec![agent.to_string()])
                .unwrap_or_default()
        })
}

fn runtime_child_task_lineage(parent_session: &Session, child_agent: &str) -> Vec<String> {
    let mut lineage = runtime_parent_task_lineage(parent_session);
    lineage.push(child_agent.to_string());
    lineage
}

fn runtime_task_governance_error(
    parent_session: &Session,
    profile: &RuntimeSubagentProfile,
) -> Option<String> {
    let lineage = runtime_parent_task_lineage(parent_session);
    let parent_agent = parent_session
        .metadata
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if parent_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && parent_agent == profile.id
    {
        return Some(format!("subagent {} cannot call itself", profile.id));
    }
    if lineage.iter().any(|agent| agent == &profile.id) {
        return Some(format!(
            "subagent {} is already in task lineage",
            profile.id
        ));
    }
    let child_depth = runtime_child_task_depth(parent_session);
    let max_depth = max_subagent_depth();
    if child_depth > max_depth {
        return Some(format!(
            "subagent nesting depth {child_depth} exceeds max subagent depth {max_depth}"
        ));
    }
    None
}

fn runtime_is_subagent_mode(mode: &str) -> bool {
    matches!(mode, "subagent" | "all")
}

fn runtime_permission_manager_for_agent(
    ruleset: PermissionRuleset,
    agent_profile: Option<&RuntimeSubagentProfile>,
) -> PermissionManager {
    let mut manager = PermissionManager::new();
    manager.set_ruleset(ruleset);
    if let Some(profile) = agent_profile {
        for rule in &profile.task_permissions {
            manager.add_rule(permission_rule(
                TASK_TOOL_ID,
                rule.action.clone(),
                Some(&rule.pattern),
            ));
        }
        for rule in &profile.skill_permissions {
            manager.add_rule(permission_rule(
                "skill",
                rule.action.clone(),
                Some(&rule.pattern),
            ));
        }
    }
    manager
}

fn sanitize_runtime_agent_id(value: &str) -> String {
    let mut output = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    while output.contains("--") {
        output = output.replace("--", "-");
    }
    if output.is_empty() {
        "agent".to_string()
    } else {
        output
    }
}

fn default_model_id() -> String {
    std::env::var("OPENAGENT_MODEL").unwrap_or_else(|_| "server-local".to_string())
}

fn mdns_payload(config: &HttpRuntimeConfig) -> Value {
    json!({
        "enabled": config.mdns_name.as_ref().is_some_and(|value| !value.is_empty()),
        "service": "_openagent._tcp",
        "name": config.mdns_name.clone().unwrap_or_default(),
        "host": config.host,
        "port": config.port,
        "url": format!("http://{}:{}", config.host, config.port),
    })
}

fn create_session_payload(config: &HttpRuntimeConfig, body: &str) -> Value {
    let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let workspace = payload
        .get("cwd")
        .or_else(|| payload.get("workspace"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| config.workspace.as_ref().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let session_id = new_id("session");
    let store = FileSessionStore::new(session_root(config));
    let mut session = if let Some(fork_from) = payload.get("fork_from").and_then(Value::as_str) {
        match store.fork_session(fork_from, &session_id, workspace.clone(), None) {
            Ok(mut forked) => {
                forked
                    .metadata
                    .insert("parent_session_id".to_string(), json!(fork_from));
                forked
            }
            Err(_) => Session::new(session_id.clone(), workspace.clone()),
        }
    } else {
        Session::new(session_id.clone(), workspace.clone())
    };
    session
        .metadata
        .insert("created_by".to_string(), json!("openagent-http-runtime"));
    if let Some(title) = payload
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        session
            .metadata
            .insert("title".to_string(), json!(title.trim()));
    }
    let _ = store.save_state(&session, None);
    json!({
        "session_id": session_id,
        "status": "created",
        "session": {
            "id": session_id,
            "session_id": session_id,
            "status": "idle",
            "message_count": 0,
            "workspace": workspace.to_string_lossy(),
        }
    })
}

fn get_session_payload(config: &HttpRuntimeConfig, session_id: &str) -> Value {
    let store = FileSessionStore::new(session_root(config));
    match store.load_session(session_id) {
        Ok(session) => json!({
            "session_id": session.id,
            "session": {
                "id": session.id,
                "session_id": session.id,
                "workspace": session.directory.to_string_lossy(),
                "status": session_status_text(&session.status),
                "message_count": session.messages.len(),
                "metadata": session.metadata,
            },
            "workspace": session.directory.to_string_lossy(),
            "status": session_status_text(&session.status),
            "message_count": session.messages.len(),
            "metadata": session.metadata,
        }),
        Err(error) => json!({"error": error.to_string()}),
    }
}

fn session_messages_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    request_path: &str,
) -> Result<Value, String> {
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let store = FileSessionStore::new(session_root(config));
    let session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let total = session.messages.len();
    let limit = query_param(request_path, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50)
        .min(200);
    let before = query_param(request_path, "before");
    let messages_v2 = store
        .list_messages_with_parts(session_id, Some(limit), before.as_deref())
        .map_err(|error| error.to_string())?;
    let start = total.saturating_sub(limit);
    let messages = session
        .messages
        .iter()
        .enumerate()
        .skip(start)
        .map(|(index, message)| {
            let mut value = serde_json::to_value(message).unwrap_or_else(|_| json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("index".to_string(), json!(index));
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "session_id": session.id,
        "message_count": total,
        "message_v2_count": messages_v2.len(),
        "limit": limit,
        "messages": messages,
        "messages_v2": messages_v2,
    }))
}

fn update_session_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    if let Some(title) = payload.get("title").and_then(Value::as_str) {
        let title = title.trim();
        if title.is_empty() {
            session.metadata.remove("title");
        } else {
            session.metadata.insert("title".to_string(), json!(title));
        }
    }
    if let Some(archived) = payload.get("archived").and_then(Value::as_bool) {
        if archived {
            session.metadata.insert("archived".to_string(), json!(true));
            session
                .metadata
                .insert("archived_at_ms".to_string(), json!(now_ms()));
        } else {
            session.metadata.remove("archived");
            session.metadata.remove("archived_at_ms");
        }
    }
    set_session_text_metadata(&mut session, &payload, "agent");
    set_session_text_metadata(&mut session, &payload, "model");
    set_session_text_metadata(&mut session, &payload, "variant");
    set_session_text_metadata(&mut session, &payload, "thinking");
    store
        .save_state(&session, None)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "session_id": session.id,
        "updated": true,
        "session": session_summary_from_session(&session),
    }))
}

fn session_state_exists(config: &HttpRuntimeConfig, session_id: &str) -> bool {
    let session_dir = session_root(config).join(session_id);
    session_dir.join("state.latest.json").is_file()
        || session_dir.join("transcript.jsonl").is_file()
}

fn delete_session_payload(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    if !valid_session_id(session_id) {
        return Err("invalid session id".to_string());
    }
    let target = session_root(config).join(session_id);
    let removed = if target.exists() {
        fs::remove_dir_all(&target).map_err(|error| error.to_string())?;
        true
    } else {
        false
    };
    Ok(json!({"session_id": session_id, "removed": removed}))
}

fn session_children_payload(config: &HttpRuntimeConfig, session_id: &str) -> Value {
    let root = session_root(config);
    let mut children = Vec::new();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let state = read_json_file(&path.join("state.latest.json"));
            let parent = state
                .get("metadata")
                .and_then(|metadata| {
                    metadata
                        .get("parent_session_id")
                        .or_else(|| metadata.get("forked_from"))
                })
                .and_then(Value::as_str)
                .unwrap_or_default();
            if parent == session_id {
                children.push(session_summary_from_state(
                    &state,
                    &entry.file_name().to_string_lossy(),
                ));
            }
        }
    }
    children.sort_by(|left, right| {
        right["updated_at_ms"]
            .as_u64()
            .cmp(&left["updated_at_ms"].as_u64())
    });
    json!({"session_id": session_id, "children": children})
}

fn session_tasks_payload(config: &HttpRuntimeConfig, session_id: &str) -> Value {
    let root = session_root(config);
    let mut all_tasks = Vec::new();
    let mut tasks = Vec::new();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let state = read_json_file(&path.join("state.latest.json"));
            let metadata = state
                .get("metadata")
                .filter(|value| value.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            let parent = metadata
                .get("parent_session_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let subagent = metadata
                .get("subagent")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if subagent {
                let task = session_task_summary_from_state(
                    &root,
                    &state,
                    &entry.file_name().to_string_lossy(),
                );
                if parent == session_id {
                    tasks.push(task.clone());
                }
                all_tasks.push(task);
            }
        }
    }
    tasks.sort_by(|left, right| {
        right["updated_at_ms"]
            .as_u64()
            .cmp(&left["updated_at_ms"].as_u64())
    });
    let tree = task_tree_for_parent(&all_tasks, session_id);
    let flat_tasks = flatten_task_tree(&tree);
    json!({
        "session_id": session_id,
        "tasks": tasks,
        "flat_tasks": flat_tasks,
        "tree": tree,
    })
}

fn task_tree_for_parent(all_tasks: &[Value], parent_session_id: &str) -> Vec<Value> {
    let mut visited = BTreeSet::new();
    task_tree_for_parent_inner(all_tasks, parent_session_id, &mut visited)
}

fn task_tree_for_parent_inner(
    all_tasks: &[Value],
    parent_session_id: &str,
    visited: &mut BTreeSet<String>,
) -> Vec<Value> {
    let mut children = all_tasks
        .iter()
        .filter(|task| {
            task.get("parent_session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                == parent_session_id
        })
        .cloned()
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        right["updated_at_ms"]
            .as_u64()
            .cmp(&left["updated_at_ms"].as_u64())
    });
    children
        .into_iter()
        .filter_map(|mut task| {
            let task_id = task
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if task_id.is_empty() || !visited.insert(task_id.clone()) {
                return None;
            }
            let nested = task_tree_for_parent_inner(all_tasks, &task_id, visited);
            if let Some(object) = task.as_object_mut() {
                object.insert("children".to_string(), Value::Array(nested));
            }
            Some(task)
        })
        .collect()
}

fn flatten_task_tree(tree: &[Value]) -> Vec<Value> {
    let mut flat = Vec::new();
    for task in tree {
        let mut without_children = task.clone();
        let children = without_children
            .as_object_mut()
            .and_then(|object| object.remove("children"))
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
        flat.push(without_children);
        flat.extend(flatten_task_tree(&children));
    }
    flat
}

fn run_session_task_payload(
    config: &HttpRuntimeConfig,
    parent_session_id: &str,
    task_id: &str,
    body: &str,
) -> Result<Value, String> {
    if !valid_session_id(parent_session_id) || !valid_session_id(task_id) {
        return Err("invalid session id".to_string());
    }
    let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let store = FileSessionStore::new(session_root(config));
    let mut child_session = store
        .load_session(task_id)
        .map_err(|error| error.to_string())?;
    let parent = child_session
        .metadata
        .get("parent_session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if parent != parent_session_id {
        return Err("task does not belong to parent session".to_string());
    }
    if !child_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err("session is not a subagent task".to_string());
    }
    let task_status = child_session
        .metadata
        .get("task_status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if task_status != "queued" {
        return Err(format!("task is not queued: {task_status}"));
    }
    let _task_run_lock = claim_session_task_run_lock(config, task_id)?;
    child_session = store
        .load_session(task_id)
        .map_err(|error| error.to_string())?;
    let parent = child_session
        .metadata
        .get("parent_session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if parent != parent_session_id {
        return Err("task does not belong to parent session".to_string());
    }
    if !child_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err("session is not a subagent task".to_string());
    }
    let task_status = child_session
        .metadata
        .get("task_status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if task_status != "queued" {
        return Err(format!("task is not queued: {task_status}"));
    }

    let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
    let run_id = state
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| new_id("turn"));
    let agent_name = child_session
        .metadata
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or("subagent")
        .to_string();
    let provider = child_session
        .metadata
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("openai")
        .to_string();
    let model = child_session
        .metadata
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let permission_raw = child_session
        .metadata
        .get("permission")
        .and_then(Value::as_str)
        .unwrap_or("PLAN_ONLY")
        .to_string();
    let permission_ruleset = parse_permission_ruleset(&permission_raw)?;
    let max_steps = child_session
        .metadata
        .get("max_steps")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| provider_max_steps(&payload));
    let skip_permissions = skip_permissions_for_turn(&payload);

    child_session.status = SessionStatus::Running;
    child_session
        .metadata
        .insert("task_status".to_string(), json!("running"));
    child_session.metadata.insert(
        "run_started_by".to_string(),
        json!(if payload
            .get("background_worker")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "background_worker"
        } else {
            "run_task"
        }),
    );
    child_session
        .metadata
        .insert("run_claimed_at_ms".to_string(), json!(now_ms()));
    store
        .start_run(
            &mut child_session,
            StartRunOptions {
                run_id: run_id.clone(),
                trace_id: new_id("trace"),
                agent_name,
                model_id: model.clone(),
                provider_id: Some(provider.clone()),
                permission: if skip_permissions {
                    format!("auto_allow:{permission_raw}")
                } else {
                    permission_raw.clone()
                },
                max_steps,
                started_at_ms: None,
            },
        )
        .map_err(|error| format!("failed to start task run: {error}"))?;

    for (index, message) in child_session.messages.iter().enumerate() {
        store
            .append_message(&child_session, message, &run_id, index as u64)
            .map_err(|error| format!("failed to record task prompt: {error}"))?;
    }

    let mut child_payload = provider_resume_payload(&payload);
    if let Some(object) = child_payload.as_object_mut() {
        object.insert("max_steps".to_string(), json!(max_steps));
    }
    let loop_result = run_provider_loop(RuntimeProviderLoopInput {
        config,
        store: &store,
        session: &mut child_session,
        run_id: &run_id,
        payload: &child_payload,
        permission_ruleset,
        skip_permissions,
        events: Vec::new(),
        carry: RuntimeProviderLoopCarry::default(),
    });

    let (status, output) = match loop_result {
        Ok(value) => {
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed")
                .to_string();
            child_session
                .metadata
                .insert("task_status".to_string(), json!(status.clone()));
            let _ = store.save_state(&child_session, Some(&run_id));
            (status, value)
        }
        Err(error) => {
            child_session.status = SessionStatus::Idle;
            child_session
                .metadata
                .insert("task_status".to_string(), json!("failed"));
            let _ = store.finish_run(
                &child_session,
                &run_id,
                "failed",
                1,
                Some("error"),
                Some(&error),
            );
            let _ = store.save_state(&child_session, Some(&run_id));
            (
                "failed".to_string(),
                json!({"status": "failed", "error": error}),
            )
        }
    };
    let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
    let task = session_task_summary_from_state(&session_root(config), &state, task_id);
    Ok(json!({
        "session_id": parent_session_id,
        "task_id": task_id,
        "run_id": run_id,
        "status": status,
        "task": task,
        "result": output,
    }))
}

struct RuntimeTaskRunLock {
    path: PathBuf,
}

impl Drop for RuntimeTaskRunLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn task_run_lock_path(config: &HttpRuntimeConfig, task_id: &str) -> PathBuf {
    session_root(config).join(task_id).join("task.run.lock")
}

fn claim_session_task_run_lock(
    config: &HttpRuntimeConfig,
    task_id: &str,
) -> Result<RuntimeTaskRunLock, String> {
    let path = task_run_lock_path(config, task_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    match create_session_task_run_lock(&path, task_id) {
        Ok(lock) => Ok(lock),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !remove_stale_task_run_lock(&path)? {
                return Err("task is already running".to_string());
            }
            match create_session_task_run_lock(&path, task_id) {
                Ok(lock) => Ok(lock),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Err("task is already running".to_string())
                }
                Err(error) => Err(format!("failed to claim task run lock: {error}")),
            }
        }
        Err(error) => Err(format!("failed to claim task run lock: {error}")),
    }
}

fn create_session_task_run_lock(path: &Path, task_id: &str) -> std::io::Result<RuntimeTaskRunLock> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let payload = json!({
        "task_id": task_id,
        "claimed_at_ms": now_ms(),
    });
    if let Err(error) = writeln!(file, "{payload}") {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(RuntimeTaskRunLock {
        path: path.to_path_buf(),
    })
}

fn task_run_lock_stale_ms() -> u64 {
    std::env::var("OPENAGENT_TASK_RUN_LOCK_STALE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TASK_RUN_LOCK_STALE_MS)
}

fn remove_stale_task_run_lock(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let lock = read_json_file(path);
    let Some(claimed_at_ms) = lock.get("claimed_at_ms").and_then(Value::as_u64) else {
        return Ok(false);
    };
    if now_ms().saturating_sub(claimed_at_ms) < task_run_lock_stale_ms() {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!("failed to remove stale task run lock: {error}")),
    }
}

fn cancel_session_task_payload(
    config: &HttpRuntimeConfig,
    parent_session_id: &str,
    task_id: &str,
) -> Result<Value, String> {
    if !valid_session_id(parent_session_id) || !valid_session_id(task_id) {
        return Err("invalid session id".to_string());
    }
    let store = FileSessionStore::new(session_root(config));
    let mut child_session = store
        .load_session(task_id)
        .map_err(|error| error.to_string())?;
    let parent = child_session
        .metadata
        .get("parent_session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if parent != parent_session_id {
        return Err("task does not belong to parent session".to_string());
    }
    if !child_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err("session is not a subagent task".to_string());
    }
    let task_status = child_session
        .metadata
        .get("task_status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if task_status != "queued" {
        return Err(format!("task is not queued: {task_status}"));
    }
    let lock_path = task_run_lock_path(config, task_id);
    if lock_path.exists() && !remove_stale_task_run_lock(&lock_path)? {
        return Err("task is already running".to_string());
    }
    let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
    let run_id = state
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| new_id("turn"));
    child_session.status = SessionStatus::Idle;
    child_session
        .metadata
        .insert("task_status".to_string(), json!("canceled"));
    child_session
        .metadata
        .insert("canceled_at_ms".to_string(), json!(now_ms()));
    store
        .save_state(&child_session, Some(&run_id))
        .map_err(|error| format!("failed to cancel task: {error}"))?;
    let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
    let task = session_task_summary_from_state(&session_root(config), &state, task_id);
    Ok(json!({
        "session_id": parent_session_id,
        "task_id": task_id,
        "run_id": run_id,
        "status": "canceled",
        "task": task,
    }))
}

fn share_session_payload(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let share_id = session
        .metadata
        .get("share_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| new_id("share"));
    let url = format!("openagent://share/{share_id}");
    session.metadata.insert("shared".to_string(), json!(true));
    session
        .metadata
        .insert("share_id".to_string(), json!(share_id));
    session.metadata.insert("share_url".to_string(), json!(url));
    session
        .metadata
        .insert("shared_at_ms".to_string(), json!(now_ms()));
    store
        .save_state(&session, None)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "session_id": session.id,
        "shared": true,
        "share_id": session.metadata.get("share_id").cloned().unwrap_or(Value::Null),
        "url": session.metadata.get("share_url").cloned().unwrap_or(Value::Null),
    }))
}

fn unshare_session_payload(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    session.metadata.remove("shared");
    session.metadata.remove("share_id");
    session.metadata.remove("share_url");
    session.metadata.remove("shared_at_ms");
    store
        .save_state(&session, None)
        .map_err(|error| error.to_string())?;
    Ok(json!({"session_id": session.id, "shared": false}))
}

fn compact_session_payload(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    if matches!(
        session.status,
        SessionStatus::Running | SessionStatus::Paused
    ) {
        return Err("session must be idle before compacting".to_string());
    }
    session.status = SessionStatus::Compacting;
    store
        .save_state(&session, None)
        .map_err(|error| error.to_string())?;
    let summary = summarize_session_messages(&session);
    session.status = SessionStatus::Idle;
    session.metadata.insert(
        "compact".to_string(),
        json!({
            "compacted_at_ms": now_ms(),
            "message_count": session.messages.len(),
            "summary": summary,
        }),
    );
    store
        .save_state(&session, None)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "session_id": session.id,
        "status": "compacted",
        "summary": session.metadata.get("compact").cloned().unwrap_or(Value::Null),
    }))
}

fn pending_approvals_payload(config: &HttpRuntimeConfig, request_path: &str) -> Value {
    pending_interactions_payload(
        config,
        request_path,
        "pending_approval",
        "approval",
        "approvals",
    )
}

fn pending_questions_payload(config: &HttpRuntimeConfig, request_path: &str) -> Value {
    pending_interactions_payload(
        config,
        request_path,
        "pending_question",
        "question",
        "questions",
    )
}

fn pending_interactions_payload(
    config: &HttpRuntimeConfig,
    request_path: &str,
    metadata_key: &str,
    item_key: &str,
    collection_key: &str,
) -> Value {
    let root = session_root(config);
    let session_filter = query_param(request_path, "session_id");
    let mut items = Vec::new();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let session_id = entry.file_name().to_string_lossy().to_string();
            if session_filter
                .as_deref()
                .is_some_and(|filter| filter != session_id)
            {
                continue;
            }
            let state = read_json_file(&path.join("state.latest.json"));
            let Some(pending) = state
                .get("metadata")
                .and_then(|metadata| metadata.get(metadata_key))
                .filter(|value| value.is_object())
                .cloned()
            else {
                continue;
            };
            let request_id = pending
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let turn_id = pending
                .get("turn_id")
                .or_else(|| pending.get("run_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut item = Map::new();
            item.insert("kind".to_string(), json!(item_key));
            item.insert("status".to_string(), json!("pending"));
            item.insert("session_id".to_string(), json!(session_id));
            item.insert("turn_id".to_string(), json!(turn_id));
            item.insert("request_id".to_string(), json!(request_id));
            item.insert(item_key.to_string(), pending);
            item.insert(
                "session".to_string(),
                session_summary_from_state(&state, &entry.file_name().to_string_lossy()),
            );
            items.push(Value::Object(item));
        }
    }
    items.sort_by(|left, right| {
        right[item_key]["created_at_ms"]
            .as_u64()
            .cmp(&left[item_key]["created_at_ms"].as_u64())
    });
    let count = items.len();
    json!({
        collection_key: items,
        "count": count,
        "session_id": session_filter.unwrap_or_default(),
    })
}

fn respond_global_approval_payload(
    config: &HttpRuntimeConfig,
    request_id: &str,
    body: &str,
) -> Result<Value, String> {
    let turn_id = find_pending_interaction_turn(config, "pending_approval", request_id)?;
    respond_approval_payload(
        config,
        &format!("/api/turns/{turn_id}/approvals/{request_id}"),
        body,
    )
}

fn respond_global_question_payload(
    config: &HttpRuntimeConfig,
    request_id: &str,
    body: &str,
) -> Result<Value, String> {
    let turn_id = find_pending_interaction_turn(config, "pending_question", request_id)?;
    respond_question_payload(
        config,
        &format!("/api/turns/{turn_id}/questions/{request_id}/reply"),
        body,
    )
}

fn find_pending_interaction_turn(
    config: &HttpRuntimeConfig,
    metadata_key: &str,
    request_id: &str,
) -> Result<String, String> {
    if request_id.is_empty() || request_id.contains('/') || request_id.contains("..") {
        return Err("invalid request id".to_string());
    }
    let root = session_root(config);
    for entry in fs::read_dir(&root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.path().is_dir() {
            continue;
        }
        let state = read_json_file(&entry.path().join("state.latest.json"));
        let Some(pending) = state
            .get("metadata")
            .and_then(|metadata| metadata.get(metadata_key))
        else {
            continue;
        };
        if pending.get("request_id").and_then(Value::as_str) != Some(request_id) {
            continue;
        }
        let turn_id = pending
            .get("turn_id")
            .or_else(|| pending.get("run_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "pending interaction is missing turn_id".to_string())?;
        return Ok(turn_id.to_string());
    }
    Err("pending interaction not found".to_string())
}

fn files_payload(config: &HttpRuntimeConfig, request_path: &str) -> Result<Value, String> {
    let root = workspace(config);
    let requested = query_param(request_path, "path").unwrap_or_default();
    let include_content = query_flag(request_path, "content");
    let depth = query_param(request_path, "depth")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2)
        .min(4);
    let target = resolve_path_in_root(&root, &requested)?;
    let relative = workspace_relative_path(&root, &target);
    if !target.exists() {
        return Ok(json!({
            "workspace": root.to_string_lossy(),
            "path": relative,
            "absolute_path": target.to_string_lossy(),
            "exists": false,
            "is_file": false,
            "is_dir": false,
            "entries": [],
            "entry_count": 0,
            "truncated": false,
            "content": Value::Null,
            "error": "not found",
        }));
    }
    let mut entries = Vec::new();
    collect_file_entries(&root, &target, depth, &mut entries)?;
    let content = if include_content && target.is_file() && file_is_text_like(&target) {
        fs::read_to_string(&target).ok()
    } else {
        None
    };
    let entry_count = entries.len();
    let truncated = entry_count >= MAX_FILE_TREE_ENTRIES;
    Ok(json!({
        "workspace": root.to_string_lossy(),
        "path": relative,
        "absolute_path": target.to_string_lossy(),
        "exists": true,
        "is_file": target.is_file(),
        "is_dir": target.is_dir(),
        "entries": entries,
        "entry_count": entry_count,
        "truncated": truncated,
        "content": content,
    }))
}

fn git_payload(config: &HttpRuntimeConfig) -> Result<Value, String> {
    let root = workspace(config);
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("status")
        .arg("--porcelain=v1")
        .arg("--branch")
        .output()
        .map_err(|error| format!("failed to run git status: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Ok(json!({
            "workspace": root.to_string_lossy(),
            "is_repo": false,
            "branch": "",
            "ahead": 0,
            "behind": 0,
            "changes": [],
            "change_count": 0,
            "error": stderr,
        }));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branch = String::new();
    let mut ahead = 0_i64;
    let mut behind = 0_i64;
    let mut changes = Vec::new();
    for line in stdout.lines() {
        if let Some(raw) = line.strip_prefix("## ") {
            let parsed = parse_git_branch_line(raw);
            branch = parsed.0;
            ahead = parsed.1;
            behind = parsed.2;
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let xy = &line[..2];
        let path = line[3..].to_string();
        changes.push(json!({
            "status": xy.trim(),
            "index": xy.chars().next().unwrap_or(' '),
            "worktree": xy.chars().nth(1).unwrap_or(' '),
            "path": path,
        }));
    }
    let change_count = changes.len();
    Ok(json!({
        "workspace": root.to_string_lossy(),
        "is_repo": true,
        "branch": branch,
        "ahead": ahead,
        "behind": behind,
        "changes": changes,
        "change_count": change_count,
    }))
}

fn terminal_run_payload(config: &HttpRuntimeConfig, body: &str) -> Result<Value, String> {
    let payload = serde_json::from_str::<Value>(body).unwrap_or_else(|_| json!({}));
    let command_text = payload
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if command_text.is_empty() {
        return Err("terminal command is required".to_string());
    }
    if command_text.chars().count() > MAX_TERMINAL_COMMAND_CHARS {
        return Err(format!(
            "terminal command exceeds {MAX_TERMINAL_COMMAND_CHARS} characters"
        ));
    }

    let root = workspace(config);
    let requested_cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let cwd = if requested_cwd.is_empty() {
        resolve_path_in_root(&root, ".")?
    } else {
        resolve_path_in_root(&root, requested_cwd)?
    };
    if !cwd.exists() {
        return Err(format!("terminal cwd does not exist: {}", cwd.display()));
    }
    if !cwd.is_dir() {
        return Err(format!(
            "terminal cwd is not a directory: {}",
            cwd.display()
        ));
    }

    let timeout_ms = payload
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TERMINAL_TIMEOUT_MS)
        .clamp(250, MAX_TERMINAL_TIMEOUT_MS);
    run_terminal_command(&command_text, &root, &cwd, timeout_ms)
}

fn run_terminal_command(
    command_text: &str,
    root: &Path,
    cwd: &Path,
    timeout_ms: u64,
) -> Result<Value, String> {
    let started_at_ms = now_ms();
    let started = Instant::now();
    let mut command = terminal_shell_command(command_text);
    let mut child = command
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to run terminal command: {error}"))?;

    let timeout = Duration::from_millis(timeout_ms);
    let mut timed_out = false;
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("failed to poll terminal command: {error}"))?
        {
            Some(_) => break,
            None if started.elapsed() >= timeout => {
                timed_out = true;
                let _ = child.kill();
                break;
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to collect terminal output: {error}"))?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let exit_code = output
        .status
        .code()
        .unwrap_or(if timed_out { -1 } else { 1 });
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let (stdout, stdout_truncated) = truncate_terminal_output(stdout);
    let (stderr, stderr_truncated) = truncate_terminal_output(stderr);

    Ok(json!({
        "command": command_text,
        "workspace": root.to_string_lossy(),
        "cwd": cwd.to_string_lossy(),
        "cwd_relative": workspace_relative_path(root, cwd),
        "success": output.status.success() && !timed_out,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "timeout_ms": timeout_ms,
        "duration_ms": duration_ms,
        "started_at_ms": started_at_ms,
        "finished_at_ms": now_ms(),
        "stdout": stdout,
        "stderr": stderr,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
    }))
}

fn terminal_shell_command(command_text: &str) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(command_text);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.arg("-lc").arg(command_text);
        command
    }
}

fn truncate_terminal_output(text: String) -> (String, bool) {
    if text.chars().count() <= MAX_TERMINAL_OUTPUT_CHARS {
        return (text, false);
    }
    (text.chars().take(MAX_TERMINAL_OUTPUT_CHARS).collect(), true)
}

fn collect_file_entries(
    root: &Path,
    target: &Path,
    depth: usize,
    entries: &mut Vec<Value>,
) -> Result<(), String> {
    if entries.len() >= MAX_FILE_TREE_ENTRIES {
        return Ok(());
    }
    if target.is_file() {
        entries.push(file_entry_value(root, target)?);
        return Ok(());
    }
    if !target.is_dir() {
        return Err("file path does not exist".to_string());
    }
    let mut children = fs::read_dir(target)
        .map_err(|error| error.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| !is_hidden_runtime_dir(path))
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        let left_dir = left.is_dir();
        let right_dir = right.is_dir();
        right_dir
            .cmp(&left_dir)
            .then_with(|| left.file_name().cmp(&right.file_name()))
    });
    for child in children {
        if entries.len() >= MAX_FILE_TREE_ENTRIES {
            break;
        }
        entries.push(file_entry_value(root, &child)?);
        if depth > 0 && child.is_dir() {
            collect_file_entries(root, &child, depth - 1, entries)?;
        }
    }
    Ok(())
}

fn file_entry_value(root: &Path, path: &Path) -> Result<Value, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    Ok(json!({
        "path": workspace_relative_path(root, path),
        "name": path.file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "kind": if metadata.is_dir() { "dir" } else { "file" },
        "size_bytes": if metadata.is_file() { metadata.len() } else { 0 },
        "text": metadata.is_file() && file_is_text_like(path),
    }))
}

fn workspace_relative_path(root: &Path, path: &Path) -> String {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical_path
        .strip_prefix(&canonical_root)
        .unwrap_or(&canonical_path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_hidden_runtime_dir(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    matches!(
        name,
        ".git" | ".openagent" | "node_modules" | "target" | "dist" | ".DS_Store"
    )
}

fn file_is_text_like(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() > 256 * 1024 {
        return false;
    }
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    !bytes.iter().take(4096).any(|byte| *byte == 0)
}

fn parse_git_branch_line(raw: &str) -> (String, i64, i64) {
    let branch = raw
        .split(['.', '['])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut ahead = 0_i64;
    let mut behind = 0_i64;
    if let Some(status) = raw
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
    {
        for item in status.0.split(',') {
            let item = item.trim();
            if let Some(value) = item.strip_prefix("ahead ") {
                ahead = value.parse::<i64>().unwrap_or_default();
            } else if let Some(value) = item.strip_prefix("behind ") {
                behind = value.parse::<i64>().unwrap_or_default();
            }
        }
    }
    (branch, ahead, behind)
}

fn session_diff_payload(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let store = FileSessionStore::new(session_root(config));
    let session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let undo_stack = file_change_stack(&session, FILE_CHANGE_UNDO_STACK_KEY);
    let redo_stack = file_change_stack(&session, FILE_CHANGE_REDO_STACK_KEY);
    let patches = undo_stack
        .iter()
        .rev()
        .map(public_file_change)
        .collect::<Vec<_>>();
    let redo = redo_stack
        .iter()
        .rev()
        .map(public_file_change)
        .collect::<Vec<_>>();
    Ok(json!({
        "session_id": session.id,
        "undo_count": undo_stack.len(),
        "redo_count": redo_stack.len(),
        "latest": undo_stack.last().map(public_file_change).unwrap_or(Value::Null),
        "patches": patches,
        "redo": redo,
    }))
}

fn session_checkpoints_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
) -> Result<Value, String> {
    if !valid_session_id(session_id) {
        return Err("invalid session id".to_string());
    }
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let store = FileSessionStore::new(session_root(config));
    let _ = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let path = store
        .root
        .join(session_id)
        .join("checkpoints")
        .join("index.jsonl");
    let mut checkpoints = read_jsonl_values(&path);
    checkpoints.sort_by(|left, right| {
        right["timestamp_ms"]
            .as_u64()
            .cmp(&left["timestamp_ms"].as_u64())
    });
    Ok(json!({
        "session_id": session_id,
        "count": checkpoints.len(),
        "latest": checkpoints.first().cloned().unwrap_or(Value::Null),
        "checkpoints": checkpoints,
    }))
}

fn restore_session_checkpoint_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    checkpoint_id: &str,
) -> Result<Value, String> {
    if !valid_session_id(session_id) || !valid_checkpoint_id(checkpoint_id) {
        return Err("invalid checkpoint request".to_string());
    }
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let run_id = new_id("restore");
    let checkpoint = store
        .restore_checkpoint(session_id, &run_id, &session.directory, checkpoint_id)
        .map_err(|error| error.to_string())?;
    let restored_at_ms = now_ms();
    let restore_record = json!({
        "checkpoint_id": checkpoint_id,
        "run_id": run_id,
        "restored_at_ms": restored_at_ms,
        "checkpoint_kind": checkpoint.kind.clone(),
        "file_count": checkpoint.file_count,
        "total_bytes": checkpoint.total_bytes,
        "message_id": checkpoint.message_id.clone(),
        "part_id": checkpoint.part_id.clone(),
    });
    let mut restore_history = session
        .metadata
        .get("checkpoint_restore_history")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    restore_history.insert(0, restore_record.clone());
    restore_history.truncate(20);
    session
        .metadata
        .insert("latest_checkpoint_restore".to_string(), restore_record);
    session.metadata.insert(
        "checkpoint_restore_history".to_string(),
        Value::Array(restore_history),
    );
    store
        .save_state(&session, Some(&run_id))
        .map_err(|error| error.to_string())?;
    let event = json!({
        "method": "checkpoint/restored",
        "params": {
            "session_id": session.id.as_str(),
            "thread_id": session.id.as_str(),
            "turn_id": run_id,
            "checkpoint_id": checkpoint_id,
            "checkpoint": checkpoint,
            "status": "restored",
        }
    });
    let mut events = vec![event];
    append_app_events(&store.root, session_id, &run_id, &mut events);
    Ok(json!({
        "session_id": session_id,
        "run_id": run_id,
        "status": "restored",
        "checkpoint": checkpoint,
        "events": events,
    }))
}

fn undo_session_payload(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let mut undo_stack = file_change_stack(&session, FILE_CHANGE_UNDO_STACK_KEY);
    let Some(change) = undo_stack.pop() else {
        return Err("nothing to undo".to_string());
    };
    apply_file_change_state(&session, &change, FileChangeState::Before)?;
    let mut redo_stack = file_change_stack(&session, FILE_CHANGE_REDO_STACK_KEY);
    let reverted = mark_file_change(change.clone(), "undone");
    push_stack_entry(&mut redo_stack, reverted.clone());
    set_file_change_stack(&mut session, FILE_CHANGE_UNDO_STACK_KEY, undo_stack.clone());
    set_file_change_stack(&mut session, FILE_CHANGE_REDO_STACK_KEY, redo_stack.clone());
    session.metadata.insert(
        "latest_file_revert".to_string(),
        public_file_change(&reverted),
    );
    let turn_id = file_change_run_id(&change);
    let public = public_file_change(&reverted);
    let event = append_patch_stack_event(&store, &session, &turn_id, "patch/undone", &public);
    store
        .save_state(&session, Some(&turn_id))
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "session_id": session.id,
        "status": "undone",
        "undo_count": undo_stack.len(),
        "redo_count": redo_stack.len(),
        "patch": public,
        "events": [event],
    }))
}

fn redo_session_payload(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let mut redo_stack = file_change_stack(&session, FILE_CHANGE_REDO_STACK_KEY);
    let Some(change) = redo_stack.pop() else {
        return Err("nothing to redo".to_string());
    };
    apply_file_change_state(&session, &change, FileChangeState::After)?;
    let mut undo_stack = file_change_stack(&session, FILE_CHANGE_UNDO_STACK_KEY);
    let reapplied = mark_file_change(change.clone(), "applied");
    push_stack_entry(&mut undo_stack, reapplied.clone());
    set_file_change_stack(&mut session, FILE_CHANGE_UNDO_STACK_KEY, undo_stack.clone());
    set_file_change_stack(&mut session, FILE_CHANGE_REDO_STACK_KEY, redo_stack.clone());
    session.metadata.insert(
        FILE_CHANGE_LATEST_KEY.to_string(),
        public_file_change(&reapplied),
    );
    let turn_id = file_change_run_id(&change);
    let public = public_file_change(&reapplied);
    let event = append_patch_stack_event(&store, &session, &turn_id, "patch/redone", &public);
    store
        .save_state(&session, Some(&turn_id))
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "session_id": session.id,
        "status": "redone",
        "undo_count": undo_stack.len(),
        "redo_count": redo_stack.len(),
        "patch": public,
        "events": [event],
    }))
}

#[derive(Clone, Debug)]
struct FileChangeBefore {
    target: PathBuf,
    display_path: String,
    existed_before: bool,
    before_content: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum FileChangeState {
    Before,
    After,
}

fn capture_file_change_before(session: &Session, call: &ToolCall) -> Option<FileChangeBefore> {
    if !matches!(call.name.as_str(), "write" | "edit") {
        return None;
    }
    let raw_path = call.input.get("file_path").and_then(Value::as_str)?;
    let target = resolve_path_in_root(&session.directory, raw_path).ok()?;
    let existed_before = target.exists();
    let before_content = if target.is_file() {
        fs::read_to_string(&target).ok()
    } else {
        None
    };
    Some(FileChangeBefore {
        display_path: session_display_path(session, &target),
        target,
        existed_before,
        before_content,
    })
}

fn file_change_preview(before: &FileChangeBefore, call: &ToolCall) -> Option<Value> {
    let after = predicted_after_content(before, call)?;
    let existed_after = true;
    let diff = render_unified_diff(
        &before.display_path,
        before.before_content.as_deref(),
        Some(after.as_str()),
    );
    Some(json!({
        "kind": "file",
        "path": before.display_path,
        "status": file_change_status(before.existed_before, existed_after),
        "diff": diff,
        "summary": format!(
            "{} {}",
            call.name,
            if before.existed_before { "will modify file" } else { "will create file" }
        ),
    }))
}

fn predicted_after_content(before: &FileChangeBefore, call: &ToolCall) -> Option<String> {
    match call.name.as_str() {
        "write" => call
            .input
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string),
        "edit" => {
            let old = call.input.get("old_string").and_then(Value::as_str)?;
            let new = call.input.get("new_string").and_then(Value::as_str)?;
            let replace_all = call
                .input
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if old.is_empty() {
                return Some(new.to_string());
            }
            preview_replace_text(before.before_content.as_deref()?, old, new, replace_all).ok()
        }
        _ => None,
    }
}

fn complete_file_change(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    call: &ToolCall,
    before: Option<FileChangeBefore>,
    result: &ToolResult,
) -> Option<Value> {
    if result.error.is_some() {
        return None;
    }
    let before = before?;
    let existed_after = before.target.exists();
    let after_content = if before.target.is_file() {
        fs::read_to_string(&before.target).ok()
    } else {
        None
    };
    if before.existed_before == existed_after && before.before_content == after_content {
        return None;
    }
    let diff = render_unified_diff(
        &before.display_path,
        before.before_content.as_deref(),
        after_content.as_deref(),
    );
    let change = json!({
        "id": new_id("patch"),
        "session_id": session.id,
        "run_id": run_id,
        "call_id": call.call_id,
        "tool": call.name,
        "created_at_ms": now_ms(),
        "workspace": session.directory.to_string_lossy(),
        "path": before.display_path,
        "absolute_path": before.target.to_string_lossy(),
        "existed_before": before.existed_before,
        "existed_after": existed_after,
        "before": before.before_content,
        "after": after_content,
        "status": "applied",
        "diff": diff,
    });
    push_file_change(session, change.clone());
    let public = public_file_change(&change);
    let _ = store.record_event(
        &session.id,
        run_id,
        "patch.detected",
        SessionEventOptions {
            kind: "patch".to_string(),
            attributes: BTreeMap::from([("patch".to_string(), public)]),
            ..SessionEventOptions::default()
        },
    );
    Some(change)
}

fn patch_detected_event(session: &Session, run_id: &str, change: &Value) -> Value {
    json!({
        "method": "patch/detected",
        "params": {
            "session_id": session.id,
            "thread_id": session.id,
            "turn_id": run_id,
            "patch": public_file_change(change),
        }
    })
}

fn append_patch_stack_event(
    store: &FileSessionStore,
    session: &Session,
    turn_id: &str,
    method: &str,
    patch: &Value,
) -> Value {
    let event_name = match method {
        "patch/undone" => "patch.undone",
        "patch/redone" => "patch.redone",
        _ => "patch.changed",
    };
    let event = json!({
        "method": method,
        "params": {
            "session_id": session.id,
            "thread_id": session.id,
            "turn_id": turn_id,
            "patch": patch,
        }
    });
    let mut events = vec![event];
    append_app_events(&store.root, &session.id, turn_id, &mut events);
    let event = events.into_iter().next().unwrap_or_else(|| json!({}));
    let _ = store.record_event(
        &session.id,
        turn_id,
        event_name,
        SessionEventOptions {
            kind: "patch".to_string(),
            attributes: BTreeMap::from([("patch".to_string(), patch.clone())]),
            ..SessionEventOptions::default()
        },
    );
    event
}

fn push_file_change(session: &mut Session, change: Value) {
    let public = public_file_change(&change);
    let mut undo_stack = file_change_stack(session, FILE_CHANGE_UNDO_STACK_KEY);
    push_stack_entry(&mut undo_stack, change);
    set_file_change_stack(session, FILE_CHANGE_UNDO_STACK_KEY, undo_stack);
    set_file_change_stack(session, FILE_CHANGE_REDO_STACK_KEY, Vec::new());
    session
        .metadata
        .insert(FILE_CHANGE_LATEST_KEY.to_string(), public);
}

fn file_change_stack(session: &Session, key: &str) -> Vec<Value> {
    session
        .metadata
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn set_file_change_stack(session: &mut Session, key: &str, stack: Vec<Value>) {
    session
        .metadata
        .insert(key.to_string(), Value::Array(stack));
}

fn push_stack_entry(stack: &mut Vec<Value>, value: Value) {
    stack.push(value);
    let excess = stack.len().saturating_sub(MAX_FILE_CHANGE_STACK);
    if excess > 0 {
        stack.drain(0..excess);
    }
}

fn apply_file_change_state(
    session: &Session,
    change: &Value,
    state: FileChangeState,
) -> Result<(), String> {
    let path = change
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| change.get("absolute_path").and_then(Value::as_str))
        .ok_or_else(|| "patch is missing path".to_string())?;
    let target = resolve_path_in_root(&session.directory, path)?;
    let (exists_key, content_key) = match state {
        FileChangeState::Before => ("existed_before", "before"),
        FileChangeState::After => ("existed_after", "after"),
    };
    let should_exist = change
        .get(exists_key)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if should_exist {
        let content = change
            .get(content_key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("patch is missing {content_key} content"))?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&target, content).map_err(|error| error.to_string())
    } else if target.exists() {
        if target.is_dir() {
            return Err(format!(
                "refusing to remove directory: {}",
                target.display()
            ));
        }
        fs::remove_file(&target).map_err(|error| error.to_string())
    } else {
        Ok(())
    }
}

fn mark_file_change(mut change: Value, status: &str) -> Value {
    if let Some(object) = change.as_object_mut() {
        object.insert("status".to_string(), json!(status));
        object.insert(format!("{status}_at_ms"), json!(now_ms()));
    }
    change
}

fn public_file_change(change: &Value) -> Value {
    let mut public = change.clone();
    let side_by_side = change.get("path").and_then(Value::as_str).map(|path| {
        render_side_by_side_diff(
            path,
            change.get("before").and_then(Value::as_str),
            change.get("after").and_then(Value::as_str),
        )
    });
    if let Some(object) = public.as_object_mut() {
        object.remove("before");
        object.remove("after");
        object.remove("absolute_path");
        if let Some(side_by_side) = side_by_side {
            object.insert("side_by_side".to_string(), side_by_side);
        }
    }
    public
}

fn file_change_run_id(change: &Value) -> String {
    change
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| new_id("turn"))
}

fn session_display_path(session: &Session, target: &Path) -> String {
    let root = session
        .directory
        .canonicalize()
        .unwrap_or_else(|_| session.directory.clone());
    target
        .strip_prefix(&root)
        .unwrap_or(target)
        .to_string_lossy()
        .replace('\\', "/")
}

fn file_change_status(existed_before: bool, existed_after: bool) -> &'static str {
    match (existed_before, existed_after) {
        (false, true) => "created",
        (true, false) => "deleted",
        (true, true) => "modified",
        (false, false) => "unchanged",
    }
}

fn preview_replace_text(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, String> {
    if old == new {
        return Err("old_string and new_string must be different".to_string());
    }
    if old.is_empty() {
        return Ok(new.to_string());
    }
    let count = content.matches(old).count();
    if count == 0 {
        return Err("old_string not found in content".to_string());
    }
    if count > 1 && !replace_all {
        return Err("old_string found multiple times".to_string());
    }
    if replace_all {
        Ok(content.replace(old, new))
    } else {
        Ok(content.replacen(old, new, 1))
    }
}

fn render_unified_diff(path: &str, before: Option<&str>, after: Option<&str>) -> String {
    let before_lines = before
        .map(|value| value.lines().collect::<Vec<_>>())
        .unwrap_or_default();
    let after_lines = after
        .map(|value| value.lines().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut lines = vec![
        format!("--- a/{path}"),
        format!("+++ b/{path}"),
        "@@".to_string(),
    ];
    let diff_lines = if before_lines.len().saturating_mul(after_lines.len()) <= 200_000 {
        lcs_diff_lines(&before_lines, &after_lines)
    } else {
        full_file_diff_lines(&before_lines, &after_lines)
    };
    lines.extend(diff_lines);
    truncate_diff_lines(lines).join("\n")
}

fn render_side_by_side_diff(path: &str, before: Option<&str>, after: Option<&str>) -> Value {
    let before_lines = before
        .map(|value| value.lines().collect::<Vec<_>>())
        .unwrap_or_default();
    let after_lines = after
        .map(|value| value.lines().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut rows = if before_lines.len().saturating_mul(after_lines.len()) <= 200_000 {
        lcs_side_by_side_rows(&before_lines, &after_lines)
    } else {
        full_file_side_by_side_rows(&before_lines, &after_lines)
    };
    let row_count = rows.len();
    let omitted_rows = row_count.saturating_sub(MAX_RENDERED_DIFF_LINES);
    if omitted_rows > 0 {
        rows.truncate(MAX_RENDERED_DIFF_LINES);
    }
    json!({
        "path": path,
        "old_label": format!("a/{path}"),
        "new_label": format!("b/{path}"),
        "row_count": row_count,
        "truncated": omitted_rows > 0,
        "omitted_rows": omitted_rows,
        "rows": rows,
    })
}

fn lcs_diff_lines(before: &[&str], after: &[&str]) -> Vec<String> {
    let rows = before.len() + 1;
    let cols = after.len() + 1;
    let mut table = vec![0_usize; rows * cols];
    for row in 1..rows {
        for col in 1..cols {
            table[row * cols + col] = if before[row - 1] == after[col - 1] {
                table[(row - 1) * cols + col - 1] + 1
            } else {
                table[(row - 1) * cols + col].max(table[row * cols + col - 1])
            };
        }
    }
    let mut row = before.len();
    let mut col = after.len();
    let mut output = Vec::new();
    while row > 0 || col > 0 {
        if row > 0 && col > 0 && before[row - 1] == after[col - 1] {
            output.push(format!(" {}", before[row - 1]));
            row -= 1;
            col -= 1;
        } else if col > 0
            && (row == 0 || table[row * cols + col - 1] >= table[(row - 1) * cols + col])
        {
            output.push(format!("+{}", after[col - 1]));
            col -= 1;
        } else if row > 0 {
            output.push(format!("-{}", before[row - 1]));
            row -= 1;
        }
    }
    output.reverse();
    output
}

fn lcs_side_by_side_rows(before: &[&str], after: &[&str]) -> Vec<Value> {
    let rows = before.len() + 1;
    let cols = after.len() + 1;
    let mut table = vec![0_usize; rows * cols];
    for row in 1..rows {
        for col in 1..cols {
            table[row * cols + col] = if before[row - 1] == after[col - 1] {
                table[(row - 1) * cols + col - 1] + 1
            } else {
                table[(row - 1) * cols + col].max(table[row * cols + col - 1])
            };
        }
    }
    let mut row = before.len();
    let mut col = after.len();
    let mut output = Vec::new();
    while row > 0 || col > 0 {
        if row > 0 && col > 0 && before[row - 1] == after[col - 1] {
            output.push(side_by_side_row(
                "context",
                Some(row),
                Some(col),
                Some(before[row - 1]),
                Some(after[col - 1]),
            ));
            row -= 1;
            col -= 1;
        } else if col > 0
            && (row == 0 || table[row * cols + col - 1] >= table[(row - 1) * cols + col])
        {
            output.push(side_by_side_row(
                "added",
                None,
                Some(col),
                None,
                Some(after[col - 1]),
            ));
            col -= 1;
        } else if row > 0 {
            output.push(side_by_side_row(
                "removed",
                Some(row),
                None,
                Some(before[row - 1]),
                None,
            ));
            row -= 1;
        }
    }
    output.reverse();
    output
}

fn full_file_diff_lines(before: &[&str], after: &[&str]) -> Vec<String> {
    before
        .iter()
        .map(|line| format!("-{line}"))
        .chain(after.iter().map(|line| format!("+{line}")))
        .collect()
}

fn full_file_side_by_side_rows(before: &[&str], after: &[&str]) -> Vec<Value> {
    before
        .iter()
        .enumerate()
        .map(|(index, line)| side_by_side_row("removed", Some(index + 1), None, Some(line), None))
        .chain(after.iter().enumerate().map(|(index, line)| {
            side_by_side_row("added", None, Some(index + 1), None, Some(line))
        }))
        .collect()
}

fn side_by_side_row(
    kind: &str,
    old_line: Option<usize>,
    new_line: Option<usize>,
    old: Option<&str>,
    new: Option<&str>,
) -> Value {
    json!({
        "kind": kind,
        "old_line": old_line,
        "new_line": new_line,
        "old": old,
        "new": new,
    })
}

fn truncate_diff_lines(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() <= MAX_RENDERED_DIFF_LINES {
        return lines;
    }
    let omitted = lines.len() - MAX_RENDERED_DIFF_LINES;
    lines.truncate(MAX_RENDERED_DIFF_LINES);
    lines.push(format!("... diff truncated ({omitted} more lines) ..."));
    lines
}

fn query_param(path: &str, target: &str) -> Option<String> {
    path.split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| (key == target).then(|| percent_decode(value)))
}

fn query_flag(path: &str, target: &str) -> bool {
    query_param(path, target)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn percent_decode(value: &str) -> String {
    let mut bytes = Vec::new();
    let raw = value.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' && index + 2 < raw.len() {
            if let (Some(high), Some(low)) = (hex_value(raw[index + 1]), hex_value(raw[index + 2]))
            {
                bytes.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        bytes.push(if raw[index] == b'+' { b' ' } else { raw[index] });
        index += 1;
    }
    String::from_utf8_lossy(&bytes).to_string()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn session_summary_from_session(session: &Session) -> Value {
    let metadata = serde_json::to_value(&session.metadata).unwrap_or_else(|_| json!({}));
    let title = metadata
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let archived = metadata
        .get("archived")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let shared = metadata
        .get("shared")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let share_url = metadata.get("share_url").cloned().unwrap_or(Value::Null);
    let forked_from = metadata.get("forked_from").cloned().unwrap_or(Value::Null);
    let parent_session_id = metadata
        .get("parent_session_id")
        .cloned()
        .unwrap_or(Value::Null);
    let compact = metadata.get("compact").cloned().unwrap_or(Value::Null);
    json!({
        "id": session.id.as_str(),
        "session_id": session.id.as_str(),
        "workspace": session.directory.to_string_lossy(),
        "status": session_status_text(&session.status),
        "updated_at_ms": now_ms(),
        "message_count": session.messages.len(),
        "metadata": metadata,
        "title": title,
        "archived": archived,
        "shared": shared,
        "share_url": share_url,
        "forked_from": forked_from,
        "parent_session_id": parent_session_id,
        "compact": compact,
    })
}

fn session_summary_from_state(state: &Value, fallback_id: &str) -> Value {
    let metadata = state
        .get("metadata")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let title = metadata
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let archived = metadata
        .get("archived")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let shared = metadata
        .get("shared")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let share_url = metadata.get("share_url").cloned().unwrap_or(Value::Null);
    let forked_from = metadata.get("forked_from").cloned().unwrap_or(Value::Null);
    let parent_session_id = metadata
        .get("parent_session_id")
        .cloned()
        .unwrap_or(Value::Null);
    let compact = metadata.get("compact").cloned().unwrap_or(Value::Null);
    json!({
        "id": state.get("session_id").cloned().unwrap_or_else(|| json!(fallback_id)),
        "session_id": state.get("session_id").cloned().unwrap_or_else(|| json!(fallback_id)),
        "workspace": state.get("workspace").cloned().unwrap_or_else(|| json!(".")),
        "status": state.get("status").cloned().unwrap_or_else(|| json!("idle")),
        "updated_at_ms": state.get("updated_at_ms").cloned().unwrap_or_else(|| json!(0)),
        "message_count": state.get("messages").and_then(Value::as_array).map_or(0, Vec::len),
        "metadata": metadata,
        "title": title,
        "archived": archived,
        "shared": shared,
        "share_url": share_url,
        "forked_from": forked_from,
        "parent_session_id": parent_session_id,
        "compact": compact,
    })
}

fn session_task_summary_from_state(root: &Path, state: &Value, fallback_id: &str) -> Value {
    let metadata = state
        .get("metadata")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let session_id = state
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or(fallback_id)
        .to_string();
    let run_id = state
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let run_dir = root.join(&session_id).join("runs").join(&run_id);
    let run_summary = if run_id.is_empty() {
        Value::Null
    } else {
        read_json_file(&run_dir.join("summary.json"))
    };
    let run_record = if run_id.is_empty() {
        Value::Null
    } else {
        read_json_file(&run_dir.join("run.json"))
    };
    let status = metadata
        .get("task_status")
        .and_then(Value::as_str)
        .or_else(|| {
            metadata
                .get("status")
                .and_then(Value::as_str)
                .filter(|value| *value == "queued")
        })
        .or_else(|| run_summary.get("status").and_then(Value::as_str))
        .or_else(|| run_record.get("status").and_then(Value::as_str))
        .or_else(|| state.get("status").and_then(Value::as_str))
        .unwrap_or("unknown")
        .to_string();
    let run_status = run_summary
        .get("status")
        .or_else(|| run_record.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let subagent_type = metadata
        .get("task_subagent_type")
        .or_else(|| metadata.get("agent"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let title = metadata
        .get("task_description")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            metadata
                .get("agent_profile")
                .and_then(|profile| profile.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(subagent_type.as_str())
        })
        .to_string();
    json!({
        "id": session_id,
        "task_id": session_id,
        "session_id": session_id,
        "run_id": run_id,
        "status": status,
        "session_status": state.get("status").cloned().unwrap_or_else(|| json!("idle")),
        "title": title,
        "description": metadata.get("task_description").cloned().unwrap_or(Value::Null),
        "subagent_type": subagent_type,
        "agent": metadata.get("agent").cloned().unwrap_or(Value::Null),
        "agent_profile": metadata.get("agent_profile").cloned().unwrap_or(Value::Null),
        "background": metadata.get("background").cloned().unwrap_or(Value::Bool(false)),
        "provider": metadata.get("provider").cloned().unwrap_or(Value::Null),
        "model": metadata.get("model").cloned().unwrap_or(Value::Null),
        "permission": metadata.get("permission").cloned().unwrap_or(Value::Null),
        "max_steps": metadata.get("max_steps").cloned().unwrap_or(Value::Null),
        "workspace": state.get("workspace").cloned().unwrap_or(Value::Null),
        "workspace_isolation": metadata.get("workspace_isolation").cloned().unwrap_or(Value::Null),
        "task_depth": metadata.get("task_depth").cloned().unwrap_or(Value::Null),
        "task_root_session_id": metadata.get("task_root_session_id").cloned().unwrap_or(Value::Null),
        "task_parent_session_id": metadata.get("task_parent_session_id").cloned().unwrap_or(Value::Null),
        "task_lineage_subagents": metadata.get("task_lineage_subagents").cloned().unwrap_or_else(|| json!([])),
        "parent_session_id": metadata.get("parent_session_id").cloned().unwrap_or(Value::Null),
        "parent_run_id": metadata.get("parent_run_id").cloned().unwrap_or(Value::Null),
        "parent_tool_call_id": metadata.get("parent_tool_call_id").cloned().unwrap_or(Value::Null),
        "updated_at_ms": state.get("updated_at_ms").cloned().unwrap_or_else(|| json!(0)),
        "message_count": state.get("messages").and_then(Value::as_array).map_or(0, Vec::len),
        "finish_reason": run_record.get("finish_reason").cloned().unwrap_or(Value::Null),
        "error": run_record.get("error").cloned().unwrap_or(Value::Null),
        "run_status": if run_status.is_empty() { Value::Null } else { json!(run_status) },
        "run": run_summary,
        "metadata": metadata,
    })
}

fn session_matches_query(summary: &Value, query: &str) -> bool {
    let query = query.to_ascii_lowercase();
    [
        "session_id",
        "id",
        "title",
        "workspace",
        "status",
        "forked_from",
        "parent_session_id",
    ]
    .iter()
    .any(|key| {
        summary
            .get(*key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(&query)
    })
}

fn summarize_session_messages(session: &Session) -> String {
    let mut pieces = Vec::new();
    for message in session.messages.iter().take(12) {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Tool => "tool",
        };
        let text = message
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            continue;
        }
        let truncated = if text.chars().count() > 160 {
            format!("{}...", text.chars().take(160).collect::<String>())
        } else {
            text
        };
        pieces.push(format!("{role}: {truncated}"));
    }
    if pieces.is_empty() {
        "No messages yet.".to_string()
    } else {
        pieces.join("\n")
    }
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && !session_id.contains("..")
}

fn valid_checkpoint_id(checkpoint_id: &str) -> bool {
    !checkpoint_id.is_empty()
        && checkpoint_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && !checkpoint_id.contains("..")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeProfile {
    agent: String,
    model: String,
    variant: String,
    thinking: String,
}

fn apply_turn_runtime_profile(session: &mut Session, payload: &Value) -> RuntimeProfile {
    set_session_text_metadata(session, payload, "agent");
    set_session_text_metadata(session, payload, "model");
    set_session_text_metadata(session, payload, "variant");
    set_session_text_metadata(session, payload, "thinking");
    let profile = RuntimeProfile {
        agent: session_text_metadata(session, "agent", "server"),
        model: session_text_metadata(session, "model", &default_model_id()),
        variant: session_text_metadata(session, "variant", "default"),
        thinking: session_text_metadata(session, "thinking", "medium"),
    };
    session
        .metadata
        .insert("agent".to_string(), json!(profile.agent.clone()));
    session
        .metadata
        .insert("model".to_string(), json!(profile.model.clone()));
    session
        .metadata
        .insert("variant".to_string(), json!(profile.variant.clone()));
    session
        .metadata
        .insert("thinking".to_string(), json!(profile.thinking.clone()));
    profile
}

fn set_session_text_metadata(session: &mut Session, payload: &Value, key: &str) {
    let Some(value) = payload.get(key).and_then(Value::as_str) else {
        return;
    };
    let value = value.trim();
    if value.is_empty() {
        session.metadata.remove(key);
    } else {
        session.metadata.insert(key.to_string(), json!(value));
    }
}

fn session_text_metadata(session: &Session, key: &str, default: &str) -> String {
    session
        .metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| default.to_string())
}

fn turn_started_event(session: &Session, run_id: &str) -> Value {
    let profile = RuntimeProfile {
        agent: session_text_metadata(session, "agent", "server"),
        model: session_text_metadata(session, "model", &default_model_id()),
        variant: session_text_metadata(session, "variant", "default"),
        thinking: session_text_metadata(session, "thinking", "medium"),
    };
    json!({
        "method": "turn/started",
        "params": {
            "session_id": session.id,
            "thread_id": session.id,
            "turn_id": run_id,
            "status": "running",
            "agent": profile.agent,
            "agent_name": profile.agent,
            "model": profile.model,
            "model_id": profile.model,
            "provider_id": "openagent",
            "variant": profile.variant,
            "thinking": profile.thinking,
        },
    })
}

fn latest_user_message(session: &Session) -> String {
    session
        .messages
        .iter()
        .rev()
        .find(|message| matches!(message.role, Role::User))
        .map(|message| message.content.clone())
        .unwrap_or_default()
}

fn usage_payload(input: &str, output: &str, tool_calls: u64) -> Value {
    let input_tokens = estimate_tokens(input);
    let output_tokens = estimate_tokens(output);
    let tool_tokens = tool_calls.saturating_mul(16);
    let total_tokens = input_tokens + output_tokens + tool_tokens;
    json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "tool_tokens": tool_tokens,
        "total_tokens": total_tokens,
        "tool_calls": tool_calls,
        "cost": 0.0,
        "estimated": true,
    })
}

fn estimate_tokens(value: &str) -> u64 {
    let by_words = value.split_whitespace().count() as u64;
    let by_chars = (value.chars().count() as u64).div_ceil(4);
    by_words.max(by_chars).max(u64::from(!value.is_empty()))
}

fn trace_payload(session: &Session, run_id: &str, tool_calls: u64) -> Value {
    json!({
        "run_id": run_id,
        "session_id": session.id,
        "agent": session_text_metadata(session, "agent", "server"),
        "model": session_text_metadata(session, "model", &default_model_id()),
        "variant": session_text_metadata(session, "variant", "default"),
        "thinking": session_text_metadata(session, "thinking", "medium"),
        "tool_calls": tool_calls,
    })
}

fn record_usage_event(store: &FileSessionStore, session: &Session, run_id: &str, usage: &Value) {
    let _ = store.record_event(
        &session.id,
        run_id,
        "model.usage",
        SessionEventOptions {
            kind: "usage".to_string(),
            attributes: usage
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect(),
            ..SessionEventOptions::default()
        },
    );
}

fn tool_calls_completed_successfully(events: &[Value]) -> bool {
    events
        .iter()
        .any(|event| event.get("method").and_then(Value::as_str) == Some("item/toolCall/completed"))
        && !events.iter().any(|event| {
            event.get("method").and_then(Value::as_str) == Some("item/toolCall/failed")
        })
}

#[derive(Clone, Debug)]
struct RuntimeProviderResult {
    answer: String,
    tool_calls: Vec<ToolCall>,
    usage: Usage,
    source: String,
    finish_reason: String,
}

struct OpenAiRuntimeProviderRequest<'a> {
    provider: &'a str,
    model: &'a str,
    api_key: &'a str,
    base_url: &'a str,
    wire_api: &'a str,
    timeout_s: u64,
    stream: bool,
    messages: &'a [ChatMessage],
    tools: &'a [openagent_protocol::ToolSchema],
    model_options: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
struct RuntimeProviderLoopCarry {
    answer: String,
    usage: Usage,
    tool_calls: u64,
    next_step: u64,
}

impl Default for RuntimeProviderLoopCarry {
    fn default() -> Self {
        Self {
            answer: String::new(),
            usage: Usage::default(),
            tool_calls: 0,
            next_step: 1,
        }
    }
}

#[derive(Clone, Debug)]
struct RuntimeProviderResume {
    payload: Value,
    carry: RuntimeProviderLoopCarry,
    permission_ruleset: PermissionRuleset,
    skip_permissions: bool,
}

struct RuntimeProviderLoopInput<'a> {
    config: &'a HttpRuntimeConfig,
    store: &'a FileSessionStore,
    session: &'a mut Session,
    run_id: &'a str,
    payload: &'a Value,
    permission_ruleset: PermissionRuleset,
    skip_permissions: bool,
    events: Vec<Value>,
    carry: RuntimeProviderLoopCarry,
}

fn provider_turn_result(
    store: &FileSessionStore,
    session: &Session,
    payload: &Value,
    tools: &[ToolSchema],
    stream_sink: Option<&mut dyn FnMut(&ProviderStreamEvent)>,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<RuntimeProviderResult, String> {
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .or_else(|| session.metadata.get("provider").and_then(Value::as_str));
    let provider_config = runtime_provider_config(provider, Some(payload), Some(session))?;
    if provider_config.requires_api_key && provider_config.api_key.is_none() {
        return Ok(RuntimeProviderResult {
            answer: format!(
                "Provider `{}` is not configured. Set {} or OPENAGENT_API_KEY, then retry this turn.",
                provider_config.provider, provider_config.api_key_env
            ),
            tool_calls: Vec::new(),
            usage: Usage::default(),
            source: "provider_missing_api_key".to_string(),
            finish_reason: "configuration_required".to_string(),
        });
    }
    let api_key = provider_config.api_key.clone().unwrap_or_default();
    let timeout = payload
        .get("timeout_s")
        .and_then(Value::as_u64)
        .unwrap_or(60);
    let stream = provider_streaming_enabled_for_turn(payload);
    let provider_messages = store
        .materialized_chat_messages(session)
        .unwrap_or_else(|_| session.messages.clone());
    let model_options = runtime_provider_model_options(session, payload);
    call_openai_compatible_provider_for_runtime(
        OpenAiRuntimeProviderRequest {
            provider: &provider_config.provider,
            model: &provider_config.model,
            api_key: &api_key,
            base_url: &provider_config.base_url,
            wire_api: &provider_config.wire_api,
            timeout_s: timeout,
            stream,
            messages: &provider_messages,
            tools,
            model_options,
        },
        stream_sink,
        should_cancel,
    )
}

fn runtime_provider_model_options(session: &Session, payload: &Value) -> BTreeMap<String, Value> {
    let mut options = BTreeMap::new();
    merge_model_options_from_value(session.metadata.get("model_options"), &mut options);
    merge_temperature_top_p_from_value(
        &serde_json::to_value(&session.metadata).unwrap_or_default(),
        &mut options,
    );
    merge_explicit_model_options_from_value(payload, &mut options);
    merge_temperature_top_p_from_value(payload, &mut options);
    options
}

fn merge_model_options_from_value(value: Option<&Value>, options: &mut BTreeMap<String, Value>) {
    let Some(value) = value else {
        return;
    };
    if let Some(object) = value.as_object() {
        for (key, item) in object {
            if key == "model_options" || key == "options" {
                if let Some(nested) = item.as_object() {
                    for (nested_key, nested_value) in nested {
                        if runtime_provider_option_allowed(nested_key) {
                            options.insert(nested_key.clone(), nested_value.clone());
                        }
                    }
                }
            } else if runtime_provider_option_allowed(key) {
                options.insert(key.clone(), item.clone());
            }
        }
    }
}

fn merge_explicit_model_options_from_value(value: &Value, options: &mut BTreeMap<String, Value>) {
    for key in ["model_options", "options"] {
        if let Some(object) = value.get(key).and_then(Value::as_object) {
            for (option_key, option_value) in object {
                if runtime_provider_option_allowed(option_key) {
                    options.insert(option_key.clone(), option_value.clone());
                }
            }
        }
    }
}

fn merge_temperature_top_p_from_value(value: &Value, options: &mut BTreeMap<String, Value>) {
    if let Some(temperature) = value.get("temperature").and_then(Value::as_f64) {
        options.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = value
        .get("top_p")
        .or_else(|| value.get("topP"))
        .and_then(Value::as_f64)
    {
        options.insert("top_p".to_string(), json!(top_p));
    }
}

fn apply_runtime_model_options_to_payload(
    payload: &mut Value,
    model_options: &BTreeMap<String, Value>,
) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    for (key, value) in model_options {
        if runtime_provider_option_allowed(key) {
            object.insert(key.clone(), value.clone());
        }
    }
}

fn runtime_provider_option_allowed(key: &str) -> bool {
    !matches!(
        key,
        "model"
            | "messages"
            | "input"
            | "tools"
            | "tool_choice"
            | "stream"
            | "skill"
            | "skills"
            | "skill_roots"
            | "skill_permissions"
            | "skill_permission"
    )
}

fn runtime_system_prompt_from_messages(messages: &[ChatMessage]) -> Option<String> {
    let prompt = messages
        .iter()
        .filter(|message| message.role == Role::System)
        .map(|message| message.content.trim())
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!prompt.is_empty()).then_some(prompt)
}

fn call_openai_compatible_provider_for_runtime(
    request: OpenAiRuntimeProviderRequest<'_>,
    mut stream_sink: Option<&mut dyn FnMut(&ProviderStreamEvent)>,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<RuntimeProviderResult, String> {
    let OpenAiRuntimeProviderRequest {
        provider,
        model,
        api_key,
        base_url,
        wire_api,
        timeout_s,
        stream,
        messages,
        tools,
        model_options,
    } = request;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(timeout_s.max(1)))
        .build()
        .map_err(|error| error.to_string())?;
    let mut config = OpenAiLanguageModelConfig::new(api_key, model);
    config.provider_id = provider.to_string();
    config.base_url = base_url.to_string();
    config.wire_api = wire_api.to_string();
    let system_prompt = runtime_system_prompt_from_messages(messages);
    let (endpoint, mut payload) = if wire_api == "chat" {
        let mut payload = build_openai_chat_payload(
            &config,
            system_prompt.as_deref(),
            messages,
            tools,
            None,
            None,
            None,
        );
        if let Some(object) = payload.as_object_mut() {
            object.insert("stream".to_string(), json!(stream));
        }
        (join_url(base_url, "chat/completions"), payload)
    } else {
        let mut payload = build_openai_responses_payload(
            &config,
            system_prompt.as_deref(),
            messages,
            tools,
            None,
            None,
        );
        if let Some(object) = payload.as_object_mut() {
            object.insert("stream".to_string(), json!(stream));
        }
        (join_url(base_url, "responses"), payload)
    };
    apply_runtime_model_options_to_payload(&mut payload, &model_options);
    let response = send_runtime_provider_request(
        &client,
        &endpoint,
        api_key,
        &payload,
        stream,
        runtime_provider_request_retries(),
    )?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if stream && content_type.contains("text/event-stream") {
        if !status.is_success() {
            let raw = response
                .text()
                .map_err(|error| format!("provider response read failed: {error}"))?;
            return Err(format!(
                "provider returned HTTP {}: {}",
                status.as_u16(),
                summarize_http_error_body(&raw, &content_type)
            ));
        }
        let mut chunks = Vec::new();
        read_sse_json_values_stream(response, |chunk| {
            if should_cancel.is_some_and(|cancelled| cancelled()) {
                return Err(TURN_INTERRUPTED_ERROR.to_string());
            }
            if let Some(event) = openai_stream_text_delta(wire_api, &chunk)
                && let Some(sink) = stream_sink.as_deref_mut()
            {
                sink(&event);
            }
            chunks.push(chunk);
            Ok(())
        })?;
        let events = if wire_api == "chat" {
            normalize_openai_chat_sse_chunks(&chunks)
        } else {
            normalize_openai_responses_stream_events(&chunks)
        };
        return Ok(provider_events_to_runtime_result(
            &events,
            format!("{provider}:{wire_api}:stream"),
            None,
        ));
    }
    let raw = response
        .text()
        .map_err(|error| format!("provider response read failed: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "provider returned HTTP {}: {}",
            status.as_u16(),
            summarize_http_error_body(&raw, &content_type)
        ));
    }
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("provider response was not JSON: {error}"))?;
    if wire_api == "chat" {
        Ok(openai_chat_response_to_runtime_result(
            &value,
            format!("{provider}:chat"),
        ))
    } else {
        let events = normalize_openai_responses_response(&value);
        Ok(provider_events_to_runtime_result(
            &events,
            format!("{provider}:responses"),
            Some(&value),
        ))
    }
}

fn send_runtime_provider_request(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    api_key: &str,
    payload: &Value,
    stream: bool,
    max_retries: u64,
) -> Result<reqwest::blocking::Response, String> {
    let mut attempt = 0_u64;
    loop {
        let mut request = client
            .post(endpoint)
            .bearer_auth(api_key)
            .header("content-type", "application/json");
        if stream {
            request = request.header("accept", "text/event-stream");
        }
        match request.json(payload).send() {
            Ok(response) => {
                let status = response.status().as_u16();
                if runtime_provider_status_retryable(status) && attempt < max_retries {
                    attempt += 1;
                    thread::sleep(runtime_provider_retry_delay(attempt));
                    continue;
                }
                return Ok(response);
            }
            Err(_error) if attempt < max_retries => {
                attempt += 1;
                thread::sleep(runtime_provider_retry_delay(attempt));
            }
            Err(error) => return Err(format!("provider request failed: {error}")),
        }
    }
}

fn runtime_provider_status_retryable(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn runtime_provider_retry_delay(attempt: u64) -> Duration {
    Duration::from_millis(750 * attempt.min(4))
}

fn runtime_provider_request_retries() -> u64 {
    std::env::var("OPENAGENT_PROVIDER_RETRIES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(2)
}

fn read_sse_json_values_stream<R, F>(mut reader: R, mut on_value: F) -> Result<(), String>
where
    R: Read,
    F: FnMut(Value) -> Result<(), String>,
{
    let mut raw = String::new();
    let mut buffer = [0_u8; 4096];
    let mut saw_done = false;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(read) => read,
            Err(_error) if saw_done => break,
            Err(error) => return Err(format!("provider SSE read failed: {error}")),
        };
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
            if sse_frame_is_done(&frame) {
                saw_done = true;
            }
            if let Some(value) = parse_sse_frame_json(&frame)? {
                let terminal = provider_sse_json_value_is_terminal(&value);
                on_value(value)?;
                if terminal {
                    saw_done = true;
                    break;
                }
            }
        }
        if saw_done {
            break;
        }
    }
    if !saw_done
        && !raw.trim().is_empty()
        && let Some(value) = parse_sse_frame_json(&raw)?
    {
        on_value(value)?;
    }
    Ok(())
}

fn provider_sse_json_value_is_terminal(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some(
            "response.completed" | "response.failed" | "response.cancelled" | "response.incomplete"
        )
    )
}

fn sse_frame_is_done(frame: &str) -> bool {
    frame.lines().any(|line| {
        let line = line.trim_end_matches('\r');
        line.strip_prefix("data:")
            .map(str::trim)
            .is_some_and(|data| data == "[DONE]")
    })
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
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(|error| format!("provider SSE data was not JSON: {error}"))
}

fn openai_stream_text_delta(wire_api: &str, chunk: &Value) -> Option<ProviderStreamEvent> {
    let text = if wire_api == "chat" {
        chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .or_else(|| {
                chunk
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|items| items.first())
                    .and_then(|choice| choice.get("text"))
            })
            .and_then(Value::as_str)
            .unwrap_or_default()
    } else if matches!(
        chunk.get("type").and_then(Value::as_str),
        Some("response.output_text.delta" | "response.refusal.delta")
    ) {
        chunk
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default()
    } else {
        ""
    };
    (!text.is_empty()).then(|| ProviderStreamEvent::TextDelta {
        text: text.to_string(),
    })
}

fn openai_chat_response_to_runtime_result(value: &Value, source: String) -> RuntimeProviderResult {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
    let answer = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let function = item.get("function")?;
            Some(ToolCall {
                call_id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("chat_tool_call_{index}"), str::to_string),
                name: function
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: parse_tool_arguments(
                    function
                        .get("arguments")
                        .unwrap_or(&Value::String(String::new())),
                ),
            })
        })
        .collect::<Vec<_>>();
    let usage = usage_from_provider_json(value.get("usage"));
    RuntimeProviderResult {
        answer: if answer.is_empty() && tool_calls.is_empty() {
            stable_json_dumps(value)
        } else {
            answer
        },
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or(if tool_calls.is_empty() {
                "stop"
            } else {
                "tool_call"
            })
            .to_string(),
        tool_calls,
        usage,
        source,
    }
}

fn provider_events_to_runtime_result(
    events: &[ProviderStreamEvent],
    source: String,
    fallback: Option<&Value>,
) -> RuntimeProviderResult {
    let mut answer = String::new();
    let mut tool_calls = Vec::new();
    let mut usage = Usage::default();
    let mut finish_reason = "stop".to_string();
    for event in events {
        match event {
            ProviderStreamEvent::TextDelta { text } => answer.push_str(text),
            ProviderStreamEvent::ToolCall {
                call_id,
                name,
                input,
            } => tool_calls.push(ToolCall {
                call_id: call_id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            ProviderStreamEvent::Finish {
                usage: item,
                finish_reason: reason,
            } => {
                usage = item.clone();
                finish_reason = reason.clone();
            }
        }
    }
    if answer.is_empty()
        && tool_calls.is_empty()
        && let Some(value) = fallback
    {
        answer = stable_json_dumps(value);
    }
    RuntimeProviderResult {
        answer,
        tool_calls,
        usage,
        source,
        finish_reason,
    }
}

fn provider_max_steps(payload: &Value) -> u64 {
    payload
        .get("max_steps")
        .or_else(|| payload.get("maxSteps"))
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(UNBOUNDED_MAX_STEPS)
}

fn add_usage(total: &mut Usage, item: &Usage) {
    total.input_tokens = total.input_tokens.saturating_add(item.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(item.output_tokens);
    total.cost += item.cost;
}

fn provider_resume_payload(payload: &Value) -> Value {
    let mut value = payload.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("input");
        object.remove("message");
        object.remove("tool_call");
        object.remove("tool_calls");
        object.remove("api_key");
    }
    value
}

fn store_pending_provider_turn(
    session: &mut Session,
    payload: &Value,
    carry: &RuntimeProviderLoopCarry,
    permission_ruleset: PermissionRuleset,
    skip_permissions: bool,
) {
    session.metadata.insert(
        "pending_provider_turn".to_string(),
        json!({
            "payload": provider_resume_payload(payload),
            "answer": carry.answer.clone(),
            "usage": carry.usage.clone(),
            "tool_calls": carry.tool_calls,
            "next_step": carry.next_step,
            "permission": permission_ruleset.as_str(),
            "skip_permissions": skip_permissions,
        }),
    );
}

fn take_pending_provider_turn(session: &mut Session) -> Option<RuntimeProviderResume> {
    let pending = session.metadata.remove("pending_provider_turn")?;
    let permission_raw = pending
        .get("permission")
        .and_then(Value::as_str)
        .unwrap_or("FULL");
    Some(RuntimeProviderResume {
        payload: pending.get("payload").cloned().unwrap_or_else(|| json!({})),
        carry: RuntimeProviderLoopCarry {
            answer: pending
                .get("answer")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            usage: usage_from_provider_json(pending.get("usage")),
            tool_calls: pending
                .get("tool_calls")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            next_step: pending
                .get("next_step")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1),
        },
        permission_ruleset: parse_permission_ruleset(permission_raw).ok()?,
        skip_permissions: pending
            .get("skip_permissions")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn usage_from_provider_json(value: Option<&Value>) -> Usage {
    let input_tokens = value
        .and_then(|item| {
            item.get("input_tokens")
                .or_else(|| item.get("prompt_tokens"))
                .and_then(Value::as_u64)
        })
        .unwrap_or_default();
    let output_tokens = value
        .and_then(|item| {
            item.get("output_tokens")
                .or_else(|| item.get("completion_tokens"))
                .and_then(Value::as_u64)
        })
        .unwrap_or_default();
    Usage {
        input_tokens,
        output_tokens,
        cost: 0.0,
    }
}

fn usage_value_from_provider(
    usage: &Usage,
    tool_calls: u64,
    fallback_input: &str,
    fallback_output: &str,
) -> Value {
    let fallback = usage_payload(fallback_input, fallback_output, tool_calls);
    let input_tokens = if usage.input_tokens == 0 {
        fallback["input_tokens"].as_u64().unwrap_or_default()
    } else {
        usage.input_tokens
    };
    let output_tokens = if usage.output_tokens == 0 {
        fallback["output_tokens"].as_u64().unwrap_or_default()
    } else {
        usage.output_tokens
    };
    let tool_tokens = tool_calls.saturating_mul(16);
    json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "tool_tokens": tool_tokens,
        "total_tokens": input_tokens + output_tokens + tool_tokens,
        "tool_calls": tool_calls,
        "cost": usage.cost,
        "estimated": usage.input_tokens == 0 && usage.output_tokens == 0,
    })
}

fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn runtime_chat_message(role: Role, content: String) -> ChatMessage {
    ChatMessage {
        role,
        content,
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::new(),
    }
}

fn runtime_message_id(index: u64) -> String {
    format!("msg_{index}")
}

fn latest_assistant_message_id_for_tool(session: &Session, tool_call: &ToolCall) -> Option<String> {
    session.messages.iter().rev().find_map(|message| {
        if message.role != Role::Assistant {
            return None;
        }
        let message_id = message
            .metadata
            .get("message_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let has_call = message
            .metadata
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| {
                calls.iter().any(|call| {
                    call.get("call_id")
                        .or_else(|| call.get("id"))
                        .and_then(Value::as_str)
                        == Some(tool_call.call_id.as_str())
                })
            });
        if has_call { message_id } else { None }
    })
}

fn session_has_message_id(session: &Session, message_id: &str) -> bool {
    session.messages.iter().any(|message| {
        message.metadata.get("message_id").and_then(Value::as_str) == Some(message_id)
    })
}

fn assistant_message_for_provider_step(content: String, tool_calls: &[ToolCall]) -> ChatMessage {
    let mut message = runtime_chat_message(Role::Assistant, content);
    if !tool_calls.is_empty() {
        message.metadata.insert(
            "tool_calls".to_string(),
            Value::Array(tool_calls.iter().map(openai_tool_call_value).collect()),
        );
    }
    message
}

fn openai_tool_call_value(call: &ToolCall) -> Value {
    json!({
        "id": call.call_id.clone(),
        "call_id": call.call_id.clone(),
        "type": "function",
        "function": {
            "name": call.name.clone(),
            "arguments": stable_json_dumps(&call.input),
        },
        "name": call.name.clone(),
        "input": call.input.clone(),
    })
}

fn run_provider_loop(input: RuntimeProviderLoopInput<'_>) -> Result<Value, String> {
    let RuntimeProviderLoopInput {
        config,
        store,
        session,
        run_id,
        payload,
        permission_ruleset,
        skip_permissions,
        mut events,
        mut carry,
    } = input;
    let max_steps = provider_max_steps(payload);
    let agent_profile = runtime_agent_profile_for_session(session);
    let mut toolkit = toolkit_with_runtime_task_tool(session, agent_profile.as_ref());
    let mcp_runtime = register_runtime_mcp_tools(config, &session.directory, &mut toolkit);
    let visible_tools =
        filter_runtime_tools_for_profile(toolkit.get_all_tools("local"), agent_profile.as_ref());
    let visible_tool_names = visible_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let mut ctx = ToolContext::new(&session.directory)
        .with_session_id(session.id.clone())
        .with_agent_options(runtime_agent_tool_options(agent_profile.as_ref()))
        .with_permission_manager(runtime_permission_manager_for_agent(
            permission_ruleset.clone(),
            agent_profile.as_ref(),
        ))
        .with_dangerously_skip_permissions(skip_permissions);
    if let Some(answers) = payload
        .get("question_answers")
        .or_else(|| payload.get("answers"))
        .and_then(question_answers_from_json)
    {
        ctx.set_question_answers(answers);
    }
    if let Some(profile) = agent_profile.as_ref()
        && let Some((system, system_index)) =
            bind_runtime_agent_system_prompt(session, profile, &profile.mode)
    {
        let _ = store.append_message(session, &system, run_id, system_index);
    }

    let mut persisted_events = 0;
    append_unpersisted_app_events(
        &store.root,
        &session.id,
        run_id,
        &mut events,
        &mut persisted_events,
    );
    while carry.next_step <= max_steps {
        if turn_cancel_requested(run_id) {
            return finish_provider_loop_interrupted(
                store,
                session,
                run_id,
                events,
                &mut persisted_events,
                "interrupt requested",
            );
        }
        let step = carry.next_step;
        let assistant_index = session.messages.len() as u64;
        let assistant_message_id = runtime_message_id(assistant_index);
        runtime_record_step_started(store, &session.id, run_id, step, None);
        let mut streamed_text = false;
        let session_id = session.id.clone();
        let root = store.root.clone();
        let mut on_provider_stream = |event: &ProviderStreamEvent| {
            if let ProviderStreamEvent::TextDelta { text } = event
                && !text.is_empty()
            {
                streamed_text = true;
                events.push(json!({
                    "method": "item/agentMessage/delta",
                    "params": {
                        "thread_id": session_id.clone(),
                        "session_id": session_id.clone(),
                        "turn_id": run_id,
                        "run_id": run_id,
                        "step": step,
                        "event": {"id": format!("assistant_{step}"), "text": text.clone()},
                        "delta": text.clone(),
                    }
                }));
                append_unpersisted_app_events(
                    &root,
                    &session_id,
                    run_id,
                    &mut events,
                    &mut persisted_events,
                );
            }
        };
        let should_cancel = || turn_cancel_requested(run_id);
        let provider_result = match provider_turn_result(
            store,
            session,
            payload,
            &visible_tools,
            Some(&mut on_provider_stream),
            Some(&should_cancel),
        ) {
            Ok(result) => result,
            Err(error) if is_turn_interrupted_error(&error) => {
                return finish_provider_loop_interrupted(
                    store,
                    session,
                    run_id,
                    events,
                    &mut persisted_events,
                    "interrupt requested",
                );
            }
            Err(error) => return Err(error),
        };
        add_usage(&mut carry.usage, &provider_result.usage);
        if provider_result.source == "provider_missing_api_key" {
            events.push(json!({
                "method": "runtime/warning",
                "params": {
                    "session_id": session.id.clone(),
                    "turn_id": run_id,
                    "message": provider_result.answer.clone(),
                    "code": "provider_missing_api_key",
                }
            }));
        }
        if !provider_result.answer.is_empty() {
            carry.answer.push_str(&provider_result.answer);
            if !streamed_text {
                events.push(json!({
                    "method": "item/agentMessage/delta",
                    "params": {
                        "thread_id": session.id.clone(),
                        "session_id": session.id.clone(),
                        "turn_id": run_id,
                        "run_id": run_id,
                        "step": step,
                        "event": {"id": format!("assistant_{step}"), "text": provider_result.answer.clone()},
                        "delta": provider_result.answer.clone(),
                    }
                }));
            }
            let _ = store.append_part(
                &session.id,
                run_id,
                "text",
                SessionPartOptions {
                    attributes: BTreeMap::from([
                        ("role".to_string(), json!("assistant")),
                        (
                            "chars".to_string(),
                            json!(provider_result.answer.chars().count()),
                        ),
                    ]),
                    step_index: Some(step),
                    ..SessionPartOptions::default()
                },
            );
        }

        let mut assistant = assistant_message_for_provider_step(
            provider_result.answer.clone(),
            &provider_result.tool_calls,
        );
        assistant.metadata.insert(
            "message_id".to_string(),
            json!(assistant_message_id.clone()),
        );
        assistant.metadata.insert("step".to_string(), json!(step));
        session.add(assistant.clone());
        let _ = store.append_message(session, &assistant, run_id, assistant_index);

        if provider_result.tool_calls.is_empty() {
            return finish_provider_loop(
                store,
                session,
                run_id,
                events,
                &mut persisted_events,
                carry,
                &provider_result.finish_reason,
            );
        }

        let step_start_checkpoint = runtime_create_step_checkpoint(
            store,
            &session.id,
            run_id,
            &session.directory,
            step,
            "step_start",
            &assistant_message_id,
        );
        let resume_carry = RuntimeProviderLoopCarry {
            next_step: step.saturating_add(1),
            ..carry.clone()
        };
        for tool_call in &provider_result.tool_calls {
            carry.tool_calls = carry.tool_calls.saturating_add(1);
            let pending_carry = RuntimeProviderLoopCarry {
                tool_calls: carry.tool_calls,
                next_step: step.saturating_add(1),
                ..resume_carry.clone()
            };
            if let Some(paused) = execute_provider_tool_call(
                store,
                session,
                run_id,
                payload,
                step,
                tool_call,
                config,
                &toolkit,
                mcp_runtime.as_ref(),
                &visible_tool_names,
                &mut ctx,
                &permission_ruleset,
                skip_permissions,
                &pending_carry,
                step_start_checkpoint.as_deref(),
                &mut events,
                &mut persisted_events,
            )? {
                return Ok(paused);
            }
        }

        runtime_finalize_step_checkpoint(
            store,
            &session.id,
            run_id,
            &session.directory,
            step,
            &assistant_message_id,
            step_start_checkpoint.as_deref(),
        );
        carry.next_step = step.saturating_add(1);
    }

    session.status = SessionStatus::Idle;
    let _ = store.finish_run(
        session,
        run_id,
        "failed",
        max_steps,
        Some("max_steps"),
        Some("agent loop exceeded max_steps"),
    );
    let usage = usage_value_from_provider(
        &carry.usage,
        carry.tool_calls,
        &latest_user_message(session),
        &carry.answer,
    );
    let trace = trace_payload(session, run_id, carry.tool_calls);
    events.push(json!({
        "method": "turn/failed",
        "params": {
            "session_id": session.id.clone(),
            "turn_id": run_id,
            "status": "failed",
            "error": "agent loop exceeded max_steps",
            "usage": usage,
            "trace": trace,
        }
    }));
    append_unpersisted_app_events(
        &store.root,
        &session.id,
        run_id,
        &mut events,
        &mut persisted_events,
    );
    Ok(json!({
        "session_id": session.id,
        "turn_id": run_id,
        "status": "failed",
        "events": events,
    }))
}

struct RuntimeTaskExecutionContext<'a> {
    config: &'a HttpRuntimeConfig,
    store: &'a FileSessionStore,
    parent_session: &'a Session,
    parent_run_id: &'a str,
    payload: &'a Value,
    skip_permissions: bool,
}

fn execute_runtime_tool_call(
    toolkit: &Toolkit,
    mcp_runtime: Option<&RuntimeMcpRuntime>,
    tool_call: &ToolCall,
    ctx: &mut ToolContext,
    task_context: RuntimeTaskExecutionContext<'_>,
) -> ToolResult {
    if tool_call.name == "skill" {
        if let Some(result) =
            toolkit.permission_result_for_tool("skill", &tool_call.input, &tool_call.call_id, ctx)
        {
            return result;
        }
        match fork_skill_task_from_input(&tool_call.input, ctx) {
            Ok(Some(fork)) => {
                let task_call = ToolCall {
                    call_id: tool_call.call_id.clone(),
                    name: TASK_TOOL_ID.to_string(),
                    input: fork.task_input,
                };
                let mut result =
                    execute_runtime_task_tool_call(toolkit, &task_call, ctx, task_context);
                result
                    .metadata
                    .insert("skill_context".to_string(), json!("fork"));
                result
                    .metadata
                    .insert("skill_name".to_string(), json!(fork.skill_name));
                result
                    .metadata
                    .insert("skill_agent".to_string(), json!(fork.agent));
                result
                    .metadata
                    .insert("background".to_string(), json!(fork.background));
                return result;
            }
            Ok(None) => {}
            Err(error) => {
                return ToolResult {
                    call_id: tool_call.call_id.clone(),
                    output: String::new(),
                    error: Some(error),
                    metadata: BTreeMap::from([
                        ("tool".to_string(), json!("skill")),
                        ("error_kind".to_string(), json!("fork_skill_error")),
                        ("call_id".to_string(), json!(tool_call.call_id.clone())),
                    ]),
                };
            }
        }
    }
    if let Some(result) = execute_runtime_mcp_tool(toolkit, mcp_runtime, tool_call, ctx) {
        return result;
    }
    if tool_call.name == TASK_TOOL_ID {
        execute_runtime_task_tool_call(toolkit, tool_call, ctx, task_context)
    } else {
        toolkit.execute(
            &tool_call.name,
            tool_call.input.clone(),
            &tool_call.call_id,
            ctx,
        )
    }
}

fn execute_runtime_mcp_tool(
    toolkit: &Toolkit,
    mcp_runtime: Option<&RuntimeMcpRuntime>,
    tool_call: &ToolCall,
    ctx: &mut ToolContext,
) -> Option<ToolResult> {
    let runtime = mcp_runtime?;
    let descriptor = runtime.descriptors.get(&tool_call.name)?;
    if let Some(result) = toolkit.permission_result_for_tool(
        &tool_call.name,
        &tool_call.input,
        &tool_call.call_id,
        ctx,
    ) {
        return Some(result);
    }
    let Some(state) = runtime.manager.servers.get(&descriptor.server_name) else {
        let result = unavailable_tool_result(&tool_call.name);
        let bridge = bridge_tool_output(descriptor, result);
        return Some(mcp_bridge_to_tool_result(tool_call, bridge));
    };
    let transport = state.selected_transport.unwrap_or(McpTransport::Http);
    if transport == McpTransport::Stdio
        && let Some(result) = execute_runtime_mcp_lifecycle_tool_call(
            tool_call,
            descriptor,
            &state.config,
            &runtime.workspace,
        )
    {
        return Some(result);
    }
    let result = match mcp_json_rpc(
        &state.config,
        transport,
        "tools/call",
        json!({
            "name": descriptor.original_name,
            "arguments": tool_call.input.clone(),
        }),
        &runtime.workspace,
    ) {
        Ok(value) => normalize_tool_call_result(descriptor, Some(transport), &value),
        Err(error) => {
            let mut result = unavailable_tool_result(&tool_call.name);
            result.error = Some(error);
            result
        }
    };
    Some(mcp_bridge_to_tool_result(
        tool_call,
        bridge_tool_output(descriptor, result),
    ))
}

fn execute_runtime_mcp_lifecycle_tool_call(
    tool_call: &ToolCall,
    descriptor: &RemoteMcpToolDescriptor,
    server: &RemoteMcpServerConfig,
    workspace: &Path,
) -> Option<ToolResult> {
    if server.server_type != McpServerType::Local {
        return None;
    }
    let key = mcp_lifecycle_key(workspace, &server.name);
    let fingerprint = mcp_server_fingerprint(server);
    let mut registry = mcp_lifecycle_registry().lock().ok()?;
    let mut remove_entry = false;
    let result = match registry.get_mut(&key) {
        Some(entry) => {
            if entry.config_fingerprint != fingerprint {
                remove_entry = true;
                let mut result = unavailable_tool_result(&tool_call.name);
                result.error = Some(
                    "MCP local lifecycle process config changed; restart the server and retry."
                        .to_string(),
                );
                result
            } else if !entry.session.running() {
                remove_entry = true;
                let mut result = unavailable_tool_result(&tool_call.name);
                result.error = Some(
                    "MCP local lifecycle process exited; restart the server and retry.".to_string(),
                );
                result
            } else {
                let pid = entry.session.pid();
                match entry.session.request(
                    "tools/call",
                    json!({
                        "name": descriptor.original_name,
                        "arguments": tool_call.input.clone(),
                    }),
                ) {
                    Ok(value) => {
                        entry.last_refreshed_at_ms = now_ms();
                        let mut result = normalize_tool_call_result(
                            descriptor,
                            Some(McpTransport::Stdio),
                            &value,
                        );
                        result
                            .metadata
                            .insert("mcp_lifecycle_reused".to_string(), json!(true));
                        result
                            .metadata
                            .insert("mcp_lifecycle_pid".to_string(), json!(pid));
                        result
                    }
                    Err(error) => {
                        remove_entry = true;
                        let mut result = unavailable_tool_result(&tool_call.name);
                        result.error =
                            Some(format!("MCP local lifecycle tools/call failed: {error}"));
                        result
                    }
                }
            }
        }
        None => return None,
    };
    if remove_entry {
        if let Some(entry) = registry.remove(&key) {
            drop(registry);
            entry.session.close();
        }
    }
    Some(mcp_bridge_to_tool_result(
        tool_call,
        bridge_tool_output(descriptor, result),
    ))
}

fn mcp_bridge_to_tool_result(tool_call: &ToolCall, bridge: McpBridgeOutput) -> ToolResult {
    ToolResult {
        call_id: tool_call.call_id.clone(),
        output: bridge.output,
        error: bridge.error,
        metadata: bridge.metadata,
    }
}

fn execute_runtime_task_tool_call(
    toolkit: &Toolkit,
    tool_call: &ToolCall,
    ctx: &mut ToolContext,
    task_context: RuntimeTaskExecutionContext<'_>,
) -> ToolResult {
    if let Some(result) =
        toolkit.permission_result_for_tool(TASK_TOOL_ID, &tool_call.input, &tool_call.call_id, ctx)
    {
        return result;
    }
    let input = &tool_call.input;
    let background = input
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let subagent_type = match runtime_task_input_string(input, "subagent_type")
        .or_else(|_| runtime_task_input_string(input, "agent_type"))
        .or_else(|_| runtime_task_input_string(input, "agent"))
    {
        Ok(value) => value,
        Err(error) => return runtime_task_tool_error(tool_call, &error, BTreeMap::new()),
    };
    let prompt = match runtime_task_input_string(input, "prompt") {
        Ok(value) => value,
        Err(error) => return runtime_task_tool_error(tool_call, &error, BTreeMap::new()),
    };
    let description =
        runtime_task_input_string(input, "description").unwrap_or_else(|_| subagent_type.clone());
    let profile =
        match runtime_subagent_profile(&subagent_type, &task_context.parent_session.directory) {
            Some(profile) => profile,
            None => {
                return runtime_task_tool_error(
                    tool_call,
                    &format!("subagent profile not found: {subagent_type}"),
                    BTreeMap::from([("subagent_type".to_string(), json!(subagent_type))]),
                );
            }
        };
    let child_permission = profile.permission.clone();
    let child_provider = profile
        .provider
        .clone()
        .or_else(|| {
            task_context
                .payload
                .get("provider")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            task_context
                .parent_session
                .metadata
                .get("provider")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "openai".to_string());
    let child_model = profile
        .model
        .clone()
        .or_else(|| {
            task_context
                .payload
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            task_context
                .parent_session
                .metadata
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(default_model_id);
    let child_max_steps = profile
        .max_steps
        .unwrap_or_else(|| provider_max_steps(task_context.payload));
    let task_id = input
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut child_session = match task_id.as_deref() {
        Some(existing) => match task_context.store.load_session(existing) {
            Ok(session) => session,
            Err(error) => {
                return runtime_task_tool_error(
                    tool_call,
                    &format!("failed to resume task session {existing}: {error}"),
                    BTreeMap::from([("task_id".to_string(), json!(existing))]),
                );
            }
        },
        None => Session::new(
            new_id("subtask"),
            task_context.parent_session.directory.clone(),
        ),
    };
    let mut workspace_isolation = None;
    if let Some(existing) = task_id.as_deref() {
        if let Err(error) = validate_runtime_task_resume_session(
            &child_session,
            task_context.parent_session,
            &profile,
            existing,
        ) {
            return runtime_task_tool_error(
                tool_call,
                &error,
                BTreeMap::from([
                    ("subagent_type".to_string(), json!(profile.id.clone())),
                    ("task_id".to_string(), json!(existing)),
                ]),
            );
        }
    }
    if task_id.is_none()
        && runtime_task_workspace_isolation_requested(input, profile.workspace_isolation)
    {
        match prepare_isolated_workspace(
            &task_context.parent_session.directory,
            task_context.store.root.join("isolated_workspaces"),
            &child_session.id,
        ) {
            Ok(isolation) => {
                child_session.directory = PathBuf::from(&isolation.workspace);
                workspace_isolation = Some(isolation);
            }
            Err(error) => {
                return runtime_task_tool_error(
                    tool_call,
                    &format!("failed to prepare isolated workspace: {error}"),
                    BTreeMap::from([("subagent_type".to_string(), json!(profile.id.clone()))]),
                );
            }
        }
    }
    if let Some(error) = runtime_task_governance_error(task_context.parent_session, &profile) {
        return runtime_task_tool_error(
            tool_call,
            &error,
            BTreeMap::from([
                ("tool".to_string(), json!(TASK_TOOL_ID)),
                ("subagent_type".to_string(), json!(profile.id.clone())),
                ("status".to_string(), json!("failed")),
                (
                    "task_depth".to_string(),
                    json!(runtime_child_task_depth(task_context.parent_session)),
                ),
                ("max_task_depth".to_string(), json!(max_subagent_depth())),
                (
                    "task_lineage_subagents".to_string(),
                    json!(runtime_parent_task_lineage(task_context.parent_session)),
                ),
            ]),
        );
    }
    let child_task_depth = runtime_child_task_depth(task_context.parent_session);
    let task_root_session_id = runtime_task_root_session_id(task_context.parent_session);
    let task_lineage_subagents =
        runtime_child_task_lineage(task_context.parent_session, &profile.id);
    let child_run_id = new_id("turn");
    child_session.status = SessionStatus::Running;
    child_session
        .metadata
        .insert("agent".to_string(), json!(profile.id.clone()));
    child_session
        .metadata
        .insert("provider".to_string(), json!(child_provider.clone()));
    child_session
        .metadata
        .insert("model".to_string(), json!(child_model.clone()));
    child_session.metadata.insert(
        "model_options".to_string(),
        json!(profile.model_options.clone()),
    );
    if let Some(temperature) = profile.temperature {
        child_session
            .metadata
            .insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = profile.top_p {
        child_session
            .metadata
            .insert("top_p".to_string(), json!(top_p));
    }
    if let Some(color) = profile.color.as_deref() {
        child_session
            .metadata
            .insert("color".to_string(), json!(color));
    }
    child_session
        .metadata
        .insert("subagent".to_string(), json!(true));
    child_session.metadata.insert(
        "parent_session_id".to_string(),
        json!(task_context.parent_session.id.clone()),
    );
    child_session.metadata.insert(
        "task_parent_session_id".to_string(),
        json!(task_context.parent_session.id.clone()),
    );
    child_session.metadata.insert(
        "task_root_session_id".to_string(),
        json!(task_root_session_id.clone()),
    );
    child_session
        .metadata
        .insert("task_depth".to_string(), json!(child_task_depth));
    child_session.metadata.insert(
        "task_lineage_subagents".to_string(),
        json!(task_lineage_subagents.clone()),
    );
    child_session.metadata.insert(
        "parent_run_id".to_string(),
        json!(task_context.parent_run_id),
    );
    child_session.metadata.insert(
        "parent_tool_call_id".to_string(),
        json!(tool_call.call_id.clone()),
    );
    child_session
        .metadata
        .insert("task_description".to_string(), json!(description.clone()));
    child_session
        .metadata
        .insert("task_subagent_type".to_string(), json!(profile.id.clone()));
    child_session
        .metadata
        .insert("permission".to_string(), json!(child_permission.as_str()));
    child_session
        .metadata
        .insert("max_steps".to_string(), json!(child_max_steps));
    if let Some(isolation) = workspace_isolation.as_ref() {
        child_session
            .metadata
            .insert("workspace_isolation".to_string(), json!(isolation));
    }
    if task_id.is_some() {
        let resume_count = child_session
            .metadata
            .get("task_resume_count")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_add(1);
        child_session
            .metadata
            .insert("task_resume_count".to_string(), json!(resume_count));
        child_session
            .metadata
            .insert("task_resumed_at_ms".to_string(), json!(now_ms()));
    }
    if background {
        child_session
            .metadata
            .insert("task_status".to_string(), json!("queued"));
        child_session
            .metadata
            .insert("background".to_string(), json!(true));
    } else {
        child_session
            .metadata
            .insert("background".to_string(), json!(false));
    }
    child_session.metadata.insert(
        "agent_profile".to_string(),
        runtime_subagent_public_value(&profile),
    );
    let system_message = bind_runtime_subagent_system_prompt(&mut child_session, &profile);
    let user = runtime_chat_message(Role::User, prompt.clone());
    let user_index = child_session.messages.len() as u64;
    child_session.add(user.clone());

    if background {
        child_session.status = SessionStatus::Idle;
        if let Err(error) = task_context
            .store
            .save_state(&child_session, Some(&child_run_id))
        {
            return runtime_task_tool_error(
                tool_call,
                &format!("failed to queue background subagent session: {error}"),
                BTreeMap::from([("subagent_type".to_string(), json!(profile.id.clone()))]),
            );
        }
        let mut metadata = BTreeMap::from([
            ("tool".to_string(), json!(TASK_TOOL_ID)),
            ("title".to_string(), json!(description)),
            ("subagent_type".to_string(), json!(profile.id.clone())),
            ("task_id".to_string(), json!(child_session.id.clone())),
            ("session_id".to_string(), json!(child_session.id.clone())),
            ("run_id".to_string(), json!(child_run_id)),
            ("status".to_string(), json!("queued")),
            ("background".to_string(), json!(true)),
            ("provider".to_string(), json!(child_provider)),
            ("model".to_string(), json!(child_model)),
            (
                "model_options".to_string(),
                json!(profile.model_options.clone()),
            ),
            ("max_steps".to_string(), json!(child_max_steps)),
            ("task_depth".to_string(), json!(child_task_depth)),
            (
                "task_root_session_id".to_string(),
                json!(task_root_session_id),
            ),
            (
                "task_parent_session_id".to_string(),
                json!(task_context.parent_session.id.clone()),
            ),
            (
                "task_lineage_subagents".to_string(),
                json!(task_lineage_subagents),
            ),
            (
                "agent_profile".to_string(),
                runtime_subagent_public_value(&profile),
            ),
        ]);
        if let Some(isolation) = workspace_isolation.as_ref() {
            metadata.insert("workspace_isolation".to_string(), json!(isolation));
        }
        return ToolResult {
            call_id: tool_call.call_id.clone(),
            output: render_runtime_task_output(
                &child_session.id,
                "queued",
                "Background subagent task queued.",
            ),
            error: None,
            metadata,
        };
    }

    if let Err(error) = task_context.store.start_run(
        &mut child_session,
        StartRunOptions {
            run_id: child_run_id.clone(),
            trace_id: new_id("trace"),
            agent_name: profile.id.clone(),
            model_id: Some(child_model.clone()),
            provider_id: Some(child_provider.clone()),
            permission: if task_context.skip_permissions {
                format!("auto_allow:{}", child_permission.as_str())
            } else {
                child_permission.as_str().to_string()
            },
            max_steps: child_max_steps,
            started_at_ms: None,
        },
    ) {
        return runtime_task_tool_error(
            tool_call,
            &format!("failed to start subagent session: {error}"),
            BTreeMap::from([("subagent_type".to_string(), json!(profile.id.clone()))]),
        );
    }
    if let Some((system, system_index)) = system_message {
        let _ =
            task_context
                .store
                .append_message(&child_session, &system, &child_run_id, system_index);
    }
    let _ = task_context
        .store
        .append_message(&child_session, &user, &child_run_id, user_index);

    let mut child_payload = provider_resume_payload(task_context.payload);
    if let Some(object) = child_payload.as_object_mut() {
        object.insert("max_steps".to_string(), json!(child_max_steps));
    }
    let child_result = run_provider_loop(RuntimeProviderLoopInput {
        config: task_context.config,
        store: task_context.store,
        session: &mut child_session,
        run_id: &child_run_id,
        payload: &child_payload,
        permission_ruleset: child_permission,
        skip_permissions: task_context.skip_permissions,
        events: Vec::new(),
        carry: RuntimeProviderLoopCarry::default(),
    });
    match child_result {
        Ok(value) => {
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let final_answer = value
                .get("turn")
                .and_then(|turn| turn.get("final_answer"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if status != "completed" {
                return runtime_task_tool_error(
                    tool_call,
                    &format!("subagent {} finished with status {status}", profile.id),
                    BTreeMap::from([
                        ("tool".to_string(), json!(TASK_TOOL_ID)),
                        ("title".to_string(), json!(description)),
                        ("subagent_type".to_string(), json!(profile.id.clone())),
                        ("task_id".to_string(), json!(child_session.id.clone())),
                        ("session_id".to_string(), json!(child_session.id.clone())),
                        ("run_id".to_string(), json!(child_run_id)),
                        ("status".to_string(), json!(status)),
                        ("provider".to_string(), json!(child_provider)),
                        ("model".to_string(), json!(child_model)),
                        (
                            "model_options".to_string(),
                            json!(profile.model_options.clone()),
                        ),
                        ("max_steps".to_string(), json!(child_max_steps)),
                        ("task_depth".to_string(), json!(child_task_depth)),
                        (
                            "task_root_session_id".to_string(),
                            json!(task_root_session_id.clone()),
                        ),
                        (
                            "task_parent_session_id".to_string(),
                            json!(task_context.parent_session.id.clone()),
                        ),
                        (
                            "task_lineage_subagents".to_string(),
                            json!(task_lineage_subagents.clone()),
                        ),
                        (
                            "agent_profile".to_string(),
                            runtime_subagent_public_value(&profile),
                        ),
                    ]),
                );
            }
            let mut metadata = BTreeMap::from([
                ("tool".to_string(), json!(TASK_TOOL_ID)),
                ("title".to_string(), json!(description)),
                ("subagent_type".to_string(), json!(profile.id.clone())),
                ("task_id".to_string(), json!(child_session.id.clone())),
                ("session_id".to_string(), json!(child_session.id.clone())),
                ("run_id".to_string(), json!(child_run_id)),
                ("status".to_string(), json!("completed")),
                ("provider".to_string(), json!(child_provider)),
                ("model".to_string(), json!(child_model)),
                (
                    "model_options".to_string(),
                    json!(profile.model_options.clone()),
                ),
                ("max_steps".to_string(), json!(child_max_steps)),
                ("task_depth".to_string(), json!(child_task_depth)),
                (
                    "task_root_session_id".to_string(),
                    json!(task_root_session_id.clone()),
                ),
                (
                    "task_parent_session_id".to_string(),
                    json!(task_context.parent_session.id.clone()),
                ),
                (
                    "task_lineage_subagents".to_string(),
                    json!(task_lineage_subagents.clone()),
                ),
                (
                    "agent_profile".to_string(),
                    runtime_subagent_public_value(&profile),
                ),
            ]);
            if let Some(isolation) = workspace_isolation.as_ref() {
                metadata.insert("workspace_isolation".to_string(), json!(isolation));
            }
            ToolResult {
                call_id: tool_call.call_id.clone(),
                output: render_runtime_task_output(&child_session.id, "completed", &final_answer),
                error: None,
                metadata,
            }
        }
        Err(error) => runtime_task_tool_error(
            tool_call,
            &format!("subagent {} failed: {error}", profile.id),
            BTreeMap::from([
                ("tool".to_string(), json!(TASK_TOOL_ID)),
                ("title".to_string(), json!(description)),
                ("subagent_type".to_string(), json!(profile.id.clone())),
                ("task_id".to_string(), json!(child_session.id.clone())),
                ("session_id".to_string(), json!(child_session.id.clone())),
                ("run_id".to_string(), json!(child_run_id)),
                ("status".to_string(), json!("failed")),
                (
                    "model_options".to_string(),
                    json!(profile.model_options.clone()),
                ),
                ("task_depth".to_string(), json!(child_task_depth)),
                (
                    "task_root_session_id".to_string(),
                    json!(task_root_session_id),
                ),
                (
                    "task_parent_session_id".to_string(),
                    json!(task_context.parent_session.id.clone()),
                ),
                (
                    "task_lineage_subagents".to_string(),
                    json!(task_lineage_subagents),
                ),
            ]),
        ),
    }
}

fn validate_runtime_task_resume_session(
    child_session: &Session,
    parent_session: &Session,
    profile: &RuntimeSubagentProfile,
    task_id: &str,
) -> Result<(), String> {
    if !child_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!("task session {task_id} is not a subagent task"));
    }
    let parent_id = child_session
        .metadata
        .get("parent_session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if parent_id != parent_session.id {
        return Err("task does not belong to parent session".to_string());
    }
    let stored_agent = child_session
        .metadata
        .get("agent")
        .and_then(Value::as_str)
        .or_else(|| {
            child_session
                .metadata
                .get("task_subagent_type")
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    if !stored_agent.is_empty() && stored_agent != profile.id {
        return Err(format!(
            "task session {task_id} belongs to subagent {stored_agent}, not {}",
            profile.id
        ));
    }
    match child_session
        .metadata
        .get("task_status")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "queued" | "running" | "canceled" => {
            return Err(format!(
                "task session {task_id} cannot be resumed while task status is {}",
                child_session
                    .metadata
                    .get("task_status")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            ));
        }
        _ => {}
    }
    if matches!(
        child_session.status,
        SessionStatus::Running | SessionStatus::Paused | SessionStatus::Compacting
    ) {
        return Err(format!(
            "task session {task_id} cannot be resumed while session status is {}",
            session_status_text(&child_session.status)
        ));
    }
    Ok(())
}

fn bind_runtime_subagent_system_prompt(
    session: &mut Session,
    profile: &RuntimeSubagentProfile,
) -> Option<(ChatMessage, u64)> {
    bind_runtime_agent_system_prompt(session, profile, "subagent")
}

fn bind_runtime_agent_system_prompt(
    session: &mut Session,
    profile: &RuntimeSubagentProfile,
    agent_mode: &str,
) -> Option<(ChatMessage, u64)> {
    let already_bound = session.messages.iter().any(|message| {
        message.role == Role::System
            && message
                .metadata
                .get("agent_profile")
                .and_then(Value::as_str)
                == Some(profile.id.as_str())
    });
    if already_bound {
        return None;
    }
    let mut prompt_parts = Vec::new();
    let prompt = profile.prompt.trim_start_matches('\u{feff}').trim();
    if !prompt.is_empty() {
        prompt_parts.push(prompt.to_string());
    }
    let preloaded_skills = runtime_preloaded_skill_documents(profile, &session.directory);
    let preloaded_skill_names = preloaded_skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    if let Some(skills) = render_preloaded_skills(&preloaded_skills) {
        prompt_parts.push(skills);
    }
    if runtime_agent_allows_tool(profile, "skill") {
        let registry = SkillRegistry::new_with_options(
            Some(session.directory.clone()),
            (!profile.skill_roots.is_empty()).then_some(profile.skill_roots.clone()),
            Option::<PathBuf>::None,
            SkillRegistryOptions {
                include_builtin_skills: true,
            },
        );
        let skills = registry
            .all()
            .into_iter()
            .filter(|skill| skill_is_visible(&profile.skill_permissions, &skill.name))
            .collect::<Vec<_>>();
        if let Some(skills) = render_available_skills(&skills) {
            prompt_parts.push(skills);
        }
    }
    if prompt_parts.is_empty() {
        return None;
    }
    if !profile.skills.is_empty() {
        session
            .metadata
            .insert("skills".to_string(), json!(profile.skills.clone()));
    }
    if !preloaded_skill_names.is_empty() {
        session.metadata.insert(
            "preloaded_skills".to_string(),
            json!(preloaded_skill_names.clone()),
        );
    }
    let mut system = runtime_chat_message(Role::System, prompt_parts.join("\n\n"));
    system
        .metadata
        .insert("agent_profile".to_string(), json!(profile.id.clone()));
    system
        .metadata
        .insert("agent_mode".to_string(), json!(agent_mode));
    if !preloaded_skill_names.is_empty() {
        system
            .metadata
            .insert("preloaded_skills".to_string(), json!(preloaded_skill_names));
    }
    let system_index = session.messages.len() as u64;
    session.add(system.clone());
    Some((system, system_index))
}

fn runtime_preloaded_skill_documents(
    profile: &RuntimeSubagentProfile,
    session_root: &Path,
) -> Vec<SkillDocument> {
    if profile.skills.is_empty() {
        return Vec::new();
    }
    let registry = SkillRegistry::new_with_options(
        Some(session_root.to_path_buf()),
        (!profile.skill_roots.is_empty()).then_some(profile.skill_roots.clone()),
        Option::<PathBuf>::None,
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    );
    let mut seen = BTreeSet::new();
    profile
        .skills
        .iter()
        .filter_map(|name| {
            let name = name.trim();
            if name.is_empty()
                || !seen.insert(name.to_string())
                || !skill_is_visible(&profile.skill_permissions, name)
            {
                return None;
            }
            registry.get(name).filter(skill_document_model_invocable)
        })
        .collect()
}

fn runtime_subagent_public_value(profile: &RuntimeSubagentProfile) -> Value {
    json!({
        "id": profile.id.clone(),
        "name": profile.name.clone(),
        "description": profile.description.clone(),
        "mode": profile.mode.clone(),
        "permission": profile.permission.as_str(),
        "task_permissions": profile.task_permissions.clone(),
        "skills": profile.skills.clone(),
        "skill_roots": profile.skill_roots.clone(),
        "skill_permissions": profile.skill_permissions.clone(),
        "tools": profile.tools.clone(),
        "provider": profile.provider.clone(),
        "model": profile.model.clone(),
        "max_steps": profile.max_steps,
        "steps": profile.max_steps,
        "temperature": profile.temperature,
        "top_p": profile.top_p,
        "color": profile.color.clone(),
        "disabled": profile.disabled,
        "model_options": profile.model_options.clone(),
        "workspace_isolation": profile.workspace_isolation,
        "hidden": profile.hidden,
        "source_path": profile.source_path.as_ref().map(|path| path.to_string_lossy().to_string()),
    })
}

fn runtime_task_input_string(input: &Value, key: &str) -> Result<String, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("task tool requires non-empty {key}"))
}

fn runtime_task_workspace_isolation_requested(input: &Value, profile_default: bool) -> bool {
    input
        .get("isolate_workspace")
        .or_else(|| input.get("workspace_isolation"))
        .and_then(Value::as_bool)
        .unwrap_or(profile_default)
}

fn runtime_task_tool_error(
    tool_call: &ToolCall,
    error: &str,
    mut metadata: BTreeMap<String, Value>,
) -> ToolResult {
    metadata
        .entry("tool".to_string())
        .or_insert_with(|| json!(TASK_TOOL_ID));
    ToolResult {
        call_id: tool_call.call_id.clone(),
        output: String::new(),
        error: Some(error.to_string()),
        metadata,
    }
}

fn render_runtime_task_output(task_id: &str, state: &str, text: &str) -> String {
    format!(
        "<task id=\"{}\" state=\"{}\">\n<task_result>\n{}\n</task_result>\n</task>",
        escape_runtime_task_text(task_id),
        escape_runtime_task_text(state),
        escape_runtime_task_text(text),
    )
}

fn escape_runtime_task_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[allow(clippy::too_many_arguments)]
fn execute_provider_tool_call(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    payload: &Value,
    step: u64,
    tool_call: &ToolCall,
    config: &HttpRuntimeConfig,
    toolkit: &Toolkit,
    mcp_runtime: Option<&RuntimeMcpRuntime>,
    visible_tool_names: &BTreeSet<String>,
    ctx: &mut ToolContext,
    permission_ruleset: &PermissionRuleset,
    skip_permissions: bool,
    pending_carry: &RuntimeProviderLoopCarry,
    step_start_checkpoint: Option<&str>,
    events: &mut Vec<Value>,
    persisted_events: &mut usize,
) -> Result<Option<Value>, String> {
    events.push(json!({
        "method": "item/toolCall/started",
        "params": {
            "session_id": session.id.clone(),
            "turn_id": run_id,
            "run_id": run_id,
            "step": step,
            "call_id": tool_call.call_id.clone(),
            "name": tool_call.name.clone(),
            "input": tool_call.input.clone(),
        }
    }));
    append_unpersisted_app_events(&store.root, &session.id, run_id, events, persisted_events);
    let _ = store.record_event(
        &session.id,
        run_id,
        "tool.call.started",
        SessionEventOptions {
            kind: "tool".to_string(),
            attributes: BTreeMap::from([
                ("call_id".to_string(), json!(tool_call.call_id.clone())),
                ("name".to_string(), json!(tool_call.name.clone())),
                ("input".to_string(), tool_call.input.clone()),
                ("step".to_string(), json!(step)),
            ]),
            ..SessionEventOptions::default()
        },
    );

    if !visible_tool_names.contains(&tool_call.name) {
        let mut tool_result = ToolResult {
            call_id: tool_call.call_id.clone(),
            output: String::new(),
            error: Some(format!(
                "tool `{}` is not available to this agent profile",
                tool_call.name
            )),
            metadata: BTreeMap::from([
                ("tool".to_string(), json!(tool_call.name.clone())),
                ("denied_by_agent_profile".to_string(), json!(true)),
            ]),
        };
        append_completed_tool_result(
            store,
            session,
            run_id,
            step,
            tool_call,
            None,
            &mut tool_result,
            events,
        )?;
        append_unpersisted_app_events(&store.root, &session.id, run_id, events, persisted_events);
        return Ok(None);
    }

    if tool_call.name == "question" && ctx.question_answers.is_none() {
        let assistant_message_id = latest_assistant_message_id_for_tool(session, tool_call);
        let mut question = question_payload_for_tool_call(session, run_id, step, tool_call);
        attach_runtime_step_to_question(&mut question, assistant_message_id.as_deref());
        session.status = SessionStatus::Paused;
        session
            .metadata
            .insert("pending_question".to_string(), question.clone());
        session.metadata.remove("pending_question_response");
        store_pending_provider_turn(
            session,
            payload,
            pending_carry,
            permission_ruleset.clone(),
            skip_permissions,
        );
        let _ = store.record_event(
            &session.id,
            run_id,
            "question.requested",
            SessionEventOptions {
                kind: "question".to_string(),
                attributes: BTreeMap::from([
                    ("call_id".to_string(), json!(tool_call.call_id.clone())),
                    (
                        "questions".to_string(),
                        tool_call
                            .input
                            .get("questions")
                            .cloned()
                            .unwrap_or_else(|| json!([])),
                    ),
                ]),
                ..SessionEventOptions::default()
            },
        );
        if let Some(message_id) = assistant_message_id {
            let _ = store.append_part(
                &session.id,
                run_id,
                "question",
                SessionPartOptions {
                    message_id: Some(message_id),
                    content: Some(json!({
                        "call_id": tool_call.call_id.clone(),
                        "name": tool_call.name.clone(),
                        "questions": tool_call.input.get("questions").cloned().unwrap_or_else(|| json!([])),
                        "status": "pending",
                    })),
                    attributes: BTreeMap::from([
                        ("call_id".to_string(), json!(tool_call.call_id.clone())),
                        ("name".to_string(), json!(tool_call.name.clone())),
                    ]),
                    step_index: Some(step),
                    status: "pending".to_string(),
                    ..SessionPartOptions::default()
                },
            );
        }
        let _ = store.save_state(session, Some(run_id));
        events.push(json!({
            "method": "item/question/requested",
            "params": {
                "session_id": session.id.clone(),
                "turn_id": run_id,
                "status": "waiting_question",
                "event": question,
            }
        }));
        append_unpersisted_app_events(&store.root, &session.id, run_id, events, persisted_events);
        return Ok(Some(json!({
            "session_id": session.id,
            "turn_id": run_id,
            "status": "waiting_question",
            "events": events,
        })));
    }

    let change_before = capture_file_change_before(session, tool_call);
    let mut tool_result = execute_runtime_tool_call(
        toolkit,
        mcp_runtime,
        tool_call,
        ctx,
        RuntimeTaskExecutionContext {
            config,
            store,
            parent_session: session,
            parent_run_id: run_id,
            payload,
            skip_permissions,
        },
    );
    if tool_result
        .metadata
        .get("requires_approval")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let assistant_message_id = latest_assistant_message_id_for_tool(session, tool_call);
        let mut approval =
            approval_payload_for_tool_call(session, run_id, step, tool_call, &tool_result.metadata);
        attach_runtime_step_to_approval(
            &mut approval,
            assistant_message_id.as_deref(),
            step_start_checkpoint,
        );
        if let Some(preview) = change_before
            .as_ref()
            .and_then(|before| file_change_preview(before, tool_call))
            && let Some(object) = approval.as_object_mut()
        {
            object.insert("preview".to_string(), preview);
        }
        session.status = SessionStatus::Paused;
        session
            .metadata
            .insert("pending_approval".to_string(), approval.clone());
        session.metadata.remove("pending_approval_response");
        store_pending_provider_turn(
            session,
            payload,
            pending_carry,
            permission_ruleset.clone(),
            skip_permissions,
        );
        let _ = store.record_event(
            &session.id,
            run_id,
            "approval.requested",
            SessionEventOptions {
                kind: "approval".to_string(),
                attributes: BTreeMap::from([
                    ("call_id".to_string(), json!(tool_call.call_id.clone())),
                    ("name".to_string(), json!(tool_call.name.clone())),
                    ("approval".to_string(), approval.clone()),
                ]),
                ..SessionEventOptions::default()
            },
        );
        if let Some(message_id) = assistant_message_id {
            let _ = store.append_part(
                &session.id,
                run_id,
                "approval",
                SessionPartOptions {
                    message_id: Some(message_id),
                    content: Some(json!({
                        "call_id": tool_call.call_id.clone(),
                        "name": tool_call.name.clone(),
                        "approval": approval.clone(),
                        "status": "pending",
                    })),
                    attributes: BTreeMap::from([
                        ("call_id".to_string(), json!(tool_call.call_id.clone())),
                        ("name".to_string(), json!(tool_call.name.clone())),
                    ]),
                    step_index: Some(step),
                    status: "pending".to_string(),
                    ..SessionPartOptions::default()
                },
            );
        }
        let _ = store.save_state(session, Some(run_id));
        events.push(json!({
            "method": "turn/approval_requested",
            "params": {
                "session_id": session.id.clone(),
                "turn_id": run_id,
                "status": "waiting_approval",
                "approval": approval,
            }
        }));
        append_unpersisted_app_events(&store.root, &session.id, run_id, events, persisted_events);
        return Ok(Some(json!({
            "session_id": session.id,
            "turn_id": run_id,
            "status": "waiting_approval",
            "events": events,
        })));
    }

    append_completed_tool_result(
        store,
        session,
        run_id,
        step,
        tool_call,
        change_before,
        &mut tool_result,
        events,
    )?;
    append_unpersisted_app_events(&store.root, &session.id, run_id, events, persisted_events);
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn append_completed_tool_result(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    step: u64,
    tool_call: &ToolCall,
    change_before: Option<FileChangeBefore>,
    tool_result: &mut ToolResult,
    events: &mut Vec<Value>,
) -> Result<(), String> {
    let failed = tool_result.error.is_some();
    let patch = complete_file_change(
        store,
        session,
        run_id,
        tool_call,
        change_before,
        tool_result,
    );
    if let Some(change) = patch.as_ref() {
        tool_result
            .metadata
            .insert("patch".to_string(), public_file_change(change));
        tool_result.metadata.insert(
            "patch_id".to_string(),
            change.get("id").cloned().unwrap_or(Value::Null),
        );
        tool_result.metadata.insert(
            "diff".to_string(),
            change.get("diff").cloned().unwrap_or(Value::Null),
        );
    }
    events.push(json!({
        "method": if failed { "item/toolCall/failed" } else { "item/toolCall/completed" },
        "params": {
            "session_id": session.id.clone(),
            "turn_id": run_id,
            "run_id": run_id,
            "step": step,
            "call_id": tool_call.call_id.clone(),
            "name": tool_call.name.clone(),
            "output": tool_result.output.clone(),
            "error": tool_result.error.clone(),
            "metadata": tool_result.metadata.clone(),
        }
    }));
    if let Some(change) = patch.as_ref() {
        events.push(patch_detected_event(session, run_id, change));
    }
    append_tool_result_to_session(store, session, run_id, step, tool_call, tool_result)
}

fn append_tool_result_to_session(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    step: u64,
    tool_call: &ToolCall,
    tool_result: &ToolResult,
) -> Result<(), String> {
    let failed = tool_result.error.is_some();
    record_runtime_skill_tool_session_event(
        store,
        &session.id,
        run_id,
        step,
        tool_call,
        tool_result,
    );
    let _ = store.record_event(
        &session.id,
        run_id,
        if failed {
            "tool.call.failed"
        } else {
            "tool.call.finished"
        },
        SessionEventOptions {
            kind: "tool".to_string(),
            status: if failed {
                "error".to_string()
            } else {
                "ok".to_string()
            },
            attributes: BTreeMap::from([
                ("call_id".to_string(), json!(tool_call.call_id.clone())),
                ("name".to_string(), json!(tool_call.name.clone())),
                ("error".to_string(), json!(tool_result.error.clone())),
                ("metadata".to_string(), json!(tool_result.metadata.clone())),
                ("step".to_string(), json!(step)),
            ]),
            ..SessionEventOptions::default()
        },
    );
    let _ = store.append_part(
        &session.id,
        run_id,
        "tool_result",
        SessionPartOptions {
            attributes: BTreeMap::from([
                ("call_id".to_string(), json!(tool_call.call_id.clone())),
                ("name".to_string(), json!(tool_call.name.clone())),
                ("failed".to_string(), json!(failed)),
            ]),
            step_index: Some(step),
            ..SessionPartOptions::default()
        },
    );
    let mut tool_message = runtime_chat_message(
        Role::Tool,
        tool_result.error.as_ref().map_or_else(
            || tool_result.output.clone(),
            |error| format!("Tool failed: {error}"),
        ),
    );
    tool_message.name = Some(tool_call.name.clone());
    tool_message.tool_call_id = Some(tool_call.call_id.clone());
    tool_message
        .metadata
        .insert("tool_result".to_string(), json!(tool_result));
    tool_message
        .metadata
        .insert("step".to_string(), json!(step));
    if let Some(message_id) = latest_assistant_message_id_for_tool(session, tool_call) {
        tool_message
            .metadata
            .insert("assistant_message_id".to_string(), json!(message_id));
    }
    let tool_index = session.messages.len() as u64;
    session.add(tool_message.clone());
    store
        .append_message(session, &tool_message, run_id, tool_index)
        .map_err(|error| format!("failed to record tool message: {error}"))
}

fn record_runtime_skill_tool_session_event(
    store: &FileSessionStore,
    session_id: &str,
    run_id: &str,
    step: u64,
    tool_call: &ToolCall,
    tool_result: &ToolResult,
) {
    if tool_call.name != "skill" || tool_result.error.is_some() {
        return;
    }
    let mut attributes = BTreeMap::from([
        ("call_id".to_string(), json!(tool_call.call_id.clone())),
        ("name".to_string(), json!(tool_call.name.clone())),
        ("input".to_string(), tool_call.input.clone()),
        ("step".to_string(), json!(step)),
        ("metadata".to_string(), json!(tool_result.metadata.clone())),
    ]);
    for key in [
        "query",
        "skill_count",
        "loaded_count",
        "scanned_files",
        "invalid_count",
        "duplicate_count",
    ] {
        if let Some(value) = tool_result.metadata.get(key) {
            attributes.insert(key.to_string(), value.clone());
        }
    }
    let event = if let Some(skill_name) = tool_result
        .metadata
        .get("skill_name")
        .and_then(Value::as_str)
    {
        attributes.insert("skill_name".to_string(), json!(skill_name));
        for key in [
            "skill_location",
            "skill_dir",
            "skill_files",
            "skill_files_truncated",
            "skill_arguments",
            "skill_context",
            "skill_agent",
            "background",
        ] {
            if let Some(value) = tool_result.metadata.get(key) {
                attributes.insert(key.to_string(), value.clone());
            }
        }
        "skill.loaded"
    } else {
        "skill.discovered"
    };
    let _ = store.record_event(
        session_id,
        run_id,
        event,
        SessionEventOptions {
            kind: "skill".to_string(),
            attributes,
            ..SessionEventOptions::default()
        },
    );
}

fn runtime_create_step_checkpoint(
    store: &FileSessionStore,
    session_id: &str,
    run_id: &str,
    workspace: &Path,
    step: u64,
    kind: &str,
    message_id: &str,
) -> Option<String> {
    store
        .create_checkpoint(
            session_id,
            run_id,
            workspace,
            kind,
            Some(message_id),
            None,
            Some(step),
        )
        .ok()
        .map(|checkpoint| checkpoint.checkpoint_id)
}

fn runtime_finalize_step_checkpoint(
    store: &FileSessionStore,
    session_id: &str,
    run_id: &str,
    workspace: &Path,
    step: u64,
    message_id: &str,
    start_checkpoint_id: Option<&str>,
) {
    let Some(end_checkpoint_id) = runtime_create_step_checkpoint(
        store, session_id, run_id, workspace, step, "step_end", message_id,
    ) else {
        return;
    };
    let _ = store.append_part(
        session_id,
        run_id,
        "context",
        SessionPartOptions {
            message_id: Some(message_id.to_string()),
            content: Some(json!({
                "kind": "checkpoint",
                "snapshot_start": start_checkpoint_id,
                "snapshot_end": end_checkpoint_id,
            })),
            attributes: BTreeMap::from([
                ("kind".to_string(), json!("checkpoint")),
                ("snapshot_start".to_string(), json!(start_checkpoint_id)),
                ("snapshot_end".to_string(), json!(end_checkpoint_id.clone())),
            ]),
            step_index: Some(step),
            status: "completed".to_string(),
            ..SessionPartOptions::default()
        },
    );
    if let Some(start_checkpoint_id) = start_checkpoint_id {
        let _ = store.append_checkpoint_patch_part(
            session_id,
            run_id,
            message_id,
            start_checkpoint_id,
            &end_checkpoint_id,
            Some(step),
        );
    }
}

fn append_runtime_completion_assistant(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    answer: &str,
    step: u64,
    assistant_message_id: &str,
    start_checkpoint_id: Option<&str>,
) {
    let assistant = ChatMessage {
        role: Role::Assistant,
        content: answer.to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([
            (
                "message_id".to_string(),
                json!(assistant_message_id.to_string()),
            ),
            ("snapshot_start".to_string(), json!(start_checkpoint_id)),
            ("step".to_string(), json!(step)),
        ]),
    };
    let assistant_index = session.messages.len() as u64;
    session.add(assistant.clone());
    let _ = store.append_message(session, &assistant, run_id, assistant_index);
    runtime_finalize_step_checkpoint(
        store,
        &session.id,
        run_id,
        &session.directory,
        step,
        assistant_message_id,
        start_checkpoint_id,
    );
}

fn append_interaction_resolution_part(
    store: &FileSessionStore,
    session: &Session,
    run_id: &str,
    part_type: &str,
    pending: &Value,
    resolved: &Value,
    resolution_status: &str,
) {
    let Some(message_id) = pending
        .get("assistant_message_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    if !ensure_interaction_message(
        store,
        session,
        run_id,
        message_id,
        part_type,
        pending,
        resolution_status,
    ) {
        return;
    }
    let part_status = match resolution_status {
        "denied" | "dismissed" | "failed" | "error" => "error",
        _ => "completed",
    };
    let request_id = pending
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let call_id = pending
        .get("call_id")
        .or_else(|| pending.get("tool_call_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let name = pending
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or(part_type)
        .to_string();
    let step = pending.get("step").and_then(Value::as_u64);
    let _ = store.append_part(
        &session.id,
        run_id,
        part_type,
        SessionPartOptions {
            message_id: Some(message_id.to_string()),
            content: Some(json!({
                "request_id": request_id,
                "call_id": call_id,
                "name": name,
                "status": resolution_status,
                "request": pending,
                "resolution": resolved,
            })),
            attributes: BTreeMap::from([
                ("request_id".to_string(), json!(request_id)),
                ("call_id".to_string(), json!(call_id)),
                ("name".to_string(), json!(name)),
                ("resolution_status".to_string(), json!(resolution_status)),
            ]),
            step_index: step,
            status: part_status.to_string(),
            ..SessionPartOptions::default()
        },
    );
}

fn ensure_interaction_message(
    store: &FileSessionStore,
    session: &Session,
    run_id: &str,
    message_id: &str,
    part_type: &str,
    pending: &Value,
    resolution_status: &str,
) -> bool {
    if session_has_message_id(session, message_id) {
        return true;
    }
    if store
        .get_message_with_parts(&session.id, message_id)
        .ok()
        .flatten()
        .is_some()
    {
        return true;
    }

    let mut assistant = runtime_chat_message(
        Role::Assistant,
        interaction_resolution_fallback_text(part_type, resolution_status),
    );
    assistant
        .metadata
        .insert("message_id".to_string(), json!(message_id));
    if let Some(step) = pending.get("step").and_then(Value::as_u64) {
        assistant.metadata.insert("step".to_string(), json!(step));
    }
    if let Some(call_id) = pending
        .get("call_id")
        .or_else(|| pending.get("tool_call_id"))
        .and_then(Value::as_str)
    {
        assistant
            .metadata
            .insert("tool_call_id".to_string(), json!(call_id));
    }

    let index = store
        .list_messages_with_parts(&session.id, None, None)
        .map(|messages| messages.len() as u64)
        .unwrap_or(session.messages.len() as u64);
    store
        .append_message(session, &assistant, run_id, index)
        .is_ok()
}

fn interaction_resolution_fallback_text(part_type: &str, resolution_status: &str) -> String {
    match (part_type, resolution_status) {
        ("approval", "denied") => "approval denied",
        ("approval", _) => "approval resolved",
        ("question", "dismissed") => "question dismissed",
        ("question", _) => "question resolved",
        _ => "interaction resolved",
    }
    .to_string()
}

fn runtime_record_step_started(
    store: &FileSessionStore,
    session_id: &str,
    run_id: &str,
    step: u64,
    checkpoint_id: Option<&str>,
) {
    let _ = store.record_event(
        session_id,
        run_id,
        "step.started",
        SessionEventOptions {
            kind: "step".to_string(),
            attributes: BTreeMap::from([
                ("step".to_string(), json!(step)),
                ("checkpoint_id".to_string(), json!(checkpoint_id)),
            ]),
            ..SessionEventOptions::default()
        },
    );
}

fn finish_provider_loop(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    mut events: Vec<Value>,
    persisted_events: &mut usize,
    carry: RuntimeProviderLoopCarry,
    finish_reason: &str,
) -> Result<Value, String> {
    session.status = SessionStatus::Idle;
    session.metadata.remove("pending_provider_turn");
    let steps = carry.next_step.max(1);
    let _ = store.finish_run(
        session,
        run_id,
        "completed",
        steps,
        Some(finish_reason),
        None,
    );
    let usage = usage_value_from_provider(
        &carry.usage,
        carry.tool_calls,
        &latest_user_message(session),
        &carry.answer,
    );
    let trace = trace_payload(session, run_id, carry.tool_calls);
    record_usage_event(store, session, run_id, &usage);
    events.push(json!({
        "method": "turn/completed",
        "params": {
            "thread_id": session.id.clone(),
            "session_id": session.id.clone(),
            "turn_id": run_id,
            "status": "completed",
            "final_answer": carry.answer,
            "usage": usage,
            "trace": trace,
            "finish_reason": finish_reason,
        }
    }));
    append_unpersisted_app_events(
        &store.root,
        &session.id,
        run_id,
        &mut events,
        persisted_events,
    );
    Ok(json!({
        "session_id": session.id,
        "turn_id": run_id,
        "status": "completed",
        "turn": {
            "id": run_id,
            "session_id": session.id,
            "status": "completed",
            "final_answer": events.last().and_then(|event| event.get("params")).and_then(|params| params.get("final_answer")).cloned().unwrap_or_else(|| json!("")),
            "agent": session_text_metadata(session, "agent", "server"),
            "model": session_text_metadata(session, "model", &default_model_id()),
            "variant": session_text_metadata(session, "variant", "default"),
            "thinking": session_text_metadata(session, "thinking", "medium"),
            "usage": usage,
            "trace": trace,
        },
        "events": events
    }))
}

fn finish_provider_loop_interrupted(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    mut events: Vec<Value>,
    _persisted_events: &mut usize,
    reason: &str,
) -> Result<Value, String> {
    let interrupted = record_turn_interrupted(store, session, run_id, reason);
    if !events
        .iter()
        .any(|event| event.get("method").and_then(Value::as_str) == Some("turn/interrupted"))
    {
        events.extend(interrupted);
    }
    Ok(json!({
        "session_id": session.id,
        "turn_id": run_id,
        "status": "interrupted",
        "turn": {
            "id": run_id,
            "session_id": session.id,
            "status": "interrupted",
            "error": reason,
        },
        "events": events,
    }))
}

#[cfg(test)]
fn start_turn_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    start_turn_payload_inner(config, session_id, payload, None)
}

fn start_turn_response(
    config: &HttpRuntimeConfig,
    session_id: &str,
    request_path: &str,
    body: &str,
) -> HttpResponseSpec {
    let payload: Value = match serde_json::from_str(body) {
        Ok(payload) => payload,
        Err(error) => return json_response(400, json!({"error": error.to_string()})),
    };
    if turn_async_requested(request_path, &payload) {
        match start_turn_async_payload(config, session_id, payload) {
            Ok((status, payload)) => json_response(status, payload),
            Err(error) => json_response(400, json!({"error": error})),
        }
    } else {
        match start_turn_payload_inner(config, session_id, payload, None) {
            Ok(payload) => json_response(200, payload),
            Err(error) => json_response(400, json!({"error": error})),
        }
    }
}

fn start_turn_async_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    payload: Value,
) -> Result<(u16, Value), String> {
    validate_start_turn_payload(&payload)?;
    let run_id = new_id("turn");
    let registration = match register_turn_job(config, session_id, &run_id, payload.clone()) {
        Ok(registration) => registration,
        Err(TurnJobRegisterError::Unavailable) => {
            return Err("turn job registry unavailable".to_string());
        }
        Err(TurnJobRegisterError::QueuePersistFailed(error)) => {
            return Ok((
                500,
                json!({
                    "error": error,
                    "error_code": "turn_queue_persist_failed",
                    "session_id": session_id,
                    "status": "rejected",
                    "accepted": false,
                    "async": true,
                    "queued": false,
                }),
            ));
        }
        Err(TurnJobRegisterError::QueueFull {
            queued_count,
            max_queued_turns_per_session,
        }) => {
            return Ok((
                429,
                json!({
                    "error": "turn queue is full",
                    "error_code": "turn_queue_full",
                    "session_id": session_id,
                    "status": "rejected",
                    "accepted": false,
                    "async": true,
                    "queued": false,
                    "queued_count": queued_count,
                    "max_queued_turns_per_session": max_queued_turns_per_session,
                    "scheduler": {
                        "max_queued_turns_per_session": max_queued_turns_per_session,
                        "max_running_turn_workers": max_running_turn_workers(config),
                        "turn_queue_lease_stale_ms": config.turn_queue_lease_stale_ms,
                        "turn_queue_timeout_ms": turn_queue_timeout_ms(config),
                    },
                }),
            ));
        }
    };
    let (status, queue_position, queue_reason) = match registration {
        TurnJobRegistration::Running(_cancel) => {
            spawn_async_turn_worker(config, session_id.to_string(), run_id.clone(), payload)?;
            ("running", None, None)
        }
        TurnJobRegistration::Queued {
            job: _job,
            queue_position,
            queue_reason,
        } => ("queued", Some(queue_position), Some(queue_reason)),
    };
    Ok((
        202,
        json!({
            "session_id": session_id,
            "turn_id": run_id,
            "status": status,
            "accepted": true,
            "async": true,
            "queued": status == "queued",
            "queue_position": queue_position,
            "queue_reason": queue_reason,
            "scheduler": {
                "max_queued_turns_per_session": config.max_queued_turns_per_session,
                "max_running_turn_workers": max_running_turn_workers(config),
                "turn_queue_lease_stale_ms": config.turn_queue_lease_stale_ms,
                "turn_queue_timeout_ms": turn_queue_timeout_ms(config),
            },
            "turn": {
                "id": run_id,
                "session_id": session_id,
                "status": status,
                "queue_position": queue_position,
                "queue_reason": queue_reason,
            },
            "events": [],
        }),
    ))
}

fn spawn_async_turn_worker(
    config: &HttpRuntimeConfig,
    session_id: String,
    run_id: String,
    payload: Value,
) -> Result<(), String> {
    let thread_config = config.clone();
    let thread_session_id = session_id;
    let thread_run_id = run_id;
    thread::Builder::new()
        .name(format!("openagent-turn-{thread_run_id}"))
        .spawn(move || {
            match start_turn_payload_inner(
                &thread_config,
                &thread_session_id,
                payload,
                Some(thread_run_id.clone()),
            ) {
                Ok(payload) => {
                    let status = payload
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("completed");
                    mark_turn_job_status(&thread_config, &thread_run_id, status);
                }
                Err(error) if is_turn_interrupted_error(&error) => {
                    let store = FileSessionStore::new(session_root(&thread_config));
                    if let Ok((_session_id, mut session)) =
                        find_session_for_turn(&store, &thread_run_id)
                    {
                        let _ = record_turn_interrupted(
                            &store,
                            &mut session,
                            &thread_run_id,
                            "interrupt requested",
                        );
                    }
                    mark_turn_job_status(&thread_config, &thread_run_id, "interrupted");
                }
                Err(error) => {
                    record_async_turn_failure(
                        &thread_config,
                        &thread_session_id,
                        &thread_run_id,
                        &error,
                    );
                    mark_turn_job_status(&thread_config, &thread_run_id, "failed");
                }
            }
            start_next_queued_turns(&thread_config);
        })
        .map(|_| ())
        .map_err(|error| format!("failed to start async turn: {error}"))
}

fn mark_turn_job_status_from_result(
    config: &HttpRuntimeConfig,
    turn_id: &str,
    result: &Result<Value, String>,
) {
    match result {
        Ok(payload) => {
            let status = payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            mark_turn_job_status(config, turn_id, status);
        }
        Err(error) if is_turn_interrupted_error(error) => {
            mark_turn_job_status(config, turn_id, "interrupted");
        }
        Err(_) => {
            mark_turn_job_status(config, turn_id, "failed");
        }
    }
}

fn start_turn_payload_inner(
    config: &HttpRuntimeConfig,
    session_id: &str,
    payload: Value,
    run_id_override: Option<String>,
) -> Result<Value, String> {
    validate_start_turn_payload(&payload)?;
    let input = payload
        .get("input")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let permission_ruleset = permission_ruleset_for_turn(&payload)?;
    let skip_permissions = skip_permissions_for_turn(&payload);
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .unwrap_or_else(|_| Session::new(session_id.to_string(), workspace(config)));
    let runtime_profile = apply_turn_runtime_profile(&mut session, &payload);
    let run_id = run_id_override.unwrap_or_else(|| new_id("turn"));
    session.status = SessionStatus::Running;
    let _ = store.start_run(
        &mut session,
        StartRunOptions {
            run_id: run_id.clone(),
            trace_id: new_id("trace"),
            agent_name: runtime_profile.agent.clone(),
            model_id: Some(runtime_profile.model.clone()),
            provider_id: Some("openagent".to_string()),
            permission: if skip_permissions {
                format!("auto_allow:{}", permission_ruleset.as_str())
            } else {
                permission_ruleset.as_str().to_string()
            },
            max_steps: 1,
            started_at_ms: None,
        },
    );
    let user = ChatMessage {
        role: Role::User,
        content: input.to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::new(),
    };
    let user_index = session.messages.len() as u64;
    session.add(user.clone());
    let _ = store.append_message(&session, &user, &run_id, user_index);
    let mut tool_calls = tool_calls_from_turn_payload(&payload)?;
    if tool_calls.is_empty()
        && let Some(call) = manual_runtime_subagent_tool_call(&input)
    {
        tool_calls.push(call);
    }
    if tool_calls.is_empty() {
        let agent_profile = runtime_agent_profile_for_session(&session);
        let descriptors = runtime_task_subagent_descriptors(
            &session.directory,
            agent_profile.as_ref(),
            Some(&session),
        );
        if let Some(route) = select_task_subagent_for_prompt(&descriptors, input) {
            session.metadata.insert(
                "auto_subagent_route".to_string(),
                runtime_auto_route_value(&route),
            );
            tool_calls.push(auto_runtime_subagent_tool_call(input, &route));
        }
    }
    if !tool_calls.is_empty() {
        return run_http_tool_turn(
            config,
            &store,
            &mut session,
            &run_id,
            tool_calls,
            permission_ruleset,
            skip_permissions,
        );
    }
    let _ = runtime_profile;
    let initial_events = vec![turn_started_event(&session, &run_id)];
    run_provider_loop(RuntimeProviderLoopInput {
        config,
        store: &store,
        session: &mut session,
        run_id: &run_id,
        payload: &payload,
        permission_ruleset,
        skip_permissions,
        events: initial_events,
        carry: RuntimeProviderLoopCarry::default(),
    })
}

fn validate_start_turn_payload(payload: &Value) -> Result<(), String> {
    let input = payload
        .get("input")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if input.trim().is_empty() {
        return Err("turn input is required".to_string());
    }
    let _ = permission_ruleset_for_turn(payload)?;
    Ok(())
}

fn turn_async_requested(request_path: &str, payload: &Value) -> bool {
    ["async", "background", "run_async"]
        .iter()
        .filter_map(|key| query_value(request_path, key))
        .any(|value| truthy(&value))
        || ["async", "background", "run_async"]
            .iter()
            .filter_map(|key| payload.get(*key))
            .any(value_truthy)
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn value_truthy(value: &Value) -> bool {
    value
        .as_bool()
        .unwrap_or_else(|| value.as_str().is_some_and(truthy))
}

fn is_turn_interrupted_error(error: &str) -> bool {
    error == TURN_INTERRUPTED_ERROR || error.contains(TURN_INTERRUPTED_ERROR)
}

fn turn_event_recorded(root: &Path, session_id: &str, turn_id: &str, method: &str) -> bool {
    read_jsonl_values(&app_events_path(root, session_id, turn_id))
        .iter()
        .any(|event| event.get("method").and_then(Value::as_str) == Some(method))
}

fn turn_status_payload(config: &HttpRuntimeConfig, turn_id: &str) -> Result<Value, String> {
    expire_queued_turns(config);
    if let Some(job) = turn_job_payload(turn_id) {
        return Ok(json!({
            "turn_id": turn_id,
            "status": job.get("status").cloned().unwrap_or_else(|| json!("running")),
            "job": job,
            "source": "runtime_job_registry",
        }));
    }
    let root = session_root(config);
    if let Some(job) = persisted_turn_job_payload(&root, turn_id) {
        return Ok(json!({
            "turn_id": turn_id,
            "status": job.get("status").cloned().unwrap_or_else(|| json!("interrupted")),
            "job": job,
            "source": "runtime_job_index",
        }));
    }
    let store = FileSessionStore::new(session_root(config));
    let (session_id, session) = find_session_for_turn(&store, turn_id)?;
    let run_dir = store.root.join(&session_id).join("runs").join(turn_id);
    let run_record = read_json_file(&run_dir.join("run.json"));
    let summary = read_json_file(&run_dir.join("summary.json"));
    let status = summary
        .get("status")
        .or_else(|| run_record.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| session_status_label(&session.status));
    Ok(json!({
        "session_id": session_id,
        "turn_id": turn_id,
        "status": status,
        "session_status": session_status_label(&session.status),
        "run": run_record,
        "summary": summary,
        "source": "session_store",
    }))
}

fn session_status_label(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::Paused => "paused",
        SessionStatus::Stop => "stop",
        SessionStatus::Compacting => "compacting",
    }
}

fn record_async_turn_failure(
    config: &HttpRuntimeConfig,
    session_id: &str,
    run_id: &str,
    error: &str,
) {
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .unwrap_or_else(|_| Session::new(session_id.to_string(), workspace(config)));
    session.status = SessionStatus::Idle;
    session.metadata.remove("pending_provider_turn");
    let _ = store.finish_run(
        &session,
        run_id,
        "failed",
        1,
        Some("async_turn_error"),
        Some(error),
    );
    let _ = store.save_state(&session, Some(run_id));
    let mut events = vec![json!({
        "method": "turn/failed",
        "params": {
            "session_id": session.id,
            "turn_id": run_id,
            "status": "failed",
            "error": error,
        }
    })];
    append_app_events(&store.root, session_id, run_id, &mut events);
}

fn record_turn_interrupted(
    store: &FileSessionStore,
    session: &mut Session,
    turn_id: &str,
    error: &str,
) -> Vec<Value> {
    session.status = SessionStatus::Stop;
    session.metadata.remove("pending_provider_turn");
    let _ = store.finish_run(
        session,
        turn_id,
        "interrupted",
        1,
        Some("interrupted"),
        Some(error),
    );
    let _ = store.save_state(session, Some(turn_id));
    mark_turn_job_status_at_root(&store.root, turn_id, "interrupted");
    let event = json!({
        "method": "turn/interrupted",
        "params": {
            "session_id": session.id.clone(),
            "thread_id": session.id.clone(),
            "turn_id": turn_id,
            "status": "interrupted",
            "error": error,
        }
    });
    let mut events = vec![event];
    if !turn_event_recorded(&store.root, &session.id, turn_id, "turn/interrupted") {
        append_app_events(&store.root, &session.id, turn_id, &mut events);
    }
    events
}

fn run_http_tool_turn(
    config: &HttpRuntimeConfig,
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    tool_calls: Vec<ToolCall>,
    permission_ruleset: PermissionRuleset,
    skip_permissions: bool,
) -> Result<Value, String> {
    let agent_profile = runtime_agent_profile_for_session(session);
    let mut toolkit = toolkit_with_runtime_task_tool(session, agent_profile.as_ref());
    let mcp_runtime = register_runtime_mcp_tools(config, &session.directory, &mut toolkit);
    let tool_call_count = tool_calls.len() as u64;
    let empty_payload = json!({});
    let mut ctx = ToolContext::new(&session.directory)
        .with_session_id(session.id.clone())
        .with_agent_options(runtime_agent_tool_options(agent_profile.as_ref()))
        .with_permission_manager(runtime_permission_manager_for_agent(
            permission_ruleset.clone(),
            agent_profile.as_ref(),
        ))
        .with_dangerously_skip_permissions(skip_permissions);
    let mut events = vec![turn_started_event(session, run_id)];
    let assistant_message_id = runtime_message_id(session.messages.len() as u64 + tool_call_count);
    let start_checkpoint = runtime_create_step_checkpoint(
        store,
        &session.id,
        run_id,
        &session.directory,
        1,
        "step_start",
        &assistant_message_id,
    );
    runtime_record_step_started(store, &session.id, run_id, 1, start_checkpoint.as_deref());

    for (index, tool_call) in tool_calls.into_iter().enumerate() {
        let step = index as u64 + 1;
        events.push(json!({
            "method": "item/toolCall/started",
            "params": {
                "session_id": session.id.clone(),
                "turn_id": run_id,
                "call_id": tool_call.call_id.clone(),
                "name": tool_call.name.clone(),
                "input": tool_call.input.clone(),
            }
        }));
        let change_before = capture_file_change_before(session, &tool_call);
        let mut tool_result = execute_runtime_tool_call(
            &toolkit,
            mcp_runtime.as_ref(),
            &tool_call,
            &mut ctx,
            RuntimeTaskExecutionContext {
                config,
                store,
                parent_session: session,
                parent_run_id: run_id,
                payload: &empty_payload,
                skip_permissions,
            },
        );
        if tool_result
            .metadata
            .get("requires_approval")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let mut approval = approval_payload_for_tool_call(
                session,
                run_id,
                step,
                &tool_call,
                &tool_result.metadata,
            );
            attach_runtime_step_to_approval(
                &mut approval,
                Some(&assistant_message_id),
                start_checkpoint.as_deref(),
            );
            if let Some(preview) = change_before
                .as_ref()
                .and_then(|before| file_change_preview(before, &tool_call))
                && let Some(object) = approval.as_object_mut()
            {
                object.insert("preview".to_string(), preview);
            }
            session.status = SessionStatus::Paused;
            session
                .metadata
                .insert("pending_approval".to_string(), approval.clone());
            let _ = store.record_event(
                &session.id,
                run_id,
                "approval.requested",
                SessionEventOptions {
                    kind: "approval".to_string(),
                    attributes: BTreeMap::from([
                        ("call_id".to_string(), json!(tool_call.call_id)),
                        ("name".to_string(), json!(tool_call.name)),
                        ("approval".to_string(), approval.clone()),
                    ]),
                    ..SessionEventOptions::default()
                },
            );
            let _ = store.save_state(session, Some(run_id));
            events.push(json!({
                "method": "turn/approval_requested",
                "params": {
                    "session_id": session.id.clone(),
                    "turn_id": run_id,
                    "status": "waiting_approval",
                    "approval": approval,
                }
            }));
            append_app_events(&store.root, &session.id, run_id, &mut events);
            return Ok(json!({
                "session_id": session.id,
                "turn_id": run_id,
                "status": "waiting_approval",
                "events": events,
            }));
        }

        append_completed_tool_result(
            store,
            session,
            run_id,
            step,
            &tool_call,
            change_before,
            &mut tool_result,
            &mut events,
        )?;
    }

    let answer = if tool_calls_completed_successfully(&events) {
        "tool execution completed".to_string()
    } else {
        "tool execution failed".to_string()
    };
    append_runtime_completion_assistant(
        store,
        session,
        run_id,
        &answer,
        tool_call_count.max(1),
        &assistant_message_id,
        start_checkpoint.as_deref(),
    );
    session.status = SessionStatus::Idle;
    let _ = store.finish_run(session, run_id, "completed", 1, Some("stop"), None);
    let input = latest_user_message(session);
    let usage = usage_payload(&input, &answer, tool_call_count);
    let trace = trace_payload(session, run_id, tool_call_count);
    record_usage_event(store, session, run_id, &usage);
    events.push(json!({
        "method": "turn/completed",
        "params": {
            "thread_id": session.id.clone(),
            "turn_id": run_id,
            "status": "completed",
            "final_answer": answer,
            "usage": usage,
            "trace": trace,
        }
    }));
    append_app_events(&store.root, &session.id, run_id, &mut events);
    Ok(json!({
        "session_id": session.id,
        "turn_id": run_id,
        "status": "completed",
        "events": events,
    }))
}

fn respond_approval_payload(
    config: &HttpRuntimeConfig,
    path: &str,
    body: &str,
) -> Result<Value, String> {
    let (turn_id, request_id) = parse_turn_approval_path(path)?;
    let payload: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let response = approval_response_payload(&payload)?;
    let action = response
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let store = FileSessionStore::new(session_root(config));
    let mut session = find_session_with_pending_approval(&store, &turn_id, &request_id)?;
    let approval = session
        .metadata
        .get("pending_approval")
        .cloned()
        .ok_or_else(|| "pending approval not found".to_string())?;
    let run_id = approval
        .get("run_id")
        .or_else(|| approval.get("turn_id"))
        .and_then(Value::as_str)
        .unwrap_or(&turn_id)
        .to_string();
    let mut resolved = approval.clone();
    if let Some(object) = resolved.as_object_mut() {
        object.insert("action".to_string(), json!(action));
        object.insert("resolved_at_ms".to_string(), json!(now_ms()));
        if let Some(scope) = response.get("scope") {
            object.insert("scope".to_string(), scope.clone());
        }
        if let Some(note) = response.get("note") {
            object.insert("note".to_string(), note.clone());
        }
    }
    session.metadata.remove("pending_approval");

    let mut events = vec![json!({
        "method": "turn/approval_resolved",
        "params": {
            "session_id": session.id.clone(),
            "thread_id": session.id.clone(),
            "turn_id": run_id.clone(),
            "request_id": request_id.clone(),
            "status": if action == "allow" { "running" } else { "denied" },
            "approval": resolved.clone(),
        }
    })];
    let _ = store.record_event(
        &session.id,
        &run_id,
        "approval.resolved",
        SessionEventOptions {
            kind: "approval".to_string(),
            status: action.to_string(),
            attributes: BTreeMap::from([
                ("request_id".to_string(), json!(request_id)),
                ("action".to_string(), json!(action)),
            ]),
            ..SessionEventOptions::default()
        },
    );

    let response_status: &str;
    if action == "allow" {
        let tool_call = pending_approval_tool_call(&approval)?;
        let agent_profile = runtime_agent_profile_for_session(&session);
        let mut toolkit = toolkit_with_runtime_task_tool(&session, agent_profile.as_ref());
        let mcp_runtime = register_runtime_mcp_tools(config, &session.directory, &mut toolkit);
        let pending_payload = session
            .metadata
            .get("pending_provider_turn")
            .and_then(|pending| pending.get("payload"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let pending_skip_permissions = session
            .metadata
            .get("pending_provider_turn")
            .and_then(|pending| pending.get("skip_permissions"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut ctx = ToolContext::new(&session.directory)
            .with_session_id(session.id.clone())
            .with_agent_options(runtime_agent_tool_options(agent_profile.as_ref()))
            .with_permission_manager(runtime_permission_manager_for_agent(
                parse_permission_ruleset(
                    session
                        .metadata
                        .get("permission")
                        .and_then(Value::as_str)
                        .unwrap_or("FULL"),
                )
                .unwrap_or(PermissionRuleset::Full),
                agent_profile.as_ref(),
            ))
            .with_dangerously_skip_permissions(true);
        let change_before = capture_file_change_before(&session, &tool_call);
        let mut tool_result = execute_runtime_tool_call(
            &toolkit,
            mcp_runtime.as_ref(),
            &tool_call,
            &mut ctx,
            RuntimeTaskExecutionContext {
                config,
                store: &store,
                parent_session: &session,
                parent_run_id: &run_id,
                payload: &pending_payload,
                skip_permissions: pending_skip_permissions,
            },
        );
        let approval_step = approval.get("step").and_then(Value::as_u64).unwrap_or(1);
        append_completed_tool_result(
            &store,
            &mut session,
            &run_id,
            approval_step,
            &tool_call,
            change_before,
            &mut tool_result,
            &mut events,
        )?;
        let approval_start_checkpoint = approval
            .get("snapshot_start")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let approval_assistant_message_id = approval
            .get("assistant_message_id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| latest_assistant_message_id_for_tool(&session, &tool_call))
            .unwrap_or_else(|| runtime_message_id(session.messages.len() as u64));
        if let Some(resume) = take_pending_provider_turn(&mut session) {
            append_interaction_resolution_part(
                &store, &session, &run_id, "approval", &approval, &resolved, "allowed",
            );
            runtime_finalize_step_checkpoint(
                &store,
                &session.id,
                &run_id,
                &session.directory,
                approval_step,
                &approval_assistant_message_id,
                approval_start_checkpoint.as_deref(),
            );
            session.status = SessionStatus::Running;
            let result = run_provider_loop(RuntimeProviderLoopInput {
                config,
                store: &store,
                session: &mut session,
                run_id: &run_id,
                payload: &resume.payload,
                permission_ruleset: resume.permission_ruleset,
                skip_permissions: resume.skip_permissions,
                events,
                carry: resume.carry,
            });
            mark_turn_job_status_from_result(config, &run_id, &result);
            return result;
        }
        let failed = tool_result.error.is_some();
        response_status = if failed { "failed" } else { "completed" };
        let answer = if failed {
            "approval resolved, but tool execution failed".to_string()
        } else {
            "approval resolved".to_string()
        };
        append_runtime_completion_assistant(
            &store,
            &mut session,
            &run_id,
            &answer,
            approval_step,
            &approval_assistant_message_id,
            approval_start_checkpoint.as_deref(),
        );
        append_interaction_resolution_part(
            &store, &session, &run_id, "approval", &approval, &resolved, "allowed",
        );
        let input = latest_user_message(&session);
        let usage = usage_payload(&input, &answer, 1);
        let trace = trace_payload(&session, &run_id, 1);
        record_usage_event(&store, &session, &run_id, &usage);
        session.status = SessionStatus::Idle;
        let _ = store.finish_run(
            &session,
            &run_id,
            if failed { "failed" } else { "completed" },
            1,
            Some(if failed { "tool_error" } else { "stop" }),
            None,
        );
        events.push(json!({
            "method": "turn/completed",
            "params": {
                "session_id": session.id.clone(),
                "turn_id": run_id,
                "status": if failed { "failed" } else { "completed" },
                "final_answer": answer,
                "usage": usage,
                "trace": trace,
            }
        }));
    } else {
        response_status = "failed";
        session.metadata.remove("pending_provider_turn");
        session.status = SessionStatus::Idle;
        append_interaction_resolution_part(
            &store, &session, &run_id, "approval", &approval, &resolved, "denied",
        );
        let _ = store.finish_run(
            &session,
            &run_id,
            "failed",
            1,
            Some("permission_denied"),
            Some("approval denied"),
        );
        events.push(json!({
            "method": "turn/failed",
            "params": {
                "session_id": session.id.clone(),
                "turn_id": run_id.clone(),
                "status": "failed",
                "error": "approval denied",
            }
        }));
    }
    let _ = store.save_state(&session, Some(&run_id));
    append_app_events(&store.root, &session.id, &run_id, &mut events);
    mark_turn_job_status(config, &run_id, response_status);
    Ok(json!({
        "session_id": session.id,
        "turn_id": run_id,
        "request_id": request_id,
        "status": response_status,
        "approval": response,
        "events": events,
    }))
}

fn respond_question_payload(
    config: &HttpRuntimeConfig,
    path: &str,
    body: &str,
) -> Result<Value, String> {
    let (turn_id, request_id) = parse_turn_question_reply_path(path)?;
    let payload: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let response = if payload
        .get("dismissed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        question_dismiss_payload(&payload)
    } else {
        question_reply_payload(&payload)?
    };
    let store = FileSessionStore::new(session_root(config));
    let mut session = find_session_with_pending_question(&store, &turn_id, &request_id)?;
    let question = session
        .metadata
        .get("pending_question")
        .cloned()
        .ok_or_else(|| "pending question not found".to_string())?;
    let run_id = question
        .get("run_id")
        .or_else(|| question.get("turn_id"))
        .and_then(Value::as_str)
        .unwrap_or(&turn_id)
        .to_string();
    session.metadata.remove("pending_question");

    let mut events = vec![json!({
        "method": "item/question/resolved",
        "params": {
            "session_id": session.id.clone(),
            "thread_id": session.id.clone(),
            "turn_id": run_id.clone(),
            "request_id": request_id.clone(),
            "status": if response.get("dismissed").and_then(Value::as_bool).unwrap_or(false) {
                "dismissed"
            } else {
                "answered"
            },
            "question": response.clone(),
        }
    })];

    if response
        .get("dismissed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        session.metadata.remove("pending_provider_turn");
        session.status = SessionStatus::Idle;
        append_interaction_resolution_part(
            &store,
            &session,
            &run_id,
            "question",
            &question,
            &response,
            "dismissed",
        );
        let _ = store.finish_run(
            &session,
            &run_id,
            "failed",
            1,
            Some("question_dismissed"),
            Some("question dismissed"),
        );
        events.push(json!({
            "method": "turn/failed",
            "params": {
                "session_id": session.id.clone(),
                "turn_id": run_id.clone(),
                "status": "failed",
                "error": response.get("note").and_then(Value::as_str).unwrap_or("question dismissed"),
            }
        }));
        let _ = store.save_state(&session, Some(&run_id));
        append_app_events(&store.root, &session.id, &run_id, &mut events);
        mark_turn_job_status(config, &run_id, "failed");
        return Ok(json!({
            "session_id": session.id,
            "turn_id": run_id,
            "request_id": request_id,
            "status": "failed",
            "question": response,
            "events": events,
        }));
    }

    let tool_call = pending_question_tool_call(&question)?;
    let agent_profile = runtime_agent_profile_for_session(&session);
    let mut ctx = ToolContext::new(&session.directory)
        .with_session_id(session.id.clone())
        .with_agent_options(runtime_agent_tool_options(agent_profile.as_ref()));
    let answers = response
        .get("answers")
        .and_then(question_answers_from_json)
        .unwrap_or_default();
    ctx.set_question_answers(answers);
    let toolkit = toolkit_with_runtime_task_tool(&session, agent_profile.as_ref());
    let mut tool_result = toolkit.execute(
        "question",
        tool_call.input.clone(),
        &tool_call.call_id,
        &mut ctx,
    );
    append_completed_tool_result(
        &store,
        &mut session,
        &run_id,
        question.get("step").and_then(Value::as_u64).unwrap_or(1),
        &tool_call,
        None,
        &mut tool_result,
        &mut events,
    )?;
    append_interaction_resolution_part(
        &store, &session, &run_id, "question", &question, &response, "answered",
    );

    if let Some(resume) = take_pending_provider_turn(&mut session) {
        session.status = SessionStatus::Running;
        let result = run_provider_loop(RuntimeProviderLoopInput {
            config,
            store: &store,
            session: &mut session,
            run_id: &run_id,
            payload: &resume.payload,
            permission_ruleset: resume.permission_ruleset,
            skip_permissions: resume.skip_permissions,
            events,
            carry: resume.carry,
        });
        mark_turn_job_status_from_result(config, &run_id, &result);
        return result;
    }
    session.status = SessionStatus::Idle;
    let answer = "question answered".to_string();
    let input = latest_user_message(&session);
    let usage = usage_payload(&input, &answer, 1);
    let trace = trace_payload(&session, &run_id, 1);
    record_usage_event(&store, &session, &run_id, &usage);
    let _ = store.finish_run(&session, &run_id, "completed", 1, Some("stop"), None);
    let _ = store.save_state(&session, Some(&run_id));
    events.push(json!({
        "method": "turn/completed",
        "params": {
            "session_id": session.id.clone(),
            "turn_id": run_id.clone(),
            "status": "completed",
            "final_answer": answer,
            "usage": usage,
            "trace": trace,
        }
    }));
    append_app_events(&store.root, &session.id, &run_id, &mut events);
    mark_turn_job_status(config, &run_id, "completed");
    Ok(json!({
        "session_id": session.id,
        "turn_id": run_id,
        "request_id": request_id,
        "status": "completed",
        "question": response,
        "events": events,
    }))
}

fn interrupt_turn_payload(config: &HttpRuntimeConfig, turn_id: &str) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let job = request_turn_job_cancel(config, turn_id);
    let (session_id, mut session) = match find_session_for_turn(&store, turn_id) {
        Ok(found) => found,
        Err(error) => {
            let session_id = job
                .as_ref()
                .and_then(|value| value.get("session_id"))
                .and_then(Value::as_str)
                .ok_or(error)?;
            let session = store
                .load_session(session_id)
                .unwrap_or_else(|_| Session::new(session_id.to_string(), workspace(config)));
            (session_id.to_string(), session)
        }
    };
    let events = record_turn_interrupted(&store, &mut session, turn_id, "interrupt requested");
    Ok(json!({
        "session_id": session_id,
        "turn_id": turn_id,
        "status": "interrupted",
        "job": job,
        "events": events,
    }))
}

fn enqueue_tui_control_payload(
    config: &HttpRuntimeConfig,
    path: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let request = tui_control_request_for_path(path, &payload)?;
    let mut queue = read_json_array(&tui_control_queue_path(config));
    queue.push(request.to_value());
    write_json_value(&tui_control_queue_path(config), &Value::Array(queue))?;
    Ok(json!({"queued": true, "request": request.to_value()}))
}

fn pop_tui_control_payload(config: &HttpRuntimeConfig) -> Value {
    let path = tui_control_queue_path(config);
    let mut queue = read_json_array(&path);
    if queue.is_empty() {
        return control_next_payload(None);
    }
    let next = queue.remove(0);
    let _ = write_json_value(&path, &Value::Array(queue));
    let request = next.as_object().map(|_| {
        openagent_app_server::TuiControlRequest::new(
            next.get("path").and_then(Value::as_str).unwrap_or_default(),
            next.get("body").cloned().unwrap_or(Value::Null),
        )
    });
    control_next_payload(request.as_ref())
}

fn record_tui_control_response(config: &HttpRuntimeConfig, body: &str) -> Value {
    let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let response = record_control_response_payload(payload);
    append_json_line(&tui_control_responses_path(config), &response);
    response
}

fn find_session_with_pending_approval(
    store: &FileSessionStore,
    turn_id: &str,
    request_id: &str,
) -> Result<Session, String> {
    for entry in fs::read_dir(&store.root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.path().is_dir() {
            continue;
        }
        let state = read_json_file(&entry.path().join("state.latest.json"));
        let Some(pending) = state
            .get("metadata")
            .and_then(|metadata| metadata.get("pending_approval"))
        else {
            continue;
        };
        let same_turn = pending
            .get("turn_id")
            .or_else(|| pending.get("run_id"))
            .and_then(Value::as_str)
            == Some(turn_id);
        let same_request = pending.get("request_id").and_then(Value::as_str) == Some(request_id);
        if same_turn && same_request {
            let session_id = state
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "pending approval session is missing session_id".to_string())?;
            return store
                .load_session(session_id)
                .map_err(|error| error.to_string());
        }
    }
    Err("pending approval not found".to_string())
}

fn find_session_with_pending_question(
    store: &FileSessionStore,
    turn_id: &str,
    request_id: &str,
) -> Result<Session, String> {
    for entry in fs::read_dir(&store.root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.path().is_dir() {
            continue;
        }
        let state = read_json_file(&entry.path().join("state.latest.json"));
        let Some(pending) = state
            .get("metadata")
            .and_then(|metadata| metadata.get("pending_question"))
        else {
            continue;
        };
        let same_turn = pending
            .get("turn_id")
            .or_else(|| pending.get("run_id"))
            .and_then(Value::as_str)
            == Some(turn_id);
        let same_request = pending.get("request_id").and_then(Value::as_str) == Some(request_id);
        if same_turn && same_request {
            let session_id = state
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "pending question session is missing session_id".to_string())?;
            return store
                .load_session(session_id)
                .map_err(|error| error.to_string());
        }
    }
    Err("pending question not found".to_string())
}

fn pending_approval_tool_call(approval: &Value) -> Result<ToolCall, String> {
    let name = approval
        .get("tool_name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "pending approval missing tool_name".to_string())?;
    let call_id = approval
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("call_approval");
    Ok(ToolCall {
        name: name.to_string(),
        input: approval
            .get("tool_input")
            .cloned()
            .unwrap_or_else(|| json!({})),
        call_id: call_id.to_string(),
    })
}

fn pending_question_tool_call(question: &Value) -> Result<ToolCall, String> {
    let name = question
        .get("tool_name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("question");
    let call_id = question
        .get("call_id")
        .or_else(|| question.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("call_question");
    Ok(ToolCall {
        name: name.to_string(),
        input: question
            .get("tool_input")
            .cloned()
            .unwrap_or_else(|| json!({})),
        call_id: call_id.to_string(),
    })
}

fn approval_payload_for_tool_call(
    session: &Session,
    run_id: &str,
    step: u64,
    call: &ToolCall,
    metadata: &BTreeMap<String, Value>,
) -> Value {
    json!({
        "request_id": format!("approval_{}", call.call_id),
        "session_id": session.id,
        "turn_id": run_id,
        "run_id": run_id,
        "step": step,
        "tool_name": call.name,
        "tool_input": call.input,
        "call_id": call.call_id,
        "created_at_ms": now_ms(),
        "permission_action": metadata.get("permission_action").cloned().unwrap_or_else(|| json!("ask")),
        "permission_pattern": metadata.get("permission_pattern").cloned().unwrap_or_else(|| json!("")),
        "reason": metadata.get("error_kind").cloned().unwrap_or_else(|| json!("permission_required")),
        "metadata": metadata,
    })
}

fn attach_runtime_step_to_approval(
    approval: &mut Value,
    assistant_message_id: Option<&str>,
    start_checkpoint_id: Option<&str>,
) {
    if let Some(object) = approval.as_object_mut() {
        if let Some(assistant_message_id) = assistant_message_id {
            object.insert(
                "assistant_message_id".to_string(),
                json!(assistant_message_id),
            );
        }
        object.insert("snapshot_start".to_string(), json!(start_checkpoint_id));
    }
}

fn attach_runtime_step_to_question(question: &mut Value, assistant_message_id: Option<&str>) {
    if let Some(object) = question.as_object_mut()
        && let Some(assistant_message_id) = assistant_message_id
    {
        object.insert(
            "assistant_message_id".to_string(),
            json!(assistant_message_id),
        );
    }
}

fn question_payload_for_tool_call(
    session: &Session,
    run_id: &str,
    step: u64,
    call: &ToolCall,
) -> Value {
    json!({
        "request_id": format!("question_{}", call.call_id),
        "session_id": session.id,
        "turn_id": run_id,
        "run_id": run_id,
        "step": step,
        "tool_name": call.name,
        "tool_input": call.input,
        "tool_call_id": call.call_id,
        "call_id": call.call_id,
        "questions": call.input.get("questions").cloned().unwrap_or_else(|| json!([])),
        "created_at_ms": now_ms(),
    })
}

fn question_answers_from_json(value: &Value) -> Option<Vec<Vec<String>>> {
    let items = value.as_array()?;
    if items.iter().all(Value::is_array) {
        return Some(
            items
                .iter()
                .map(|item| {
                    item.as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(value_to_answer_string)
                        .collect::<Vec<_>>()
                })
                .collect(),
        );
    }
    Some(
        items
            .iter()
            .filter_map(value_to_answer_string)
            .map(|answer| vec![answer])
            .collect(),
    )
}

fn value_to_answer_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_bool().map(|item| item.to_string()))
        .or_else(|| value.as_i64().map(|item| item.to_string()))
        .or_else(|| value.as_u64().map(|item| item.to_string()))
        .or_else(|| value.as_f64().map(|item| item.to_string()))
}

fn tool_calls_from_turn_payload(payload: &Value) -> Result<Vec<ToolCall>, String> {
    if let Some(tool_call) = payload.get("tool_call") {
        return Ok(vec![tool_call_from_value(tool_call, 0)?]);
    }
    if let Some(items) = payload.get("tool_calls").and_then(Value::as_array) {
        return items
            .iter()
            .enumerate()
            .map(|(index, item)| tool_call_from_value(item, index))
            .collect();
    }
    Ok(Vec::new())
}

fn manual_runtime_subagent_tool_call(input: &str) -> Option<ToolCall> {
    let trimmed = input.trim_start();
    let rest = trimmed.strip_prefix('@')?;
    let (subagent_type, prompt) = rest.split_once(char::is_whitespace)?;
    let subagent_type = subagent_type.trim();
    let prompt = prompt.trim();
    if subagent_type.is_empty()
        || prompt.is_empty()
        || !subagent_type
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return None;
    }
    Some(ToolCall {
        name: TASK_TOOL_ID.to_string(),
        input: json!({
            "description": format!("@{subagent_type}"),
            "prompt": prompt,
            "subagent_type": subagent_type,
        }),
        call_id: format!("manual_task_{subagent_type}"),
    })
}

fn auto_runtime_subagent_tool_call(input: &str, route: &TaskSubagentRoute) -> ToolCall {
    ToolCall {
        name: TASK_TOOL_ID.to_string(),
        input: json!({
            "description": format!("Auto-routed to {}", route.subagent_id),
            "prompt": input,
            "subagent_type": route.subagent_id.clone(),
            "command": "auto_route",
        }),
        call_id: format!(
            "auto_task_{}",
            sanitize_runtime_agent_id(&route.subagent_id)
        ),
    }
}

fn runtime_auto_route_value(route: &TaskSubagentRoute) -> Value {
    json!({
        "subagent_type": route.subagent_id.clone(),
        "score": route.score,
        "matched_terms": route.matched_terms.clone(),
    })
}

fn tool_call_from_value(value: &Value, index: usize) -> Result<ToolCall, String> {
    let name = value
        .get("name")
        .or_else(|| value.get("tool"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "tool call name is required".to_string())?;
    Ok(ToolCall {
        name: name.to_string(),
        input: value
            .get("input")
            .or_else(|| value.get("arguments"))
            .cloned()
            .unwrap_or_else(|| json!({})),
        call_id: value
            .get("call_id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .map_or_else(|| format!("call_{index}"), str::to_string),
    })
}

fn permission_ruleset_for_turn(payload: &Value) -> Result<PermissionRuleset, String> {
    let raw = payload
        .get("permission")
        .or_else(|| payload.get("permissions"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| std::env::var("OPENAGENT_APP_PERMISSION").ok())
        .unwrap_or_else(|| "FULL".to_string());
    parse_permission_ruleset(&raw)
}

fn parse_permission_ruleset(raw: &str) -> Result<PermissionRuleset, String> {
    match raw.trim().to_ascii_uppercase().replace('-', "_").as_str() {
        "FULL" | "ALLOW" | "AUTO" => Ok(PermissionRuleset::Full),
        "READONLY" | "READ_ONLY" => Ok(PermissionRuleset::Readonly),
        "PLAN_ONLY" | "ASK" => Ok(PermissionRuleset::PlanOnly),
        "NONE" | "DENY" => Ok(PermissionRuleset::None),
        _ => Err("permission must be FULL, READONLY, PLAN_ONLY, or NONE".to_string()),
    }
}

fn skip_permissions_for_turn(payload: &Value) -> bool {
    payload
        .get("dangerously_skip_permissions")
        .or_else(|| payload.get("skip_permissions"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            std::env::var("OPENAGENT_APP_DANGEROUSLY_SKIP_PERMISSIONS")
                .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes"))
        })
}

fn provider_streaming_enabled_for_turn(payload: &Value) -> bool {
    payload
        .get("stream")
        .or_else(|| payload.get("provider_stream"))
        .or_else(|| payload.get("stream_provider"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            std::env::var("OPENAGENT_PROVIDER_STREAM")
                .map(|value| !matches!(value.as_str(), "0" | "false" | "FALSE" | "no"))
                .unwrap_or(true)
        })
}

fn append_app_events(root: &Path, session_id: &str, turn_id: &str, events: &mut [Value]) {
    let path = app_events_path(root, session_id, turn_id);
    let existing = read_jsonl_values(&path).len() as u64;
    for (index, event) in events.iter_mut().enumerate() {
        normalize_app_event(event, session_id, turn_id, existing + index as u64 + 1);
        append_json_line(&path, event);
    }
}

fn append_unpersisted_app_events(
    root: &Path,
    session_id: &str,
    turn_id: &str,
    events: &mut [Value],
    persisted_events: &mut usize,
) {
    if *persisted_events >= events.len() {
        return;
    }
    append_app_events(root, session_id, turn_id, &mut events[*persisted_events..]);
    *persisted_events = events.len();
}

fn normalize_app_event(event: &mut Value, session_id: &str, turn_id: &str, fallback_sequence: u64) {
    let Some(object) = event.as_object_mut() else {
        return;
    };
    object
        .entry("schema_version".to_string())
        .or_insert_with(|| json!(APP_BRIDGE_EVENT_SCHEMA_VERSION));
    object
        .entry("protocol_version".to_string())
        .or_insert_with(|| json!(APP_BRIDGE_PROTOCOL_VERSION));
    let sequence = object
        .get("sequence")
        .and_then(Value::as_u64)
        .unwrap_or(fallback_sequence);
    object
        .entry("sequence".to_string())
        .or_insert_with(|| json!(sequence));
    object
        .entry("created_at_ms".to_string())
        .or_insert_with(|| json!(now_ms()));
    object
        .entry("global_sequence".to_string())
        .or_insert_with(|| json!(sequence));
    object
        .entry("event_id".to_string())
        .or_insert_with(|| json!(app_event_id(session_id, turn_id, sequence)));
}

fn app_event_id(session_id: &str, turn_id: &str, sequence: u64) -> String {
    format!("app_evt:{session_id}:{turn_id}:{sequence}")
}

fn global_sse_frames(config: &HttpRuntimeConfig, request_path: &str) -> String {
    let last_id = last_event_id_from_path(request_path);
    let mut frames = String::new();
    for (index, event) in all_app_events(config).into_iter().enumerate() {
        let id = event
            .get("global_sequence")
            .or_else(|| event.get("sequence"))
            .and_then(Value::as_u64)
            .unwrap_or(index as u64 + 1);
        if id <= last_id {
            continue;
        }
        frames.push_str(&sse_frame(id, &event));
    }
    if frames.is_empty() {
        frames.push_str(": ping\n\n");
    }
    frames
}

fn turn_sse_frames(config: &HttpRuntimeConfig, turn_id: &str, request_path: &str) -> String {
    let last_id = last_event_id_from_path(request_path);
    let mut frames = String::new();
    for event in turn_app_events(config, turn_id) {
        let id = event
            .get("sequence")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if id <= last_id {
            continue;
        }
        frames.push_str(&sse_frame(id, &event));
    }
    if frames.is_empty() {
        frames.push_str(": ping\n\n");
    }
    frames
}

fn sse_frame(id: u64, event: &Value) -> String {
    let event_name = event
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("message");
    format!(
        "id: {id}\nevent: {event_name}\ndata: {}\n\n",
        stable_json_dumps(event)
    )
}

fn all_app_events(config: &HttpRuntimeConfig) -> Vec<Value> {
    let root = session_root(config);
    let mut events = Vec::new();
    if let Ok(sessions) = fs::read_dir(&root) {
        for session in sessions.flatten() {
            let runs_dir = session.path().join("runs");
            if let Ok(runs) = fs::read_dir(runs_dir) {
                for run in runs.flatten() {
                    events.extend(read_jsonl_values(&run.path().join(APP_EVENTS_FILE)));
                }
            }
        }
    }
    events.sort_by_key(|event| {
        event
            .get("created_at_ms")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    });
    for (index, event) in events.iter_mut().enumerate() {
        if let Some(object) = event.as_object_mut() {
            object.insert("global_sequence".to_string(), json!(index as u64 + 1));
        }
    }
    events
}

fn turn_app_events(config: &HttpRuntimeConfig, turn_id: &str) -> Vec<Value> {
    let root = session_root(config);
    if let Ok(sessions) = fs::read_dir(&root) {
        for session in sessions.flatten() {
            let path = app_events_path(&root, &session.file_name().to_string_lossy(), turn_id);
            if path.exists() {
                return read_jsonl_values(&path);
            }
        }
    }
    Vec::new()
}

fn app_events_path(root: &Path, session_id: &str, turn_id: &str) -> PathBuf {
    root.join(session_id)
        .join("runs")
        .join(turn_id)
        .join(APP_EVENTS_FILE)
}

fn last_event_id_from_path(path: &str) -> u64 {
    query_value(path, "last_event_id")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default()
}

fn query_value(path: &str, name: &str) -> Option<String> {
    path.split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.replace("%20", " ").replace('+', " ")))
}

fn find_session_for_turn(
    store: &FileSessionStore,
    turn_id: &str,
) -> Result<(String, Session), String> {
    for entry in fs::read_dir(&store.root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.path().join("runs").join(turn_id).is_dir() {
            continue;
        }
        let session_id = entry.file_name().to_string_lossy().to_string();
        let session = store
            .load_session(&session_id)
            .map_err(|error| error.to_string())?;
        return Ok((session_id, session));
    }
    Err("turn not found".to_string())
}

fn tui_control_queue_path(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config).join(TUI_CONTROL_QUEUE_FILE)
}

fn tui_control_responses_path(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config).join(TUI_CONTROL_RESPONSES_FILE)
}

fn read_json_array(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
}

fn read_jsonl_values(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .ok()
        .map(|raw| {
            raw.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn write_json_value(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(path, stable_json_dumps(value)).map_err(|error| error.to_string())
}

fn append_json_line(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", stable_json_dumps(value));
    }
}

fn session_root(config: &HttpRuntimeConfig) -> PathBuf {
    config
        .session_store_root
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace(config).join(".openagent/sessions"))
}

fn workspace(config: &HttpRuntimeConfig) -> PathBuf {
    config
        .workspace
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn read_json_file(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn session_status_text(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::Paused => "paused",
        SessionStatus::Stop => "stop",
        SessionStatus::Compacting => "compacting",
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn new_id(prefix: &str) -> String {
    static ID_COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{prefix}_{}_{}_{}", now_ms(), std::process::id(), sequence)
}

#[must_use]
pub fn http_runtime_fixture() -> Value {
    let workspace = "/tmp/openagent-rust-rewrite-fixture-goal12/workspace";
    let session_root = "/tmp/openagent-rust-rewrite-fixture-goal12/workspace/.openagent/sessions";
    let config = HttpRuntimeConfig {
        host: "0.0.0.0".to_string(),
        port: 8787,
        serve_static: false,
        workspace: Some(workspace.to_string()),
        session_store_root: Some(session_root.to_string()),
        auth_token: Some("server-secret".to_string()),
        ..HttpRuntimeConfig::default()
    };
    let events = fixture_events();
    let text = emit_app_bridge_events(&events, "text", true);
    let emitted_json = emit_app_bridge_events(&events, "json", false);
    let sse_lines = [
        ": ping\n",
        "\n",
        "id: 1\n",
        "event: item/agentMessage/delta\n",
        "data: {\"sequence\": 1, \"method\": \"item/agentMessage/delta\", \"params\": {\"event\": {\"text\": \"provider fixture answer\"}}}\n",
        "\n",
        "id: 2\n",
        "event: turn/completed\n",
        "data: {\"sequence\": 2, \"method\": \"turn/completed\", \"params\": {\"status\": \"completed\", \"final_answer\": \"provider fixture answer\"}}\n",
        "\n",
    ];

    json!({
        "schema_version": 1,
        "sdk": {"http_runtime_exports": sdk_exports()},
        "serve": {
            "args": {
                "host": "0.0.0.0",
                "port": 8787,
                "workspace": workspace,
                "session_root": session_root,
                "headless": true,
            },
            "call": {
                "host": "0.0.0.0",
                "port": 8787,
                "workspace": workspace,
                "session_store_root": session_root,
                "serve_static": false,
                "auth_token": "server-secret",
            },
        },
        "prompt": {
            "message_text": command_text_from_args(&["hello", "runtime"], Some(""), true),
            "stdin_text": command_text_from_args(&[], Some("from stdin\n"), false),
            "empty_tty_text": command_text_from_args(&[], Some(""), true),
            "with_file": build_run_prompt(
                "summarize",
                &[(format!("{workspace}/notes.txt").as_str(), "alpha\nbeta\n")]
            ),
        },
        "client": {
            "select_sessions": {
                "records": [
                    {"method": "GET", "server_url": "http://app.test", "path": "/api/sessions/session_existing", "auth_token": "server-secret"},
                    {"method": "GET", "server_url": "http://app.test", "path": "/api/sessions", "auth_token": "server-secret"},
                    {"method": "POST", "server_url": "http://app.test", "path": "/api/sessions", "payload": {"cwd": workspace}, "auth_token": "server-secret"},
                ],
                "explicit": {"id": "session_existing"},
                "continue": {"id": "session_latest"},
                "new": {"id": "session_new"},
            },
            "sse_parse": parse_sse_response_lines(&sse_lines).unwrap_or_default(),
            "emit_text": {
                "exit_code": text.exit_code,
                "stdout": text.stdout,
                "stderr": text.stderr,
            },
            "emit_json": {
                "exit_code": emitted_json.exit_code,
                "stdout_lines": emitted_json.stdout.lines().collect::<Vec<_>>(),
                "stderr": emitted_json.stderr,
            },
            "http_error": format_http_error("GET", "/api/health", 401, Some(&json!({"error": "unauthorized"}))),
        },
        "runtime": {
            "config": config.to_public_value(),
            "health": health_payload(&config),
            "skills": skills_fixture_payload(workspace),
            "routes": {
                "health": route_health().to_value(),
                "unauthorized": route_unauthorized().to_value(),
                "options": route_options().to_value(),
                "unknown": route_unknown().to_value(),
            },
        },
        "docker": {
            "dockerfile": dockerfile_lines(),
            "smoke_command": docker_smoke_command(),
            "expected_stdout_json": health_payload(&HttpRuntimeConfig {
                serve_static: false,
                ..HttpRuntimeConfig::default()
            }),
            "daemon_required": true,
        },
    })
}

fn skills_fixture_payload(workspace: &str) -> Value {
    json!({
        "skills": [{
            "name": "rooted",
            "description": "Rooted fixture skill",
            "location": format!("{workspace}/.openagent/skills/rooted/SKILL.md"),
            "directory": format!("{workspace}/.openagent/skills/rooted"),
            "metadata": {"audience": "fixture"},
            "score": null,
        }],
        "loaded_count": 1,
        "scanned_files": 1,
        "invalid_count": 0,
        "duplicate_count": 0,
        "issues": [],
    })
}

fn fixture_events() -> Vec<Value> {
    vec![
        json!({
            "sequence": 1,
            "method": "item/agentMessage/delta",
            "params": {"event": {"text": "provider fixture answer"}},
        }),
        json!({
            "sequence": 2,
            "method": "turn/completed",
            "params": {"status": "completed", "final_answer": "provider fixture answer"},
        }),
    ]
}

fn sdk_exports() -> Vec<&'static str> {
    vec![
        "AgentConfig",
        "AgentLoop",
        "ExploreAgent",
        "LanguageModel",
        "Model",
        "OpenAIProvider",
        "PermissionAction",
        "PermissionManager",
        "PermissionRule",
        "PermissionRuleset",
        "PlanAgent",
        "QuestionManager",
        "RemoteMcpManager",
        "Session",
        "SkillDiscoveryReport",
        "SkillDocument",
        "SkillInfo",
        "SkillIssue",
        "SkillRegistry",
        "ToolkitAdapter",
        "UniversalAgent",
        "load_mcp_config_from_sources",
        "new_id",
    ]
}

fn emit_text_event(event: &Value, verbose: bool, stdout: &mut String, stderr: &mut String) -> bool {
    let method = event_method(event);
    let params = event_params(event);
    let payload = params.get("event").filter(|value| value.is_object());
    if method == "item/agentMessage/delta"
        && let Some(payload) = payload
    {
        stdout.push_str(
            payload
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        return true;
    }
    if matches!(method.as_str(), "turn/error" | "turn/failed") {
        let error = params
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_default();
        stderr.push_str(&format!("{method}: {error}\n"));
        return false;
    }
    if verbose {
        stderr.push_str(&format!("[{method}]\n"));
    }
    false
}

fn event_method(event: &Value) -> String {
    event
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn event_params(event: &Value) -> Map<String, Value> {
    event
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

pub fn stable_json_dumps(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => {
            if *value {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(stable_json_dumps)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        Value::Object(items) => {
            let mut keys = items.keys().collect::<Vec<_>>();
            keys.sort();
            let inner = keys
                .into_iter()
                .map(|key| {
                    let rendered_key =
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string());
                    let value = stable_json_dumps(&items[key]);
                    format!("{rendered_key}: {value}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn provider_sse_stream_stops_on_responses_completed_without_done() {
        let raw = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"after\"}\n\n",
        );
        let mut chunks = Vec::new();
        read_sse_json_values_stream(raw.as_bytes(), |chunk| {
            chunks.push(chunk);
            Ok(())
        })
        .expect("read stream");

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0]["type"], json!("response.output_text.delta"));
        assert_eq!(chunks[1]["type"], json!("response.completed"));
    }

    #[test]
    fn exposes_command_boundary() {
        assert_eq!(crate_name(), "openagent-http-runtime");
        assert_eq!(command_name(), "openagent-http-runtime");
        assert_eq!(app_server_crate_name(), "openagent-app-server");
    }

    #[test]
    fn provider_max_steps_matches_opencode_unbounded_default() {
        assert_eq!(provider_max_steps(&json!({})), UNBOUNDED_MAX_STEPS);
        assert_eq!(
            provider_max_steps(&json!({"max_steps": 0})),
            UNBOUNDED_MAX_STEPS
        );
        assert_eq!(provider_max_steps(&json!({"max_steps": 25})), 25);
        assert_eq!(provider_max_steps(&json!({"maxSteps": 100})), 100);
    }

    #[test]
    fn app_bridge_terminal_run_is_workspace_scoped() {
        let root = std::env::temp_dir().join(format!("openagent-http-terminal-{}", now_ms()));
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");
        fs::create_dir_all(&nested).expect("workspace");
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(root.join("sessions").to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };

        let ok_response = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/terminal/run".to_string(),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({
                    "command": "printf terminal-ok",
                    "cwd": "nested",
                })),
            },
            &config,
        );
        assert_eq!(ok_response.status, 200);
        let ok = ok_response.body.expect("terminal body");
        assert_eq!(ok["success"], true);
        assert_eq!(ok["exit_code"], 0);
        assert_eq!(ok["stdout"], "terminal-ok");
        assert_eq!(ok["stderr"], "");
        assert_eq!(ok["timed_out"], false);
        assert_eq!(ok["cwd_relative"], "nested");

        let escape_response = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/terminal/run".to_string(),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({
                    "command": "printf outside",
                    "cwd": "..",
                })),
            },
            &config,
        );
        assert_eq!(escape_response.status, 400);
        assert!(
            escape_response.body.expect("escape body")["error"]
                .as_str()
                .unwrap_or_default()
                .contains("escapes session root")
        );
    }

    #[test]
    fn app_bridge_mcp_status_sanitizes_config() {
        let config = HttpRuntimeConfig {
            serve_static: false,
            mcp_config: Some(stable_json_dumps(&json!({
                "mcp": {
                    "servers": {
                        "local-tools": {
                            "type": "stdio",
                            "command": ["npx", "secret-package", "--token", "secret-command-token"],
                            "env": {"API_KEY": "secret-env-token"},
                            "headers": {"Authorization": "Bearer secret-header-token"},
                            "enabled": true,
                        },
                        "remote-tools": {
                            "url": "https://example.test/mcp?token=secret-url-token",
                            "transport": "http",
                            "enabled": false,
                        }
                    }
                }
            }))),
            ..HttpRuntimeConfig::default()
        };
        let protocol = app_bridge_protocol_payload();
        assert_eq!(
            protocol["endpoints"]["mcp"],
            "GET /api/mcp; POST /api/mcp/servers; PATCH|DELETE /api/mcp/servers/{name}; POST /api/mcp/servers/{name}/test|start|stop|restart"
        );

        let response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/mcp".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(response.status, 200);
        let body = response.body.expect("mcp body");
        assert_eq!(body["configured"], true);
        assert_eq!(body["enabled"], true);
        assert_eq!(body["server_count"], 2);
        assert_eq!(body["tool_count"], 0);
        assert_eq!(body["source"], "config");

        let servers = body["servers"].as_array().expect("servers");
        let local = servers
            .iter()
            .find(|server| server["name"] == "local-tools")
            .expect("local server");
        assert_eq!(local["type"], "local");
        assert_eq!(local["transport"], "stdio");
        assert_eq!(local["command"], "npx");
        assert_eq!(local["args_count"], 3);
        assert_eq!(local["env_count"], 1);
        assert_eq!(local["header_count"], 1);

        let remote = servers
            .iter()
            .find(|server| server["name"] == "remote-tools")
            .expect("remote server");
        assert_eq!(remote["remote_url_configured"], true);
        assert_eq!(remote["status"], "disabled");

        let serialized = stable_json_dumps(&body);
        for secret in [
            "secret-command-token",
            "secret-env-token",
            "secret-header-token",
            "secret-url-token",
            "Authorization",
            "API_KEY",
        ] {
            assert!(
                !serialized.contains(secret),
                "MCP status leaked secret marker: {secret}"
            );
        }
    }

    #[test]
    fn app_bridge_mcp_server_config_crud_writes_default_file() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-crud-{}", now_ms()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(root.join("sessions").to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };

        let initial = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/mcp".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(initial.status, 200);
        let initial_body = initial.body.expect("initial mcp body");
        assert_eq!(initial_body["configured"], false);
        assert_eq!(initial_body["source"], "none");
        assert_eq!(initial_body["writable"], true);

        let create = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers".to_string(),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({
                    "name": "remote-tools",
                    "type": "remote",
                    "url": "http://127.0.0.1:9/mcp?token=crud-secret",
                    "transport": "http",
                    "enabled": true,
                    "timeout_ms": 2000
                })),
            },
            &config,
        );
        assert_eq!(create.status, 200);
        let created = create.body.expect("created mcp body");
        assert_eq!(created["configured"], true);
        assert_eq!(created["source"], "default");
        assert_eq!(created["server_count"], 1);
        assert_eq!(created["servers"][0]["name"], "remote-tools");
        assert_eq!(created["servers"][0]["remote_url_configured"], true);
        assert!(!stable_json_dumps(&created).contains("crud-secret"));

        let config_path = workspace.join(".openagent").join("mcp.json");
        assert!(config_path.is_file());
        let stored = read_json_file(&config_path);
        assert_eq!(
            stored["mcp"]["servers"]["remote-tools"]["url"],
            "http://127.0.0.1:9/mcp?token=crud-secret"
        );

        let disable = route_http_request(
            &HttpRequest {
                method: "PATCH".to_string(),
                path: "/api/mcp/servers/remote-tools".to_string(),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({"enabled": false})),
            },
            &config,
        );
        assert_eq!(disable.status, 200);
        let disabled = disable.body.expect("disabled mcp body");
        assert_eq!(disabled["servers"][0]["enabled"], false);
        assert_eq!(disabled["servers"][0]["status"], "disabled");

        let delete = route_http_request(
            &HttpRequest {
                method: "DELETE".to_string(),
                path: "/api/mcp/servers/remote-tools".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(delete.status, 200);
        let deleted = delete.body.expect("deleted mcp body");
        assert_eq!(deleted["configured"], false);
        assert_eq!(deleted["server_count"], 0);
        let stored_after_delete = read_json_file(&config_path);
        assert!(
            stored_after_delete["mcp"]["servers"]
                .as_object()
                .expect("servers object")
                .is_empty()
        );
    }

    #[test]
    fn app_bridge_mcp_server_config_crud_writes_local_stdio_fields() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-local-crud-{}", now_ms()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(root.join("sessions").to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };

        let create = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers".to_string(),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({
                    "name": "local-tools",
                    "type": "local",
                    "command": "node",
                    "args": ["server.js", "--stdio"],
                    "cwd": "/tmp/local-tools",
                    "env": {"LOCAL_SECRET": "local-secret-value"},
                    "headers": {"X-Local-Token": "local-header-secret"},
                    "timeout_ms": 3000,
                    "enabled": true
                })),
            },
            &config,
        );
        assert_eq!(create.status, 200);
        let created = create.body.expect("created local mcp body");
        assert_eq!(created["configured"], true);
        assert_eq!(created["server_count"], 1);
        assert_eq!(created["servers"][0]["name"], "local-tools");
        assert_eq!(created["servers"][0]["type"], "local");
        assert_eq!(created["servers"][0]["transport"], "stdio");
        assert_eq!(created["servers"][0]["command"], "node");
        assert_eq!(created["servers"][0]["args_count"], 2);
        assert_eq!(created["servers"][0]["cwd_configured"], true);
        assert_eq!(created["servers"][0]["env_count"], 1);
        assert_eq!(created["servers"][0]["header_count"], 1);
        assert_eq!(created["servers"][0]["timeout_ms"], 3000);
        let serialized = stable_json_dumps(&created);
        assert!(!serialized.contains("local-secret-value"));
        assert!(!serialized.contains("local-header-secret"));
        assert!(!serialized.contains("LOCAL_SECRET"));
        assert!(!serialized.contains("X-Local-Token"));

        let stored = read_json_file(&workspace.join(".openagent").join("mcp.json"));
        assert_eq!(
            stored["mcp"]["servers"]["local-tools"]["command"],
            json!(["node", "server.js", "--stdio"])
        );
        assert_eq!(
            stored["mcp"]["servers"]["local-tools"]["env"]["LOCAL_SECRET"],
            "local-secret-value"
        );
        assert_eq!(
            stored["mcp"]["servers"]["local-tools"]["headers"]["X-Local-Token"],
            "local-header-secret"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_bridge_mcp_refresh_and_test_discover_local_stdio_tools() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-stdio-{}", now_ms()));
        let workspace = root.join("workspace");
        let tools_dir = workspace.join("tools");
        fs::create_dir_all(&tools_dir).expect("workspace tools dir");
        let fake_server = compile_fake_stdio_mcp_server(&root);
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            mcp_config: Some(stable_json_dumps(&json!({
                "mcp": {
                    "servers": {
                        "local-tools": {
                            "type": "local",
                            "command": [fake_server.to_string_lossy(), "--flag"],
                            "cwd": "tools",
                            "env": {"LOCAL_SECRET": "stdio-secret-value"},
                            "timeout_ms": 5000,
                            "enabled": true,
                        }
                    }
                }
            }))),
            ..HttpRuntimeConfig::default()
        };

        let refreshed = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/mcp?refresh=true".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(refreshed.status, 200);
        let refreshed_body = refreshed.body.expect("stdio refresh body");
        assert_local_stdio_mcp_connected(&refreshed_body);

        let tested = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/local-tools/test".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(tested.status, 200);
        let tested_body = tested.body.expect("stdio test body");
        assert_local_stdio_mcp_connected(&tested_body);

        for payload in [refreshed_body, tested_body] {
            let serialized = stable_json_dumps(&payload);
            assert!(!serialized.contains("stdio-secret-value"));
            assert!(!serialized.contains("LOCAL_SECRET"));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_bridge_mcp_local_stdio_lifecycle_start_stop_restart() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-lifecycle-{}", now_ms()));
        let workspace = root.join("workspace");
        let tools_dir = workspace.join("tools");
        fs::create_dir_all(&tools_dir).expect("workspace tools dir");
        let fake_server = compile_fake_stdio_mcp_server(&root);
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            mcp_config: Some(stable_json_dumps(&json!({
                "mcp": {
                    "servers": {
                        "local-tools": {
                            "type": "local",
                            "command": [fake_server.to_string_lossy(), "--flag"],
                            "cwd": "tools",
                            "env": {"LOCAL_SECRET": "stdio-lifecycle-secret"},
                            "timeout_ms": 5000,
                            "enabled": false,
                        }
                    }
                }
            }))),
            ..HttpRuntimeConfig::default()
        };

        let started = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/local-tools/start".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(started.status, 200);
        let started_body = started.body.expect("start body");
        assert_local_stdio_lifecycle_running(&started_body);
        let first_pid = started_body["servers"][0]["lifecycle_pid"]
            .as_u64()
            .expect("lifecycle pid");

        let status = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/mcp".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(status.status, 200);
        let status_body = status.body.expect("status body");
        assert_local_stdio_lifecycle_running(&status_body);
        assert_eq!(status_body["servers"][0]["lifecycle_pid"], json!(first_pid));

        let refreshed = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/mcp?refresh=true".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(refreshed.status, 200);
        let refreshed_body = refreshed.body.expect("refresh body");
        assert_local_stdio_lifecycle_running(&refreshed_body);
        assert_eq!(
            refreshed_body["servers"][0]["lifecycle_pid"],
            json!(first_pid)
        );

        let stopped = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/local-tools/stop".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(stopped.status, 200);
        let stopped_body = stopped.body.expect("stop body");
        assert_eq!(stopped_body["servers"][0]["status"], "stopped");
        assert_eq!(stopped_body["servers"][0]["lifecycle_status"], "stopped");
        assert_eq!(stopped_body["servers"][0]["tool_count"], 0);

        let restarted = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/local-tools/restart".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(restarted.status, 200);
        let restarted_body = restarted.body.expect("restart body");
        assert_local_stdio_lifecycle_running(&restarted_body);

        let cleanup = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/local-tools/stop".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(cleanup.status, 200);

        let serialized = stable_json_dumps(&json!([
            started_body,
            status_body,
            refreshed_body,
            restarted_body
        ]));
        assert!(!serialized.contains("stdio-lifecycle-secret"));
        assert!(!serialized.contains("LOCAL_SECRET"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_bridge_mcp_tool_call_reuses_local_stdio_lifecycle_session() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-lifecycle-call-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        let tools_dir = workspace.join("tools");
        fs::create_dir_all(&tools_dir).expect("workspace tools dir");
        let request_log = root.join("stdio-requests.log");
        let fake_server = compile_fake_stdio_mcp_server(&root);
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            mcp_config: Some(stable_json_dumps(&json!({
                "mcp": {
                    "servers": {
                        "local-tools": {
                            "type": "local",
                            "command": [fake_server.to_string_lossy(), "--flag"],
                            "cwd": "tools",
                            "env": {
                                "LOCAL_SECRET": "stdio-lifecycle-call-secret",
                                "LOCAL_REQUEST_LOG": request_log.to_string_lossy(),
                            },
                            "timeout_ms": 5000,
                            "enabled": true,
                        }
                    }
                }
            }))),
            ..HttpRuntimeConfig::default()
        };

        let started_lifecycle = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/local-tools/start".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(started_lifecycle.status, 200);
        let started_lifecycle_body = started_lifecycle.body.expect("start body");
        assert_local_stdio_lifecycle_running(&started_lifecycle_body);
        let lifecycle_pid = started_lifecycle_body["servers"][0]["lifecycle_pid"]
            .as_u64()
            .expect("lifecycle pid");

        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        );
        let session_id = created
            .get("session_id")
            .and_then(Value::as_str)
            .expect("session id");
        let turn = start_turn_payload(
            &config,
            session_id,
            &stable_json_dumps(&json!({
                "input": "call local lifecycle MCP",
                "permission": "FULL",
                "dangerously_skip_permissions": true,
                "tool_call": {
                    "call_id": "call_stdio_echo",
                    "name": "mcp_tool_local_tools_stdio_echo",
                    "input": {"text": "from-lifecycle"}
                }
            })),
        )
        .expect("mcp tool turn");
        assert_eq!(turn["status"], "completed");
        let completed = turn["events"]
            .as_array()
            .expect("events")
            .iter()
            .find(|event| {
                event["method"] == "item/toolCall/completed"
                    && event["params"]["call_id"] == "call_stdio_echo"
            })
            .expect("completed MCP call");
        assert!(
            completed["params"]["output"]
                .as_str()
                .is_some_and(|value| value.contains("stdio echo: from-lifecycle"))
        );
        assert_eq!(
            completed["params"]["metadata"]["mcp_lifecycle_reused"],
            json!(true)
        );
        assert_eq!(
            completed["params"]["metadata"]["mcp_lifecycle_pid"],
            json!(lifecycle_pid)
        );

        let status = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/mcp".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(status.status, 200);
        let status_body = status.body.expect("status body");
        assert_eq!(
            status_body["servers"][0]["lifecycle_pid"],
            json!(lifecycle_pid)
        );

        let cleanup = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/local-tools/stop".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(cleanup.status, 200);

        let log = fs::read_to_string(&request_log).expect("stdio request log");
        let entries = log.lines().collect::<Vec<_>>();
        let pids = entries
            .iter()
            .filter_map(|line| line.split_whitespace().next())
            .collect::<BTreeSet<_>>();
        assert_eq!(pids.len(), 1, "expected one stdio process, got log:\n{log}");
        assert!(pids.contains(lifecycle_pid.to_string().as_str()));
        let methods = entries
            .iter()
            .filter_map(|line| line.split_whitespace().nth(1))
            .collect::<Vec<_>>();
        assert_eq!(
            methods
                .iter()
                .filter(|method| **method == "initialize")
                .count(),
            1,
            "runtime should not short-start another stdio session:\n{log}"
        );
        assert_eq!(
            methods
                .iter()
                .filter(|method| **method == "tools/call")
                .count(),
            1,
            "expected one lifecycle tools/call:\n{log}"
        );

        let serialized = stable_json_dumps(&json!([started_lifecycle_body, turn, status_body]));
        assert!(!serialized.contains("stdio-lifecycle-call-secret"));
        assert!(!serialized.contains("LOCAL_SECRET"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_bridge_mcp_lifecycle_survives_enable_toggle() {
        let root =
            std::env::temp_dir().join(format!("openagent-mcp-lifecycle-toggle-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        let tools_dir = workspace.join("tools");
        let openagent_dir = workspace.join(".openagent");
        fs::create_dir_all(&tools_dir).expect("workspace tools dir");
        fs::create_dir_all(&openagent_dir).expect("workspace .openagent dir");
        let request_log = root.join("stdio-requests.log");
        let fake_server = compile_fake_stdio_mcp_server(&root);
        fs::write(
            openagent_dir.join("mcp.json"),
            stable_json_dumps(&json!({
                "mcp": {
                    "servers": {
                        "local-tools": {
                            "type": "local",
                            "command": [fake_server.to_string_lossy(), "--flag"],
                            "cwd": "tools",
                            "env": {
                                "LOCAL_SECRET": "stdio-lifecycle-toggle-secret",
                                "LOCAL_REQUEST_LOG": request_log.to_string_lossy(),
                            },
                            "timeout_ms": 5000,
                            "enabled": false,
                        }
                    }
                }
            })),
        )
        .expect("write mcp config");
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };

        let initial = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/mcp?refresh=true".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(initial.status, 200);
        let initial_body = initial.body.expect("initial mcp body");
        assert_eq!(initial_body["servers"][0]["enabled"], false);
        assert_eq!(initial_body["servers"][0]["lifecycle_status"], "stopped");
        assert!(
            !request_log.exists(),
            "refresh should not start disabled local MCP"
        );

        let started_lifecycle = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/local-tools/start".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(started_lifecycle.status, 200);
        let started_lifecycle_body = started_lifecycle.body.expect("start body");
        assert_local_stdio_lifecycle_running(&started_lifecycle_body);
        assert_eq!(started_lifecycle_body["servers"][0]["enabled"], false);
        let lifecycle_pid = started_lifecycle_body["servers"][0]["lifecycle_pid"]
            .as_u64()
            .expect("lifecycle pid");

        let enabled = route_http_request(
            &HttpRequest {
                method: "PATCH".to_string(),
                path: "/api/mcp/servers/local-tools".to_string(),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({"enabled": true})),
            },
            &config,
        );
        assert_eq!(enabled.status, 200);
        let enabled_body = enabled.body.expect("enabled body");
        assert_eq!(enabled_body["servers"][0]["enabled"], true);
        assert_eq!(enabled_body["servers"][0]["lifecycle_status"], "running");
        assert_eq!(
            enabled_body["servers"][0]["lifecycle_pid"],
            json!(lifecycle_pid)
        );

        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        );
        let session_id = created
            .get("session_id")
            .and_then(Value::as_str)
            .expect("session id");
        let turn = start_turn_payload(
            &config,
            session_id,
            &stable_json_dumps(&json!({
                "input": "call local lifecycle MCP after enable",
                "permission": "FULL",
                "dangerously_skip_permissions": true,
                "tool_call": {
                    "call_id": "call_stdio_echo_after_enable",
                    "name": "mcp_tool_local_tools_stdio_echo",
                    "input": {"text": "after-enable"}
                }
            })),
        )
        .expect("mcp tool turn");
        assert_eq!(turn["status"], "completed");
        let completed = turn["events"]
            .as_array()
            .expect("events")
            .iter()
            .find(|event| {
                event["method"] == "item/toolCall/completed"
                    && event["params"]["call_id"] == "call_stdio_echo_after_enable"
            })
            .expect("completed MCP call");
        assert!(
            completed["params"]["output"]
                .as_str()
                .is_some_and(|value| value.contains("stdio echo: after-enable"))
        );
        assert_eq!(
            completed["params"]["metadata"]["mcp_lifecycle_reused"],
            json!(true)
        );
        assert_eq!(
            completed["params"]["metadata"]["mcp_lifecycle_pid"],
            json!(lifecycle_pid)
        );

        let cleanup = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/local-tools/stop".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(cleanup.status, 200);

        let log = fs::read_to_string(&request_log).expect("stdio request log");
        let entries = log.lines().collect::<Vec<_>>();
        let pids = entries
            .iter()
            .filter_map(|line| line.split_whitespace().next())
            .collect::<BTreeSet<_>>();
        assert_eq!(pids.len(), 1, "expected one stdio process, got log:\n{log}");
        assert!(pids.contains(lifecycle_pid.to_string().as_str()));
        let methods = entries
            .iter()
            .filter_map(|line| line.split_whitespace().nth(1))
            .collect::<Vec<_>>();
        assert_eq!(
            methods
                .iter()
                .filter(|method| **method == "initialize")
                .count(),
            1,
            "enabled toggle should not short-start another stdio session:\n{log}"
        );
        assert_eq!(
            methods
                .iter()
                .filter(|method| **method == "tools/call")
                .count(),
            1,
            "expected one lifecycle tools/call:\n{log}"
        );

        let serialized = stable_json_dumps(&json!([started_lifecycle_body, enabled_body, turn]));
        assert!(!serialized.contains("stdio-lifecycle-toggle-secret"));
        assert!(!serialized.contains("LOCAL_SECRET"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_bridge_mcp_refresh_discovers_tools_without_leaking_endpoint_secret() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock mcp bind");
        let port = listener.local_addr().expect("mock mcp addr").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("mock mcp accept");
            let request = read_http_request(&mut stream).expect("mock mcp request");
            assert_eq!(request.method, "POST");
            assert!(
                request.path.starts_with("/mcp?token=refresh-secret"),
                "unexpected MCP request path: {}",
                request.path
            );
            let request_json =
                serde_json::from_str::<Value>(&request.body).expect("mock mcp json request");
            assert_eq!(request_json["method"], "tools/list");
            let response_body = serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": request_json.get("id").cloned().unwrap_or(Value::Null),
                "result": {
                    "tools": [
                        {
                            "name": "echo",
                            "title": "Echo",
                            "description": "Echo input",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "text": {"type": "string"}
                                }
                            }
                        }
                    ]
                }
            }))
            .expect("mock mcp response json");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("mock mcp response write");
        });

        let workspace = std::env::temp_dir().join(format!("openagent-mcp-refresh-{}", now_ms()));
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            mcp_config: Some(stable_json_dumps(&json!({
                "mcp": {
                    "servers": {
                        "remote-tools": {
                            "url": format!("http://127.0.0.1:{port}/mcp?token=refresh-secret"),
                            "transport": "http",
                            "timeout_ms": 2000,
                            "enabled": true,
                        }
                    }
                }
            }))),
            ..HttpRuntimeConfig::default()
        };
        let response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/mcp?refresh=true".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(response.status, 200);
        let body = response.body.expect("mcp refresh body");
        assert_eq!(body["configured"], true);
        assert_eq!(body["enabled"], true);
        assert_eq!(body["status"], "connected");
        assert_eq!(body["server_count"], 1);
        assert_eq!(body["tool_count"], 1);

        let servers = body["servers"].as_array().expect("servers");
        let remote = servers.first().expect("remote server");
        assert_eq!(remote["name"], "remote-tools");
        assert_eq!(remote["selected_transport"], "http");
        assert_eq!(remote["status"], "connected");
        assert_eq!(remote["tool_count"], 1);
        let tools = remote["tools"].as_array().expect("tools");
        assert_eq!(tools[0]["name"], "mcp_tool_remote_tools_echo");
        assert_eq!(tools[0]["original_name"], "echo");

        let serialized = stable_json_dumps(&body);
        assert!(!serialized.contains("refresh-secret"));
        assert!(!serialized.contains("token="));
        server.join().expect("mock mcp join");
        let _ = fs::remove_dir_all(workspace);
    }

    fn assert_local_stdio_mcp_connected(body: &Value) {
        assert_eq!(body["configured"], true);
        assert_eq!(body["enabled"], true);
        assert_eq!(body["server_count"], 1);
        assert_eq!(body["tool_count"], 1);
        assert_eq!(body["status"], "connected");
        let local = body["servers"].as_array().expect("servers")[0].clone();
        assert_eq!(local["name"], "local-tools");
        assert_eq!(local["type"], "local");
        assert_eq!(local["transport"], "stdio");
        assert_eq!(local["selected_transport"], "stdio");
        assert_eq!(local["status"], "connected");
        assert_eq!(local["tool_count"], 1);
        assert_eq!(local["args_count"], 1);
        assert_eq!(local["cwd_configured"], true);
        assert_eq!(local["env_count"], 1);
        let tool = local["tools"].as_array().expect("stdio tools")[0].clone();
        assert_eq!(tool["name"], "mcp_tool_local_tools_stdio_echo");
        assert_eq!(tool["original_name"], "stdio_echo");
        assert!(
            tool["description"]
                .as_str()
                .is_some_and(|description| description.contains("cwd=tools"))
        );
    }

    fn assert_local_stdio_lifecycle_running(body: &Value) {
        assert_eq!(body["configured"], true);
        assert_eq!(body["server_count"], 1);
        assert_eq!(body["tool_count"], 1);
        let local = body["servers"].as_array().expect("servers")[0].clone();
        assert_eq!(local["name"], "local-tools");
        assert_eq!(local["type"], "local");
        assert_eq!(local["selected_transport"], "stdio");
        assert_eq!(local["status"], "connected");
        assert_eq!(local["tool_count"], 1);
        assert_eq!(local["lifecycle_status"], "running");
        assert!(local["lifecycle_pid"].as_u64().is_some());
        assert!(local["lifecycle_started_at"].as_f64().is_some());
        assert!(local["lifecycle_last_refreshed_at"].as_f64().is_some());
        assert_eq!(local["lifecycle_tool_count"], 1);
        assert_eq!(local["tools"][0]["name"], "mcp_tool_local_tools_stdio_echo");
    }

    fn compile_fake_stdio_mcp_server(root: &Path) -> PathBuf {
        fs::create_dir_all(root).expect("fake stdio root");
        let source = root.join("fake_stdio_mcp.rs");
        let binary = root.join(if cfg!(windows) {
            "fake_stdio_mcp.exe"
        } else {
            "fake_stdio_mcp"
        });
        fs::write(&source, FAKE_STDIO_MCP_SERVER).expect("write fake stdio source");
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
        let output = Command::new(rustc)
            .arg("--edition=2021")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("compile fake stdio server");
        assert!(
            output.status.success(),
            "fake stdio server compile failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        binary
    }

    const FAKE_STDIO_MCP_SERVER: &str = r#"
use std::env;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process;

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout();
    while let Some(body) = read_frame(&mut reader)? {
        log_request(&body);
        if body.contains("\"method\":\"initialize\"") {
            write_response(
                &mut stdout,
                &body,
                "{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"fake-stdio\",\"version\":\"1.0\"}}",
            )?;
        } else if body.contains("\"method\":\"tools/list\"") {
            let arg_ok = env::args().any(|arg| arg == "--flag");
            let env_ok = env::var("LOCAL_SECRET").is_ok();
            let cwd = env::current_dir()
                .ok()
                .and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string()))
                .unwrap_or_else(|| "unknown".to_string());
            let tool_name = if arg_ok && env_ok && cwd == "tools" {
                "stdio_echo"
            } else {
                "stdio_misconfigured"
            };
            let result = format!(
                "{{\"tools\":[{{\"name\":\"{}\",\"title\":\"Stdio Echo\",\"description\":\"local stdio discovery reached cwd={} arg_ok={} env_ok={}\",\"inputSchema\":{{\"type\":\"object\",\"properties\":{{\"text\":{{\"type\":\"string\"}}}}}}}}]}}",
                tool_name, cwd, arg_ok, env_ok
            );
            write_response(&mut stdout, &body, &result)?;
        } else if body.contains("\"method\":\"tools/call\"") {
            let text = extract_text_argument(&body);
            let result = format!(
                "{{\"content\":[{{\"type\":\"text\",\"text\":\"stdio echo: {}\"}}]}}",
                escape_json_string(&text)
            );
            write_response(&mut stdout, &body, &result)?;
        } else if body.contains("\"method\":\"shutdown\"") {
            write_response(&mut stdout, &body, "{}")?;
        } else if body.contains("\"method\":\"exit\"") {
            break;
        }
    }
    Ok(())
}

fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length = None::<usize>;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
    }
    let Some(length) = content_length else {
        return Ok(None);
    };
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).to_string()))
}

fn write_response<W: Write>(writer: &mut W, request: &str, result: &str) -> io::Result<()> {
    let id = extract_id(request);
    let body = format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}", id, result);
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()
}

fn log_request(request: &str) {
    let Ok(path) = env::var("LOCAL_REQUEST_LOG") else {
        return;
    };
    let method = extract_method(request);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} {}", process::id(), method);
    }
}

fn extract_method(request: &str) -> String {
    if let Some(start) = request.find("\"method\":\"") {
        let rest = &request[start + 10..];
        if let Some(end) = rest.find('"') {
            return rest[..end].to_string();
        }
    }
    "unknown".to_string()
}

fn extract_id(request: &str) -> String {
    if let Some(start) = request.find("\"id\":\"") {
        let rest = &request[start + 6..];
        if let Some(end) = rest.find('"') {
            return format!("\"{}\"", &rest[..end]);
        }
    }
    if let Some(start) = request.find("\"id\":") {
        let rest = &request[start + 5..];
        let end = rest
            .find(|character| character == ',' || character == '}')
            .unwrap_or(rest.len());
        return rest[..end].trim().to_string();
    }
    "null".to_string()
}

fn extract_text_argument(request: &str) -> String {
    if let Some(start) = request.find("\"text\":\"") {
        let rest = &request[start + 8..];
        if let Some(end) = rest.find('"') {
            return rest[..end].replace("\\\"", "\"");
        }
    }
    String::new()
}

fn escape_json_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
"#;

    #[test]
    fn app_bridge_mcp_server_test_discovers_disabled_server_tools() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock mcp bind");
        let port = listener.local_addr().expect("mock mcp addr").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("mock mcp accept");
            let request = read_http_request(&mut stream).expect("mock mcp request");
            assert_eq!(request.method, "POST");
            assert!(
                request.path.starts_with("/mcp?token=test-secret"),
                "unexpected MCP request path: {}",
                request.path
            );
            let request_json =
                serde_json::from_str::<Value>(&request.body).expect("mock mcp json request");
            assert_eq!(request_json["method"], "tools/list");
            let response_body = serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": request_json.get("id").cloned().unwrap_or(Value::Null),
                "result": {
                    "tools": [
                        {
                            "name": "lookup",
                            "title": "Lookup",
                            "description": "Lookup docs",
                            "inputSchema": {"type": "object"}
                        }
                    ]
                }
            }))
            .expect("mock mcp response json");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("mock mcp response write");
        });

        let workspace = std::env::temp_dir().join(format!("openagent-mcp-test-{}", now_ms()));
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            mcp_config: Some(stable_json_dumps(&json!({
                "mcp": {
                    "servers": {
                        "remote-tools": {
                            "url": format!("http://127.0.0.1:{port}/mcp?token=test-secret"),
                            "transport": "http",
                            "timeout_ms": 2000,
                            "enabled": false,
                        }
                    }
                }
            }))),
            ..HttpRuntimeConfig::default()
        };
        let response = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/mcp/servers/remote-tools/test".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(response.status, 200);
        let body = response.body.expect("mcp test body");
        assert_eq!(body["configured"], true);
        assert_eq!(body["enabled"], false);
        assert_eq!(body["status"], "disabled");
        assert_eq!(body["server_count"], 1);
        assert_eq!(body["tool_count"], 1);
        let remote = body["servers"].as_array().expect("servers")[0].clone();
        assert_eq!(remote["name"], "remote-tools");
        assert_eq!(remote["enabled"], false);
        assert_eq!(remote["selected_transport"], "http");
        assert_eq!(remote["status"], "connected");
        assert_eq!(remote["tool_count"], 1);
        assert_eq!(remote["tools"][0]["name"], "mcp_tool_remote_tools_lookup");
        assert_eq!(remote["tools"][0]["original_name"], "lookup");

        let serialized = stable_json_dumps(&body);
        assert!(!serialized.contains("test-secret"));
        assert!(!serialized.contains("token="));
        server.join().expect("mock mcp join");
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn app_bridge_permission_approval_round_trip_executes_allowed_tool() {
        let root = std::env::temp_dir().join(format!("openagent-http-permission-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let _ = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .arg("init")
            .output();
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        );
        let session_id = created
            .get("session_id")
            .and_then(Value::as_str)
            .expect("session id");
        let started = start_turn_payload(
            &config,
            session_id,
            &stable_json_dumps(&json!({
                "input": "run approved command",
                "permission": "PLAN_ONLY",
                "tool_call": {
                    "call_id": "call_bash",
                    "name": "bash",
                    "input": {"command": "printf approved"}
                }
            })),
        )
        .expect("start turn");
        assert_eq!(started["status"], "waiting_approval");
        let approval = started["events"]
            .as_array()
            .expect("events")
            .iter()
            .find(|event| event["method"] == "turn/approval_requested")
            .and_then(|event| event["params"]["approval"].as_object())
            .cloned()
            .expect("approval");
        let turn_id = approval
            .get("turn_id")
            .and_then(Value::as_str)
            .expect("turn id");
        let request_id = approval
            .get("request_id")
            .and_then(Value::as_str)
            .expect("request id");
        let resolved = respond_approval_payload(
            &config,
            &format!("/api/turns/{turn_id}/approvals/{request_id}"),
            &stable_json_dumps(&json!({"action": "allow", "scope": "once"})),
        )
        .expect("resolve approval");
        let events = resolved["events"].as_array().expect("resolved events");
        assert!(events.iter().any(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["output"] == "approved"
        }));
        assert!(events.iter().any(|event| {
            event["method"] == "turn/completed" && event["params"]["status"] == "completed"
        }));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint() {
        let root = std::env::temp_dir().join(format!("openagent-http-trust-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            serve_static: false,
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        );
        let session_id = created
            .get("session_id")
            .and_then(Value::as_str)
            .expect("session id");
        let file_path = workspace.join("notes.txt");

        let started = start_turn_payload(
            &config,
            session_id,
            &stable_json_dumps(&json!({
                "input": "write notes with approval",
                "permission": "PLAN_ONLY",
                "tool_call": {
                    "call_id": "call_write_notes",
                    "name": "write",
                    "input": {"file_path": "notes.txt", "content": "alpha\n"}
                }
            })),
        )
        .expect("start turn");
        assert_eq!(started["status"], "waiting_approval");

        let approvals_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/approvals".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let approvals = approvals_response.body.expect("approvals body");
        let request_id = approvals["approvals"][0]["request_id"]
            .as_str()
            .expect("request id")
            .to_string();
        assert_eq!(approvals["count"], json!(1));
        assert_eq!(approvals["approvals"][0]["session_id"], json!(session_id));

        let approved_response = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: format!("/api/approvals/{request_id}"),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({"action": "allow", "scope": "once"})),
            },
            &config,
        );
        let approved = approved_response.body.expect("approved body");
        assert!(approved["events"].as_array().is_some_and(|events| {
            events
                .iter()
                .any(|event| event["method"] == "item/toolCall/completed")
        }));
        assert_eq!(fs::read_to_string(&file_path).expect("file"), "alpha\n");
        let approvals_after_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/approvals".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let approvals_after = approvals_after_response.body.expect("approvals after body");
        assert_eq!(approvals_after["count"], json!(0));

        let messages_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: format!("/api/sessions/{session_id}/messages?limit=20"),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let messages = messages_response.body.expect("messages body");
        assert_eq!(messages["session_id"], json!(session_id));
        assert!(messages["message_v2_count"].as_u64().unwrap_or_default() >= 3);
        let approval_messages_v2 = messages["messages_v2"].as_array().expect("messages v2");
        let roles = approval_messages_v2
            .iter()
            .filter_map(|message| message["info"]["role"].as_str())
            .collect::<Vec<_>>();
        assert!(roles.contains(&"user"));
        assert!(roles.contains(&"tool"));
        assert!(roles.contains(&"assistant"));
        assert!(approval_messages_v2.iter().any(|message| {
            message["parts"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part["content"] == "write notes with approval")
            })
        }));
        let approval_assistant = approval_messages_v2
            .iter()
            .find(|message| message["info"]["role"] == "assistant")
            .expect("approval assistant message");
        let approval_assistant_parts = approval_assistant["parts"]
            .as_array()
            .expect("approval assistant parts");
        let approval_assistant_part_ids = approval_assistant_parts
            .iter()
            .filter_map(|part| part["id"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            approval_assistant_part_ids.len(),
            approval_assistant_parts.len()
        );
        assert!(
            approval_assistant_parts
                .iter()
                .any(|part| part["kind"] == "patch")
        );
        assert!(
            approval_assistant_parts.iter().any(|part| {
                part["kind"] == "context" && part["content"]["kind"] == "checkpoint"
            })
        );
        assert!(approval_assistant_parts.iter().any(|part| {
            part["kind"] == "approval"
                && part["status"] == "completed"
                && part["content"]["status"] == "allowed"
                && part["content"]["resolution"]["action"] == "allow"
        }));
        let approval_checkpoints =
            session_checkpoints_payload(&config, session_id).expect("approval checkpoints");
        assert!(
            approval_checkpoints["checkpoints"]
                .as_array()
                .expect("approval checkpoints list")
                .iter()
                .any(|checkpoint| checkpoint["kind"] == "step_end")
        );

        let diff = session_diff_payload(&config, session_id).expect("diff");
        assert_eq!(diff["undo_count"], json!(1));
        assert!(
            diff["latest"]["diff"]
                .as_str()
                .is_some_and(|value| value.contains("+alpha"))
        );

        let direct = start_turn_payload(
            &config,
            session_id,
            &stable_json_dumps(&json!({
                "input": "update notes directly",
                "permission": "FULL",
                "tool_call": {
                    "call_id": "call_bash_beta",
                    "name": "bash",
                    "input": {"command": "printf 'beta\\n' > notes.txt"}
                }
            })),
        )
        .expect("direct write");
        assert_eq!(direct["status"], "completed");
        assert_eq!(fs::read_to_string(&file_path).expect("file"), "beta\n");

        let direct_messages_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: format!("/api/sessions/{session_id}/messages?limit=20"),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let direct_messages = direct_messages_response.body.expect("direct messages body");
        let direct_roles = direct_messages["messages_v2"]
            .as_array()
            .expect("direct messages v2")
            .iter()
            .filter_map(|message| message["info"]["role"].as_str())
            .collect::<Vec<_>>();
        assert!(direct_roles.contains(&"user"));
        assert!(direct_roles.contains(&"tool"));
        assert!(direct_roles.contains(&"assistant"));
        let direct_messages_v2 = direct_messages["messages_v2"]
            .as_array()
            .expect("direct messages v2");
        assert!(direct_messages_v2.iter().any(|message| {
            message["info"]["role"] == "tool"
                && message["parts"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|part| part["kind"] == "tool"))
        }));
        let direct_assistant = direct_messages_v2
            .iter()
            .find(|message| message["info"]["role"] == "assistant")
            .expect("assistant message");
        let direct_assistant_parts = direct_assistant["parts"]
            .as_array()
            .expect("assistant parts");
        let assistant_part_ids = direct_assistant_parts
            .iter()
            .filter_map(|part| part["id"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(assistant_part_ids.len(), direct_assistant_parts.len());
        assert!(
            direct_assistant_parts
                .iter()
                .any(|part| part["kind"] == "patch")
        );
        assert!(
            direct_assistant_parts.iter().any(|part| {
                part["kind"] == "context" && part["content"]["kind"] == "checkpoint"
            })
        );

        let all_events_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/events?last_event_id=0".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let all_events_body = all_events_response.body_text.expect("events body");
        let all_events = parse_sse_response_lines(&all_events_body.lines().collect::<Vec<_>>())
            .expect("parse events");
        assert!(
            all_events
                .iter()
                .any(|event| event["method"] == "turn/approval_requested")
        );
        assert!(
            all_events
                .iter()
                .any(|event| event["method"] == "turn/approval_resolved")
        );
        assert!(
            all_events
                .iter()
                .any(|event| event["method"] == "item/toolCall/completed")
        );
        let last_sequence = all_events
            .last()
            .and_then(|event| event["global_sequence"].as_u64())
            .expect("last global sequence");
        let resumed_events_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: format!("/api/events?last_event_id={}", last_sequence - 1),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let resumed_events_body = resumed_events_response
            .body_text
            .expect("resumed events body");
        let resumed_events =
            parse_sse_response_lines(&resumed_events_body.lines().collect::<Vec<_>>())
                .expect("parse resumed events");
        assert_eq!(resumed_events.len(), 1);
        assert_eq!(resumed_events[0]["global_sequence"], json!(last_sequence));

        let git_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/git".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let git = git_response.body.expect("git body");
        if git["is_repo"] == json!(true) {
            assert!(git["change_count"].as_u64().unwrap_or_default() >= 1);
            assert!(
                git["changes"]
                    .as_array()
                    .expect("git changes")
                    .iter()
                    .any(|change| change["path"] == "notes.txt")
            );
        } else {
            assert!(git["error"].as_str().is_some_and(|value| !value.is_empty()));
        }

        let checkpoints_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: format!("/api/sessions/{session_id}/checkpoints"),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let checkpoints = checkpoints_response.body.expect("checkpoints body");
        assert!(checkpoints["count"].as_u64().unwrap_or_default() >= 2);
        let start_checkpoint = checkpoints["checkpoints"]
            .as_array()
            .expect("checkpoints")
            .iter()
            .find(|checkpoint| checkpoint["kind"] == "step_start")
            .and_then(|checkpoint| checkpoint["checkpoint_id"].as_str())
            .expect("step_start checkpoint")
            .to_string();

        let restore_response = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: format!("/api/sessions/{session_id}/checkpoints/{start_checkpoint}/restore"),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let restored = restore_response.body.expect("restore body");
        assert_eq!(restored["status"], json!("restored"));
        assert_eq!(fs::read_to_string(&file_path).expect("file"), "alpha\n");
        assert!(restored["events"].as_array().is_some_and(|events| {
            events
                .iter()
                .any(|event| event["method"] == "checkpoint/restored")
        }));
        let events_after_restore_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/events?last_event_id=0".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let events_after_restore_body = events_after_restore_response
            .body_text
            .expect("events after restore body");
        let events_after_restore =
            parse_sse_response_lines(&events_after_restore_body.lines().collect::<Vec<_>>())
                .expect("parse events after restore");
        assert!(
            events_after_restore
                .iter()
                .any(|event| event["method"] == "checkpoint/restored")
        );

        let files_response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/files?path=notes.txt&content=true".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        let files = files_response.body.expect("files body");
        assert_eq!(files["path"], json!("notes.txt"));
        assert_eq!(files["content"], json!("alpha\n"));
        assert_eq!(files["entries"][0]["kind"], json!("file"));

        let _ = fs::remove_dir_all(root);
    }
}

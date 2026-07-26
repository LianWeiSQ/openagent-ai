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

use openagent_bridge_server::{
    approval_response_payload, control_next_payload, parse_turn_approval_path,
    parse_turn_question_reply_path, question_dismiss_payload, question_reply_payload,
    record_control_response_payload, tui_control_request_for_path,
};
use openagent_core::{
    ContextAttachment, ContextAttachmentKind, ContextBudgetOptions, ContextCheckpoint,
    ContextFailure, ContextFailureCode, ContextItem, ContextPack, ContextPackBuildOptions,
    ContextPackBuilder, ContextPackInput, ContextPackPerformance, ContextPackReceipt,
    ContextPackTraceEntry, ContextSystemDiagnostics, ContextSystemSources, ContextTodo,
    ContextWorkState, DurableGoal, DurableGoalStatus, DurablePlan, DurablePlanStatus,
    PermissionManager, SkillDocument, SkillRegistry, SkillRegistryOptions,
    context_pack_build_options_for_model, load_context_budget_options, materialize_context_history,
    permission_rule, skill_document_model_invocable, skill_info_model_invocable,
    tool_manifest_context_item,
};
use openagent_lsp::{
    LspOperation, LspQuery, lsp_doctor, lsp_status, operation_from_str, query_workspace,
};
use openagent_mcp::{
    McpBridgeOutput, McpServerType, McpTransport, RemoteMcpManager, RemoteMcpServerConfig,
    RemoteMcpToolDescriptor, StdioMcpSession, bridge_tool_output,
    build_tool_descriptors_from_values, discover_mcp_server_tools, load_mcp_config,
    load_mcp_config_from_value, mcp_json_rpc, mcp_tool_definition, normalize_tool_call_result,
    unavailable_tool_result,
};
use openagent_protocol::{
    ChatMessage, MessagePartKind, MessageStatus, Model, PermissionRuleset, Role, ToolCall,
    ToolResult, ToolSchema, Usage, WorkState, render_work_state,
};
use openagent_provider::{
    AnthropicLanguageModelConfig, GeminiLanguageModelConfig, OpenAiLanguageModelConfig,
    ProviderCapability, ProviderStreamEvent, ToolCallDialect, apply_tool_call_dialect,
    build_anthropic_payload_with_policy, build_gemini_payload,
    build_openai_chat_payload_with_policy, build_openai_responses_payload_with_policy,
    default_env_mapping, negotiate_tool_call_policy, normalize_anthropic_events,
    normalize_anthropic_response, normalize_gemini_events, normalize_openai_chat_response,
    normalize_openai_chat_sse_chunks, normalize_openai_responses_response,
    normalize_openai_responses_stream_events, normalize_provider, openagent_context_model,
    openagent_text_model_supported, provider_capabilities, provider_default_base_url,
    provider_default_model, provider_label, provider_requires_api_key, summarize_http_error_body,
    tool_call_dialect_from_options, tool_call_policy_from_options,
};
use openagent_session::{
    FileSessionStore, Session, SessionCheckpointRecord, SessionEventOptions, SessionPartOptions,
    SessionStatus, StartRunOptions, TodoItem as SessionTodoItem,
};
use openagent_tools::{
    DEFAULT_BUILD_AGENT_PROMPT, SessionRunnerFacade, SkillPermissionRule, TASK_TOOL_ID,
    TaskPermissionRule, TaskSubagentDescriptor, TaskSubagentRoute, ToolContext, Toolkit,
    builtin_agent_profile_specs, fork_skill_task_from_input, parse_agent_profile_schema,
    prepare_isolated_workspace, register_task_tool, resolve_path_in_root,
    select_task_subagent_for_prompt, skill_is_visible, task_subagent_is_visible,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

mod bridge_routes;
mod capability_runtime;
mod git_runtime;
mod mcp_runtime;
mod performance_runtime;
mod plugin_runtime;
mod provider_runtime;
mod storage_runtime;
mod terminal_runtime;
mod turn_runtime;

use bridge_routes::*;
pub use bridge_routes::{
    CliRunResult, HttpResponseSpec, command_text_from_args, docker_smoke_command, dockerfile_lines,
    emit_bridge_events, format_http_error, health_payload, parse_cli_args, parse_sse_data,
    parse_sse_response_lines, route_health, route_options, route_unauthorized, route_unknown,
    run_cli,
};
use capability_runtime::*;
use git_runtime::*;
use mcp_runtime::*;
use performance_runtime::*;
use plugin_runtime::*;
use provider_runtime::*;
use storage_runtime::*;
use terminal_runtime::*;
use turn_runtime::*;

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8787;
pub const DEFAULT_CORS_ORIGINS: &str =
    "tauri://localhost,http://tauri.localhost,http://127.0.0.1:5173,http://localhost:5173";
const BRIDGE_EVENTS_FILE: &str = "bridge_events.jsonl";
const LEGACY_APP_EVENTS_FILE: &str = "app_events.jsonl";
const BRIDGE_PROTOCOL_VERSION: u64 = 1;
const BRIDGE_EVENT_SCHEMA_VERSION: &str = "openagent.bridge_event.v1";
const TUI_CONTROL_QUEUE_FILE: &str = "tui_control_queue.json";
const TUI_CONTROL_RESPONSES_FILE: &str = "tui_control_responses.jsonl";
const FILE_CHANGE_UNDO_STACK_KEY: &str = "file_change_undo_stack";
const FILE_CHANGE_REDO_STACK_KEY: &str = "file_change_redo_stack";
const FILE_CHANGE_LATEST_KEY: &str = "latest_file_change";
const FINAL_RESULT_METADATA_KEY: &str = "latest_final_result";
const FINAL_RESULT_SCHEMA_VERSION: &str = "openagent.final_result.v1";
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
const TURN_INTERRUPTED_ERROR: &str = "turn interrupted";
const TURN_JOB_INDEX_FILE: &str = ".openagent-runtime/turn_jobs.json";
const TURN_QUEUE_DIR: &str = ".openagent-runtime/turn_queue";
const TURN_QUEUE_LEASE_DIR: &str = ".openagent-runtime/turn_queue_leases";
const TURN_RETRY_PAYLOAD_DIR: &str = ".openagent-runtime/turn_retry";
const TURN_JOB_INDEX_SCHEMA_VERSION: u64 = 1;
const TURN_QUEUE_PAYLOAD_SCHEMA_VERSION: u64 = 1;
const TURN_QUEUE_LEASE_SCHEMA_VERSION: u64 = 1;
const TURN_RETRY_PAYLOAD_SCHEMA_VERSION: u64 = 1;
const INTERNAL_TURN_RETRY_KEY: &str = "_openagent_retry";
const DEFAULT_PROVIDER_REQUEST_RETRIES: u64 = 1;
const MAX_PROVIDER_REQUEST_RETRIES: u64 = 3;
const MAX_PROVIDER_FALLBACK_MODELS: usize = 3;
const DEFAULT_MANUAL_TURN_RETRIES: u64 = 3;
const MAX_MANUAL_TURN_RETRIES: u64 = 5;
const CONTEXT_DIAGNOSTICS_SCHEMA_VERSION: &str = "openagent.context_diagnostics.v1";
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
pub fn bridge_server_crate_name() -> &'static str {
    openagent_bridge_server::crate_name()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HttpRuntimeConfig {
    pub host: String,
    pub port: u16,
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
            workspace: None,
            session_store_root: None,
            mcp_config: None,
            auth_token: None,
            auth_username: None,
            auth_password: None,
            cors_origin: DEFAULT_CORS_ORIGINS.to_string(),
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

// bridge_routes implementation lives in `bridge_routes.rs`.
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

// mcp_runtime implementation lives in `mcp_runtime.rs`.
fn runtime_text_model_supported(model: &str) -> bool {
    openagent_text_model_supported(model)
}

fn runtime_image_model_supported(model: &str) -> bool {
    matches!(model, "gpt-image-1.5" | "gpt-image-2")
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

#[derive(Clone, Copy)]
struct RuntimeProviderSources<'a> {
    payload: Option<&'a Value>,
    session: Option<&'a Session>,
    managed_record: Option<&'a Value>,
    auth_record: Option<&'a Value>,
}

fn runtime_provider_config(
    managed_state_path: Option<&Path>,
    provider: Option<&str>,
    payload: Option<&Value>,
    session: Option<&Session>,
) -> Result<RuntimeProviderConfig, String> {
    let provider = normalize_provider(Some(&active_provider_id(provider)))?;
    let env = default_env_mapping(&provider)?;
    let auth_record = runtime_auth_record(&provider);
    let managed_record = managed_provider_record(managed_state_path);
    let sources = RuntimeProviderSources {
        payload,
        session,
        managed_record: managed_record.as_ref(),
        auth_record: auth_record.as_ref(),
    };
    let api_key_env = env
        .get("api_key")
        .cloned()
        .unwrap_or_else(|| "OPENAI_API_KEY".to_string());
    let api_key = runtime_provider_field(
        "api_key",
        &api_key_env,
        &["OPENAGENT_API_KEY"],
        None,
        sources,
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
        sources,
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
        sources,
    )
    .expect("model has default");
    let model = if model.source == "session" && model.value == default_model_id() {
        runtime_provider_field(
            "model",
            env.get("model")
                .map(String::as_str)
                .unwrap_or("OPENAI_MODEL"),
            &["OPENAGENT_MODEL"],
            provider_default_model(&provider)
                .ok()
                .flatten()
                .or_else(|| Some(default_model_id())),
            RuntimeProviderSources {
                session: None,
                ..sources
            },
        )
        .expect("model has default")
    } else {
        model
    };
    let wire_api = runtime_provider_field(
        "wire_api",
        env.get("wire_api")
            .map(String::as_str)
            .unwrap_or("OPENAI_WIRE_API"),
        &["OPENAGENT_WIRE_API"],
        Some(match provider.as_str() {
            "anthropic" => "messages".to_string(),
            "gemini" | "google" => "generate_content".to_string(),
            _ => "responses".to_string(),
        }),
        sources,
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
    sources: RuntimeProviderSources<'_>,
) -> Option<RuntimeProviderField> {
    sources
        .payload
        .and_then(|payload| payload.get(field))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| RuntimeProviderField {
            value: value.to_string(),
            source: "payload".to_string(),
        })
        .or_else(|| {
            sources
                .session
                .and_then(|session| session.metadata.get(field))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| RuntimeProviderField {
                    value: value.to_string(),
                    source: "session".to_string(),
                })
        })
        .or_else(|| {
            sources
                .managed_record
                .and_then(|record| record.get(field))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| RuntimeProviderField {
                    value: value.to_string(),
                    source: "bridge_private_state".to_string(),
                })
        })
        .or_else(|| env_field(provider_env_name, "env"))
        .or_else(|| {
            generic_env_names
                .iter()
                .find_map(|name| env_field(name, "env"))
        })
        .or_else(|| {
            sources
                .auth_record
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
    builtin_agent_profile_specs()
        .into_iter()
        .filter(|profile| runtime_is_subagent_mode(profile.mode))
        .map(|profile| RuntimeSubagentProfile {
            id: profile.id.to_string(),
            name: profile.name.to_string(),
            description: profile.description.to_string(),
            mode: profile.mode.to_string(),
            permission: profile.permission,
            task_permissions: Vec::new(),
            skills: Vec::new(),
            skill_roots: Vec::new(),
            skill_permissions: Vec::new(),
            prompt: profile.prompt.trim_start_matches('\u{feff}').to_string(),
            tools: profile
                .tools
                .iter()
                .map(|item| (*item).to_string())
                .collect(),
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
        })
        .collect()
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
            .unwrap_or(DEFAULT_BUILD_AGENT_PROMPT)
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
    let profile_id = session
        .metadata
        .get("agent_profile")
        .and_then(|profile| profile.get("id"))
        .and_then(Value::as_str)
        .or_else(|| session.metadata.get("agent").and_then(Value::as_str));
    if let Some(profile) = profile_id.and_then(|id| runtime_agent_profile(id, &session.directory)) {
        return Some(profile);
    }
    if let Some(profile_value) = session.metadata.get("agent_profile") {
        let fallback_id = profile_value
            .get("id")
            .and_then(Value::as_str)
            .or(profile_id)
            .unwrap_or("agent");
        if let Some(profile) = runtime_agent_profile_from_value(profile_value, fallback_id, None) {
            return Some(profile);
        }
    }
    None
}

fn skills_payload(config: &HttpRuntimeConfig) -> Value {
    let workspace = config
        .workspace
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let runtime = plugin_runtime_options(config);
    let registry = SkillRegistry::new_with_options(
        Some(workspace),
        Option::<Vec<String>>::None,
        Option::<PathBuf>::None,
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    )
    .with_extra_roots(runtime.extra_skill_roots)
    .with_disabled_names(runtime.disabled_skills);
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

fn lsp_status_payload(config: &HttpRuntimeConfig) -> Result<Value, String> {
    let workspace = workspace(config);
    let servers = lsp_status(&workspace)?;
    Ok(json!({
        "workspace": workspace.to_string_lossy(),
        "servers": servers,
    }))
}

fn lsp_doctor_payload(config: &HttpRuntimeConfig) -> Result<Value, String> {
    lsp_doctor(workspace(config)).map(|report| json!(report))
}

fn lsp_query_payload(config: &HttpRuntimeConfig, body: &str) -> Result<Value, String> {
    let payload: Value =
        serde_json::from_str(body).map_err(|error| format!("invalid lsp query JSON: {error}"))?;
    let operation_name = payload
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("documentSymbol");
    let operation = operation_from_str(operation_name)
        .ok_or_else(|| format!("unsupported LSP operation: {operation_name}"))?;
    if operation == LspOperation::Status {
        return lsp_status_payload(config);
    }
    let file_path = payload
        .get("file_path")
        .or_else(|| payload.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| "lsp query requires file_path".to_string())?;
    let workspace = workspace(config);
    let result = query_workspace(
        &workspace,
        LspQuery {
            operation,
            file_path: PathBuf::from(file_path),
            line: payload.get("line").and_then(Value::as_u64),
            character: payload
                .get("character")
                .or_else(|| payload.get("column"))
                .and_then(Value::as_u64),
            query: payload
                .get("query")
                .and_then(Value::as_str)
                .map(str::to_string),
            timeout_ms: payload
                .get("timeout_ms")
                .or_else(|| payload.get("timeout"))
                .and_then(Value::as_u64),
        },
    )?;
    Ok(json!(result))
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

fn session_metadata_string_list(session: &Session, key: &str) -> Vec<String> {
    session
        .metadata
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn runtime_agent_tool_options(
    session: &Session,
    profile: Option<&RuntimeSubagentProfile>,
) -> BTreeMap<String, Value> {
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
    let extra_skill_roots = session_metadata_string_list(session, "extra_skill_roots");
    if !extra_skill_roots.is_empty() {
        options.insert("extra_skill_roots".to_string(), json!(extra_skill_roots));
    }
    let disabled_skills = session_metadata_string_list(session, "disabled_skills");
    if !disabled_skills.is_empty() {
        options.insert("disabled_skills".to_string(), json!(disabled_skills));
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
    let mut mcp_config = match source
        .read_source
        .as_deref()
        .map(load_mcp_config)
        .transpose()
    {
        Ok(Some(config)) if config.enabled() => config,
        _ => return None,
    };
    apply_mcp_oauth_credentials(config, &mut mcp_config);
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

fn runtime_session_runner_facade(
    session: &Session,
    agent_profile: Option<&RuntimeSubagentProfile>,
    permission_ruleset: PermissionRuleset,
    skip_permissions: bool,
) -> SessionRunnerFacade {
    SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
        .with_agent_options(runtime_agent_tool_options(session, agent_profile))
        .with_permission_manager(runtime_permission_manager_for_agent(
            permission_ruleset,
            agent_profile,
        ))
        .with_dangerously_skip_permissions(skip_permissions)
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

fn create_session_payload(config: &HttpRuntimeConfig, body: &str) -> Result<Value, String> {
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
    store
        .save_state(&session, None)
        .map_err(|error| format!("Failed to persist session {session_id}: {error}"))?;
    let persisted = store
        .load_session(&session_id)
        .map_err(|error| format!("Failed to verify session {session_id}: {error}"))?;
    Ok(json!({
        "session_id": session_id,
        "status": "created",
        "session": {
            "id": session_id,
            "session_id": session_id,
            "status": "idle",
            "message_count": 0,
            "workspace": persisted.directory.to_string_lossy(),
        }
    }))
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
    if let Some(change_review) = payload.get("change_review") {
        let review = change_review
            .as_object()
            .ok_or_else(|| "change_review must be an object".to_string())?;
        let status = review
            .get("status")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| matches!(*value, "accepted" | "changes_requested"))
            .ok_or_else(|| {
                "change_review.status must be accepted or changes_requested".to_string()
            })?;
        let bounded_text = |key: &str| {
            review
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.chars().take(512).collect::<String>())
                .unwrap_or_default()
        };
        session.metadata.insert(
            "change_review".to_string(),
            json!({
                "status": status,
                "patch_id": bounded_text("patch_id"),
                "path": bounded_text("path"),
                "run_id": bounded_text("run_id"),
                "updated_at_ms": now_ms(),
            }),
        );
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

fn session_goal(session: &Session) -> Result<Option<DurableGoal>, String> {
    session
        .metadata
        .get("durable_goal")
        .cloned()
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|error| format!("stored durable goal is invalid: {error}"))
        })
        .transpose()
}

fn session_goal_payload(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let session = FileSessionStore::new(session_root(config))
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "schema_version": "openagent.session_goal_response.v1",
        "session_id": session.id,
        "goal": session_goal(&session)?,
    }))
}

fn goal_text(payload: &Value, key: &str, max_chars: usize) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(max_chars).collect())
}

fn goal_acceptance_criteria(payload: &Value) -> Result<Option<Vec<String>>, String> {
    let Some(criteria) = payload.get("acceptance_criteria") else {
        return Ok(None);
    };
    let criteria = criteria
        .as_array()
        .ok_or_else(|| "acceptance_criteria must be an array of strings".to_string())?;
    if criteria.iter().any(|value| !value.is_string()) {
        return Err("acceptance_criteria must contain only strings".to_string());
    }
    Ok(Some(
        criteria
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .take(20)
            .map(|value| value.chars().take(512).collect::<String>())
            .collect(),
    ))
}

fn mutate_session_goal_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    body: &str,
) -> Result<Value, String> {
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let payload: Value = serde_json::from_str(body).map_err(|_| "invalid JSON body".to_string())?;
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("update");
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let existing = session_goal(&session)?;
    let timestamp_ms = now_ms();
    let mut goal = match action {
        "create" => {
            if existing
                .as_ref()
                .is_some_and(|goal| goal.status != DurableGoalStatus::Completed)
            {
                return Err("an active or paused durable goal already exists".to_string());
            }
            let objective = goal_text(&payload, "objective", 8_000)
                .ok_or_else(|| "objective is required".to_string())?;
            let title = goal_text(&payload, "title", 160)
                .unwrap_or_else(|| objective.chars().take(80).collect::<String>());
            DurableGoal::new(
                new_id("goal"),
                title,
                objective,
                goal_acceptance_criteria(&payload)?.unwrap_or_default(),
                timestamp_ms,
            )
        }
        "update" => {
            let mut goal = existing.ok_or_else(|| "durable goal not found".to_string())?;
            if goal.status == DurableGoalStatus::Completed {
                return Err("completed durable goals cannot be edited".to_string());
            }
            if let Some(title) = goal_text(&payload, "title", 160) {
                goal.title = title;
            }
            if let Some(objective) = goal_text(&payload, "objective", 8_000) {
                goal.objective = objective;
            }
            if let Some(criteria) = goal_acceptance_criteria(&payload)? {
                goal.acceptance_criteria = criteria;
            }
            goal.revision = goal.revision.saturating_add(1);
            goal.updated_at_ms = timestamp_ms;
            goal
        }
        "pause" => {
            let mut goal = existing.ok_or_else(|| "durable goal not found".to_string())?;
            if goal.status != DurableGoalStatus::Active {
                return Err("only an active durable goal can be paused".to_string());
            }
            goal.status = DurableGoalStatus::Paused;
            goal.revision = goal.revision.saturating_add(1);
            goal.updated_at_ms = timestamp_ms;
            goal
        }
        "resume" => {
            let mut goal = existing.ok_or_else(|| "durable goal not found".to_string())?;
            if goal.status != DurableGoalStatus::Paused {
                return Err("only a paused durable goal can be resumed".to_string());
            }
            goal.status = DurableGoalStatus::Active;
            goal.revision = goal.revision.saturating_add(1);
            goal.updated_at_ms = timestamp_ms;
            goal
        }
        "complete" => {
            let mut goal = existing.ok_or_else(|| "durable goal not found".to_string())?;
            if goal.status == DurableGoalStatus::Completed {
                return Err("durable goal is already completed".to_string());
            }
            goal.status = DurableGoalStatus::Completed;
            goal.revision = goal.revision.saturating_add(1);
            goal.updated_at_ms = timestamp_ms;
            goal.completed_at_ms = Some(timestamp_ms);
            goal
        }
        _ => return Err("action must be create, update, pause, resume, or complete".to_string()),
    };
    if goal.title.trim().is_empty() || goal.objective.trim().is_empty() {
        return Err("durable goal title and objective cannot be empty".to_string());
    }
    goal.schema_version = openagent_core::DURABLE_GOAL_SCHEMA_VERSION.to_string();
    session.metadata.insert(
        "durable_goal".to_string(),
        serde_json::to_value(&goal).map_err(|error| error.to_string())?,
    );
    store
        .save_state(&session, None)
        .map_err(|error| error.to_string())?;
    let persisted = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "schema_version": "openagent.session_goal_response.v1",
        "session_id": persisted.id,
        "goal": session_goal(&persisted)?,
    }))
}

fn session_plan(session: &Session) -> Result<Option<DurablePlan>, String> {
    session
        .metadata
        .get("durable_plan")
        .cloned()
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|error| format!("stored durable plan is invalid: {error}"))
        })
        .transpose()
}

fn session_plan_payload(config: &HttpRuntimeConfig, session_id: &str) -> Result<Value, String> {
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let session = FileSessionStore::new(session_root(config))
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "schema_version": "openagent.session_plan_response.v1",
        "session_id": session.id,
        "plan": session_plan(&session)?,
    }))
}

fn plan_steps(payload: &Value) -> Result<Option<Vec<String>>, String> {
    let Some(steps) = payload.get("steps") else {
        return Ok(None);
    };
    let steps = steps
        .as_array()
        .ok_or_else(|| "steps must be an array of strings".to_string())?;
    if steps.iter().any(|value| !value.is_string()) {
        return Err("steps must contain only strings".to_string());
    }
    Ok(Some(
        steps
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .take(40)
            .map(|value| value.chars().take(1_000).collect::<String>())
            .collect(),
    ))
}

fn mutate_session_plan_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    body: &str,
) -> Result<Value, String> {
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let payload: Value = serde_json::from_str(body).map_err(|_| "invalid JSON body".to_string())?;
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("update");
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let existing = session_plan(&session)?;
    let timestamp_ms = now_ms();
    let mut plan = match action {
        "create" => {
            if existing
                .as_ref()
                .is_some_and(|plan| plan.status != DurablePlanStatus::Completed)
            {
                return Err("an active durable plan already exists".to_string());
            }
            let objective = goal_text(&payload, "objective", 12_000)
                .ok_or_else(|| "objective is required".to_string())?;
            let title = goal_text(&payload, "title", 160)
                .unwrap_or_else(|| objective.chars().take(80).collect::<String>());
            DurablePlan::new(
                new_id("plan"),
                title,
                objective,
                plan_steps(&payload)?.unwrap_or_default(),
                timestamp_ms,
            )
        }
        "update" => {
            let mut plan = existing.ok_or_else(|| "durable plan not found".to_string())?;
            if plan.status != DurablePlanStatus::Planning {
                return Err("only a planning durable plan can be edited".to_string());
            }
            if let Some(title) = goal_text(&payload, "title", 160) {
                plan.title = title;
            }
            if let Some(objective) = goal_text(&payload, "objective", 12_000) {
                plan.objective = objective;
            }
            if let Some(steps) = plan_steps(&payload)? {
                plan.steps = steps;
            }
            plan.revision = plan.revision.saturating_add(1);
            plan.updated_at_ms = timestamp_ms;
            plan
        }
        "execute" => {
            let mut plan = existing.ok_or_else(|| "durable plan not found".to_string())?;
            if plan.status != DurablePlanStatus::Planning {
                return Err("only a planning durable plan can enter execution".to_string());
            }
            plan.status = DurablePlanStatus::Executing;
            plan.revision = plan.revision.saturating_add(1);
            plan.updated_at_ms = timestamp_ms;
            plan.execution_started_at_ms = Some(timestamp_ms);
            plan
        }
        "complete" => {
            let mut plan = existing.ok_or_else(|| "durable plan not found".to_string())?;
            if plan.status == DurablePlanStatus::Completed {
                return Err("durable plan is already completed".to_string());
            }
            plan.status = DurablePlanStatus::Completed;
            plan.revision = plan.revision.saturating_add(1);
            plan.updated_at_ms = timestamp_ms;
            plan.completed_at_ms = Some(timestamp_ms);
            plan
        }
        _ => return Err("action must be create, update, execute, or complete".to_string()),
    };
    if plan.title.trim().is_empty() || plan.objective.trim().is_empty() {
        return Err("durable plan title and objective cannot be empty".to_string());
    }
    plan.schema_version = openagent_core::DURABLE_PLAN_SCHEMA_VERSION.to_string();
    session.metadata.insert(
        "durable_plan".to_string(),
        serde_json::to_value(&plan).map_err(|error| error.to_string())?,
    );
    store
        .save_state(&session, None)
        .map_err(|error| error.to_string())?;
    let persisted = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "schema_version": "openagent.session_plan_response.v1",
        "session_id": persisted.id,
        "plan": session_plan(&persisted)?,
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
    let mut status_counts = BTreeMap::<String, u64>::new();
    for task in &flat_tasks {
        let status = task
            .get("canonical_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        *status_counts.entry(status).or_default() += 1;
    }
    json!({
        "schema_version": "openagent.session_task_tree.v2",
        "session_id": session_id,
        "count": flat_tasks.len(),
        "status_counts": status_counts,
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

fn load_owned_session_task(
    store: &FileSessionStore,
    parent_session_id: &str,
    task_id: &str,
) -> Result<Session, String> {
    if !valid_session_id(parent_session_id) || !valid_session_id(task_id) {
        return Err("invalid session id".to_string());
    }
    let child_session = store
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
    Ok(child_session)
}

fn task_status_value(session: &Session) -> &str {
    session
        .metadata
        .get("task_status")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn session_task_cancel_marker_path(root: &Path, task_id: &str) -> PathBuf {
    root.join(task_id).join("task.cancel.json")
}

fn session_task_cancel_requested(root: &Path, task_id: &str, run_id: &str) -> bool {
    let marker_path = session_task_cancel_marker_path(root, task_id);
    if !marker_path.is_file() {
        return false;
    }
    let marker = read_json_file(&marker_path);
    marker
        .get("run_id")
        .and_then(Value::as_str)
        .is_none_or(|marked_run_id| marked_run_id.is_empty() || marked_run_id == run_id)
}

fn write_session_task_cancel_marker(
    root: &Path,
    task_id: &str,
    run_id: &str,
) -> Result<(), String> {
    write_json_value(
        &session_task_cancel_marker_path(root, task_id),
        &json!({
            "task_id": task_id,
            "run_id": run_id,
            "requested_at_ms": now_ms(),
        }),
    )
}

fn clear_session_task_cancel_marker(root: &Path, task_id: &str) {
    let _ = fs::remove_file(session_task_cancel_marker_path(root, task_id));
}

fn run_session_task_payload(
    config: &HttpRuntimeConfig,
    parent_session_id: &str,
    task_id: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let store = FileSessionStore::new(session_root(config));
    let mut child_session = load_owned_session_task(&store, parent_session_id, task_id)?;
    let task_status = task_status_value(&child_session);
    if task_status != "queued" {
        return Err(format!("task is not queued: {task_status}"));
    }
    let _task_run_lock = claim_session_task_run_lock(config, task_id)?;
    child_session = load_owned_session_task(&store, parent_session_id, task_id)?;
    let task_status = task_status_value(&child_session);
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
            .get("promoted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "foreground_promote"
        } else if payload
            .get("background_start")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "background_start"
        } else if payload
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
    child_session.metadata.insert(
        "execution_mode".to_string(),
        json!(if payload
            .get("promoted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "foreground"
        } else {
            "background"
        }),
    );
    child_session.metadata.remove("cancel_requested_at_ms");
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

    let cancel_requested = session_task_cancel_requested(&store.root, task_id, &run_id);
    let (status, output) = match loop_result {
        Ok(value)
            if cancel_requested
                || value.get("status").and_then(Value::as_str) == Some("interrupted") =>
        {
            child_session.status = SessionStatus::Idle;
            child_session
                .metadata
                .insert("task_status".to_string(), json!("canceled"));
            child_session
                .metadata
                .insert("canceled_at_ms".to_string(), json!(now_ms()));
            child_session.metadata.remove("cancel_requested_at_ms");
            let _ = store.finish_run(
                &child_session,
                &run_id,
                "interrupted",
                0,
                Some("canceled"),
                Some("task canceled"),
            );
            let _ = store.save_state(&child_session, Some(&run_id));
            ("canceled".to_string(), value)
        }
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
        Err(error) if cancel_requested => {
            child_session.status = SessionStatus::Idle;
            child_session
                .metadata
                .insert("task_status".to_string(), json!("canceled"));
            child_session
                .metadata
                .insert("canceled_at_ms".to_string(), json!(now_ms()));
            child_session.metadata.remove("cancel_requested_at_ms");
            let _ = store.finish_run(
                &child_session,
                &run_id,
                "interrupted",
                0,
                Some("canceled"),
                Some("task canceled"),
            );
            let _ = store.save_state(&child_session, Some(&run_id));
            (
                "canceled".to_string(),
                json!({"status": "interrupted", "error": error}),
            )
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
    clear_session_task_cancel_marker(&store.root, task_id);
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

fn dispatch_session_task_payload(
    config: &HttpRuntimeConfig,
    parent_session_id: &str,
    task_id: &str,
    promoted: bool,
) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let mut child_session = load_owned_session_task(&store, parent_session_id, task_id)?;
    let task_status = task_status_value(&child_session);
    if task_status != "queued" {
        return Err(format!("task is not queued: {task_status}"));
    }
    let lock_path = task_run_lock_path(config, task_id);
    if lock_path.exists() && !remove_stale_task_run_lock(&lock_path)? {
        return Err("task is already running".to_string());
    }
    child_session.metadata.insert(
        "execution_mode".to_string(),
        json!(if promoted { "foreground" } else { "background" }),
    );
    child_session
        .metadata
        .insert("background".to_string(), json!(!promoted));
    child_session.metadata.insert(
        if promoted {
            "promoted_at_ms".to_string()
        } else {
            "start_requested_at_ms".to_string()
        },
        json!(now_ms()),
    );
    store
        .save_state(&child_session, None)
        .map_err(|error| format!("failed to dispatch task: {error}"))?;

    let thread_config = config.clone();
    let thread_parent_session_id = parent_session_id.to_string();
    let thread_task_id = task_id.to_string();
    thread::Builder::new()
        .name(format!("openagent-task-{thread_task_id}"))
        .spawn(move || {
            let payload = if promoted {
                json!({"promoted": true})
            } else {
                json!({"background_start": true})
            };
            if let Err(error) = run_session_task_payload(
                &thread_config,
                &thread_parent_session_id,
                &thread_task_id,
                &payload.to_string(),
            ) {
                eprintln!(
                    "openagent task {} dispatch failed: {}",
                    thread_task_id, error
                );
            }
        })
        .map_err(|error| format!("failed to start task worker: {error}"))?;

    let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
    let task = session_task_summary_from_state(&session_root(config), &state, task_id);
    Ok(json!({
        "accepted": true,
        "session_id": parent_session_id,
        "task_id": task_id,
        "status": "queued",
        "execution_mode": if promoted { "foreground" } else { "background" },
        "task": task,
    }))
}

fn wait_session_task_payload(
    config: &HttpRuntimeConfig,
    parent_session_id: &str,
    task_id: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let timeout_ms = payload
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(5_000)
        .clamp(10, 30_000);
    let store = FileSessionStore::new(session_root(config));
    load_owned_session_task(&store, parent_session_id, task_id)?;
    let started = Instant::now();
    loop {
        let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
        let task = session_task_summary_from_state(&session_root(config), &state, task_id);
        let status = task
            .get("canonical_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let terminal = matches!(status, "completed" | "failed" | "cancelled");
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if terminal || elapsed_ms >= timeout_ms {
            return Ok(json!({
                "session_id": parent_session_id,
                "task_id": task_id,
                "status": status,
                "completed": terminal,
                "timed_out": !terminal,
                "elapsed_ms": elapsed_ms,
                "task": task,
            }));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn resume_session_task_payload(
    config: &HttpRuntimeConfig,
    parent_session_id: &str,
    task_id: &str,
) -> Result<Value, String> {
    let store = FileSessionStore::new(session_root(config));
    let mut child_session = load_owned_session_task(&store, parent_session_id, task_id)?;
    let task_status = canonical_task_status(task_status_value(&child_session));
    if !matches!(task_status, "failed" | "cancelled") {
        return Err(format!("task cannot be resumed from status: {task_status}"));
    }
    if task_run_lock_path(config, task_id).exists()
        && !remove_stale_task_run_lock(&task_run_lock_path(config, task_id))?
    {
        return Err("task is still stopping".to_string());
    }
    let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
    if let Some(previous_run_id) = state
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        child_session
            .metadata
            .insert("previous_run_id".to_string(), json!(previous_run_id));
    }
    let resume_count = child_session
        .metadata
        .get("resume_count")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        .saturating_add(1);
    child_session.status = SessionStatus::Idle;
    child_session
        .metadata
        .insert("task_status".to_string(), json!("queued"));
    child_session
        .metadata
        .insert("background".to_string(), json!(true));
    child_session
        .metadata
        .insert("execution_mode".to_string(), json!("background"));
    child_session
        .metadata
        .insert("resume_count".to_string(), json!(resume_count));
    child_session
        .metadata
        .insert("resumed_at_ms".to_string(), json!(now_ms()));
    for key in [
        "canceled_at_ms",
        "cancel_requested_at_ms",
        "run_claimed_at_ms",
    ] {
        child_session.metadata.remove(key);
    }
    clear_session_task_cancel_marker(&store.root, task_id);
    store
        .save_state(&child_session, None)
        .map_err(|error| format!("failed to resume task: {error}"))?;
    let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
    let task = session_task_summary_from_state(&session_root(config), &state, task_id);
    Ok(json!({
        "session_id": parent_session_id,
        "task_id": task_id,
        "status": "queued",
        "resumed": true,
        "resume_count": resume_count,
        "task": task,
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
    let store = FileSessionStore::new(session_root(config));
    let mut child_session = load_owned_session_task(&store, parent_session_id, task_id)?;
    let task_status = canonical_task_status(task_status_value(&child_session));
    if matches!(task_status, "completed" | "failed" | "cancelled") {
        return Err(format!(
            "task cannot be canceled from status: {task_status}"
        ));
    }
    let lock_path = task_run_lock_path(config, task_id);
    let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
    let run_id = state
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| new_id("turn"));
    if lock_path.exists() && !remove_stale_task_run_lock(&lock_path)? {
        write_session_task_cancel_marker(&store.root, task_id, &run_id)?;
        child_session
            .metadata
            .insert("cancel_requested_at_ms".to_string(), json!(now_ms()));
        let _ = store.save_state(&child_session, Some(&run_id));
        let state = read_json_file(&session_root(config).join(task_id).join("state.latest.json"));
        let task = session_task_summary_from_state(&session_root(config), &state, task_id);
        return Ok(json!({
            "session_id": parent_session_id,
            "task_id": task_id,
            "run_id": run_id,
            "status": "cancel_requested",
            "task": task,
        }));
    }
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
    let message_count = session.messages.len();
    let run_id = new_id("compact");
    let compacted_until_message_id = session.messages.last().map(|message| {
        message
            .metadata
            .get("message_id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| runtime_message_id(message_count.saturating_sub(1) as u64))
    });
    let boundary_message_id = match compacted_until_message_id.as_deref() {
        Some(message_id) => Some(
            store
                .append_compaction_boundary(&mut session, &run_id, &summary, message_id)
                .map_err(|error| format!("failed to create compaction boundary: {error}"))?,
        ),
        None => None,
    };
    session.status = SessionStatus::Idle;
    session.metadata.insert(
        "compact".to_string(),
        json!({
            "compacted_at_ms": now_ms(),
            "message_count": message_count,
            "summary": summary,
            "format": "session_summary_v1",
            "compacted_until_message_id": compacted_until_message_id,
            "boundary_message_id": boundary_message_id,
        }),
    );
    store
        .save_state(&session, Some(&run_id))
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

fn workspace_for_session(
    config: &HttpRuntimeConfig,
    session_id: Option<&str>,
) -> Result<PathBuf, String> {
    let Some(session_id) = session_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(workspace(config));
    };
    if session_id.contains('/') || session_id.contains('\\') || session_id.contains("..") {
        return Err("invalid session id".to_string());
    }
    FileSessionStore::new(session_root(config))
        .load_session(session_id)
        .map(|session| session.directory)
        .map_err(|error| error.to_string())
}

fn files_payload(config: &HttpRuntimeConfig, request_path: &str) -> Result<Value, String> {
    let scoped_session_id = query_param(request_path, "session_id");
    let root = workspace_for_session(config, scoped_session_id.as_deref())?;
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
            "session_id": scoped_session_id,
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
        "session_id": scoped_session_id,
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

fn git_payload(config: &HttpRuntimeConfig, request_path: &str) -> Result<Value, String> {
    let scoped_session_id = query_param(request_path, "session_id");
    let root = workspace_for_session(config, scoped_session_id.as_deref())?;
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
            "session_id": scoped_session_id,
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
    let numstat = git_numstat(&root);
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
        let (additions, deletions, binary) = git_change_counts(&root, &path, xy, &numstat);
        changes.push(json!({
            "status": xy.trim(),
            "index": xy.chars().next().unwrap_or(' '),
            "worktree": xy.chars().nth(1).unwrap_or(' '),
            "path": path,
            "additions": additions,
            "deletions": deletions,
            "binary": binary,
        }));
    }
    let change_count = changes.len();
    let selected_diff = query_param(request_path, "path")
        .filter(|path| !path.trim().is_empty())
        .map(|path| {
            let status = changes
                .iter()
                .find(|change| change.get("path").and_then(Value::as_str) == Some(path.as_str()))
                .and_then(|change| change.get("status"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            git_file_diff_payload(&root, &path, status, &numstat)
        })
        .transpose()?;
    Ok(json!({
        "session_id": scoped_session_id,
        "workspace": root.to_string_lossy(),
        "is_repo": true,
        "branch": branch,
        "ahead": ahead,
        "behind": behind,
        "changes": changes,
        "change_count": change_count,
        "selected_diff": selected_diff,
    }))
}

fn git_numstat(root: &Path) -> BTreeMap<String, (Option<u64>, Option<u64>)> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--numstat", "HEAD", "--"])
        .output();
    let Ok(output) = output else {
        return BTreeMap::new();
    };
    if !output.status.success() {
        return BTreeMap::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let additions = fields.next()?;
            let deletions = fields.next()?;
            let path = fields.next()?.to_string();
            Some((
                path,
                (additions.parse::<u64>().ok(), deletions.parse::<u64>().ok()),
            ))
        })
        .collect()
}

fn git_change_counts(
    root: &Path,
    path: &str,
    status: &str,
    numstat: &BTreeMap<String, (Option<u64>, Option<u64>)>,
) -> (u64, u64, bool) {
    if let Some((additions, deletions)) = numstat.get(path) {
        return (
            additions.unwrap_or_default(),
            deletions.unwrap_or_default(),
            additions.is_none() || deletions.is_none(),
        );
    }
    if status == "??" {
        let target = root.join(path);
        if file_is_text_like(&target) {
            let additions = fs::read_to_string(target)
                .map(|content| content.lines().count() as u64)
                .unwrap_or_default();
            return (additions, 0, false);
        }
        return (0, 0, true);
    }
    (0, 0, false)
}

fn git_file_diff_payload(
    root: &Path,
    requested_path: &str,
    status: &str,
    numstat: &BTreeMap<String, (Option<u64>, Option<u64>)>,
) -> Result<Value, String> {
    let target = resolve_path_in_root(root, requested_path)?;
    let path = workspace_relative_path(root, &target);
    let (additions, deletions, binary) = git_change_counts(root, &path, status, numstat);
    let mut source = "git";
    let diff = if status == "??" {
        source = "untracked";
        if target.is_file() && file_is_text_like(&target) {
            let content = fs::read_to_string(&target).map_err(|error| error.to_string())?;
            render_unified_diff(&path, None, Some(&content))
        } else {
            String::new()
        }
    } else {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--no-color", "--no-ext-diff", "HEAD", "--"])
            .arg(&path)
            .output()
            .map_err(|error| format!("failed to run git diff: {error}"))?;
        if output.status.success() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if stderr.is_empty() {
                format!("git diff failed for {path}")
            } else {
                stderr
            });
        }
    };
    let lines = diff.lines().map(str::to_string).collect::<Vec<_>>();
    let truncated = lines.len() > MAX_RENDERED_DIFF_LINES;
    let diff = truncate_diff_lines(lines).join("\n");
    Ok(json!({
        "path": path,
        "status": status.trim(),
        "source": source,
        "diff": diff,
        "additions": additions,
        "deletions": deletions,
        "binary": binary,
        "truncated": truncated,
    }))
}

fn terminal_run_payload(config: &HttpRuntimeConfig, body: &str) -> Result<Value, String> {
    ensure_direct_capability_allowed(config, "terminal")?;
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

    let scoped_session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let root = workspace_for_session(config, scoped_session_id)?;
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
    let mut result = run_terminal_command(&command_text, &root, &cwd, timeout_ms)?;
    if let Some(object) = result.as_object_mut() {
        object.insert("session_id".to_string(), json!(scoped_session_id));
    }
    Ok(result)
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
        ".git"
            | ".openagent"
            | ".runtime_http"
            | ".claude"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | "jobs"
            | "runs"
            | "coverage"
            | "htmlcov"
            | "__pycache__"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".venv"
            | "venv"
            | "env"
            | ".DS_Store"
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

fn session_context_diagnostics_payload(
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
    let limit = query_param(request_path, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(12)
        .clamp(1, MAX_CONTEXT_PACK_RECEIPTS);
    Ok(context_diagnostics_payload_for_session(&session, limit))
}

fn replay_session_context_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    body: &str,
) -> Result<Value, String> {
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let request = if body.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str::<Value>(body)
            .map_err(|error| format!("invalid context replay request: {error}"))?
    };
    if !request.is_object() {
        return Err("context replay request must be a JSON object".to_string());
    }
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let requested_run_id = request
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let requested_step = request.get("step").and_then(Value::as_u64);
    let explicit_target = requested_run_id.is_some() || requested_step.is_some();
    let raw_latest = session.metadata.get("context_pack").cloned();
    let latest_replayable = raw_latest
        .as_ref()
        .and_then(|raw| runtime_context_replay_target(raw).ok());
    let target = if explicit_target {
        context_replay_history(&session).into_iter().find(|target| {
            requested_run_id.is_none_or(|run_id| target.run_id == run_id)
                && requested_step.is_none_or(|step| target.step == step)
        })
    } else {
        latest_replayable.or_else(|| context_replay_history(&session).into_iter().next())
    };
    let latest_requires_recovery = !explicit_target
        && raw_latest
            .as_ref()
            .is_none_or(|raw| runtime_context_replay_target(raw).is_err());

    let mut reasons = Vec::new();
    let (status, target_run_id, target_step, target_receipt, rebuilt_pack, replay_spec) =
        if let Some(target) = target {
            if !target.spec.unsafe_model_option_keys.is_empty() {
                reasons.push(format!(
                    "unsupported_model_options:{}",
                    target.spec.unsafe_model_option_keys.join(",")
                ));
                (
                    "unrecoverable",
                    Some(target.run_id),
                    Some(target.step),
                    Some(target.receipt),
                    None,
                    None,
                )
            } else {
                let agent_profile = runtime_agent_profile_for_session(&session);
                let rebuilt = runtime_context_pack_from_replay_spec(
                    &store,
                    &mut session,
                    agent_profile.as_ref(),
                    &target.spec,
                );
                reasons.extend(context_replay_drift_reasons(
                    &target.receipt,
                    &rebuilt.receipt,
                ));
                let status = if latest_requires_recovery {
                    "rebuilt"
                } else if reasons.is_empty() {
                    "verified"
                } else {
                    "drifted"
                };
                (
                    status,
                    Some(target.run_id),
                    Some(target.step),
                    Some(target.receipt),
                    Some(rebuilt),
                    Some(target.spec),
                )
            }
        } else if explicit_target {
            reasons.push("target_receipt_not_found".to_string());
            ("unrecoverable", None, None, None, None, None)
        } else {
            reasons.push(if raw_latest.is_some() {
                "latest_receipt_corrupt_or_legacy".to_string()
            } else {
                "latest_receipt_missing".to_string()
            });
            let (pack, spec) = runtime_current_context_recovery_pack(&store, &mut session)?;
            if spec.unsafe_model_option_keys.is_empty() {
                ("rebuilt", None, None, None, Some(pack), Some(spec))
            } else {
                reasons.push(format!(
                    "unsupported_model_options:{}",
                    spec.unsafe_model_option_keys.join(",")
                ));
                ("unrecoverable", None, None, None, Some(pack), None)
            }
        };

    let replay_id = new_id("context_replay");
    let rebuilt_receipt = rebuilt_pack.as_ref().map(|pack| pack.receipt.clone());
    let failure = context_replay_failure(status, &reasons);
    let summary = json!({
        "schema_version": CONTEXT_REPLAY_RESULT_SCHEMA_VERSION,
        "replay_id": replay_id,
        "session_id": session.id,
        "status": status,
        "target": {
            "run_id": target_run_id,
            "step": target_step,
            "receipt": target_receipt,
        },
        "rebuilt": {
            "receipt": rebuilt_receipt,
        },
        "reasons": reasons,
        "failure": failure,
        "side_effects": {
            "provider_calls": 0,
            "tool_calls": 0,
            "checkpoint_restores": 0,
            "mcp_lifecycle_changes": 0,
        },
    });
    if status == "rebuilt" {
        let pack = rebuilt_pack
            .as_ref()
            .ok_or_else(|| "context replay did not produce a rebuilt pack".to_string())?;
        let spec = replay_spec
            .as_ref()
            .ok_or_else(|| "context replay did not produce a replay spec".to_string())?;
        runtime_persist_context_recovery(&store, &mut session, &replay_id, pack, spec, &summary)?;
    } else {
        persist_context_replay_summary(&store, &mut session, &replay_id, &summary)?;
    }
    if let Some(event) = context_replayed_bridge_event(&session, &summary) {
        append_bridge_events(&store.root, &session.id, &replay_id, &mut [event]);
    }
    let mut response = summary;
    if let Some(object) = response.as_object_mut() {
        object.insert(
            "diagnostics".to_string(),
            context_diagnostics_payload_for_session(&session, 12),
        );
    }
    Ok(response)
}

struct RuntimeContextReplayTarget {
    run_id: String,
    step: u64,
    receipt: ContextPackReceipt,
    spec: RuntimeContextReplaySpec,
}

fn runtime_context_replay_target(raw: &Value) -> Result<RuntimeContextReplayTarget, String> {
    let run_id = raw
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "context receipt is missing run_id".to_string())?
        .to_string();
    let step = raw
        .get("step")
        .and_then(Value::as_u64)
        .ok_or_else(|| "context receipt is missing step".to_string())?;
    let receipt = serde_json::from_value::<ContextPackReceipt>(
        raw.get("receipt")
            .cloned()
            .ok_or_else(|| "context receipt is missing receipt data".to_string())?,
    )
    .map_err(|error| format!("context receipt is corrupt: {error}"))?;
    let spec = serde_json::from_value::<RuntimeContextReplaySpec>(
        raw.get("replay_spec")
            .cloned()
            .ok_or_else(|| "context receipt does not contain a replay spec".to_string())?,
    )
    .map_err(|error| format!("context replay spec is corrupt: {error}"))?;
    if spec.schema_version != CONTEXT_REPLAY_SPEC_SCHEMA_VERSION {
        return Err(format!(
            "unsupported context replay spec: {}",
            spec.schema_version
        ));
    }
    Ok(RuntimeContextReplayTarget {
        run_id,
        step,
        receipt,
        spec,
    })
}

fn context_replay_history(session: &Session) -> Vec<RuntimeContextReplayTarget> {
    session
        .metadata
        .get("context_pack_receipts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rev()
        .filter_map(|raw| runtime_context_replay_target(raw).ok())
        .collect()
}

fn context_replay_drift_reasons(
    target: &ContextPackReceipt,
    rebuilt: &ContextPackReceipt,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if target.provider_input_hash != rebuilt.provider_input_hash {
        reasons.push("provider_input_changed".to_string());
    }
    if target.message_count != rebuilt.message_count
        || target.message_role_counts != rebuilt.message_role_counts
    {
        reasons.push("message_projection_changed".to_string());
    }
    if target.tool_names != rebuilt.tool_names
        || target.tool_manifest_count != rebuilt.tool_manifest_count
    {
        reasons.push("tool_catalog_changed".to_string());
    }
    if target.model_option_keys != rebuilt.model_option_keys {
        reasons.push("model_options_changed".to_string());
    }
    if target.item_kind_counts != rebuilt.item_kind_counts {
        reasons.push("context_sources_changed".to_string());
    }
    if target.drop_reason_counts != rebuilt.drop_reason_counts
        || target.truncation_reason_counts != rebuilt.truncation_reason_counts
    {
        reasons.push("context_budget_selection_changed".to_string());
    }
    if target.budget != rebuilt.budget {
        reasons.push("context_budget_changed".to_string());
    }
    if target.stable_prefix.hash != rebuilt.stable_prefix.hash {
        reasons.push("stable_prefix_changed".to_string());
    }
    if target.pack_hash != rebuilt.pack_hash && reasons.is_empty() {
        reasons.push("context_pack_changed".to_string());
    }
    reasons
}

fn runtime_current_context_recovery_pack(
    store: &FileSessionStore,
    session: &mut Session,
) -> Result<(ContextPack, RuntimeContextReplaySpec), String> {
    let payload = json!({});
    let provider = session.metadata.get("provider").and_then(Value::as_str);
    let provider_state = provider_state_for_root(&store.root);
    let provider_config = runtime_provider_config(
        Some(&provider_state),
        provider,
        Some(&payload),
        Some(session),
    )?;
    let model_options = runtime_provider_model_options(session, &payload);
    let context_model = runtime_context_model(&provider_config, session, &payload);
    let context_budget_options = runtime_context_budget_options(session, &payload);
    let build_options =
        context_pack_build_options_for_model(Some(&context_budget_options), &context_model, false)?;
    let agent_profile = runtime_agent_profile_for_session(session);
    let toolkit = toolkit_with_runtime_task_tool(session, agent_profile.as_ref());
    let tools = filter_runtime_tools_for_capabilities(
        &store.root,
        filter_runtime_tools_for_profile(toolkit.get_all_tools("local"), agent_profile.as_ref()),
    );
    let pack = runtime_context_pack_for_agent(
        store,
        session,
        &tools,
        &model_options,
        None,
        agent_profile.as_ref(),
        build_options.clone(),
    );
    let spec = runtime_context_replay_spec(
        store,
        session,
        &pack,
        None,
        agent_profile.as_ref(),
        build_options,
    );
    Ok((pack, spec))
}

fn runtime_persist_context_recovery(
    store: &FileSessionStore,
    session: &mut Session,
    replay_id: &str,
    pack: &ContextPack,
    spec: &RuntimeContextReplaySpec,
    summary: &Value,
) -> Result<(), String> {
    let mut receipts = session
        .metadata
        .get("context_pack_receipts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let prefix_cache = runtime_context_prefix_cache_status(&receipts, pack, replay_id, 0);
    let envelope = json!({
        "schema_version": "openagent.turn_context_pack.v1",
        "mode": "recovery",
        "run_id": replay_id,
        "step": 0,
        "receipt": pack.receipt,
        "trace": pack.trace,
        "system_diagnostics": pack.system_diagnostics,
        "prefix_cache": prefix_cache,
        "replay_spec": spec,
        "recovery": {
            "status": summary["status"],
            "reasons": summary["reasons"],
            "target": summary["target"],
        },
    });
    receipts.push(envelope.clone());
    if receipts.len() > MAX_CONTEXT_PACK_RECEIPTS {
        receipts.drain(..receipts.len() - MAX_CONTEXT_PACK_RECEIPTS);
    }
    session
        .metadata
        .insert("context_pack".to_string(), envelope);
    session
        .metadata
        .insert("context_pack_receipts".to_string(), Value::Array(receipts));
    persist_context_replay_summary(store, session, replay_id, summary)
}

fn persist_context_replay_summary(
    store: &FileSessionStore,
    session: &mut Session,
    replay_id: &str,
    summary: &Value,
) -> Result<(), String> {
    let public = public_context_replay_result(summary);
    session
        .metadata
        .insert("context_replay_last".to_string(), public.clone());
    store
        .record_event(
            &session.id,
            replay_id,
            "context.pack_replayed",
            SessionEventOptions {
                kind: "context".to_string(),
                attributes: public
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
                ..SessionEventOptions::default()
            },
        )
        .map_err(|error| error.to_string())?;
    store
        .save_state(session, Some(replay_id))
        .map_err(|error| error.to_string())
}

fn context_diagnostics_payload_for_session(session: &Session, limit: usize) -> Value {
    let raw_history = session
        .metadata
        .get("context_pack_receipts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut corrupt_count = 0_u64;
    let history = raw_history
        .iter()
        .rev()
        .filter_map(|raw| match public_context_pack_envelope(raw) {
            Ok(envelope) => Some(envelope),
            Err(()) => {
                corrupt_count = corrupt_count.saturating_add(1);
                None
            }
        })
        .take(limit)
        .collect::<Vec<_>>();
    let raw_latest = session.metadata.get("context_pack");
    let latest_result = raw_latest.map(public_context_pack_envelope);
    let latest_corrupt = latest_result.as_ref().is_some_and(Result::is_err);
    if latest_corrupt {
        corrupt_count = corrupt_count.saturating_add(1);
    }
    let latest = latest_result
        .and_then(Result::ok)
        .or_else(|| history.first().cloned());
    let latest_failure = latest
        .as_ref()
        .and_then(|envelope| envelope.get("failure"))
        .filter(|failure| !failure.is_null())
        .cloned();
    let status = if latest.is_some() {
        if latest_corrupt || latest_failure.is_some() {
            "degraded"
        } else {
            "ready"
        }
    } else if raw_latest.is_some() || !raw_history.is_empty() {
        "corrupt"
    } else {
        "unavailable"
    };
    let failure = latest_failure.or_else(|| match status {
        "corrupt" => Some(public_context_failure(&ContextFailure::new(
            ContextFailureCode::ReceiptCorrupt,
            "diagnostics",
            "Context receipt is corrupt and cannot be inspected.",
        ))),
        "unavailable" => Some(public_context_failure(&ContextFailure::new(
            ContextFailureCode::Unavailable,
            "diagnostics",
            "Context diagnostics are not available until the first turn is built.",
        ))),
        _ if latest_corrupt => Some(public_context_failure(&ContextFailure::new(
            ContextFailureCode::ReceiptCorrupt,
            "diagnostics",
            "The latest Context receipt is corrupt; a historical receipt is shown.",
        ))),
        _ => None,
    });
    json!({
        "schema_version": CONTEXT_DIAGNOSTICS_SCHEMA_VERSION,
        "session_id": session.id,
        "status": status,
        "latest": latest,
        "history": history,
        "history_count": raw_history.len(),
        "returned_count": history.len(),
        "corrupt_count": corrupt_count,
        "failure": failure,
        "last_replay": session
            .metadata
            .get("context_replay_last")
            .map(public_context_replay_result),
        "redaction": {
            "content_included": false,
            "prompt_included": false,
            "attachment_content_included": false,
            "secret_values_included": false,
        },
    })
}

fn public_context_replay_result(raw: &Value) -> Value {
    let reasons = raw
        .get("reasons")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(sanitize_context_diagnostic_label)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "schema_version": CONTEXT_REPLAY_RESULT_SCHEMA_VERSION,
        "replay_id": raw.get("replay_id").and_then(Value::as_str),
        "session_id": raw.get("session_id").and_then(Value::as_str),
        "status": raw.get("status").and_then(Value::as_str),
        "target": {
            "run_id": raw.pointer("/target/run_id").and_then(Value::as_str),
            "step": raw.pointer("/target/step").and_then(Value::as_u64),
        },
        "reasons": reasons,
        "failure": raw.get("failure").and_then(|failure| {
            serde_json::from_value::<ContextFailure>(failure.clone()).ok()
        }).map(|failure| public_context_failure(&failure)),
        "side_effects": {
            "provider_calls": raw.pointer("/side_effects/provider_calls").and_then(Value::as_u64).unwrap_or_default(),
            "tool_calls": raw.pointer("/side_effects/tool_calls").and_then(Value::as_u64).unwrap_or_default(),
            "checkpoint_restores": raw.pointer("/side_effects/checkpoint_restores").and_then(Value::as_u64).unwrap_or_default(),
            "mcp_lifecycle_changes": raw.pointer("/side_effects/mcp_lifecycle_changes").and_then(Value::as_u64).unwrap_or_default(),
        },
    })
}

fn public_context_pack_envelope(raw: &Value) -> Result<Value, ()> {
    let receipt =
        raw.get("receipt").cloned().ok_or(()).and_then(|value| {
            serde_json::from_value::<ContextPackReceipt>(value).map_err(|_| ())
        })?;
    let trace = raw
        .get("trace")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    serde_json::from_value::<ContextPackTraceEntry>(entry.clone())
                        .ok()
                        .map(|entry| public_context_trace_entry(&entry))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let prefix_cache = public_context_prefix_cache(raw.get("prefix_cache"));
    let rebuild_reason = raw
        .pointer("/rebuild/reason")
        .and_then(Value::as_str)
        .map(sanitize_context_diagnostic_label);
    let performance = raw
        .get("performance")
        .and_then(|value| serde_json::from_value::<ContextPackPerformance>(value.clone()).ok())
        .map(|value| public_context_performance(&value));
    let failure = raw
        .get("failure")
        .and_then(|value| serde_json::from_value::<ContextFailure>(value.clone()).ok())
        .map(|value| public_context_failure(&value));
    let system_diagnostics = raw
        .get("system_diagnostics")
        .and_then(|value| serde_json::from_value::<ContextSystemDiagnostics>(value.clone()).ok())
        .map(|value| public_context_system_diagnostics(&value));
    Ok(json!({
        "schema_version": "openagent.turn_context_diagnostics.v1",
        "mode": raw.get("mode").and_then(Value::as_str).unwrap_or("active"),
        "run_id": raw.get("run_id").and_then(Value::as_str),
        "step": raw.get("step").and_then(Value::as_u64),
        "receipt": receipt,
        "trace": trace,
        "prefix_cache": prefix_cache,
        "rebuilt": raw.get("rebuild").is_some_and(|value| !value.is_null()),
        "rebuild_reason": rebuild_reason,
        "performance": performance,
        "failure": failure,
        "system_diagnostics": system_diagnostics,
    }))
}

fn public_context_system_diagnostics(diagnostics: &ContextSystemDiagnostics) -> Value {
    json!({
        "schema_version": diagnostics.schema_version,
        "profile_id": diagnostics.profile_id,
        "profile_mode": diagnostics.profile_mode,
        "content_hash": diagnostics.content_hash,
        "preloaded_skill_names": diagnostics.preloaded_skill_names,
        "instruction_count": diagnostics.instruction_count,
        "instruction_total_bytes": diagnostics.instruction_total_bytes,
        "instructions_truncated": diagnostics.instructions_truncated,
        "legacy_system_count": diagnostics.legacy_system_count,
        "instruction_issues": diagnostics
            .instruction_issues
            .iter()
            .map(|issue| sanitize_context_diagnostic_label(issue))
            .collect::<Vec<_>>(),
    })
}

fn public_context_performance(performance: &ContextPackPerformance) -> Value {
    json!({
        "schema_version": performance.schema_version,
        "status": performance.status(),
        "materialize_us": performance.materialize_us,
        "build_us": performance.build_us,
        "persist_us": performance.persist_us,
        "provider_payload_build_us": performance.provider_payload_build_us,
        "provider_payload_serialize_us": performance.provider_payload_serialize_us,
        "provider_payload_bytes": performance.provider_payload_bytes,
        "source_message_count": performance.source_message_count,
        "tool_count": performance.tool_count,
        "item_count": performance.item_count,
        "warning_codes": performance
            .warning_codes
            .iter()
            .map(|code| sanitize_context_diagnostic_label(code))
            .collect::<Vec<_>>(),
    })
}

fn public_context_failure(failure: &ContextFailure) -> Value {
    let mut details = serde_json::Map::new();
    for key in [
        "model",
        "estimated_input_tokens",
        "input_limit_tokens",
        "receipt_count",
    ] {
        let Some(value) = failure.details.get(key) else {
            continue;
        };
        let value = if value.is_string() {
            Value::String(sanitize_context_diagnostic_label(
                value.as_str().unwrap_or_default(),
            ))
        } else if value.is_number() || value.is_boolean() {
            value.clone()
        } else {
            continue;
        };
        details.insert(key.to_string(), value);
    }
    json!({
        "schema_version": failure.schema_version,
        "code": sanitize_context_diagnostic_label(&failure.code),
        "stage": sanitize_context_diagnostic_label(&failure.stage),
        "message": sanitize_context_diagnostic_label(&failure.message),
        "retryable": failure.retryable,
        "recoverable": failure.recoverable,
        "details": details,
    })
}

fn context_replay_failure(status: &str, reasons: &[String]) -> Option<ContextFailure> {
    let reason_count = reasons.len();
    match status {
        "drifted" => Some(
            ContextFailure::new(
                ContextFailureCode::SourceDrift,
                "replay",
                "Context replay detected source drift.",
            )
            .with_details(BTreeMap::from([(
                "receipt_count".to_string(),
                json!(reason_count),
            )])),
        ),
        "unrecoverable" => Some(
            ContextFailure::new(
                ContextFailureCode::ReplayUnsupported,
                "replay",
                "Context replay cannot rebuild this receipt safely.",
            )
            .with_details(BTreeMap::from([(
                "receipt_count".to_string(),
                json!(reason_count),
            )])),
        ),
        _ => None,
    }
}

fn public_context_trace_entry(entry: &ContextPackTraceEntry) -> Value {
    json!({
        "kind": sanitize_context_diagnostic_label(&entry.kind),
        "source": sanitize_context_diagnostic_source(&entry.source),
        "priority": entry.priority,
        "pinned": entry.pinned,
        "stable_prefix": entry.stable_prefix,
        "token_estimate": entry.token_estimate,
        "included": entry.included,
        "drop_reason": entry.drop_reason.as_deref().map(sanitize_context_diagnostic_label),
        "delivery": entry.delivery,
        "truncated": entry.truncated,
        "original_token_estimate": entry.original_token_estimate,
        "truncation_reason": entry.truncation_reason.as_deref().map(sanitize_context_diagnostic_label),
        "truncation_strategy": entry.truncation_strategy.as_deref().map(sanitize_context_diagnostic_label),
        "semantic_duplicate": entry.semantic_duplicate_of.is_some(),
        "attachment": entry.attachment.as_ref().map(|attachment| json!({
            "id": attachment.id,
            "kind": attachment.kind,
            "name": attachment.name,
            "content_type": attachment.content_type,
            "size_bytes": attachment.size_bytes,
            "source": attachment.source.as_deref().map(sanitize_context_diagnostic_label),
            "page_count": attachment.page_count,
            "media_metadata": attachment.media_metadata.iter().filter(|(key, _)| {
                matches!(
                    key.as_str(),
                    "width_px" | "height_px" | "duration_ms" | "frame_count" | "dpi" | "orientation" | "extension"
                )
            }).map(|(key, value)| (key.clone(), value.clone())).collect::<BTreeMap<_, _>>(),
            "source_truncated": attachment.source_truncated,
            "source_truncation_reason": attachment.source_truncation_reason.as_deref().map(sanitize_context_diagnostic_label),
            "original_content_bytes": attachment.original_content_bytes,
            "included_content_bytes": attachment.included_content_bytes,
        })),
    })
}

fn public_context_prefix_cache(raw: Option<&Value>) -> Value {
    let raw = raw.and_then(Value::as_object);
    json!({
        "schema_version": "openagent.context_prefix_cache.v1",
        "scope": raw.and_then(|value| value.get("scope")).and_then(Value::as_str),
        "status": raw.and_then(|value| value.get("status")).and_then(Value::as_str),
        "cache_eligible": raw.and_then(|value| value.get("cache_eligible")).and_then(Value::as_bool).unwrap_or(false),
        "stable_prefix_hash": raw.and_then(|value| value.get("stable_prefix_hash")).and_then(Value::as_str),
        "stable_prefix_token_estimate": raw.and_then(|value| value.get("stable_prefix_token_estimate")).and_then(Value::as_u64).unwrap_or_default(),
        "retry_reuses_pack": raw.and_then(|value| value.get("retry_reuses_pack")).and_then(Value::as_bool).unwrap_or(false),
        "reused_from": raw.and_then(|value| value.get("reused_from")).and_then(Value::as_object).map(|value| json!({
            "run_id": value.get("run_id").and_then(Value::as_str),
            "step": value.get("step").and_then(Value::as_u64),
        })),
    })
}

fn sanitize_context_diagnostic_source(raw: &str) -> String {
    let without_query = raw.split(['?', '#']).next().unwrap_or_default().trim();
    let without_credentials = without_query
        .split_once("://")
        .map(|(scheme, rest)| {
            let host_and_path = rest.rsplit_once('@').map_or(rest, |(_, tail)| tail);
            format!("{scheme}://{host_and_path}")
        })
        .unwrap_or_else(|| without_query.to_string());
    sanitize_context_diagnostic_label(&without_credentials)
}

fn sanitize_context_diagnostic_label(raw: &str) -> String {
    let trimmed = raw.trim();
    let normalized = trimmed.to_ascii_lowercase();
    if [
        "authorization:",
        "api_key=",
        "apikey=",
        "access_token=",
        "auth_token=",
        "bearer ",
        "sk-",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
    {
        return "[redacted]".to_string();
    }
    trimmed.chars().take(240).collect()
}

fn context_updated_bridge_event(session: &Session, run_id: &str, step: u64) -> Option<Value> {
    let payload = context_diagnostics_payload_for_session(session, 1);
    let latest = payload.get("latest")?.clone();
    if latest.is_null() {
        return None;
    }
    Some(json!({
        "method": "context/updated",
        "params": {
            "thread_id": session.id,
            "session_id": session.id,
            "turn_id": run_id,
            "run_id": run_id,
            "step": step,
            "status": payload["status"],
            "diagnostics": latest,
        }
    }))
}

fn context_replayed_bridge_event(session: &Session, replay: &Value) -> Option<Value> {
    let replay_id = replay.get("replay_id").and_then(Value::as_str)?;
    Some(json!({
        "method": "context/replayed",
        "params": {
            "thread_id": session.id,
            "session_id": session.id,
            "replay_id": replay_id,
            "status": replay.get("status").and_then(Value::as_str),
            "reasons": replay.get("reasons").cloned().unwrap_or_else(|| json!([])),
            "failure": replay
                .get("failure")
                .and_then(|failure| serde_json::from_value::<ContextFailure>(failure.clone()).ok())
                .map(|failure| public_context_failure(&failure)),
            "side_effects": replay.get("side_effects").cloned().unwrap_or_else(|| json!({})),
        }
    }))
}

fn context_performance_bridge_event(
    session: &Session,
    run_id: &str,
    step: u64,
    performance: &ContextPackPerformance,
) -> Value {
    json!({
        "method": "context/performance",
        "params": {
            "thread_id": session.id,
            "session_id": session.id,
            "turn_id": run_id,
            "run_id": run_id,
            "step": step,
            "performance": public_context_performance(performance),
        }
    })
}

fn context_failed_bridge_event(
    session: &Session,
    run_id: &str,
    step: u64,
    failure: &ContextFailure,
) -> Value {
    json!({
        "method": "context/failed",
        "params": {
            "thread_id": session.id,
            "session_id": session.id,
            "turn_id": run_id,
            "run_id": run_id,
            "step": step,
            "failure": public_context_failure(failure),
        }
    })
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
    let checkpoints = store
        .list_checkpoints(session_id)
        .map_err(|error| error.to_string())?;
    let latest = checkpoints.first().cloned();
    Ok(json!({
        "session_id": session_id,
        "count": checkpoints.len(),
        "latest": latest,
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
    append_bridge_events(&store.root, session_id, &run_id, &mut events);
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

fn undo_session_file_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    body: &str,
) -> Result<Value, String> {
    let payload = serde_json::from_str::<Value>(body).unwrap_or_else(|_| json!({}));
    let requested_path = payload
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "path is required".to_string())?;
    let store = FileSessionStore::new(session_root(config));
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let target = resolve_path_in_root(&session.directory, requested_path)?;
    let path = session_display_path(&session, &target);
    let mut undo_stack = file_change_stack(&session, FILE_CHANGE_UNDO_STACK_KEY);
    let requested_run_id = payload
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let target_run_id = requested_run_id
        .map(str::to_string)
        .or_else(|| {
            undo_stack
                .iter()
                .rev()
                .find(|change| change.get("path").and_then(Value::as_str) == Some(path.as_str()))
                .map(file_change_run_id)
        })
        .ok_or_else(|| format!("no agent file change found for {path}"))?;
    let matches_target = |change: &Value| {
        change.get("path").and_then(Value::as_str) == Some(path.as_str())
            && file_change_run_id(change) == target_run_id
    };
    let matching = undo_stack
        .iter()
        .filter(|change| matches_target(change))
        .cloned()
        .collect::<Vec<_>>();
    let first = matching
        .first()
        .ok_or_else(|| format!("no agent file change found for {path}"))?;
    let latest = matching
        .last()
        .ok_or_else(|| format!("no agent file change found for {path}"))?;
    if !file_change_matches_state(&session, latest, FileChangeState::After)? {
        return Err(format!(
            "{path} changed after the agent edit; refusing to overwrite newer workspace content"
        ));
    }

    apply_file_change_state(&session, first, FileChangeState::Before)?;
    let matching_ids = matching
        .iter()
        .filter_map(|change| change.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    undo_stack.retain(|change| !matches_target(change));

    let mut grouped = latest.clone();
    if let (Some(grouped), Some(first)) = (grouped.as_object_mut(), first.as_object()) {
        grouped.insert("id".to_string(), json!(new_id("patch_file_undo")));
        grouped.insert(
            "existed_before".to_string(),
            first["existed_before"].clone(),
        );
        grouped.insert("before".to_string(), first["before"].clone());
        grouped.insert(
            "grouped_patch_ids".to_string(),
            Value::Array(matching_ids.iter().cloned().map(Value::String).collect()),
        );
    }
    let reverted = mark_file_change(grouped, "undone");
    let mut redo_stack = file_change_stack(&session, FILE_CHANGE_REDO_STACK_KEY);
    push_stack_entry(&mut redo_stack, reverted.clone());
    set_file_change_stack(&mut session, FILE_CHANGE_UNDO_STACK_KEY, undo_stack.clone());
    set_file_change_stack(&mut session, FILE_CHANGE_REDO_STACK_KEY, redo_stack.clone());
    if let Some(latest) = undo_stack.last() {
        session.metadata.insert(
            FILE_CHANGE_LATEST_KEY.to_string(),
            public_file_change(latest),
        );
    } else {
        session.metadata.remove(FILE_CHANGE_LATEST_KEY);
    }
    let turn_id = file_change_run_id(latest);
    let public = public_file_change(&reverted);
    let event = append_patch_stack_event(&store, &session, &turn_id, "patch/file_undone", &public);
    store
        .save_state(&session, Some(&turn_id))
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "session_id": session.id,
        "status": "file_undone",
        "path": path,
        "run_id": target_run_id,
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
        "patch/file_undone" => "patch.file_undone",
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
    append_bridge_events(&store.root, &session.id, turn_id, &mut events);
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

fn file_change_matches_state(
    session: &Session,
    change: &Value,
    state: FileChangeState,
) -> Result<bool, String> {
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
    if !should_exist {
        return Ok(!target.exists());
    }
    if !target.is_file() {
        return Ok(false);
    }
    let expected = change
        .get(content_key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("patch is missing {content_key} content"))?;
    Ok(fs::read_to_string(target).is_ok_and(|content| content == expected))
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
    let mut status = metadata
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
    let cancel_requested = session_task_cancel_requested(root, &session_id, &run_id);
    if cancel_requested
        && matches!(
            canonical_task_status(&status),
            "queued" | "running" | "waiting"
        )
    {
        status = "cancel_requested".to_string();
    }
    let canonical_status = canonical_task_status(&status);
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
    let agent_profile = metadata
        .get("agent_profile")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let final_result = metadata
        .get(FINAL_RESULT_METADATA_KEY)
        .filter(|result| {
            result
                .get("run_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == run_id)
        })
        .cloned()
        .unwrap_or(Value::Null);
    let progress = session_task_progress(&run_dir, &run_summary, &run_record, canonical_status);
    let failure = if canonical_status == "failed" {
        json!({
            "message": run_record.get("error").cloned().unwrap_or(Value::Null),
            "finish_reason": run_record.get("finish_reason").cloned().unwrap_or(Value::Null),
            "phase": progress.get("last_event").cloned().unwrap_or(Value::Null),
        })
    } else {
        Value::Null
    };
    let mut summary = json!({
        "id": session_id,
        "task_id": session_id,
        "session_id": session_id,
        "run_id": run_id,
        "status": status,
        "canonical_status": canonical_status,
        "session_status": state.get("status").cloned().unwrap_or_else(|| json!("idle")),
        "title": title,
        "description": metadata.get("task_description").cloned().unwrap_or(Value::Null),
        "subagent_type": subagent_type,
        "agent": metadata.get("agent").cloned().unwrap_or(Value::Null),
        "agent_profile": agent_profile,
        "background": metadata.get("background").cloned().unwrap_or(Value::Bool(false)),
        "execution_mode": metadata.get("execution_mode").cloned().unwrap_or_else(|| json!(if metadata.get("background").and_then(Value::as_bool).unwrap_or(false) { "background" } else { "foreground" })),
        "resume_count": metadata.get("resume_count").cloned().unwrap_or_else(|| json!(0)),
        "cancel_requested": cancel_requested,
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
        "error": if canonical_status == "failed" { run_record.get("error").cloned().unwrap_or(Value::Null) } else { Value::Null },
        "run_status": if run_status.is_empty() { Value::Null } else { json!(run_status) },
        "run": run_summary,
        "metadata": metadata,
    });
    if let Some(object) = summary.as_object_mut() {
        object.insert(
            "role".to_string(),
            json!({
                "id": subagent_type,
                "name": agent_profile.get("name").cloned().unwrap_or_else(|| json!(subagent_type)),
                "description": agent_profile.get("description").cloned().unwrap_or(Value::Null),
                "permission": agent_profile.get("permission").cloned().or_else(|| metadata.get("permission").cloned()).unwrap_or(Value::Null),
            }),
        );
        object.insert(
            "input".to_string(),
            json!({
                "summary": metadata.get("task_description").cloned().unwrap_or(Value::Null),
                "redacted": true,
            }),
        );
        object.insert(
            "allowed_tools".to_string(),
            agent_profile
                .get("tools")
                .cloned()
                .unwrap_or_else(|| json!([])),
        );
        object.insert("progress".to_string(), progress);
        object.insert("result".to_string(), final_result);
        object.insert("failure".to_string(), failure);
    }
    summary
}

fn session_task_progress(
    run_dir: &Path,
    run_summary: &Value,
    run_record: &Value,
    canonical_status: &str,
) -> Value {
    let events = read_jsonl_values(&run_dir.join("events.jsonl"));
    let completed_tool_calls = events
        .iter()
        .filter(|event| event.get("event").and_then(Value::as_str) == Some("tool.call.finished"))
        .count() as u64;
    let failed_tool_calls = events
        .iter()
        .filter(|event| event.get("event").and_then(Value::as_str) == Some("tool.call.failed"))
        .count() as u64;
    let last_event = events.last();
    let completed_steps = run_summary
        .get("step_count")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        .max(
            run_record
                .get("steps")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        );

    json!({
        "status": canonical_status,
        "completed_steps": completed_steps,
        "tool_call_count": run_summary.get("tool_call_count").cloned().unwrap_or_else(|| json!(completed_tool_calls + failed_tool_calls)),
        "completed_tool_calls": completed_tool_calls,
        "failed_tool_calls": failed_tool_calls,
        "event_count": run_summary.get("event_count").cloned().unwrap_or_else(|| json!(events.len())),
        "last_event": last_event.and_then(|event| event.get("event")).cloned().unwrap_or(Value::Null),
        "last_event_at_ms": last_event.and_then(|event| event.get("timestamp_ms")).cloned().unwrap_or(Value::Null),
        "started_at_ms": run_record.get("started_at_ms").cloned().unwrap_or(Value::Null),
        "ended_at_ms": run_record.get("ended_at_ms").cloned().unwrap_or(Value::Null),
        "duration_ms": run_record.get("duration_ms").cloned().unwrap_or(Value::Null),
    })
}

fn canonical_task_status(status: &str) -> &'static str {
    match status.trim().to_ascii_lowercase().as_str() {
        "queued" | "pending" => "queued",
        "running" | "in_progress" | "streaming" | "retrying" => "running",
        "waiting" | "waiting_approval" | "waiting_question" | "pending_approval"
        | "pending_question" | "blocked" | "cancel_requested" => "waiting",
        "completed" | "complete" | "success" | "succeeded" => "completed",
        "failed" | "error" | "expired" => "failed",
        "canceled" | "cancelled" | "interrupted" => "cancelled",
        _ => "unknown",
    }
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
    let model_was_explicit = payload
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        || session
            .metadata
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
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
    if model_was_explicit {
        session
            .metadata
            .insert("model".to_string(), json!(profile.model.clone()));
    }
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
    SessionRunnerFacade::estimated_turn_usage_payload(input, output, tool_calls)
}

fn trace_payload(session: &Session, run_id: &str, tool_calls: u64) -> Value {
    SessionRunnerFacade::new(PathBuf::new(), session.id.clone()).turn_trace_payload(
        run_id,
        &session_text_metadata(session, "agent", "server"),
        &session_text_metadata(session, "model", &default_model_id()),
        &session_text_metadata(session, "variant", "default"),
        &session_text_metadata(session, "thinking", "medium"),
        tool_calls,
    )
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
    payload_performance: RuntimeProviderPayloadPerformance,
}

#[derive(Clone, Debug, Default)]
struct RuntimeProviderPayloadPerformance {
    build_us: u64,
    serialize_us: u64,
    bytes: u64,
}

struct OpenAiRuntimeProviderRequest<'a> {
    provider: &'a str,
    model: &'a str,
    api_key: &'a str,
    base_url: &'a str,
    wire_api: &'a str,
    timeout_s: u64,
    stream: bool,
    context_pack: &'a ContextPack,
}

struct NativeRuntimeProviderRequest<'a> {
    provider: &'a str,
    model: &'a str,
    api_key: &'a str,
    base_url: &'a str,
    timeout_s: u64,
    stream: bool,
    context_pack: &'a ContextPack,
}

#[derive(Clone, Debug)]
struct RuntimeProviderCallError {
    message: String,
    retryable: bool,
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

struct RuntimeProviderTurnInput<'a> {
    store: &'a FileSessionStore,
    session: &'a mut Session,
    run_id: &'a str,
    step: u64,
    payload: &'a Value,
    tools: &'a [ToolSchema],
    mcp_runtime: Option<&'a RuntimeMcpRuntime>,
    agent_profile: Option<&'a RuntimeSubagentProfile>,
}

fn provider_turn_result(
    input: RuntimeProviderTurnInput<'_>,
    stream_sink: Option<&mut dyn FnMut(&ProviderStreamEvent)>,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<RuntimeProviderResult, String> {
    let RuntimeProviderTurnInput {
        store,
        session,
        run_id,
        step,
        payload,
        tools,
        mcp_runtime,
        agent_profile,
    } = input;
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .or_else(|| session.metadata.get("provider").and_then(Value::as_str));
    let provider_state = provider_state_for_root(&store.root);
    let provider_config = runtime_provider_config(
        Some(&provider_state),
        provider,
        Some(payload),
        Some(session),
    )?;
    let model_options = runtime_provider_model_options(session, payload);
    let context_model = runtime_context_model(&provider_config, session, payload);
    let context_budget_options = runtime_context_budget_options(session, payload);
    let context_budget =
        load_context_budget_options(Some(&context_budget_options), Some(&context_model))?;
    let context_pack_options =
        context_pack_build_options_for_model(Some(&context_budget_options), &context_model, false)?;
    let (mut context_pack, mut context_performance) = runtime_context_pack_for_agent_timed(
        store,
        session,
        tools,
        &model_options,
        mcp_runtime,
        agent_profile,
        context_pack_options.clone(),
    );
    let mut rebuild = None;
    if runtime_context_pack_needs_auto_compaction(&context_pack, &context_budget)
        && let Some(compaction) = runtime_auto_compact_context(
            store,
            session,
            run_id,
            step,
            &context_pack,
            &context_budget,
        )?
    {
        let before_receipt = context_pack.receipt.clone();
        let (rebuilt_pack, rebuilt_performance) = runtime_context_pack_for_agent_timed(
            store,
            session,
            tools,
            &model_options,
            mcp_runtime,
            agent_profile,
            context_pack_options.clone(),
        );
        context_pack = rebuilt_pack;
        context_performance.materialize_us = context_performance
            .materialize_us
            .saturating_add(rebuilt_performance.materialize_us);
        context_performance.build_us = context_performance
            .build_us
            .saturating_add(rebuilt_performance.build_us);
        context_performance.source_message_count = rebuilt_performance.source_message_count;
        context_performance.tool_count = rebuilt_performance.tool_count;
        context_performance.item_count = rebuilt_performance.item_count;
        context_performance.refresh_warnings();
        rebuild = Some(json!({
            "reason": compaction["reason"],
            "before_receipt": before_receipt,
            "after_receipt": context_pack.receipt,
            "compaction": compaction,
        }));
    }
    let replay_spec = runtime_context_replay_spec(
        store,
        session,
        &context_pack,
        mcp_runtime,
        agent_profile,
        context_pack_options,
    );
    let persist_started = Instant::now();
    runtime_persist_context_pack_receipt_with_diagnostics(
        store,
        session,
        run_id,
        step,
        &context_pack,
        rebuild.as_ref(),
        Some(&replay_spec),
        Some(&context_performance),
        None,
    )?;
    context_performance.persist_us = elapsed_micros(persist_started);
    context_performance.refresh_warnings();
    runtime_update_context_pack_diagnostics(
        store,
        session,
        run_id,
        step,
        Some(&context_performance),
        None,
    )?;
    if let Some(event) = context_updated_bridge_event(session, run_id, step) {
        append_bridge_events(&store.root, &session.id, run_id, &mut [event]);
    }
    if context_pack.budget.overflowed {
        let message = format!(
            "required context exceeds model input budget for `{}`: estimated_input_tokens={}, input_limit_tokens={}",
            provider_config.model,
            context_pack.estimated_input_tokens,
            context_pack.budget.input_limit_tokens.unwrap_or_default(),
        );
        let failure = ContextFailure::new(
            ContextFailureCode::BudgetExceeded,
            "budget",
            message.clone(),
        )
        .with_details(BTreeMap::from([
            ("model".to_string(), json!(provider_config.model)),
            (
                "estimated_input_tokens".to_string(),
                json!(context_pack.estimated_input_tokens),
            ),
            (
                "input_limit_tokens".to_string(),
                json!(context_pack.budget.input_limit_tokens.unwrap_or_default()),
            ),
        ]));
        runtime_update_context_pack_diagnostics(
            store,
            session,
            run_id,
            step,
            Some(&context_performance),
            Some(&failure),
        )?;
        let event = context_failed_bridge_event(session, run_id, step, &failure);
        append_bridge_events(&store.root, &session.id, run_id, &mut [event]);
        return Err(format!("[{}] {message}", failure.code));
    }
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
            payload_performance: RuntimeProviderPayloadPerformance::default(),
        });
    }
    let api_key = provider_config.api_key.clone().unwrap_or_default();
    let timeout = payload
        .get("timeout_s")
        .and_then(Value::as_u64)
        .unwrap_or(60);
    let stream = provider_streaming_enabled_for_turn(payload);
    let result = match provider_config.provider.as_str() {
        "anthropic" => call_anthropic_provider_for_runtime(
            NativeRuntimeProviderRequest {
                provider: &provider_config.provider,
                model: &provider_config.model,
                api_key: &api_key,
                base_url: &provider_config.base_url,
                timeout_s: timeout,
                stream,
                context_pack: &context_pack,
            },
            stream_sink,
            should_cancel,
        )
        .map_err(|error| error.message),
        "gemini" | "google" => call_gemini_provider_for_runtime(
            NativeRuntimeProviderRequest {
                provider: &provider_config.provider,
                model: &provider_config.model,
                api_key: &api_key,
                base_url: &provider_config.base_url,
                timeout_s: timeout,
                stream,
                context_pack: &context_pack,
            },
            stream_sink,
            should_cancel,
        )
        .map_err(|error| error.message),
        _ => call_openai_compatible_provider_for_runtime(
            OpenAiRuntimeProviderRequest {
                provider: &provider_config.provider,
                model: &provider_config.model,
                api_key: &api_key,
                base_url: &provider_config.base_url,
                wire_api: &provider_config.wire_api,
                timeout_s: timeout,
                stream,
                context_pack: &context_pack,
            },
            stream_sink,
            should_cancel,
        ),
    }?;
    context_performance.provider_payload_build_us = result.payload_performance.build_us;
    context_performance.provider_payload_serialize_us = result.payload_performance.serialize_us;
    context_performance.provider_payload_bytes = result.payload_performance.bytes;
    context_performance.refresh_warnings();
    runtime_update_context_pack_diagnostics(
        store,
        session,
        run_id,
        step,
        Some(&context_performance),
        None,
    )?;
    let event = context_performance_bridge_event(session, run_id, step, &context_performance);
    append_bridge_events(&store.root, &session.id, run_id, &mut [event]);
    Ok(result)
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

fn runtime_context_model(
    provider: &RuntimeProviderConfig,
    session: &Session,
    payload: &Value,
) -> Model {
    let context_window =
        runtime_context_limit_value(session, payload, "context_window").or_else(|| {
            std::env::var("OPENAGENT_CONTEXT_WINDOW")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        });
    let max_output =
        runtime_context_limit_value(session, payload, "max_output_tokens").or_else(|| {
            std::env::var("OPENAGENT_MAX_OUTPUT_TOKENS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        });
    openagent_context_model(
        &provider.provider,
        &provider.model,
        context_window,
        max_output,
    )
}

fn runtime_context_limit_value(session: &Session, payload: &Value, key: &str) -> Option<u64> {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .or_else(|| {
            payload
                .get("context_budget")
                .and_then(|value| value.get(key))
                .and_then(Value::as_u64)
        })
        .or_else(|| session.metadata.get(key).and_then(Value::as_u64))
        .or_else(|| {
            session
                .metadata
                .get("context_budget")
                .and_then(|value| value.get(key))
                .and_then(Value::as_u64)
        })
}

fn runtime_context_budget_options(session: &Session, payload: &Value) -> Value {
    let mut context_budget = Map::new();
    for source in [
        session.metadata.get("context_budget"),
        session
            .metadata
            .get("model_options")
            .and_then(|value| value.get("context_budget")),
        payload
            .get("model_options")
            .and_then(|value| value.get("context_budget")),
        payload
            .get("options")
            .and_then(|value| value.get("context_budget")),
        payload.get("context_budget"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_object)
    {
        context_budget.extend(source.clone());
    }
    json!({"context_budget": context_budget})
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
                        if runtime_model_option_allowed(nested_key) {
                            options.insert(nested_key.clone(), nested_value.clone());
                        }
                    }
                }
            } else if runtime_model_option_allowed(key) {
                options.insert(key.clone(), item.clone());
            }
        }
    }
}

fn merge_explicit_model_options_from_value(value: &Value, options: &mut BTreeMap<String, Value>) {
    for key in ["model_options", "options"] {
        if let Some(object) = value.get(key).and_then(Value::as_object) {
            for (option_key, option_value) in object {
                if runtime_model_option_allowed(option_key) {
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
            | "parallel_tool_calls"
            | "tool_call_dialect"
            | "stream"
            | "skill"
            | "skills"
            | "skill_roots"
            | "skill_permissions"
            | "skill_permission"
            | "context_budget"
            | "context_window"
    )
}

fn runtime_model_option_allowed(key: &str) -> bool {
    !matches!(
        key,
        "model"
            | "messages"
            | "input"
            | "tools"
            | "stream"
            | "skill"
            | "skills"
            | "skill_roots"
            | "skill_permissions"
            | "skill_permission"
            | "context_budget"
            | "context_window"
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

#[derive(Clone, Copy)]
enum NativeProviderAuth {
    Anthropic,
    Gemini,
}

fn call_anthropic_provider_for_runtime(
    request: NativeRuntimeProviderRequest<'_>,
    mut stream_sink: Option<&mut dyn FnMut(&ProviderStreamEvent)>,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<RuntimeProviderResult, RuntimeProviderCallError> {
    let NativeRuntimeProviderRequest {
        provider,
        model,
        api_key,
        base_url,
        timeout_s,
        stream,
        context_pack,
    } = request;
    context_pack
        .validate_provider_input()
        .map_err(|message| RuntimeProviderCallError {
            message,
            retryable: false,
        })?;
    let messages = context_pack.messages.as_slice();
    let tools = context_pack.tools.as_slice();
    let model_options = &context_pack.model_options;
    let dialect = tool_call_dialect_from_options(provider, "messages", model_options)
        .map_err(non_retryable_provider_error)?;
    let tool_policy = negotiate_tool_call_policy(
        tool_call_policy_from_options(model_options),
        provider_capabilities(provider, dialect),
        tools,
    )
    .map_err(non_retryable_provider_error)?
    .effective;
    let payload_build_started = Instant::now();
    let mut config = AnthropicLanguageModelConfig::new(api_key, model);
    config.base_url = Some(base_url.to_string());
    let mut payload = build_anthropic_payload_with_policy(
        &config,
        runtime_system_prompt_from_messages(messages).as_deref(),
        messages,
        tools,
        None,
        None,
        Some(model_options),
        &tool_policy,
    );
    if let Some(object) = payload.as_object_mut() {
        object.insert("stream".to_string(), json!(stream));
    }
    let payload_performance = provider_payload_performance(payload_build_started, &payload)?;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(timeout_s.max(1)))
        .build()
        .map_err(|error| non_retryable_provider_error(error.to_string()))?;
    let endpoint = join_url(base_url, "messages");
    let response = send_runtime_native_provider_request(
        &client,
        &endpoint,
        api_key,
        &payload,
        stream,
        runtime_provider_request_retries(),
        model,
        NativeProviderAuth::Anthropic,
        &mut stream_sink,
    )?;
    let status = response.status();
    let content_type = response_content_type(&response);
    let events = if stream && content_type.contains("text/event-stream") {
        if !status.is_success() {
            let raw = response
                .text()
                .map_err(|error| provider_read_error(error, Some(status.as_u16())))?;
            return Err(provider_http_error(status.as_u16(), &raw, &content_type));
        }
        let mut chunks = Vec::new();
        read_sse_json_values_stream(response, |chunk| {
            if should_cancel.is_some_and(|cancelled| cancelled()) {
                return Err(TURN_INTERRUPTED_ERROR.to_string());
            }
            if let Some(event) = anthropic_stream_text_delta(&chunk)
                && let Some(sink) = stream_sink.as_deref_mut()
            {
                sink(&event);
            }
            chunks.push(chunk);
            Ok(())
        })
        .map_err(provider_stream_error)?;
        normalize_anthropic_events(&chunks)
    } else {
        let raw = response
            .text()
            .map_err(|error| provider_read_error(error, Some(status.as_u16())))?;
        if !status.is_success() {
            return Err(provider_http_error(status.as_u16(), &raw, &content_type));
        }
        let value: Value = serde_json::from_str(&raw).map_err(|error| {
            non_retryable_provider_error(format!("anthropic response was not JSON: {error}"))
        })?;
        normalize_anthropic_response(&value)
    };
    let events = apply_tool_call_dialect(events, dialect);
    Ok(runtime_result_with_payload_performance(
        provider_events_to_runtime_result(
            &events,
            if stream {
                "anthropic:messages:stream".to_string()
            } else {
                "anthropic:messages".to_string()
            },
            None,
        )?,
        payload_performance,
    ))
}

fn call_gemini_provider_for_runtime(
    request: NativeRuntimeProviderRequest<'_>,
    mut stream_sink: Option<&mut dyn FnMut(&ProviderStreamEvent)>,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<RuntimeProviderResult, RuntimeProviderCallError> {
    let NativeRuntimeProviderRequest {
        provider,
        model,
        api_key,
        base_url,
        timeout_s,
        stream,
        context_pack,
    } = request;
    context_pack
        .validate_provider_input()
        .map_err(non_retryable_provider_error)?;
    let messages = context_pack.messages.as_slice();
    let tools = context_pack.tools.as_slice();
    let model_options = &context_pack.model_options;
    let dialect = tool_call_dialect_from_options(provider, "generate_content", model_options)
        .map_err(non_retryable_provider_error)?;
    let tool_policy = negotiate_tool_call_policy(
        tool_call_policy_from_options(model_options),
        provider_capabilities(provider, dialect),
        tools,
    )
    .map_err(non_retryable_provider_error)?
    .effective;
    let payload_build_started = Instant::now();
    let mut config = GeminiLanguageModelConfig::new(api_key, model);
    config.base_url = base_url.to_string();
    let payload = build_gemini_payload(
        runtime_system_prompt_from_messages(messages).as_deref(),
        messages,
        tools,
        Some(model_options),
        &tool_policy,
    );
    let payload_performance = provider_payload_performance(payload_build_started, &payload)?;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(timeout_s.max(1)))
        .build()
        .map_err(|error| non_retryable_provider_error(error.to_string()))?;
    let response = send_runtime_native_provider_request(
        &client,
        &config.endpoint(stream),
        api_key,
        &payload,
        stream,
        runtime_provider_request_retries(),
        model,
        NativeProviderAuth::Gemini,
        &mut stream_sink,
    )?;
    let status = response.status();
    let content_type = response_content_type(&response);
    let events = if stream && content_type.contains("text/event-stream") {
        if !status.is_success() {
            let raw = response
                .text()
                .map_err(|error| provider_read_error(error, Some(status.as_u16())))?;
            return Err(provider_http_error(status.as_u16(), &raw, &content_type));
        }
        let mut chunks = Vec::new();
        read_sse_json_values_stream(response, |chunk| {
            if should_cancel.is_some_and(|cancelled| cancelled()) {
                return Err(TURN_INTERRUPTED_ERROR.to_string());
            }
            for event in normalize_gemini_events(std::slice::from_ref(&chunk)) {
                if matches!(
                    event,
                    ProviderStreamEvent::TextDelta { .. }
                        | ProviderStreamEvent::ReasoningDelta { .. }
                ) && let Some(sink) = stream_sink.as_deref_mut()
                {
                    sink(&event);
                }
            }
            chunks.push(chunk);
            Ok(())
        })
        .map_err(provider_stream_error)?;
        normalize_gemini_events(&chunks)
    } else {
        let raw = response
            .text()
            .map_err(|error| provider_read_error(error, Some(status.as_u16())))?;
        if !status.is_success() {
            return Err(provider_http_error(status.as_u16(), &raw, &content_type));
        }
        let value: Value = serde_json::from_str(&raw).map_err(|error| {
            non_retryable_provider_error(format!("gemini response was not JSON: {error}"))
        })?;
        normalize_gemini_events(&[value])
    };
    let events = apply_tool_call_dialect(events, dialect);
    Ok(runtime_result_with_payload_performance(
        provider_events_to_runtime_result(
            &events,
            if stream {
                "gemini:generate_content:stream".to_string()
            } else {
                "gemini:generate_content".to_string()
            },
            None,
        )?,
        payload_performance,
    ))
}

fn provider_payload_performance(
    started: Instant,
    payload: &Value,
) -> Result<RuntimeProviderPayloadPerformance, RuntimeProviderCallError> {
    let build_us = elapsed_micros(started);
    let serialize_started = Instant::now();
    let bytes = serde_json::to_vec(payload)
        .map_err(|error| {
            non_retryable_provider_error(format!("provider payload serialization failed: {error}"))
        })?
        .len()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(RuntimeProviderPayloadPerformance {
        build_us,
        serialize_us: elapsed_micros(serialize_started),
        bytes,
    })
}

fn response_content_type(response: &reqwest::blocking::Response) -> String {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn non_retryable_provider_error(message: impl Into<String>) -> RuntimeProviderCallError {
    RuntimeProviderCallError {
        message: message.into(),
        retryable: false,
    }
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
        context_pack,
    } = request;
    context_pack.validate_provider_input()?;
    let messages = context_pack.messages.as_slice();
    let tools = context_pack.tools.as_slice();
    let model_options = &context_pack.model_options;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(timeout_s.max(1)))
        .build()
        .map_err(|error| error.to_string())?;
    let system_prompt = runtime_system_prompt_from_messages(messages);
    let models = runtime_provider_model_candidates(model);
    let mut last_error = None;
    for (index, candidate_model) in models.iter().enumerate() {
        let result = call_openai_compatible_provider_model(
            &client,
            provider,
            candidate_model,
            api_key,
            base_url,
            wire_api,
            stream,
            messages,
            tools,
            model_options,
            system_prompt.as_deref(),
            &mut stream_sink,
            should_cancel,
        );
        match result {
            Ok(mut result) => {
                if candidate_model != model {
                    result.source = format!(
                        "{};primary_model={};fallback_model={}",
                        result.source, model, candidate_model
                    );
                }
                return Ok(result);
            }
            Err(error) => {
                let can_try_next = error.retryable && index + 1 < models.len();
                if can_try_next && let Some(sink) = stream_sink.as_deref_mut() {
                    sink(&ProviderStreamEvent::Reset {
                        model: candidate_model.clone(),
                        reason: error.message.clone(),
                    });
                    sink(&ProviderStreamEvent::Fallback {
                        from_model: candidate_model.clone(),
                        to_model: models[index + 1].clone(),
                        reason: error.message.clone(),
                    });
                }
                last_error = Some(error);
                if can_try_next {
                    continue;
                }
                break;
            }
        }
    }
    Err(last_error
        .map(|error| error.message)
        .unwrap_or_else(|| "provider request failed".to_string()))
}

#[allow(clippy::too_many_arguments)]
fn call_openai_compatible_provider_model(
    client: &reqwest::blocking::Client,
    provider: &str,
    model: &str,
    api_key: &str,
    base_url: &str,
    wire_api: &str,
    stream: bool,
    messages: &[ChatMessage],
    tools: &[openagent_protocol::ToolSchema],
    model_options: &BTreeMap<String, Value>,
    system_prompt: Option<&str>,
    stream_sink: &mut Option<&mut dyn FnMut(&ProviderStreamEvent)>,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<RuntimeProviderResult, RuntimeProviderCallError> {
    let payload_build_started = Instant::now();
    let mut config = OpenAiLanguageModelConfig::new(api_key, model);
    config.provider_id = provider.to_string();
    config.base_url = base_url.to_string();
    config.wire_api = wire_api.to_string();
    let dialect =
        tool_call_dialect_from_options(provider, wire_api, model_options).map_err(|message| {
            RuntimeProviderCallError {
                message,
                retryable: false,
            }
        })?;
    let requested_tool_policy = tool_call_policy_from_options(model_options);
    let tool_policy = negotiate_tool_call_policy(
        requested_tool_policy,
        provider_capabilities(provider, dialect),
        tools,
    )
    .map_err(|message| RuntimeProviderCallError {
        message,
        retryable: false,
    })?
    .effective;
    let (endpoint, mut payload) = if wire_api == "chat" {
        let mut payload = build_openai_chat_payload_with_policy(
            &config,
            system_prompt,
            messages,
            tools,
            None,
            None,
            None,
            &tool_policy,
        );
        if let Some(object) = payload.as_object_mut() {
            object.insert("stream".to_string(), json!(stream));
        }
        (join_url(base_url, "chat/completions"), payload)
    } else {
        let mut payload = build_openai_responses_payload_with_policy(
            &config,
            system_prompt,
            messages,
            tools,
            None,
            None,
            &tool_policy,
        );
        if let Some(object) = payload.as_object_mut() {
            object.insert("stream".to_string(), json!(stream));
        }
        (join_url(base_url, "responses"), payload)
    };
    apply_runtime_model_options_to_payload(&mut payload, model_options);
    let payload_build_us = elapsed_micros(payload_build_started);
    let payload_serialize_started = Instant::now();
    let payload_bytes = serde_json::to_vec(&payload)
        .map_err(|error| RuntimeProviderCallError {
            message: format!("provider payload serialization failed: {error}"),
            retryable: false,
        })?
        .len()
        .try_into()
        .unwrap_or(u64::MAX);
    let payload_performance = RuntimeProviderPayloadPerformance {
        build_us: payload_build_us,
        serialize_us: elapsed_micros(payload_serialize_started),
        bytes: payload_bytes,
    };
    let response = send_runtime_provider_request(
        client,
        &endpoint,
        api_key,
        &payload,
        stream,
        runtime_provider_request_retries(),
        model,
        stream_sink,
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
                .map_err(|error| provider_read_error(error, Some(status.as_u16())))?;
            return Err(provider_http_error(status.as_u16(), &raw, &content_type));
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
        })
        .map_err(provider_stream_error)?;
        let events = if wire_api == "chat" {
            normalize_openai_chat_sse_chunks(&chunks)
        } else {
            normalize_openai_responses_stream_events(&chunks)
        };
        let events = apply_tool_call_dialect(events, dialect);
        return Ok(runtime_result_with_payload_performance(
            provider_events_to_runtime_result(
                &events,
                format!("{provider}:{wire_api}:stream"),
                None,
            )?,
            payload_performance,
        ));
    }
    let raw = response
        .text()
        .map_err(|error| provider_read_error(error, Some(status.as_u16())))?;
    if !status.is_success() {
        return Err(provider_http_error(status.as_u16(), &raw, &content_type));
    }
    let value: Value = serde_json::from_str(&raw).map_err(|error| RuntimeProviderCallError {
        message: format!("provider response was not JSON: {error}"),
        retryable: false,
    })?;
    if wire_api == "chat" {
        let events = apply_tool_call_dialect(normalize_openai_chat_response(&value), dialect);
        Ok(runtime_result_with_payload_performance(
            provider_events_to_runtime_result(&events, format!("{provider}:chat"), Some(&value))?,
            payload_performance,
        ))
    } else {
        let events = apply_tool_call_dialect(normalize_openai_responses_response(&value), dialect);
        Ok(runtime_result_with_payload_performance(
            provider_events_to_runtime_result(
                &events,
                format!("{provider}:responses"),
                Some(&value),
            )?,
            payload_performance,
        ))
    }
}

fn runtime_result_with_payload_performance(
    mut result: RuntimeProviderResult,
    performance: RuntimeProviderPayloadPerformance,
) -> RuntimeProviderResult {
    result.payload_performance = performance;
    result
}

fn runtime_provider_model_candidates(primary_model: &str) -> Vec<String> {
    runtime_provider_model_candidates_with_fallback(
        primary_model,
        std::env::var("OPENAGENT_PROVIDER_FALLBACK_MODELS")
            .ok()
            .as_deref(),
    )
}

fn runtime_provider_model_candidates_with_fallback(
    primary_model: &str,
    configured_fallbacks: Option<&str>,
) -> Vec<String> {
    let primary = if primary_model.trim().is_empty() {
        "gpt-5.5".to_string()
    } else {
        primary_model.trim().to_string()
    };
    let configured = configured_fallbacks
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|model| !model.is_empty() && !runtime_image_model_supported(model))
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|models| !models.is_empty())
        .unwrap_or_else(|| {
            if runtime_text_model_supported(&primary) {
                match primary.as_str() {
                    "gpt-5.5" => vec!["gpt-5.4".to_string()],
                    "gpt-5.4" => vec!["gpt-5.5".to_string()],
                    "gpt-5.6-sol" => vec!["gpt-5.6-terra".to_string(), "gpt-5.6-luna".to_string()],
                    "gpt-5.6-terra" => vec!["gpt-5.6-sol".to_string(), "gpt-5.6-luna".to_string()],
                    "gpt-5.6-luna" => vec!["gpt-5.6-sol".to_string(), "gpt-5.6-terra".to_string()],
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            }
        });
    let mut models = Vec::with_capacity(1 + configured.len());
    models.push(primary);
    for model in configured {
        if !models.iter().any(|existing| existing == &model) {
            models.push(model);
        }
        if models.len() > MAX_PROVIDER_FALLBACK_MODELS {
            break;
        }
    }
    models
}

fn provider_http_error(status: u16, raw: &str, content_type: &str) -> RuntimeProviderCallError {
    RuntimeProviderCallError {
        message: format!(
            "provider returned HTTP {}: {}",
            status,
            summarize_http_error_body(raw, content_type)
        ),
        retryable: runtime_provider_status_retryable(status),
    }
}

fn provider_read_error(error: reqwest::Error, status: Option<u16>) -> RuntimeProviderCallError {
    RuntimeProviderCallError {
        message: format!("provider response read failed: {error}"),
        retryable: status.is_none_or(|status| {
            (200..=299).contains(&status) || runtime_provider_status_retryable(status)
        }),
    }
}

fn provider_stream_error(error: String) -> RuntimeProviderCallError {
    let interrupted = is_turn_interrupted_error(&error);
    let retryable = !interrupted
        && (error.contains("timed out")
            || error.contains("connection")
            || error.contains("reset")
            || error.contains("closed")
            || error.contains("SSE read failed")
            || error.contains("unexpected EOF")
            || error.contains("end of file"));
    RuntimeProviderCallError {
        message: error,
        retryable,
    }
}

#[allow(clippy::too_many_arguments)]
fn send_runtime_provider_request(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    api_key: &str,
    payload: &Value,
    stream: bool,
    max_retries: u64,
    model: &str,
    stream_sink: &mut Option<&mut dyn FnMut(&ProviderStreamEvent)>,
) -> Result<reqwest::blocking::Response, RuntimeProviderCallError> {
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
                    let delay = runtime_provider_retry_delay(attempt);
                    if let Some(sink) = stream_sink.as_deref_mut() {
                        sink(&ProviderStreamEvent::Retry {
                            attempt: attempt + 1,
                            max_attempts: max_retries + 1,
                            delay_ms: delay.as_millis() as u64,
                            model: model.to_string(),
                            reason: format!("provider returned HTTP {status}"),
                        });
                    }
                    thread::sleep(delay);
                    continue;
                }
                return Ok(response);
            }
            Err(error) if attempt < max_retries => {
                attempt += 1;
                let delay = runtime_provider_retry_delay(attempt);
                if let Some(sink) = stream_sink.as_deref_mut() {
                    sink(&ProviderStreamEvent::Retry {
                        attempt: attempt + 1,
                        max_attempts: max_retries + 1,
                        delay_ms: delay.as_millis() as u64,
                        model: model.to_string(),
                        reason: format!("provider request failed: {error}"),
                    });
                }
                thread::sleep(delay);
            }
            Err(error) => {
                return Err(RuntimeProviderCallError {
                    message: format!("provider request failed: {error}"),
                    retryable: true,
                });
            }
        }
    }
}

fn runtime_provider_status_retryable(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn runtime_provider_retry_delay(attempt: u64) -> Duration {
    Duration::from_millis(750 * (1_u64 << attempt.saturating_sub(1).min(3)))
}

#[allow(clippy::too_many_arguments)]
fn send_runtime_native_provider_request(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    api_key: &str,
    payload: &Value,
    stream: bool,
    max_retries: u64,
    model: &str,
    auth: NativeProviderAuth,
    stream_sink: &mut Option<&mut dyn FnMut(&ProviderStreamEvent)>,
) -> Result<reqwest::blocking::Response, RuntimeProviderCallError> {
    let mut attempt = 0_u64;
    loop {
        let request = client
            .post(endpoint)
            .header("content-type", "application/json");
        let mut request = match auth {
            NativeProviderAuth::Anthropic => request
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01"),
            NativeProviderAuth::Gemini => request.header("x-goog-api-key", api_key),
        };
        if stream {
            request = request.header("accept", "text/event-stream");
        }
        match request.json(payload).send() {
            Ok(response) => {
                let status = response.status().as_u16();
                if runtime_provider_status_retryable(status) && attempt < max_retries {
                    attempt += 1;
                    let delay = runtime_provider_retry_delay(attempt);
                    if let Some(sink) = stream_sink.as_deref_mut() {
                        sink(&ProviderStreamEvent::Retry {
                            attempt: attempt + 1,
                            max_attempts: max_retries + 1,
                            delay_ms: delay.as_millis() as u64,
                            model: model.to_string(),
                            reason: format!("provider returned HTTP {status}"),
                        });
                    }
                    thread::sleep(delay);
                    continue;
                }
                return Ok(response);
            }
            Err(error) if attempt < max_retries => {
                attempt += 1;
                let delay = runtime_provider_retry_delay(attempt);
                if let Some(sink) = stream_sink.as_deref_mut() {
                    sink(&ProviderStreamEvent::Retry {
                        attempt: attempt + 1,
                        max_attempts: max_retries + 1,
                        delay_ms: delay.as_millis() as u64,
                        model: model.to_string(),
                        reason: error.to_string(),
                    });
                }
                thread::sleep(delay);
            }
            Err(error) => {
                return Err(RuntimeProviderCallError {
                    message: format!("provider request failed: {error}"),
                    retryable: true,
                });
            }
        }
    }
}

fn runtime_provider_request_retries() -> u64 {
    std::env::var("OPENAGENT_PROVIDER_RETRIES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_PROVIDER_REQUEST_RETRIES)
        .min(MAX_PROVIDER_REQUEST_RETRIES)
}

fn runtime_manual_turn_retries() -> u64 {
    std::env::var("OPENAGENT_MANUAL_TURN_RETRIES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MANUAL_TURN_RETRIES)
        .clamp(1, MAX_MANUAL_TURN_RETRIES)
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
    if wire_api == "chat" {
        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|items| items.first());
        let delta = choice.and_then(|choice| choice.get("delta"));
        let text = delta
            .and_then(|delta| delta.get("content"))
            .or_else(|| choice.and_then(|choice| choice.get("text")))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !text.is_empty() {
            return Some(ProviderStreamEvent::TextDelta {
                text: text.to_string(),
            });
        }
        let reasoning = delta
            .and_then(|delta| {
                delta
                    .get("reasoning_content")
                    .or_else(|| delta.get("reasoning"))
                    .or_else(|| delta.get("thinking"))
            })
            .and_then(Value::as_str)
            .unwrap_or_default();
        return (!reasoning.is_empty()).then(|| ProviderStreamEvent::ReasoningDelta {
            text: reasoning.to_string(),
        });
    }
    let text = if matches!(
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

fn anthropic_stream_text_delta(chunk: &Value) -> Option<ProviderStreamEvent> {
    let text = match chunk
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "content_block_start" => chunk
            .get("content_block")
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|block| block.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "content_block_delta" => chunk
            .get("delta")
            .filter(|delta| delta.get("type").and_then(Value::as_str) == Some("text_delta"))
            .and_then(|delta| delta.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        _ => "",
    };
    (!text.is_empty()).then(|| ProviderStreamEvent::TextDelta {
        text: text.to_string(),
    })
}

fn provider_reasoning_heartbeat_event(
    session_id: &str,
    run_id: &str,
    step: u64,
    reasoning_chars: u64,
) -> Value {
    json!({
        "method": "item/agentMessage/thinking",
        "params": {
            "thread_id": session_id,
            "session_id": session_id,
            "turn_id": run_id,
            "run_id": run_id,
            "step": step,
            "status": "thinking",
            "reasoning_chars": reasoning_chars,
        }
    })
}

fn should_emit_reasoning_heartbeat(reasoning_chars: u64, last_emitted_chars: u64) -> bool {
    last_emitted_chars == 0 || reasoning_chars.saturating_sub(last_emitted_chars) >= 80
}

fn provider_events_to_runtime_result(
    events: &[ProviderStreamEvent],
    source: String,
    fallback: Option<&Value>,
) -> Result<RuntimeProviderResult, RuntimeProviderCallError> {
    let mut answer = String::new();
    let mut tool_calls = Vec::new();
    let mut usage = Usage::default();
    let mut finish_reason = "stop".to_string();
    for event in events {
        match event {
            ProviderStreamEvent::TextDelta { text } => answer.push_str(text),
            ProviderStreamEvent::ReasoningDelta { .. } => {}
            ProviderStreamEvent::Retry { .. }
            | ProviderStreamEvent::Reset { .. }
            | ProviderStreamEvent::Fallback { .. } => {}
            ProviderStreamEvent::ToolCall {
                call_id,
                name,
                input,
            } => tool_calls.push(ToolCall {
                call_id: call_id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            ProviderStreamEvent::ToolCallError { error } => {
                return Err(RuntimeProviderCallError {
                    message: format!(
                        "provider tool call assembly failed [{}]: {}",
                        error.code, error.message
                    ),
                    retryable: false,
                });
            }
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
    Ok(RuntimeProviderResult {
        answer,
        tool_calls,
        usage,
        source,
        finish_reason,
        payload_performance: RuntimeProviderPayloadPerformance::default(),
    })
}

fn provider_max_steps(payload: &Value) -> u64 {
    let env_max_steps = std::env::var("OPENAGENT_BRIDGE_MAX_STEPS").ok();
    provider_max_steps_with_env(payload, env_max_steps.as_deref())
}

fn provider_max_steps_with_env(payload: &Value, env_max_steps: Option<&str>) -> u64 {
    payload
        .get("max_steps")
        .or_else(|| payload.get("maxSteps"))
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .or_else(|| {
            env_max_steps
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
        })
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

fn runtime_turn_message_id(run_id: &str, kind: &str, step: u64) -> String {
    format!("msg_{run_id}_{kind}_{step}")
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
    sync_plugin_runtime_metadata(config, session);
    let max_steps = provider_max_steps(payload);
    let agent_profile = runtime_agent_profile_for_session(session);
    let mut toolkit = toolkit_with_runtime_task_tool(session, agent_profile.as_ref());
    let mcp_runtime = register_runtime_mcp_tools(config, &session.directory, &mut toolkit);
    let visible_tools = filter_runtime_tools_for_capabilities(
        &store.root,
        filter_runtime_tools_for_profile(toolkit.get_all_tools("local"), agent_profile.as_ref()),
    );
    let visible_tool_names = visible_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let mut runner_facade = runtime_session_runner_facade(
        session,
        agent_profile.as_ref(),
        permission_ruleset.clone(),
        skip_permissions,
    );
    if let Some(value) = payload
        .get("question_answers")
        .or_else(|| payload.get("answers"))
    {
        runner_facade = runner_facade.with_question_answers_value(value);
    }
    let mut ctx = runner_facade.tool_context();
    let mut persisted_events = 0;
    append_unpersisted_bridge_events(
        &store.root,
        &session.id,
        run_id,
        &mut events,
        &mut persisted_events,
    );
    while carry.next_step <= max_steps {
        if turn_cancel_requested(run_id)
            || session_task_cancel_requested(&store.root, &session.id, run_id)
        {
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
        let assistant_message_id = runtime_turn_message_id(run_id, "assistant", step);
        runtime_record_step_started(store, &session.id, run_id, step, None);
        let mut streamed_text = false;
        let mut reasoning_chars = 0_u64;
        let mut last_reasoning_heartbeat_chars = 0_u64;
        let session_id = session.id.clone();
        let root = store.root.clone();
        let mut on_provider_stream = |event: &ProviderStreamEvent| match event {
            ProviderStreamEvent::TextDelta { text } if !text.is_empty() => {
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
                append_unpersisted_bridge_events(
                    &root,
                    &session_id,
                    run_id,
                    &mut events,
                    &mut persisted_events,
                );
            }
            ProviderStreamEvent::ReasoningDelta { text } if !text.is_empty() => {
                reasoning_chars = reasoning_chars.saturating_add(text.chars().count() as u64);
                if should_emit_reasoning_heartbeat(reasoning_chars, last_reasoning_heartbeat_chars)
                {
                    last_reasoning_heartbeat_chars = reasoning_chars;
                    events.push(provider_reasoning_heartbeat_event(
                        &session_id,
                        run_id,
                        step,
                        reasoning_chars,
                    ));
                    append_unpersisted_bridge_events(
                        &root,
                        &session_id,
                        run_id,
                        &mut events,
                        &mut persisted_events,
                    );
                }
            }
            ProviderStreamEvent::Retry {
                attempt,
                max_attempts,
                delay_ms,
                model,
                reason,
            } => {
                events.push(json!({
                    "method": "turn/retrying",
                    "params": {
                        "thread_id": session_id.clone(),
                        "session_id": session_id.clone(),
                        "turn_id": run_id,
                        "run_id": run_id,
                        "step": step,
                        "status": "retrying",
                        "attempt": attempt,
                        "max_attempts": max_attempts,
                        "delay_ms": delay_ms,
                        "model": model,
                        "reason": reason,
                        "retryable": true,
                    }
                }));
                append_unpersisted_bridge_events(
                    &root,
                    &session_id,
                    run_id,
                    &mut events,
                    &mut persisted_events,
                );
            }
            ProviderStreamEvent::Reset { model, reason } => {
                streamed_text = false;
                events.push(json!({
                    "method": "item/agentMessage/reset",
                    "params": {
                        "thread_id": session_id.clone(),
                        "session_id": session_id.clone(),
                        "turn_id": run_id,
                        "run_id": run_id,
                        "step": step,
                        "status": "running",
                        "model": model,
                        "reason": reason,
                    }
                }));
                append_unpersisted_bridge_events(
                    &root,
                    &session_id,
                    run_id,
                    &mut events,
                    &mut persisted_events,
                );
            }
            ProviderStreamEvent::Fallback {
                from_model,
                to_model,
                reason,
            } => {
                events.push(json!({
                    "method": "turn/fallback",
                    "params": {
                        "thread_id": session_id.clone(),
                        "session_id": session_id.clone(),
                        "turn_id": run_id,
                        "run_id": run_id,
                        "step": step,
                        "status": "running",
                        "from_model": from_model,
                        "to_model": to_model,
                        "reason": reason,
                        "retryable": true,
                    }
                }));
                append_unpersisted_bridge_events(
                    &root,
                    &session_id,
                    run_id,
                    &mut events,
                    &mut persisted_events,
                );
            }
            _ => {}
        };
        let should_cancel = || {
            turn_cancel_requested(run_id)
                || session_task_cancel_requested(&store.root, &session_id, run_id)
        };
        let provider_result = match provider_turn_result(
            RuntimeProviderTurnInput {
                store,
                session,
                run_id,
                step,
                payload,
                tools: &visible_tools,
                mcp_runtime: mcp_runtime.as_ref(),
                agent_profile: agent_profile.as_ref(),
            },
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

        let step_outcome = SessionRunnerFacade::provider_step_outcome(
            provider_result.tool_calls.len() as u64,
            &provider_result.finish_reason,
        );
        if step_outcome.is_complete() {
            return finish_provider_loop(
                store,
                session,
                run_id,
                events,
                &mut persisted_events,
                carry,
                &step_outcome.finish_reason,
            );
        }
        debug_assert!(step_outcome.continues_with_tools());

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
    let outcome = SessionRunnerFacade::failed_turn_outcome(
        max_steps,
        "max_steps",
        "agent loop exceeded max_steps",
    );
    let _ = store.finish_run(
        session,
        run_id,
        &outcome.run_status,
        outcome.steps,
        Some(&outcome.finish_reason),
        outcome.error.as_deref(),
    );
    let usage = usage_value_from_provider(
        &carry.usage,
        carry.tool_calls,
        &latest_user_message(session),
        &carry.answer,
    );
    let trace = trace_payload(session, run_id, carry.tool_calls);
    events.push(
        SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
            .turn_terminal_event(
                &outcome.event_method,
                run_id,
                &outcome.event_status,
                false,
                true,
                false,
                BTreeMap::from([
                    ("error".to_string(), json!("agent loop exceeded max_steps")),
                    ("usage".to_string(), usage.clone()),
                    ("trace".to_string(), trace.clone()),
                ]),
            ),
    );
    append_unpersisted_bridge_events(
        &store.root,
        &session.id,
        run_id,
        &mut events,
        &mut persisted_events,
    );
    Ok(json!({
        "session_id": session.id,
        "turn_id": run_id,
        "status": outcome.event_status,
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
    if let Some(result) = capability_gate_for_tool(task_context.config, tool_call, ctx) {
        return result;
    }
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

const MAX_CONTEXT_PACK_RECEIPTS: usize = 64;
const CONTEXT_REPLAY_SPEC_SCHEMA_VERSION: &str = "openagent.context_replay_spec.v1";
const CONTEXT_REPLAY_RESULT_SCHEMA_VERSION: &str = "openagent.context_replay.v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RuntimeContextReplaySpec {
    schema_version: String,
    materialized_message_count: usize,
    tools: Vec<ToolSchema>,
    model_options: BTreeMap<String, Value>,
    unsafe_model_option_keys: Vec<String>,
    build_options: ContextPackBuildOptions,
    todos: Vec<ContextTodo>,
    checkpoints: Vec<ContextCheckpoint>,
    tool_manifests: Vec<ContextItem>,
    work_state: Option<ContextWorkState>,
    #[serde(default)]
    goal: Option<DurableGoal>,
    #[serde(default)]
    plan: Option<DurablePlan>,
}

fn runtime_context_metadata(
    goal: Option<&DurableGoal>,
    plan: Option<&DurablePlan>,
) -> BTreeMap<String, Value> {
    let mut metadata = BTreeMap::new();
    if let Some(value) = goal.and_then(|goal| serde_json::to_value(goal).ok()) {
        metadata.insert("durable_goal".to_string(), value);
    }
    if let Some(value) = plan.and_then(|plan| serde_json::to_value(plan).ok()) {
        metadata.insert("durable_plan".to_string(), value);
    }
    metadata
}

fn runtime_context_pack_for_agent(
    store: &FileSessionStore,
    session: &mut Session,
    tools: &[ToolSchema],
    model_options: &BTreeMap<String, Value>,
    mcp_runtime: Option<&RuntimeMcpRuntime>,
    agent_profile: Option<&RuntimeSubagentProfile>,
    build_options: ContextPackBuildOptions,
) -> ContextPack {
    runtime_context_pack_for_agent_timed(
        store,
        session,
        tools,
        model_options,
        mcp_runtime,
        agent_profile,
        build_options,
    )
    .0
}

fn runtime_context_pack_for_agent_timed(
    store: &FileSessionStore,
    session: &mut Session,
    tools: &[ToolSchema],
    model_options: &BTreeMap<String, Value>,
    mcp_runtime: Option<&RuntimeMcpRuntime>,
    agent_profile: Option<&RuntimeSubagentProfile>,
    build_options: ContextPackBuildOptions,
) -> (ContextPack, ContextPackPerformance) {
    let materialize_started = Instant::now();
    let materialized =
        runtime_materialized_provider_context_for_agent(store, session, agent_profile);
    let tool_manifests = runtime_mcp_tool_manifest_items(mcp_runtime, tools);
    let todos = runtime_context_todos(&session.todos);
    let checkpoints = runtime_context_checkpoints(store, session);
    let goal = session_goal(session).ok().flatten();
    let plan = session_plan(session).ok().flatten();
    let materialize_us = elapsed_micros(materialize_started);
    let source_message_count = materialized.source_message_count as u64;
    let build_started = Instant::now();
    let pack = ContextPackBuilder::new(Some(build_options)).build(ContextPackInput {
        system_sources: Some(materialized.system_sources),
        messages: materialized.messages,
        tools: tools.to_vec(),
        model_options: model_options.clone(),
        attachments: materialized.attachments,
        work_state: materialized.work_state,
        todos,
        checkpoints,
        skills: Vec::new(),
        tool_manifests,
        metadata: runtime_context_metadata(goal.as_ref(), plan.as_ref()),
        runtime_context: None,
        sandbox_metadata: None,
        extra_items: Vec::new(),
    });
    runtime_apply_context_system_diagnostics(session, pack.system_diagnostics.as_ref());
    let mut performance = ContextPackPerformance::new();
    performance.materialize_us = materialize_us;
    performance.build_us = elapsed_micros(build_started);
    performance.source_message_count = source_message_count;
    performance.tool_count = pack.tools.len() as u64;
    performance.item_count = pack.items.len() as u64;
    performance.refresh_warnings();
    (pack, performance)
}

fn elapsed_micros(started: Instant) -> u64 {
    started.elapsed().as_micros().try_into().unwrap_or(u64::MAX)
}

fn runtime_context_replay_spec(
    store: &FileSessionStore,
    session: &mut Session,
    pack: &ContextPack,
    mcp_runtime: Option<&RuntimeMcpRuntime>,
    agent_profile: Option<&RuntimeSubagentProfile>,
    build_options: ContextPackBuildOptions,
) -> RuntimeContextReplaySpec {
    let materialized =
        runtime_materialized_provider_context_for_agent(store, session, agent_profile);
    let (model_options, unsafe_model_option_keys) =
        safe_context_replay_model_options(&pack.model_options);
    RuntimeContextReplaySpec {
        schema_version: CONTEXT_REPLAY_SPEC_SCHEMA_VERSION.to_string(),
        materialized_message_count: materialized.source_message_count,
        tools: pack.tools.clone(),
        model_options,
        unsafe_model_option_keys,
        build_options,
        todos: runtime_context_todos(&session.todos),
        checkpoints: runtime_context_checkpoints(store, session),
        tool_manifests: runtime_mcp_tool_manifest_items(mcp_runtime, &pack.tools),
        work_state: materialized.work_state,
        goal: session_goal(session).ok().flatten(),
        plan: session_plan(session).ok().flatten(),
    }
}

fn safe_context_replay_model_options(
    model_options: &BTreeMap<String, Value>,
) -> (BTreeMap<String, Value>, Vec<String>) {
    const SAFE_KEYS: &[&str] = &[
        "frequency_penalty",
        "max_completion_tokens",
        "max_output_tokens",
        "max_tokens",
        "parallel_tool_calls",
        "presence_penalty",
        "reasoning",
        "reasoning_effort",
        "response_format",
        "seed",
        "service_tier",
        "stop",
        "temperature",
        "tool_call_dialect",
        "tool_choice",
        "top_p",
        "verbosity",
    ];
    let mut safe = BTreeMap::new();
    let mut unsafe_keys = Vec::new();
    for (key, value) in model_options {
        if SAFE_KEYS.contains(&key.as_str()) {
            safe.insert(key.clone(), value.clone());
        } else {
            unsafe_keys.push(key.clone());
        }
    }
    (safe, unsafe_keys)
}

fn runtime_context_pack_from_replay_spec(
    store: &FileSessionStore,
    session: &mut Session,
    agent_profile: Option<&RuntimeSubagentProfile>,
    spec: &RuntimeContextReplaySpec,
) -> ContextPack {
    let materialized = runtime_materialized_provider_context_for_agent_bounded(
        store,
        session,
        agent_profile,
        Some(spec.materialized_message_count),
    );
    let pack = ContextPackBuilder::new(Some(spec.build_options.clone())).build(ContextPackInput {
        system_sources: Some(materialized.system_sources),
        messages: materialized.messages,
        tools: spec.tools.clone(),
        model_options: spec.model_options.clone(),
        attachments: materialized.attachments,
        work_state: spec.work_state.clone(),
        todos: spec.todos.clone(),
        checkpoints: spec.checkpoints.clone(),
        skills: Vec::new(),
        tool_manifests: spec.tool_manifests.clone(),
        metadata: runtime_context_metadata(spec.goal.as_ref(), spec.plan.as_ref()),
        runtime_context: None,
        sandbox_metadata: None,
        extra_items: Vec::new(),
    });
    runtime_apply_context_system_diagnostics(session, pack.system_diagnostics.as_ref());
    pack
}

fn runtime_context_pack_needs_auto_compaction(
    pack: &ContextPack,
    options: &ContextBudgetOptions,
) -> bool {
    if !options.enabled || !matches!(options.strategy.as_str(), "auto" | "compact") {
        return false;
    }
    pack.budget.overflowed
        || pack.trace.iter().any(|entry| {
            !entry.included
                && entry.drop_reason.as_deref() == Some("model_context_budget")
                && matches!(entry.kind.as_str(), "message" | "tool_result")
        })
}

fn runtime_auto_compact_context(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    step: u64,
    pack: &ContextPack,
    options: &ContextBudgetOptions,
) -> Result<Option<Value>, String> {
    let overflowed = pack.budget.overflowed;
    let reason = if overflowed {
        "required_context_overflow"
    } else {
        "history_budget_pressure"
    };
    let keep_recent_user_turns = if overflowed {
        options.overflow_keep_recent_user_turns
    } else {
        options.prune_keep_recent_user_turns
    };
    let Some((boundary_index, compacted_until_message_id)) =
        runtime_auto_compaction_boundary(&session.messages, keep_recent_user_turns)
    else {
        return Ok(None);
    };
    let compacted_message_count = boundary_index.saturating_add(1);
    if !overflowed && compacted_message_count < options.compact_refresh_min_new_messages as usize {
        return Ok(None);
    }

    let state = runtime_compaction_work_state(session, &session.messages[..=boundary_index]);
    let item_budget = pack.budget.item_budget_tokens.unwrap_or_default();
    let summary_token_budget = options
        .compact_summary_max_output_tokens
        .min(item_budget.saturating_div(3).max(1));
    let summary = truncate_runtime_context_text(
        &render_work_state(&state),
        summary_token_budget.saturating_mul(options.bytes_per_token) as usize,
    );
    if summary.trim().is_empty() {
        return Ok(None);
    }

    let source = "runtime_auto_compaction_v1";
    let boundary_metadata = BTreeMap::from([
        ("automatic".to_string(), json!(true)),
        ("before_pack_hash".to_string(), json!(pack.pack_hash)),
        (
            "compacted_message_count".to_string(),
            json!(compacted_message_count),
        ),
        ("format".to_string(), json!("structured_work_state")),
        ("reason".to_string(), json!(reason)),
        ("source".to_string(), json!(source)),
        ("step".to_string(), json!(step)),
    ]);
    let boundary_message_id = store
        .append_compaction_boundary_with_metadata(
            session,
            run_id,
            &summary,
            &compacted_until_message_id,
            boundary_metadata,
        )
        .map_err(|error| format!("failed to create automatic compaction boundary: {error}"))?;
    let compacted_at_ms = now_ms();
    session.metadata.insert(
        "compact".to_string(),
        json!({
            "automatic": true,
            "before_pack_hash": pack.pack_hash,
            "boundary_message_id": boundary_message_id,
            "compacted_at_ms": compacted_at_ms,
            "compacted_message_count": compacted_message_count,
            "compacted_until_message_id": compacted_until_message_id,
            "format": "structured_work_state",
            "message_count": session.messages.len(),
            "reason": reason,
            "run_id": run_id,
            "source": source,
            "state": state,
            "step": step,
            "summary": summary,
        }),
    );
    let compaction = json!({
        "automatic": true,
        "before_pack_hash": pack.pack_hash,
        "boundary_message_id": boundary_message_id,
        "compacted_message_count": compacted_message_count,
        "compacted_until_message_id": compacted_until_message_id,
        "reason": reason,
        "source": source,
        "step": step,
        "summary_tokens_estimate": summary_token_budget,
    });
    store
        .record_event(
            &session.id,
            run_id,
            "context.auto_compacted",
            SessionEventOptions {
                kind: "context".to_string(),
                attributes: compaction
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
                ..SessionEventOptions::default()
            },
        )
        .map_err(|error| error.to_string())?;
    store
        .save_state(session, Some(run_id))
        .map_err(|error| error.to_string())?;
    Ok(Some(compaction))
}

fn runtime_auto_compaction_boundary(
    messages: &[ChatMessage],
    keep_recent_user_turns: u64,
) -> Option<(usize, String)> {
    let user_positions = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == Role::User).then_some(index))
        .collect::<Vec<_>>();
    let keep_recent_user_turns = keep_recent_user_turns.max(1) as usize;
    if user_positions.len() <= keep_recent_user_turns {
        return None;
    }
    let preserve_from = user_positions[user_positions.len() - keep_recent_user_turns];
    (0..preserve_from).rev().find_map(|index| {
        let message_id = messages[index]
            .metadata
            .get("message_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())?;
        Some((index, message_id.to_string()))
    })
}

fn runtime_compaction_work_state(session: &Session, messages: &[ChatMessage]) -> WorkState {
    let task = messages
        .iter()
        .rev()
        .find(|message| message.role == Role::User && !message.content.trim().is_empty())
        .map(|message| compacted_message_line(message, 480))
        .unwrap_or_else(|| "Continue the current workspace task.".to_string());
    let mut progress = messages
        .iter()
        .rev()
        .filter(|message| message.role == Role::Assistant && !message.content.trim().is_empty())
        .take(4)
        .map(|message| compacted_message_line(message, 280))
        .collect::<Vec<_>>();
    progress.reverse();
    let mut tool_findings = messages
        .iter()
        .rev()
        .filter(|message| message.role == Role::Tool && !message.content.trim().is_empty())
        .take(6)
        .map(|message| {
            let finding = compacted_message_line(message, 220);
            message
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .map_or(finding.clone(), |name| format!("{name}: {finding}"))
        })
        .collect::<Vec<_>>();
    tool_findings.reverse();
    let blockers = messages
        .iter()
        .filter(|message| {
            message.role == Role::Tool
                && (message.metadata.contains_key("error")
                    || message.content.to_ascii_lowercase().contains("error"))
        })
        .rev()
        .take(3)
        .map(|message| compacted_message_line(message, 220))
        .collect::<Vec<_>>();
    let todos = session
        .todos
        .iter()
        .filter(|todo| !matches!(todo.status.as_str(), "completed" | "cancelled" | "canceled"))
        .map(|todo| todo.content.trim().to_string())
        .filter(|todo| !todo.is_empty())
        .collect::<Vec<_>>();
    WorkState {
        task,
        progress,
        tool_findings,
        todos: todos.clone(),
        blockers,
        next_steps: todos,
        ..WorkState::default()
    }
}

fn compacted_message_line(message: &ChatMessage, max_chars: usize) -> String {
    let text = message
        .content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() <= max_chars {
        text
    } else {
        format!("{}...", text.chars().take(max_chars).collect::<String>())
    }
}

fn truncate_runtime_context_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let suffix = "\n[Compaction summary truncated]";
    if end <= suffix.len() {
        return text[..end].to_string();
    }
    let mut content_end = end - suffix.len();
    while content_end > 0 && !text.is_char_boundary(content_end) {
        content_end -= 1;
    }
    format!("{}{}", &text[..content_end], suffix)
}

fn runtime_mcp_tool_manifest_items(
    mcp_runtime: Option<&RuntimeMcpRuntime>,
    tools: &[ToolSchema],
) -> Vec<ContextItem> {
    let Some(runtime) = mcp_runtime else {
        return Vec::new();
    };
    let visible_tools = tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<BTreeMap<_, _>>();
    runtime
        .descriptors
        .values()
        .filter_map(|descriptor| {
            let tool = visible_tools.get(descriptor.dynamic_name.as_str())?;
            Some(tool_manifest_context_item(
                format!("mcp_tool:{}", descriptor.dynamic_name),
                format!("mcp.server:{}", descriptor.server_name),
                tool,
                BTreeMap::from([
                    ("server_name".to_string(), json!(descriptor.server_name)),
                    ("original_name".to_string(), json!(descriptor.original_name)),
                    ("dynamic_name".to_string(), json!(descriptor.dynamic_name)),
                    ("title".to_string(), json!(descriptor.title)),
                ]),
            ))
        })
        .collect()
}

#[cfg(test)]
fn runtime_persist_context_pack_receipt_with_replay(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    step: u64,
    pack: &ContextPack,
    rebuild: Option<&Value>,
    replay_spec: Option<&RuntimeContextReplaySpec>,
) -> Result<(), String> {
    runtime_persist_context_pack_receipt_with_diagnostics(
        store,
        session,
        run_id,
        step,
        pack,
        rebuild,
        replay_spec,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn runtime_persist_context_pack_receipt_with_diagnostics(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    step: u64,
    pack: &ContextPack,
    rebuild: Option<&Value>,
    replay_spec: Option<&RuntimeContextReplaySpec>,
    performance: Option<&ContextPackPerformance>,
    failure: Option<&ContextFailure>,
) -> Result<(), String> {
    let mut receipts = session
        .metadata
        .get("context_pack_receipts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let prefix_cache = runtime_context_prefix_cache_status(&receipts, pack, run_id, step);
    let mut envelope = json!({
        "schema_version": "openagent.turn_context_pack.v1",
        "mode": "active",
        "run_id": run_id,
        "step": step,
        "receipt": pack.receipt,
        "trace": pack.trace,
        "system_diagnostics": pack.system_diagnostics,
        "prefix_cache": prefix_cache,
    });
    if let Some(rebuild) = rebuild
        && let Some(object) = envelope.as_object_mut()
    {
        object.insert("rebuild".to_string(), rebuild.clone());
    }
    if let Some(replay_spec) = replay_spec
        && let Some(object) = envelope.as_object_mut()
    {
        object.insert(
            "replay_spec".to_string(),
            serde_json::to_value(replay_spec).map_err(|error| error.to_string())?,
        );
    }
    if let Some(performance) = performance
        && let Some(object) = envelope.as_object_mut()
    {
        object.insert(
            "performance".to_string(),
            serde_json::to_value(performance).map_err(|error| error.to_string())?,
        );
    }
    if let Some(failure) = failure
        && let Some(object) = envelope.as_object_mut()
    {
        object.insert(
            "failure".to_string(),
            serde_json::to_value(failure).map_err(|error| error.to_string())?,
        );
    }
    let already_persisted = receipts.iter().any(|existing| {
        existing.get("run_id").and_then(Value::as_str) == Some(run_id)
            && existing.get("step").and_then(Value::as_u64) == Some(step)
            && existing
                .pointer("/receipt/pack_hash")
                .and_then(Value::as_str)
                == Some(pack.pack_hash.as_str())
    });
    receipts.retain(|existing| {
        existing.get("run_id").and_then(Value::as_str) != Some(run_id)
            || existing.get("step").and_then(Value::as_u64) != Some(step)
    });
    receipts.push(envelope.clone());
    if receipts.len() > MAX_CONTEXT_PACK_RECEIPTS {
        receipts.drain(..receipts.len() - MAX_CONTEXT_PACK_RECEIPTS);
    }
    session
        .metadata
        .insert("context_pack".to_string(), envelope.clone());
    session
        .metadata
        .insert("context_pack_receipts".to_string(), Value::Array(receipts));
    if !already_persisted {
        store
            .record_event(
                &session.id,
                run_id,
                "context.pack_built",
                SessionEventOptions {
                    kind: "context".to_string(),
                    attributes: BTreeMap::from([
                        ("mode".to_string(), json!("active")),
                        ("step".to_string(), json!(step)),
                        ("receipt".to_string(), json!(pack.receipt)),
                        (
                            "trace".to_string(),
                            Value::Array(
                                pack.trace.iter().map(public_context_trace_entry).collect(),
                            ),
                        ),
                        ("prefix_cache".to_string(), json!(prefix_cache)),
                        ("rebuild".to_string(), json!(rebuild)),
                        (
                            "performance".to_string(),
                            performance
                                .map(public_context_performance)
                                .unwrap_or(Value::Null),
                        ),
                        (
                            "failure".to_string(),
                            failure.map(public_context_failure).unwrap_or(Value::Null),
                        ),
                    ]),
                    ..SessionEventOptions::default()
                },
            )
            .map_err(|error| error.to_string())?;
    }
    store
        .save_state(session, Some(run_id))
        .map_err(|error| error.to_string())
}

fn runtime_update_context_pack_diagnostics(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    step: u64,
    performance: Option<&ContextPackPerformance>,
    failure: Option<&ContextFailure>,
) -> Result<(), String> {
    let performance = performance
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| error.to_string())?;
    let failure = failure
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| error.to_string())?;
    let apply = |value: &mut Value| {
        if value.get("run_id").and_then(Value::as_str) != Some(run_id)
            || value.get("step").and_then(Value::as_u64) != Some(step)
        {
            return;
        }
        let Some(object) = value.as_object_mut() else {
            return;
        };
        if let Some(performance) = &performance {
            object.insert("performance".to_string(), performance.clone());
        }
        if let Some(failure) = &failure {
            object.insert("failure".to_string(), failure.clone());
        }
    };
    if let Some(latest) = session.metadata.get_mut("context_pack") {
        apply(latest);
    }
    if let Some(history) = session
        .metadata
        .get_mut("context_pack_receipts")
        .and_then(Value::as_array_mut)
    {
        for envelope in history {
            apply(envelope);
        }
    }
    store
        .save_state(session, Some(run_id))
        .map_err(|error| error.to_string())
}

fn runtime_context_prefix_cache_status(
    receipts: &[Value],
    pack: &ContextPack,
    run_id: &str,
    step: u64,
) -> Value {
    let prefix = &pack.stable_prefix;
    if !prefix.cache_eligible {
        return json!({
            "schema_version": "openagent.context_prefix_cache.v1",
            "scope": "logical_prefix_reuse",
            "status": "bypass",
            "cache_eligible": false,
            "stable_prefix_hash": prefix.hash,
            "stable_prefix_token_estimate": prefix.token_estimate,
            "retry_reuses_pack": true,
        });
    }
    let previous = receipts.iter().rev().find(|receipt| {
        (receipt.get("run_id").and_then(Value::as_str) != Some(run_id)
            || receipt.get("step").and_then(Value::as_u64) != Some(step))
            && receipt
                .pointer("/receipt/stable_prefix/cache_eligible")
                .and_then(Value::as_bool)
                == Some(true)
    });
    let reused = receipts.iter().rev().find(|receipt| {
        (receipt.get("run_id").and_then(Value::as_str) != Some(run_id)
            || receipt.get("step").and_then(Value::as_u64) != Some(step))
            && receipt
                .pointer("/receipt/stable_prefix/cache_eligible")
                .and_then(Value::as_bool)
                == Some(true)
            && receipt
                .pointer("/receipt/stable_prefix/hash")
                .and_then(Value::as_str)
                == Some(prefix.hash.as_str())
    });
    let status = if reused.is_some() {
        "reused"
    } else if previous.is_some() {
        "changed"
    } else {
        "miss"
    };
    json!({
        "schema_version": "openagent.context_prefix_cache.v1",
        "scope": "logical_prefix_reuse",
        "status": status,
        "cache_eligible": true,
        "stable_prefix_hash": prefix.hash,
        "stable_prefix_token_estimate": prefix.token_estimate,
        "retry_reuses_pack": true,
        "reused_from": reused.map(|receipt| json!({
            "run_id": receipt.get("run_id").cloned().unwrap_or(Value::Null),
            "step": receipt.get("step").cloned().unwrap_or(Value::Null),
        })),
    })
}

struct RuntimeMaterializedProviderContext {
    messages: Vec<ChatMessage>,
    attachments: Vec<ContextAttachment>,
    work_state: Option<ContextWorkState>,
    system_sources: ContextSystemSources,
    source_message_count: usize,
}

fn runtime_materialized_provider_context_for_agent(
    store: &FileSessionStore,
    session: &mut Session,
    profile: Option<&RuntimeSubagentProfile>,
) -> RuntimeMaterializedProviderContext {
    runtime_materialized_provider_context_for_agent_bounded(store, session, profile, None)
}

fn runtime_materialized_provider_context_for_agent_bounded(
    store: &FileSessionStore,
    session: &mut Session,
    profile: Option<&RuntimeSubagentProfile>,
    message_limit: Option<usize>,
) -> RuntimeMaterializedProviderContext {
    let mut source_messages = store
        .materialized_chat_messages(session)
        .unwrap_or_else(|_| session.messages.clone());
    if let Some(message_limit) = message_limit {
        source_messages.truncate(message_limit);
    }
    let source_message_count = source_messages.len();
    let history = materialize_context_history(source_messages);
    let messages = history.messages;
    let mut system_sources = runtime_context_system_sources(session, profile);
    system_sources.legacy_system_sources = history.legacy_system_sources;
    let work_state = history
        .work_state
        .or_else(|| runtime_legacy_context_work_state(session, messages.len()));
    let attachments = runtime_context_attachments(&messages);
    RuntimeMaterializedProviderContext {
        messages,
        attachments,
        work_state,
        system_sources,
        source_message_count,
    }
}

fn runtime_legacy_context_work_state(
    session: &Session,
    message_position: usize,
) -> Option<ContextWorkState> {
    let compact = session.metadata.get("compact")?.as_object()?;
    let summary = compact.get("summary")?.as_str()?.trim();
    if summary.is_empty() {
        return None;
    }
    Some(ContextWorkState {
        id: compact
            .get("boundary_message_id")
            .and_then(Value::as_str)
            .unwrap_or("legacy_compact")
            .to_string(),
        summary: summary.to_string(),
        format: compact
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("session_summary_v1")
            .to_string(),
        source: "session.metadata.compact".to_string(),
        message_position: Some(message_position),
        compacted_until_message_id: compact
            .get("compacted_until_message_id")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        metadata: BTreeMap::new(),
    })
}

fn runtime_context_todos(todos: &[SessionTodoItem]) -> Vec<ContextTodo> {
    todos
        .iter()
        .map(|todo| {
            ContextTodo::new(
                Some(todo.id.clone()),
                todo.content.clone(),
                todo.status.clone(),
                todo.priority.clone(),
            )
        })
        .collect()
}

fn runtime_context_checkpoints(
    store: &FileSessionStore,
    session: &Session,
) -> Vec<ContextCheckpoint> {
    let restored_id = session
        .metadata
        .get("latest_checkpoint_restore")
        .and_then(Value::as_object)
        .and_then(|restore| restore.get("checkpoint_id"))
        .and_then(Value::as_str);
    let checkpoints = store.list_checkpoints(&session.id).unwrap_or_default();
    let mut selected = checkpoints.iter().take(1).collect::<Vec<_>>();
    if let Some(restored_id) = restored_id
        && let Some(restored) = checkpoints
            .iter()
            .find(|checkpoint| checkpoint.checkpoint_id == restored_id)
        && !selected
            .iter()
            .any(|checkpoint| checkpoint.checkpoint_id == restored_id)
    {
        selected.push(restored);
    }
    selected
        .into_iter()
        .map(|checkpoint| context_checkpoint(checkpoint, restored_id))
        .collect()
}

fn context_checkpoint(
    checkpoint: &SessionCheckpointRecord,
    restored_id: Option<&str>,
) -> ContextCheckpoint {
    ContextCheckpoint {
        id: checkpoint.checkpoint_id.clone(),
        kind: checkpoint.kind.clone(),
        run_id: checkpoint.run_id.clone(),
        timestamp_ms: checkpoint.timestamp_ms,
        message_id: checkpoint.message_id.clone(),
        part_id: checkpoint.part_id.clone(),
        step_index: checkpoint.step_index,
        file_count: checkpoint.file_count,
        total_bytes: checkpoint.total_bytes,
        restored: restored_id == Some(checkpoint.checkpoint_id.as_str()),
        metadata: BTreeMap::new(),
    }
}

fn runtime_context_attachments(messages: &[ChatMessage]) -> Vec<ContextAttachment> {
    messages
        .iter()
        .enumerate()
        .flat_map(|(message_index, message)| {
            message
                .metadata
                .get("context_attachments")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(move |value| {
                    let mut attachment =
                        serde_json::from_value::<ContextAttachment>(value.clone()).ok()?;
                    attachment.id = attachment.stable_id();
                    attachment.source_message_index = Some(message_index);
                    Some(attachment)
                })
        })
        .collect()
}

fn runtime_context_system_sources(
    session: &mut Session,
    profile: Option<&RuntimeSubagentProfile>,
) -> ContextSystemSources {
    let (preloaded_skills, available_skills) = profile.map_or_else(
        || (Vec::new(), Vec::new()),
        |profile| {
            (
                runtime_preloaded_skill_documents(profile, &session.directory),
                runtime_available_skill_infos(profile, &session.directory),
            )
        },
    );
    if let Some(profile) = profile.filter(|profile| !profile.skills.is_empty()) {
        session
            .metadata
            .insert("skills".to_string(), json!(profile.skills.clone()));
    } else {
        session.metadata.remove("skills");
    }
    ContextSystemSources {
        profile_id: profile.map(|profile| profile.id.clone()),
        profile_mode: profile.map(|profile| profile.mode.clone()),
        profile_prompt: profile.map(|profile| profile.prompt.clone()),
        workspace_root: session.directory.clone(),
        preloaded_skills,
        available_skills,
        legacy_system_sources: Vec::new(),
        include_instructions: true,
    }
}

fn runtime_apply_context_system_diagnostics(
    session: &mut Session,
    diagnostics: Option<&ContextSystemDiagnostics>,
) {
    let Some(diagnostics) = diagnostics else {
        session.metadata.remove("preloaded_skills");
        session.metadata.remove("dynamic_system_prompt");
        return;
    };
    if !diagnostics.preloaded_skill_names.is_empty() {
        session.metadata.insert(
            "preloaded_skills".to_string(),
            json!(diagnostics.preloaded_skill_names.clone()),
        );
    } else {
        session.metadata.remove("preloaded_skills");
    }
    session.metadata.insert(
        "dynamic_system_prompt".to_string(),
        diagnostics.session_metadata(),
    );
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

fn runtime_available_skill_infos(
    profile: &RuntimeSubagentProfile,
    session_root: &Path,
) -> Vec<openagent_core::SkillInfo> {
    if !runtime_agent_allows_tool(profile, "skill") {
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
    registry
        .all()
        .into_iter()
        .filter(|skill| skill_is_visible(&profile.skill_permissions, &skill.name))
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
    events.push(
        SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
            .tool_call_started_event(run_id, step, tool_call, Some(run_id), BTreeMap::new()),
    );
    append_unpersisted_bridge_events(&store.root, &session.id, run_id, events, persisted_events);
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
        append_unpersisted_bridge_events(
            &store.root,
            &session.id,
            run_id,
            events,
            persisted_events,
        );
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
        append_unpersisted_bridge_events(
            &store.root,
            &session.id,
            run_id,
            events,
            persisted_events,
        );
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
        append_unpersisted_bridge_events(
            &store.root,
            &session.id,
            run_id,
            events,
            persisted_events,
        );
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
    append_unpersisted_bridge_events(&store.root, &session.id, run_id, events, persisted_events);
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
    events.push(
        SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
            .tool_call_finished_event(
                run_id,
                step,
                tool_call,
                tool_result,
                Some(run_id),
                BTreeMap::new(),
            ),
    );
    if let Some(change) = patch.as_ref() {
        events.push(patch_detected_event(session, run_id, change));
    }
    let todos_changed = sync_session_todos_from_tool_result(session, tool_call, tool_result);
    append_tool_result_to_session(store, session, run_id, step, tool_call, tool_result)?;
    if todos_changed {
        store
            .save_state(session, Some(run_id))
            .map_err(|error| format!("failed to persist session todos: {error}"))?;
    }
    Ok(())
}

fn sync_session_todos_from_tool_result(
    session: &mut Session,
    tool_call: &ToolCall,
    tool_result: &ToolResult,
) -> bool {
    if tool_result.error.is_some() || !matches!(tool_call.name.as_str(), "todowrite" | "todoread") {
        return false;
    }
    let Some(todos) = tool_result.metadata.get("todos").and_then(Value::as_array) else {
        return false;
    };
    let todos = todos
        .iter()
        .filter_map(|todo| serde_json::from_value::<SessionTodoItem>(todo.clone()).ok())
        .collect::<Vec<_>>();
    if todos.len()
        != tool_result
            .metadata
            .get("todos")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
        || session.todos == todos
    {
        return false;
    }
    session.set_todos(todos);
    true
}

fn append_tool_result_to_session(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    step: u64,
    tool_call: &ToolCall,
    tool_result: &ToolResult,
) -> Result<(), String> {
    let runner_facade = SessionRunnerFacade::new(session.directory.clone(), session.id.clone());
    let assistant_message_id = latest_assistant_message_id_for_tool(session, tool_call);
    let settlement = runner_facade.tool_result_settlement(
        step,
        tool_call,
        tool_result,
        assistant_message_id.as_deref(),
        None,
    );
    for intent in &settlement.event_intents {
        let _ = store.record_event(
            &session.id,
            run_id,
            &intent.event_name,
            SessionEventOptions {
                kind: intent.kind.clone(),
                status: intent.status.clone(),
                attributes: intent.attributes.clone(),
                ..SessionEventOptions::default()
            },
        );
    }
    let part_intent = &settlement.part_intent;
    let _ = store.append_part(
        &session.id,
        run_id,
        &part_intent.part_type,
        SessionPartOptions {
            attributes: part_intent.attributes.clone(),
            step_index: part_intent.step_index,
            status: part_intent.status.clone(),
            ..SessionPartOptions::default()
        },
    );
    let tool_message = settlement.message;
    let tool_index = session.messages.len() as u64;
    session.add(tool_message.clone());
    store
        .append_message(session, &tool_message, run_id, tool_index)
        .map_err(|error| format!("failed to record tool message: {error}"))
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

fn runtime_file_change_status(change: &Value) -> &'static str {
    match (
        change
            .get("existed_before")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        change
            .get("existed_after")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    ) {
        (false, true) => "added",
        (true, false) => "deleted",
        _ => "modified",
    }
}

fn bounded_result_label(value: &str) -> String {
    value.trim().chars().take(160).collect()
}

fn runtime_final_result(
    store: &FileSessionStore,
    session: &Session,
    run_id: &str,
    answer: &str,
) -> Value {
    let mut changed_by_path = BTreeMap::new();
    for change in file_change_stack(session, FILE_CHANGE_UNDO_STACK_KEY) {
        if change.get("run_id").and_then(Value::as_str) != Some(run_id) {
            continue;
        }
        let Some(path) = change
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let status = runtime_file_change_status(&change);
        changed_by_path.insert(
            path.to_string(),
            json!({
                "kind": "file",
                "label": format!("{status} {path}"),
                "path": path,
                "status": status,
            }),
        );
    }

    let mut latest_tools = BTreeMap::new();
    if let Ok(messages) = store.list_messages_with_parts(&session.id, None, None) {
        for message in messages {
            if message.info.run_id.as_deref() != Some(run_id) {
                continue;
            }
            for part in message.parts {
                if part.kind != MessagePartKind::Tool {
                    continue;
                }
                let call_id = part
                    .content
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(&part.id)
                    .to_string();
                let tool = part
                    .content
                    .get("name")
                    .and_then(Value::as_str)
                    .or_else(|| part.attributes.get("name").and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("tool")
                    .to_string();
                latest_tools.insert(call_id, (tool, part.status));
            }
        }
    }

    let mut verified = Vec::new();
    let mut remaining = Vec::new();
    for (_, (tool, status)) in latest_tools {
        match status {
            MessageStatus::Completed => verified.push(json!({
                "kind": "tool",
                "label": format!("{tool} completed"),
                "tool": tool,
                "status": "completed",
            })),
            MessageStatus::Error | MessageStatus::Interrupted => remaining.push(json!({
                "kind": "tool",
                "label": format!("{tool} failed"),
                "tool": tool,
                "status": "failed",
            })),
            MessageStatus::Pending | MessageStatus::Running => remaining.push(json!({
                "kind": "tool",
                "label": format!("{tool} pending"),
                "tool": tool,
                "status": "pending",
            })),
        }
    }
    for todo in &session.todos {
        let status = todo.status.trim().to_ascii_lowercase();
        if matches!(
            status.as_str(),
            "completed" | "done" | "cancelled" | "canceled"
        ) {
            continue;
        }
        let label = bounded_result_label(&todo.content);
        if label.is_empty() {
            continue;
        }
        remaining.push(json!({
            "kind": "todo",
            "label": label,
            "todo_id": todo.id,
            "status": if status.is_empty() { "pending" } else { status.as_str() },
        }));
    }

    json!({
        "schema_version": FINAL_RESULT_SCHEMA_VERSION,
        "run_id": run_id,
        "summary": answer,
        "changed": changed_by_path.into_values().collect::<Vec<_>>(),
        "verified": verified,
        "remaining": remaining,
    })
}

fn persist_runtime_final_result(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    message_id: &str,
    answer: &str,
    step: u64,
) -> Value {
    if let Ok(Some(message)) = store.get_message_with_parts(&session.id, message_id)
        && let Some(existing) = message
            .parts
            .iter()
            .find(|part| part.kind == MessagePartKind::Result)
            .map(|part| part.content.clone())
    {
        session
            .metadata
            .insert(FINAL_RESULT_METADATA_KEY.to_string(), existing.clone());
        return existing;
    }

    let result = runtime_final_result(store, session, run_id, answer);
    let _ = store.append_part(
        &session.id,
        run_id,
        "result",
        SessionPartOptions {
            message_id: Some(message_id.to_string()),
            content: Some(result.clone()),
            attributes: BTreeMap::from([
                (
                    "schema_version".to_string(),
                    json!(FINAL_RESULT_SCHEMA_VERSION),
                ),
                ("run_id".to_string(), json!(run_id)),
            ]),
            step_index: Some(step),
            status: "completed".to_string(),
            ..SessionPartOptions::default()
        },
    );
    session
        .metadata
        .insert(FINAL_RESULT_METADATA_KEY.to_string(), result.clone());
    let _ = store.save_state(session, Some(run_id));
    result
}

fn append_runtime_completion_assistant(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    answer: &str,
    step: u64,
    assistant_message_id: &str,
    start_checkpoint_id: Option<&str>,
) -> Value {
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
    persist_runtime_final_result(store, session, run_id, assistant_message_id, answer, step)
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
    let outcome = SessionRunnerFacade::completed_turn_outcome(carry.next_step, finish_reason);
    let final_result = persist_runtime_final_result(
        store,
        session,
        run_id,
        &runtime_turn_message_id(run_id, "assistant", carry.next_step),
        &carry.answer,
        carry.next_step,
    );
    let _ = store.finish_run(
        session,
        run_id,
        &outcome.run_status,
        outcome.steps,
        Some(&outcome.finish_reason),
        outcome.error.as_deref(),
    );
    let usage = usage_value_from_provider(
        &carry.usage,
        carry.tool_calls,
        &latest_user_message(session),
        &carry.answer,
    );
    let trace = trace_payload(session, run_id, carry.tool_calls);
    record_usage_event(store, session, run_id, &usage);
    events.push(
        SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
            .turn_terminal_event(
                &outcome.event_method,
                run_id,
                &outcome.event_status,
                true,
                true,
                false,
                BTreeMap::from([
                    ("final_answer".to_string(), json!(carry.answer.clone())),
                    ("final_result".to_string(), final_result.clone()),
                    ("usage".to_string(), usage.clone()),
                    ("trace".to_string(), trace.clone()),
                    (
                        "finish_reason".to_string(),
                        json!(outcome.finish_reason.clone()),
                    ),
                ]),
            ),
    );
    append_unpersisted_bridge_events(
        &store.root,
        &session.id,
        run_id,
        &mut events,
        persisted_events,
    );
    Ok(json!({
        "session_id": session.id,
        "turn_id": run_id,
        "status": outcome.event_status.clone(),
        "turn": {
            "id": run_id,
            "session_id": session.id,
            "status": outcome.event_status.clone(),
            "final_answer": events.last().and_then(|event| event.get("params")).and_then(|params| params.get("final_answer")).cloned().unwrap_or_else(|| json!("")),
            "final_result": final_result,
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
    let mut payload: Value = match serde_json::from_str(body) {
        Ok(payload) => payload,
        Err(error) => return json_response(400, json!({"error": error.to_string()})),
    };
    if let Some(object) = payload.as_object_mut() {
        object.remove(INTERNAL_TURN_RETRY_KEY);
    }
    if turn_async_requested(request_path, &payload) {
        match start_turn_async_payload(config, session_id, payload) {
            Ok((status, payload)) => json_response(status, payload),
            Err(error) => session_error_response(error),
        }
    } else {
        match start_turn_payload_inner(config, session_id, payload, None) {
            Ok(payload) => json_response(200, payload),
            Err(error) => session_error_response(error),
        }
    }
}

fn start_turn_async_payload(
    config: &HttpRuntimeConfig,
    session_id: &str,
    mut payload: Value,
) -> Result<(u16, Value), String> {
    if let Some(object) = payload.as_object_mut() {
        object.remove(INTERNAL_TURN_RETRY_KEY);
    }
    start_turn_async_payload_trusted(config, session_id, payload)
}

fn start_turn_async_payload_trusted(
    config: &HttpRuntimeConfig,
    session_id: &str,
    payload: Value,
) -> Result<(u16, Value), String> {
    validate_start_turn_payload(&payload)?;
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let run_id = new_id("turn");
    let root = session_root(config);
    persist_turn_retry_payload(&root, session_id, &run_id, &payload)?;
    let registration = match register_turn_job(config, session_id, &run_id, payload.clone()) {
        Ok(registration) => registration,
        Err(TurnJobRegisterError::Unavailable) => {
            remove_turn_retry_payload(&root, &run_id);
            return Err("turn job registry unavailable".to_string());
        }
        Err(TurnJobRegisterError::QueuePersistFailed(error)) => {
            remove_turn_retry_payload(&root, &run_id);
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
            remove_turn_retry_payload(&root, &run_id);
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
    let retry_metadata = payload
        .get(INTERNAL_TURN_RETRY_KEY)
        .and_then(Value::as_object)
        .cloned();
    let input = payload
        .get("input")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let attachments = turn_attachments_from_payload(&payload)?;
    let mut permission_ruleset = permission_ruleset_for_turn(&payload)?;
    let mut skip_permissions = skip_permissions_for_turn(&payload);
    let store = FileSessionStore::new(session_root(config));
    if !session_state_exists(config, session_id) {
        return Err("session_not_found".to_string());
    }
    let mut session = store
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    let plan_mode_enforced =
        session_plan(&session)?.is_some_and(|plan| plan.status == DurablePlanStatus::Planning);
    if plan_mode_enforced {
        permission_ruleset = PermissionRuleset::Readonly;
        skip_permissions = false;
        session.metadata.insert(
            "latest_plan_enforcement".to_string(),
            json!({
                "schema_version": "openagent.plan_enforcement.v1",
                "mode": "planning",
                "permission": "READONLY",
                "skip_permissions": false,
                "enforced_at_ms": now_ms(),
            }),
        );
    }
    let runtime_profile = apply_turn_runtime_profile(&mut session, &payload);
    let run_id = run_id_override.unwrap_or_else(|| new_id("turn"));
    let max_steps = provider_max_steps(&payload);
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
            max_steps,
            started_at_ms: None,
        },
    );
    if retry_metadata.is_none() {
        let mut user_metadata = BTreeMap::new();
        if !attachments.is_empty() {
            user_metadata.insert(
                "display_content".to_string(),
                json!(input.trim().to_string()),
            );
            user_metadata.insert(
                "attachments".to_string(),
                Value::Array(
                    attachments
                        .iter()
                        .map(|attachment| attachment.metadata.clone())
                        .collect(),
                ),
            );
            user_metadata.insert(
                "context_attachments".to_string(),
                Value::Array(
                    attachments
                        .iter()
                        .map(|attachment| {
                            serde_json::to_value(&attachment.context).unwrap_or_default()
                        })
                        .collect(),
                ),
            );
            user_metadata.insert("attachment_count".to_string(), json!(attachments.len()));
        }
        user_metadata.insert(
            "message_id".to_string(),
            json!(runtime_turn_message_id(&run_id, "user", 0)),
        );
        if plan_mode_enforced {
            user_metadata.insert("plan_mode".to_string(), json!("planning"));
            user_metadata.insert("plan_read_only_enforced".to_string(), json!(true));
        }
        let user = ChatMessage {
            role: Role::User,
            content: input.to_string(),
            name: None,
            tool_call_id: None,
            metadata: user_metadata,
        };
        let user_index = session.messages.len() as u64;
        session.add(user.clone());
        let _ = store.append_message(&session, &user, &run_id, user_index);
    }
    let mut tool_calls = tool_calls_from_turn_payload(&payload)?;
    if tool_calls.is_empty()
        && let Some(call) = manual_runtime_subagent_tool_call(input)
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
    let mut initial_events = vec![turn_started_event(&session, &run_id)];
    if let Some(retry) = retry_metadata {
        let retry_of_turn_id = retry
            .get("retry_of_turn_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let retry_root_turn_id = retry
            .get("retry_root_turn_id")
            .and_then(Value::as_str)
            .unwrap_or(retry_of_turn_id);
        let retry_count = retry
            .get("retry_count")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let max_retries = retry
            .get("max_retries")
            .and_then(Value::as_u64)
            .unwrap_or_else(runtime_manual_turn_retries);
        initial_events.push(json!({
            "method": "turn/retried",
            "params": {
                "thread_id": session.id.clone(),
                "session_id": session.id.clone(),
                "turn_id": run_id,
                "run_id": run_id,
                "status": "running",
                "retry_of_turn_id": retry_of_turn_id,
                "retry_root_turn_id": retry_root_turn_id,
                "retry_count": retry_count,
                "max_retries": max_retries,
            }
        }));
    }
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
    let _ = turn_attachments_from_payload(payload)?;
    let _ = permission_ruleset_for_turn(payload)?;
    Ok(())
}

#[derive(Clone, Debug)]
struct TurnAttachment {
    context: ContextAttachment,
    metadata: Value,
}

fn turn_attachments_from_payload(payload: &Value) -> Result<Vec<TurnAttachment>, String> {
    const MAX_ATTACHMENTS: usize = 12;
    const MAX_ATTACHMENT_BYTES: usize = 256 * 1024;
    let Some(raw) = payload.get("attachments") else {
        return Ok(Vec::new());
    };
    let items = raw
        .as_array()
        .ok_or_else(|| "attachments must be an array".to_string())?;
    if items.len() > MAX_ATTACHMENTS {
        return Err(format!(
            "attachments must contain at most {MAX_ATTACHMENTS} files"
        ));
    }

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let object = item
                .as_object()
                .ok_or_else(|| format!("attachment {index} must be an object"))?;
            let kind = object
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("file")
                .trim();
            let kind = ContextAttachmentKind::parse(kind)
                .ok_or_else(|| format!("attachment {index} has unsupported kind {kind}"))?;
            let content = object
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if content.len() > MAX_ATTACHMENT_BYTES {
                return Err(format!(
                    "attachment {index} content is larger than {MAX_ATTACHMENT_BYTES} bytes"
                ));
            }
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let size_bytes = object
                .get("size_bytes")
                .or_else(|| object.get("sizeBytes"))
                .and_then(Value::as_u64)
                .unwrap_or(content.len() as u64);
            let content_type = object
                .get("content_type")
                .or_else(|| object.get("contentType"))
                .and_then(Value::as_str)
                .unwrap_or("text/plain");
            let source = object
                .get("source")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string);
            let page_count = object
                .get("page_count")
                .or_else(|| object.get("pageCount"))
                .and_then(Value::as_u64);
            let media_metadata = object
                .get("media_metadata")
                .or_else(|| object.get("mediaMetadata"))
                .or_else(|| object.get("media"))
                .and_then(Value::as_object)
                .map(|values| {
                    values
                        .iter()
                        .filter(|(key, value)| {
                            matches!(
                                key.as_str(),
                                "width_px"
                                    | "height_px"
                                    | "duration_ms"
                                    | "frame_count"
                                    | "dpi"
                                    | "orientation"
                                    | "extension"
                            ) && (value.is_number() || value.is_boolean() || value.is_string())
                        })
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            let included_content_bytes = object
                .get("included_content_bytes")
                .or_else(|| object.get("includedContentBytes"))
                .and_then(Value::as_u64)
                .or(Some(content.len() as u64));
            let original_content_bytes = object
                .get("original_content_bytes")
                .or_else(|| object.get("originalContentBytes"))
                .and_then(Value::as_u64)
                .or_else(|| (size_bytes > content.len() as u64).then_some(size_bytes));
            let truncated = object
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| {
                    original_content_bytes.is_some_and(|original| {
                        original > included_content_bytes.unwrap_or_default()
                    })
                });
            let truncation_reason = object
                .get("truncation_reason")
                .or_else(|| object.get("truncationReason"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string);
            let content_chars = content.chars().count();
            let content_lines = content.lines().count();
            let mut context = ContextAttachment::new(
                kind,
                (!path.is_empty()).then_some(path.clone()),
                (!name.is_empty()).then_some(name.clone()),
                content_type,
                size_bytes,
                content,
            );
            context.source = source.clone();
            context.page_count = page_count;
            context.media_metadata = media_metadata.clone();
            context.truncated = truncated;
            context.truncation_reason = truncation_reason.clone();
            context.original_content_bytes = original_content_bytes;
            context.included_content_bytes = included_content_bytes;
            if let Some(external_id) = object
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                context
                    .metadata
                    .insert("external_id".to_string(), json!(external_id.trim()));
            }
            Ok(TurnAttachment {
                metadata: json!({
                    "id": context.id,
                    "kind": context.kind,
                    "path": path,
                    "name": name,
                    "size_bytes": size_bytes,
                    "content_type": content_type,
                    "content_chars": content_chars,
                    "content_lines": content_lines,
                    "source": source,
                    "page_count": page_count,
                    "media_metadata": media_metadata,
                    "truncated": truncated,
                    "truncation_reason": truncation_reason,
                    "original_content_bytes": original_content_bytes,
                    "included_content_bytes": included_content_bytes,
                }),
                context,
            })
        })
        .collect()
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
    read_bridge_event_values(root, session_id, turn_id)
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
    let Ok(mut session) = store.load_session(session_id) else {
        return;
    };
    session.status = SessionStatus::Idle;
    session.metadata.remove("pending_provider_turn");
    let outcome = SessionRunnerFacade::failed_turn_outcome(1, "async_turn_error", error);
    let retryable = runtime_provider_error_retryable(error);
    let saved_retry = read_turn_retry_payload(&store.root, run_id);
    let retry_count = saved_retry
        .as_ref()
        .and_then(|saved| saved.get("payload"))
        .and_then(|payload| payload.get(INTERNAL_TURN_RETRY_KEY))
        .and_then(|metadata| metadata.get("retry_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let max_retries = runtime_manual_turn_retries();
    let resumable = retryable && saved_retry.is_some() && retry_count < max_retries;
    let _ = store.finish_run(
        &session,
        run_id,
        &outcome.run_status,
        outcome.steps,
        Some(&outcome.finish_reason),
        outcome.error.as_deref(),
    );
    let _ = store.save_state(&session, Some(run_id));
    let mut events = vec![
        SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
            .turn_terminal_event(
                &outcome.event_method,
                run_id,
                &outcome.event_status,
                false,
                true,
                false,
                BTreeMap::from([
                    ("error".to_string(), json!(error)),
                    ("retryable".to_string(), json!(retryable)),
                    ("resumable".to_string(), json!(resumable)),
                    ("retry_count".to_string(), json!(retry_count)),
                    ("max_retries".to_string(), json!(max_retries)),
                    (
                        "retry_url".to_string(),
                        json!(format!("/api/turns/{run_id}/retry")),
                    ),
                ]),
            ),
    ];
    append_bridge_events(&store.root, session_id, run_id, &mut events);
}

fn runtime_provider_error_retryable(error: &str) -> bool {
    let text = error.to_ascii_lowercase();
    text.contains("provider returned http 429")
        || (500..=599).any(|status| text.contains(&format!("provider returned http {status}")))
        || text.contains("provider request failed")
        || text.contains("timed out")
        || text.contains("connection")
        || text.contains("reset")
}

fn retry_turn_response(config: &HttpRuntimeConfig, turn_id: &str) -> HttpResponseSpec {
    let root = session_root(config);
    let Some(saved) = read_turn_retry_payload(&root, turn_id) else {
        return json_response(
            404,
            json!({
                "error": "turn retry payload not found",
                "error_code": "turn_not_resumable",
                "turn_id": turn_id,
            }),
        );
    };
    let status = turn_status_payload(config, turn_id)
        .ok()
        .and_then(|value| {
            value
                .get("status")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_default();
    if status != "failed" {
        return json_response(
            409,
            json!({
                "error": "only failed turns can be retried",
                "error_code": "turn_not_failed",
                "turn_id": turn_id,
                "status": status,
            }),
        );
    }
    let Some(session_id) = saved.get("session_id").and_then(Value::as_str) else {
        return json_response(500, json!({"error": "turn retry payload is invalid"}));
    };
    let mut payload = saved.get("payload").cloned().unwrap_or_else(|| json!({}));
    let prior_retry = payload
        .get(INTERNAL_TURN_RETRY_KEY)
        .and_then(Value::as_object);
    let retry_count = prior_retry
        .and_then(|metadata| metadata.get("retry_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1);
    let max_retries = runtime_manual_turn_retries();
    if retry_count > max_retries {
        return json_response(
            429,
            json!({
                "error": "turn retry limit reached",
                "error_code": "turn_retry_limit_reached",
                "turn_id": turn_id,
                "retry_count": retry_count.saturating_sub(1),
                "max_retries": max_retries,
            }),
        );
    }
    let retry_root_turn_id = prior_retry
        .and_then(|metadata| metadata.get("retry_root_turn_id"))
        .and_then(Value::as_str)
        .unwrap_or(turn_id)
        .to_string();
    if let Some(object) = payload.as_object_mut() {
        object.insert("async".to_string(), json!(true));
        object.remove("retry_of_turn_id");
        object.insert(
            INTERNAL_TURN_RETRY_KEY.to_string(),
            json!({
                "retry_of_turn_id": turn_id,
                "retry_root_turn_id": retry_root_turn_id,
                "retry_count": retry_count,
                "max_retries": max_retries,
            }),
        );
    }
    match start_turn_async_payload_trusted(config, session_id, payload) {
        Ok((status, mut response)) => {
            if response
                .get("accepted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                remove_turn_retry_payload(&root, turn_id);
            }
            if let Some(object) = response.as_object_mut() {
                object.insert("retry_of_turn_id".to_string(), json!(turn_id));
                object.insert("retry_root_turn_id".to_string(), json!(retry_root_turn_id));
                object.insert("retry_count".to_string(), json!(retry_count));
                object.insert("max_retries".to_string(), json!(max_retries));
            }
            json_response(status, response)
        }
        Err(error) => session_error_response(error),
    }
}

fn record_turn_interrupted(
    store: &FileSessionStore,
    session: &mut Session,
    turn_id: &str,
    error: &str,
) -> Vec<Value> {
    session.status = SessionStatus::Stop;
    session.metadata.remove("pending_provider_turn");
    let outcome = SessionRunnerFacade::interrupted_turn_outcome(error);
    let _ = store.finish_run(
        session,
        turn_id,
        &outcome.run_status,
        outcome.steps,
        Some(&outcome.finish_reason),
        outcome.error.as_deref(),
    );
    let _ = store.save_state(session, Some(turn_id));
    mark_turn_job_status_at_root(&store.root, turn_id, &outcome.event_status);
    let event = SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
        .turn_terminal_event(
            &outcome.event_method,
            turn_id,
            &outcome.event_status,
            true,
            true,
            false,
            BTreeMap::from([("error".to_string(), json!(error))]),
        );
    let mut events = vec![event];
    if !turn_event_recorded(&store.root, &session.id, turn_id, "turn/interrupted") {
        append_bridge_events(&store.root, &session.id, turn_id, &mut events);
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
    let mut ctx = runtime_session_runner_facade(
        session,
        agent_profile.as_ref(),
        permission_ruleset.clone(),
        skip_permissions,
    )
    .tool_context();
    let mut events = vec![turn_started_event(session, run_id)];
    let assistant_message_id = runtime_turn_message_id(run_id, "assistant", tool_call_count.max(1));
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
        events.push(
            SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
                .tool_call_started_event(run_id, step, &tool_call, Some(run_id), BTreeMap::new()),
        );
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
            append_bridge_events(&store.root, &session.id, run_id, &mut events);
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
    let final_result = append_runtime_completion_assistant(
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
    events.push(
        SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
            .turn_terminal_event(
                "turn/completed",
                run_id,
                "completed",
                true,
                true,
                false,
                BTreeMap::from([
                    ("final_answer".to_string(), json!(answer.clone())),
                    ("final_result".to_string(), final_result.clone()),
                    ("usage".to_string(), usage.clone()),
                    ("trace".to_string(), trace.clone()),
                ]),
            ),
    );
    append_bridge_events(&store.root, &session.id, run_id, &mut events);
    Ok(json!({
        "session_id": session.id,
        "turn_id": run_id,
        "status": "completed",
        "final_result": final_result,
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
    if approval.get("source").and_then(Value::as_str) == Some("git_workflow") {
        return resolve_git_workflow_approval(&store, session, approval, &response);
    }
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
        let mut ctx = runtime_session_runner_facade(
            &session,
            agent_profile.as_ref(),
            parse_permission_ruleset(
                session
                    .metadata
                    .get("permission")
                    .and_then(Value::as_str)
                    .unwrap_or("FULL"),
            )
            .unwrap_or(PermissionRuleset::Full),
            true,
        )
        .tool_context();
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
            .unwrap_or_else(|| runtime_turn_message_id(&run_id, "assistant", approval_step.max(1)));
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
        let final_result = append_runtime_completion_assistant(
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
        events.push(
            SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
                .turn_terminal_event(
                    "turn/completed",
                    &run_id,
                    if failed { "failed" } else { "completed" },
                    false,
                    true,
                    false,
                    BTreeMap::from([
                        ("final_answer".to_string(), json!(answer.clone())),
                        ("final_result".to_string(), final_result),
                        ("usage".to_string(), usage.clone()),
                        ("trace".to_string(), trace.clone()),
                    ]),
                ),
        );
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
        events.push(
            SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
                .turn_terminal_event(
                    "turn/failed",
                    &run_id,
                    "failed",
                    false,
                    true,
                    false,
                    BTreeMap::from([("error".to_string(), json!("approval denied"))]),
                ),
        );
    }
    let _ = store.save_state(&session, Some(&run_id));
    append_bridge_events(&store.root, &session.id, &run_id, &mut events);
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
        events.push(
            SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
                .turn_terminal_event(
                    "turn/failed",
                    &run_id,
                    "failed",
                    false,
                    true,
                    false,
                    BTreeMap::from([(
                        "error".to_string(),
                        json!(
                            response
                                .get("note")
                                .and_then(Value::as_str)
                                .unwrap_or("question dismissed")
                        ),
                    )]),
                ),
        );
        let _ = store.save_state(&session, Some(&run_id));
        append_bridge_events(&store.root, &session.id, &run_id, &mut events);
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
    sync_plugin_runtime_metadata(config, &mut session);
    let mut runner_facade = SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
        .with_agent_options(runtime_agent_tool_options(&session, agent_profile.as_ref()));
    if let Some(value) = response.get("answers") {
        runner_facade = runner_facade.with_question_answers_value(value);
    }
    let mut ctx = runner_facade.tool_context();
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
    events.push(
        SessionRunnerFacade::new(session.directory.clone(), session.id.clone())
            .turn_terminal_event(
                "turn/completed",
                &run_id,
                "completed",
                false,
                true,
                false,
                BTreeMap::from([
                    ("final_answer".to_string(), json!(answer.clone())),
                    ("usage".to_string(), usage.clone()),
                    ("trace".to_string(), trace.clone()),
                ]),
            ),
    );
    append_bridge_events(&store.root, &session.id, &run_id, &mut events);
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
        openagent_bridge_server::TuiControlRequest::new(
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
        .or_else(|| std::env::var("OPENAGENT_BRIDGE_PERMISSION").ok())
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
            std::env::var("OPENAGENT_BRIDGE_DANGEROUSLY_SKIP_PERMISSIONS")
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

fn append_bridge_events(root: &Path, session_id: &str, turn_id: &str, events: &mut [Value]) {
    let path = bridge_events_path(root, session_id, turn_id);
    let existing = read_jsonl_values(&path).len() as u64;
    for (index, event) in events.iter_mut().enumerate() {
        normalize_bridge_event(event, session_id, turn_id, existing + index as u64 + 1);
        append_json_line(&path, event);
    }
}

fn append_unpersisted_bridge_events(
    root: &Path,
    session_id: &str,
    turn_id: &str,
    events: &mut [Value],
    persisted_events: &mut usize,
) {
    if *persisted_events >= events.len() {
        return;
    }
    append_bridge_events(root, session_id, turn_id, &mut events[*persisted_events..]);
    *persisted_events = events.len();
}

fn normalize_bridge_event(
    event: &mut Value,
    session_id: &str,
    turn_id: &str,
    fallback_sequence: u64,
) {
    let Some(object) = event.as_object_mut() else {
        return;
    };
    object
        .entry("schema_version".to_string())
        .or_insert_with(|| json!(BRIDGE_EVENT_SCHEMA_VERSION));
    object
        .entry("protocol_version".to_string())
        .or_insert_with(|| json!(BRIDGE_PROTOCOL_VERSION));
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
        .or_insert_with(|| json!(bridge_event_id(session_id, turn_id, sequence)));
}

fn bridge_event_id(session_id: &str, turn_id: &str, sequence: u64) -> String {
    format!("bridge_evt:{session_id}:{turn_id}:{sequence}")
}

fn global_sse_frames(config: &HttpRuntimeConfig, request_path: &str) -> String {
    let last_id = last_event_id_from_path(request_path);
    let mut frames = String::new();
    for (index, event) in all_bridge_events(config).into_iter().enumerate() {
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
    for event in turn_bridge_events(config, turn_id) {
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

fn all_bridge_events(config: &HttpRuntimeConfig) -> Vec<Value> {
    let root = session_root(config);
    let mut events = Vec::new();
    if let Ok(sessions) = fs::read_dir(&root) {
        for session in sessions.flatten() {
            let runs_dir = session.path().join("runs");
            if let Ok(runs) = fs::read_dir(runs_dir) {
                for run in runs.flatten() {
                    events.extend(read_jsonl_values(&run.path().join(BRIDGE_EVENTS_FILE)));
                    events.extend(read_jsonl_values(&run.path().join(LEGACY_APP_EVENTS_FILE)));
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

fn turn_bridge_events(config: &HttpRuntimeConfig, turn_id: &str) -> Vec<Value> {
    let root = session_root(config);
    if let Ok(sessions) = fs::read_dir(&root) {
        for session in sessions.flatten() {
            let session_id = session.file_name().to_string_lossy().to_string();
            let events = read_bridge_event_values(&root, &session_id, turn_id);
            if !events.is_empty() {
                return events;
            }
        }
    }
    Vec::new()
}

fn read_bridge_event_values(root: &Path, session_id: &str, turn_id: &str) -> Vec<Value> {
    let mut events = read_jsonl_values(&bridge_events_path(root, session_id, turn_id));
    events.extend(read_jsonl_values(&legacy_app_events_path(
        root, session_id, turn_id,
    )));
    events
}

fn bridge_events_path(root: &Path, session_id: &str, turn_id: &str) -> PathBuf {
    root.join(session_id)
        .join("runs")
        .join(turn_id)
        .join(BRIDGE_EVENTS_FILE)
}

fn legacy_app_events_path(root: &Path, session_id: &str, turn_id: &str) -> PathBuf {
    root.join(session_id)
        .join("runs")
        .join(turn_id)
        .join(LEGACY_APP_EVENTS_FILE)
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
        workspace: Some(workspace.to_string()),
        session_store_root: Some(session_root.to_string()),
        auth_token: Some("server-secret".to_string()),
        ..HttpRuntimeConfig::default()
    };
    let events = fixture_events();
    let text = emit_bridge_events(&events, "text", true);
    let emitted_json = emit_bridge_events(&events, "json", false);
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
            },
            "call": {
                "host": "0.0.0.0",
                "port": 8787,
                "workspace": workspace,
                "session_store_root": session_root,
                "auth_token": "server-secret",
            },
        },
        "prompt": {
            "message_text": command_text_from_args(&["hello", "runtime"], Some(""), true),
            "stdin_text": command_text_from_args(&[], Some("from stdin\n"), false),
            "empty_tty_text": command_text_from_args(&[], Some(""), true),
            "structured_turn": {
                "message": "summarize",
                "attachments": [{
                    "kind": "file",
                    "path": format!("{workspace}/notes.txt"),
                    "content_type": "text/plain",
                    "size_bytes": 11,
                }],
            },
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
            "expected_stdout_json": health_payload(&HttpRuntimeConfig::default()),
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
        assert_eq!(bridge_server_crate_name(), "openagent-bridge-server");
    }

    #[test]
    fn security_defaults_are_fail_closed() {
        let config = HttpRuntimeConfig::default();
        assert_ne!(config.cors_origin, "*");
        assert!(config.cors_origin.contains("tauri://localhost"));
        assert!(config.cors_origin.contains("http://tauri.localhost"));
    }

    #[test]
    fn bridge_auth_token_can_be_loaded_from_file_without_cli_secret() {
        let path = std::env::temp_dir().join(format!(
            "openagent-bridge-auth-{}-{}.token",
            std::process::id(),
            now_ms()
        ));
        fs::write(&path, "file-only-secret\n").expect("write auth file");
        let args = vec!["--auth-token-file".to_string(), path.display().to_string()];
        let (config, _, _) = parse_cli_args(&args);
        assert_eq!(config.auth_token.as_deref(), Some("file-only-secret"));
        assert!(!args.iter().any(|arg| arg.contains("file-only-secret")));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn bridge_lsp_routes_report_status_and_query_symbols() {
        if !openagent_lsp::command_available("python3") {
            return;
        }
        let Some(python) = python3_executable() else {
            return;
        };
        let root = std::env::temp_dir().join(format!("openagent-http-lsp-{}", now_ms()));
        fs::create_dir_all(root.join("src")).expect("workspace");
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"fake\"\n").expect("manifest");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("file");
        let fake = write_fake_lsp_server(&root);
        fs::create_dir_all(root.join(".openagent")).expect("config dir");
        fs::write(
            root.join(".openagent/lsp.json"),
            serde_json::to_string_pretty(&json!({
                "servers": {
                    "fake": {
                        "command": [python, fake],
                        "extensions": [".rs"],
                        "root_markers": ["Cargo.toml"]
                    }
                }
            }))
            .expect("config json"),
        )
        .expect("config");
        let config = HttpRuntimeConfig {
            workspace: Some(root.to_string_lossy().to_string()),
            auth_token: None,
            ..HttpRuntimeConfig::default()
        };

        for path in ["/api/lsp", "/lsp"] {
            let response = route_http_request(
                &HttpRequest {
                    method: "GET".to_string(),
                    path: path.to_string(),
                    headers: BTreeMap::new(),
                    body: String::new(),
                },
                &config,
            );
            assert_eq!(response.status, 200);
            let body = response.body.expect("lsp status");
            assert!(body["servers"].as_array().is_some_and(|servers| {
                servers
                    .iter()
                    .any(|server| server["id"] == "fake" && server["available"] == true)
            }));
        }

        let query = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/lsp/query".to_string(),
                headers: BTreeMap::new(),
                body: json!({
                    "operation": "documentSymbol",
                    "file_path": "src/main.rs",
                    "timeout_ms": 3000,
                })
                .to_string(),
            },
            &config,
        );
        assert_eq!(query.status, 200);
        let payload = query.body.expect("lsp query body");
        assert_eq!(payload["server_id"], "fake");
        assert_eq!(payload["server_ids"], json!(["fake"]));
        assert_eq!(payload["result"][0]["name"], "main");

        let _ = fs::remove_dir_all(root);
    }

    fn python3_executable() -> Option<String> {
        let output = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
        (!path.is_empty()).then_some(path)
    }

    fn write_fake_lsp_server(root: &Path) -> PathBuf {
        let path = root.join("fake_lsp.py");
        fs::write(
            &path,
            r#"
import json
import sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode("utf-8").split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers.get("content-length", "0"))
    if length <= 0:
        return None
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))

def send(message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

def result(id, value):
    send({"jsonrpc": "2.0", "id": id, "result": value})

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    id = message.get("id")
    params = message.get("params") or {}
    uri = ((params.get("textDocument") or {}).get("uri")) or "file:///fake.rs"
    if method == "initialize":
        result(id, {"capabilities": {"textDocumentSync": {"change": 1}}})
    elif method in ("initialized", "workspace/didChangeConfiguration"):
        pass
    elif method == "shutdown":
        result(id, None)
    elif method == "exit":
        break
    elif method == "textDocument/documentSymbol":
        result(id, [{
            "name": "main",
            "kind": 12,
            "location": {
                "uri": uri,
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 9}}
            }
        }])
    elif id is not None:
        result(id, [])
"#,
        )
        .expect("fake lsp script");
        path
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
        assert_eq!(provider_max_steps_with_env(&json!({}), Some("8")), 8);
        assert_eq!(
            provider_max_steps_with_env(&json!({}), Some("0")),
            UNBOUNDED_MAX_STEPS
        );
        assert_eq!(
            provider_max_steps_with_env(&json!({"max_steps": 25}), Some("8")),
            25
        );
    }

    #[test]
    fn runtime_provider_fallback_candidates_filter_unsupported_models() {
        assert_eq!(
            runtime_provider_model_candidates_with_fallback(
                "gpt-5.5",
                Some("gpt-5.3,gpt-5.4,gpt-image-2,gpt-5.5")
            ),
            vec!["gpt-5.5", "gpt-5.3", "gpt-5.4"]
        );
        assert_eq!(
            runtime_provider_model_candidates_with_fallback("gpt-5.3", Some("gpt-5.3,gpt-5.4")),
            vec!["gpt-5.3", "gpt-5.4"]
        );
        assert_eq!(
            runtime_provider_model_candidates_with_fallback("gpt-5.6-sol", None),
            vec!["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
        );
        assert_eq!(
            runtime_provider_model_candidates_with_fallback("custom-child-model", None),
            vec!["custom-child-model"]
        );
    }

    #[test]
    fn bridge_terminal_run_is_workspace_scoped() {
        let root = std::env::temp_dir().join(format!("openagent-http-terminal-{}", now_ms()));
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");
        fs::create_dir_all(&nested).expect("workspace");
        let config = HttpRuntimeConfig {
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
    fn bridge_persistent_terminal_routes_complete_the_session_lifecycle() {
        let root =
            std::env::temp_dir().join(format!("openagent-http-terminal-session-{}", now_ms()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(root.join("sessions").to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };

        let started = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: "/api/terminal/sessions".to_string(),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({})),
            },
            &config,
        );
        assert_eq!(started.status, 201);
        let started = started.body.expect("started body");
        let terminal_id = started["terminal_id"].as_str().expect("terminal id");

        let input = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: format!("/api/terminal/sessions/{terminal_id}/input"),
                headers: BTreeMap::new(),
                body: stable_json_dumps(&json!({"input": "echo route-ok"})),
            },
            &config,
        );
        assert_eq!(input.status, 200);
        thread::sleep(Duration::from_millis(100));

        let snapshot = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: format!("/api/terminal/sessions/{terminal_id}?after=0"),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(snapshot.status, 200);
        let output = snapshot.body.expect("snapshot body")["chunks"]
            .as_array()
            .expect("chunks")
            .iter()
            .filter_map(|chunk| chunk["text"].as_str())
            .collect::<String>();
        assert!(output.contains("route-ok"));

        let listed = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: "/api/terminal/sessions".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(listed.status, 200);
        assert_eq!(
            listed.body.expect("list body")["terminals"][0]["terminal_id"],
            terminal_id
        );

        let interrupted = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: format!("/api/terminal/sessions/{terminal_id}/interrupt"),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(interrupted.status, 200);
        assert_eq!(
            interrupted.body.expect("interrupt body")["status"],
            "interrupted"
        );

        let closed = route_http_request(
            &HttpRequest {
                method: "DELETE".to_string(),
                path: format!("/api/terminal/sessions/{terminal_id}"),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(closed.status, 200);
        assert_eq!(closed.body.expect("close body")["closed"], true);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bridge_mcp_status_sanitizes_config() {
        let config = HttpRuntimeConfig {
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
        let protocol = bridge_protocol_payload();
        assert_eq!(
            protocol["endpoints"]["mcp"],
            "GET /api/mcp; POST /api/mcp/servers; PATCH|DELETE /api/mcp/servers/{name}; POST /api/mcp/servers/{name}/test|start|stop|restart; GET /api/mcp/servers/{name}/oauth; POST /api/mcp/servers/{name}/oauth/login|refresh|revoke; GET /api/mcp/oauth/callback"
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
    fn bridge_mcp_server_config_crud_writes_default_file() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-crud-{}", now_ms()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
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
    fn bridge_mcp_server_config_crud_writes_local_stdio_fields() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-local-crud-{}", now_ms()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
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
    fn bridge_mcp_refresh_and_test_discover_local_stdio_tools() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-stdio-{}", now_ms()));
        let workspace = root.join("workspace");
        let tools_dir = workspace.join("tools");
        fs::create_dir_all(&tools_dir).expect("workspace tools dir");
        let fake_server = compile_fake_stdio_mcp_server(&root);
        let config = HttpRuntimeConfig {
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
    fn bridge_mcp_local_stdio_lifecycle_start_stop_restart() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-lifecycle-{}", now_ms()));
        let workspace = root.join("workspace");
        let tools_dir = workspace.join("tools");
        fs::create_dir_all(&tools_dir).expect("workspace tools dir");
        let fake_server = compile_fake_stdio_mcp_server(&root);
        let config = HttpRuntimeConfig {
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
    fn bridge_mcp_tool_call_reuses_local_stdio_lifecycle_session() {
        let root = std::env::temp_dir().join(format!("openagent-mcp-lifecycle-call-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        let tools_dir = workspace.join("tools");
        fs::create_dir_all(&tools_dir).expect("workspace tools dir");
        let request_log = root.join("stdio-requests.log");
        let fake_server = compile_fake_stdio_mcp_server(&root);
        let config = HttpRuntimeConfig {
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
        )
        .expect("create session");
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
    fn bridge_mcp_lifecycle_survives_enable_toggle() {
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
        )
        .expect("create session");
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
    fn bridge_mcp_refresh_discovers_tools_without_leaking_endpoint_secret() {
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
    fn bridge_mcp_server_test_discovers_disabled_server_tools() {
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
    fn bridge_permission_approval_round_trip_executes_allowed_tool() {
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
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
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
    fn durable_goal_lifecycle_persists_and_enters_context_pack() {
        let root = std::env::temp_dir().join(format!("openagent-http-goal-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
        let session_id = created["session_id"].as_str().expect("session id");
        let goal_path = format!("/api/sessions/{session_id}/goal");
        let mutate = |body: Value| {
            route_http_request(
                &HttpRequest {
                    method: "PUT".to_string(),
                    path: goal_path.clone(),
                    headers: BTreeMap::new(),
                    body: stable_json_dumps(&body),
                },
                &config,
            )
        };
        let created_goal = mutate(json!({
            "action": "create",
            "title": "Ship durable goal",
            "objective": "Keep the desktop goal across reloads and Bridge restarts.",
            "acceptance_criteria": ["Create and edit", "Pause and resume", "Complete"]
        }));
        assert_eq!(created_goal.status, 200);
        let created_body = created_goal.body.expect("created goal body");
        assert_eq!(created_body["goal"]["status"], "active");
        assert_eq!(created_body["goal"]["revision"], 1);

        let store = FileSessionStore::new(session_root.clone());
        let mut session = store.load_session(session_id).expect("load session");
        let pack = runtime_context_pack_for_agent(
            &store,
            &mut session,
            &[],
            &BTreeMap::new(),
            None,
            None,
            ContextPackBuildOptions {
                trace_only: false,
                ..ContextPackBuildOptions::default()
            },
        );
        let goal_trace = pack
            .trace
            .iter()
            .find(|entry| entry.kind == "goal")
            .expect("goal context trace");
        assert!(goal_trace.included);
        assert!(goal_trace.pinned);
        assert!(pack.messages.iter().any(|message| {
            message.content.contains("[Durable goal]")
                && message.content.contains("Keep the desktop goal")
        }));

        let updated = mutate(json!({
            "action": "update",
            "objective": "Persist and explain the goal through ContextPack."
        }));
        assert_eq!(updated.body.expect("updated body")["goal"]["revision"], 2);
        assert_eq!(
            mutate(json!({"action": "pause"})).body.expect("pause body")["goal"]["status"],
            "paused"
        );

        let restarted_config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let restored = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: goal_path.clone(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &restarted_config,
        );
        assert_eq!(restored.status, 200);
        assert_eq!(
            restored.body.expect("restored body")["goal"]["status"],
            "paused"
        );
        assert_eq!(
            mutate(json!({"action": "resume"}))
                .body
                .expect("resume body")["goal"]["status"],
            "active"
        );
        let completed = mutate(json!({"action": "complete"}))
            .body
            .expect("complete body");
        assert_eq!(completed["goal"]["status"], "completed");
        assert!(completed["goal"]["completed_at_ms"].as_u64().is_some());
        let invalid_resume = mutate(json!({"action": "resume"}));
        assert_eq!(invalid_resume.status, 400);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn durable_plan_enforces_read_only_until_explicit_execution() {
        let root = std::env::temp_dir().join(format!("openagent-http-plan-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
        let session_id = created["session_id"].as_str().expect("session id");
        let plan_path = format!("/api/sessions/{session_id}/plan");
        let mutate = |body: Value| {
            route_http_request(
                &HttpRequest {
                    method: "PUT".to_string(),
                    path: plan_path.clone(),
                    headers: BTreeMap::new(),
                    body: stable_json_dumps(&body),
                },
                &config,
            )
        };
        let created_plan = mutate(json!({
            "action": "create",
            "title": "Plan a safe change",
            "objective": "Inspect first, then explicitly execute.",
            "steps": ["Read the workspace", "Write only after conversion"]
        }));
        assert_eq!(created_plan.status, 200);
        assert_eq!(
            created_plan.body.expect("created plan")["plan"]["status"],
            "planning"
        );

        let store = FileSessionStore::new(session_root.clone());
        let mut session = store.load_session(session_id).expect("load session");
        let pack = runtime_context_pack_for_agent(
            &store,
            &mut session,
            &[],
            &BTreeMap::new(),
            None,
            None,
            ContextPackBuildOptions {
                trace_only: false,
                ..ContextPackBuildOptions::default()
            },
        );
        let plan_trace = pack
            .trace
            .iter()
            .find(|entry| entry.kind == "plan")
            .expect("plan trace");
        assert!(plan_trace.included);
        assert!(plan_trace.pinned);
        assert!(pack.messages.iter().any(|message| {
            message.content.contains("[Durable plan]")
                && message.content.contains("inspect and plan only")
        }));

        let blocked_path = workspace.join("blocked.txt");
        let blocked = start_turn_payload(
            &config,
            session_id,
            &stable_json_dumps(&json!({
                "input": "try to write while planning",
                "permission": "FULL",
                "dangerously_skip_permissions": true,
                "tool_call": {
                    "call_id": "call_plan_blocked",
                    "name": "write",
                    "input": {"file_path": "blocked.txt", "content": "must not exist\n"}
                }
            })),
        )
        .expect("planning turn");
        assert!(
            !blocked_path.exists(),
            "plan mode allowed a write despite FULL + skip"
        );
        assert!(stable_json_dumps(&blocked).contains("permission_denied"));
        let enforced_session = store.load_session(session_id).expect("enforced session");
        assert_eq!(
            enforced_session.metadata["latest_plan_enforcement"]["permission"],
            "READONLY"
        );
        assert_eq!(
            enforced_session.metadata["latest_plan_enforcement"]["skip_permissions"],
            false
        );

        let restarted_config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let restored = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: plan_path.clone(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &restarted_config,
        );
        assert_eq!(
            restored.body.expect("restored plan")["plan"]["status"],
            "planning"
        );

        let executing = mutate(json!({"action": "execute"}));
        assert_eq!(
            executing.body.expect("executing plan")["plan"]["status"],
            "executing"
        );
        let allowed_path = workspace.join("allowed.txt");
        start_turn_payload(
            &config,
            session_id,
            &stable_json_dumps(&json!({
                "input": "execute the approved plan",
                "permission": "FULL",
                "dangerously_skip_permissions": true,
                "tool_call": {
                    "call_id": "call_plan_allowed",
                    "name": "write",
                    "input": {"file_path": "allowed.txt", "content": "executed\n"}
                }
            })),
        )
        .expect("execution turn");
        assert_eq!(
            fs::read_to_string(allowed_path).expect("allowed write"),
            "executed\n"
        );
        let completed = mutate(json!({"action": "complete"}));
        assert_eq!(
            completed.body.expect("completed plan")["plan"]["status"],
            "completed"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_task_tree_preserves_parent_child_statuses_across_restart() {
        let root = std::env::temp_dir().join(format!("openagent-http-task-tree-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let store = FileSessionStore::new(session_root.clone());
        let parent_id = "session_task_tree_parent";
        let parent = Session::new(parent_id, workspace.clone());
        store.save_state(&parent, None).expect("save parent");

        let task = |id: &str, parent: &str, title: &str, status: &str, depth: u64| {
            let mut session = Session::new(id, workspace.clone());
            session.metadata.extend(BTreeMap::from([
                ("subagent".to_string(), json!(true)),
                ("parent_session_id".to_string(), json!(parent)),
                ("task_description".to_string(), json!(title)),
                ("task_status".to_string(), json!(status)),
                ("task_depth".to_string(), json!(depth)),
                ("agent".to_string(), json!("explorer")),
            ]));
            store.save_state(&session, None).expect("save task");
        };
        task(
            "session_task_tree_running",
            parent_id,
            "Inspect runtime",
            "running",
            1,
        );
        task(
            "session_task_tree_waiting",
            "session_task_tree_running",
            "Wait for approval",
            "waiting_approval",
            2,
        );
        task(
            "session_task_tree_cancelled",
            parent_id,
            "Discard stale branch",
            "canceled",
            1,
        );

        let request = HttpRequest {
            method: "GET".to_string(),
            path: format!("/api/sessions/{parent_id}/tasks"),
            headers: BTreeMap::new(),
            body: String::new(),
        };
        let response = route_http_request(&request, &config);
        assert_eq!(response.status, 200);
        let payload = response.body.expect("task tree payload");
        assert_eq!(payload["schema_version"], "openagent.session_task_tree.v2");
        assert_eq!(payload["count"], 3);
        assert_eq!(payload["status_counts"]["running"], 1);
        assert_eq!(payload["status_counts"]["waiting"], 1);
        assert_eq!(payload["status_counts"]["cancelled"], 1);
        let tree = payload["tree"].as_array().expect("task tree");
        let running = tree
            .iter()
            .find(|task| task["title"] == "Inspect runtime")
            .expect("running root task");
        assert_eq!(running["canonical_status"], "running");
        let waiting = running["children"]
            .as_array()
            .and_then(|children| children.first())
            .expect("nested waiting task");
        assert_eq!(waiting["title"], "Wait for approval");
        assert_eq!(waiting["canonical_status"], "waiting");

        let restarted_config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let restored = route_http_request(&request, &restarted_config)
            .body
            .expect("restored task tree");
        assert_eq!(restored["tree"], payload["tree"]);
        assert_eq!(restored["status_counts"], payload["status_counts"]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bridge_turn_persists_rich_attachment_metadata_and_context_decisions() {
        let root = std::env::temp_dir().join(format!("openagent-http-attachment-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
        let session_id = created
            .get("session_id")
            .and_then(Value::as_str)
            .expect("session id");
        let started = start_turn_payload(
            &config,
            session_id,
            &stable_json_dumps(&json!({
                "input": "review attachment",
                "permission": "FULL",
                "attachments": [
                    {
                        "kind": "document",
                        "path": "/tmp/openagent-note.md",
                        "name": "openagent-note.md",
                        "size_bytes": 900000,
                        "content_type": "text/markdown",
                        "content": "hello attached\n",
                        "source": "desktop_file_picker",
                        "truncated": true,
                        "truncation_reason": "desktop_attachment_content_limit",
                        "original_content_bytes": 900000,
                        "included_content_bytes": 15
                    },
                    {
                        "kind": "pdf",
                        "path": "/tmp/spec.pdf",
                        "name": "spec.pdf",
                        "size_bytes": 1200000,
                        "content_type": "application/pdf",
                        "content": "",
                        "source": "desktop_file_picker",
                        "page_count": 9,
                        "truncated": true,
                        "truncation_reason": "pdf_binary_metadata_only",
                        "original_content_bytes": 1200000,
                        "included_content_bytes": 0
                    },
                    {
                        "kind": "image",
                        "path": "/tmp/design.png",
                        "name": "design.png",
                        "size_bytes": 64000,
                        "content_type": "image/png",
                        "content": "",
                        "source": "desktop_file_picker",
                        "media_metadata": {"width_px": 1440, "height_px": 900},
                        "truncated": true,
                        "truncation_reason": "image_binary_metadata_only",
                        "original_content_bytes": 64000,
                        "included_content_bytes": 0
                    }
                ],
                "tool_call": {
                    "call_id": "call_bash",
                    "name": "bash",
                    "input": {"command": "printf ok"}
                }
            })),
        )
        .expect("start turn");
        assert_eq!(started["status"], "completed");

        let messages = session_messages_payload(
            &config,
            session_id,
            &format!("/api/sessions/{session_id}/messages?limit=20"),
        )
        .expect("messages");
        let messages_v2 = messages["messages_v2"].as_array().expect("messages v2");
        let user = messages_v2
            .iter()
            .find(|message| message["info"]["role"] == "user")
            .expect("user message");
        assert_eq!(
            user["info"]["metadata"]["display_content"],
            json!("review attachment")
        );
        assert_eq!(
            user["info"]["metadata"]["attachments"][0]["name"],
            json!("openagent-note.md")
        );
        assert_eq!(
            user["info"]["metadata"]["attachments"][1]["page_count"],
            json!(9)
        );
        assert_eq!(
            user["info"]["metadata"]["attachments"][2]["media_metadata"]["width_px"],
            json!(1440)
        );
        let attachment_id = user["info"]["metadata"]["attachments"][0]["id"]
            .as_str()
            .expect("attachment id");
        assert!(attachment_id.starts_with("att_"));
        let text = user["parts"][0]["content"]
            .as_str()
            .expect("user text part");
        assert_eq!(text, "review attachment");
        let file = user["parts"]
            .as_array()
            .expect("user parts")
            .iter()
            .find(|part| part["kind"] == "file")
            .expect("file part");
        assert_eq!(file["content"]["id"], json!(attachment_id));
        assert_eq!(file["content"]["kind"], json!("document"));
        assert_eq!(file["content"]["path"], json!("/tmp/openagent-note.md"));
        assert_eq!(file["content"]["content"], json!("hello attached\n"));

        let store = FileSessionStore::new(session_root);
        let mut restored = store.load_session(session_id).expect("restored session");
        let materialized =
            runtime_materialized_provider_context_for_agent(&store, &mut restored, None);
        assert_eq!(materialized.attachments.len(), 3);
        assert_eq!(materialized.attachments[0].id, attachment_id);
        assert_eq!(materialized.attachments[0].source_message_index, Some(0));
        assert_eq!(materialized.attachments[1].kind, ContextAttachmentKind::Pdf);
        assert_eq!(materialized.attachments[1].page_count, Some(9));
        assert_eq!(
            materialized.attachments[2].media_metadata["height_px"],
            json!(900)
        );
        let pack = runtime_context_pack_for_agent(
            &store,
            &mut restored,
            &[],
            &BTreeMap::new(),
            None,
            None,
            ContextPackBuildOptions {
                trace_only: false,
                ..ContextPackBuildOptions::default()
            },
        );
        let user_index = pack
            .messages
            .iter()
            .position(|message| {
                message.role == Role::User && message.content == "review attachment"
            })
            .expect("original user message");
        assert_eq!(pack.messages[user_index + 1].role, Role::User);
        assert!(
            pack.messages[user_index + 1]
                .content
                .contains("hello attached")
        );
        assert_eq!(
            pack.receipt.item_kind_counts.get("attachment_document"),
            Some(&1)
        );
        assert_eq!(
            pack.receipt.item_kind_counts.get("attachment_pdf"),
            Some(&1)
        );
        assert_eq!(
            pack.receipt.item_kind_counts.get("attachment_image"),
            Some(&1)
        );
        let attachment_trace = pack
            .trace
            .iter()
            .filter_map(|entry| entry.attachment.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(attachment_trace.len(), 3);
        assert!(attachment_trace.iter().all(|entry| entry.source_truncated));
        assert_eq!(attachment_trace[1].page_count, Some(9));
        let public = public_context_trace_entry(
            pack.trace
                .iter()
                .find(|entry| entry.kind == "attachment_pdf")
                .expect("pdf trace"),
        );
        assert_eq!(public["attachment"]["name"], json!("spec.pdf"));
        assert_eq!(public["attachment"]["page_count"], json!(9));
        assert!(public["attachment"].get("content").is_none());
        assert_eq!(pack.receipt.item_kind_counts.get("checkpoint"), Some(&1));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_system_history_survives_restart_and_replays_through_context_builder() {
        let root = std::env::temp_dir().join(format!("openagent-http-legacy-system-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
        let session_id = created["session_id"].as_str().expect("session id");
        let store = FileSessionStore::new(session_root);
        let mut session = store.load_session(session_id).expect("session");

        let mut compacted_user =
            runtime_chat_message(Role::User, "Old request before compaction".to_string());
        compacted_user
            .metadata
            .insert("message_id".to_string(), json!("compacted-user-0"));
        session.add(compacted_user.clone());
        store
            .append_message(&session, &compacted_user, "run_legacy", 0)
            .expect("append compacted user");

        store
            .append_compaction_boundary(
                &mut session,
                "run_legacy",
                "Resume the compacted implementation",
                "compacted-user-0",
            )
            .expect("append compaction boundary");

        let mut stale_profile =
            runtime_chat_message(Role::System, "STALE_PROFILE_SYSTEM".to_string());
        stale_profile
            .metadata
            .insert("agent_profile".to_string(), json!("old-profile"));
        stale_profile
            .metadata
            .insert("message_id".to_string(), json!("legacy-profile-2"));
        session.add(stale_profile.clone());
        store
            .append_message(&session, &stale_profile, "run_legacy", 2)
            .expect("append stale profile");

        let mut legacy = runtime_chat_message(Role::System, "LEGACY_PROJECT_SYSTEM".to_string());
        legacy
            .metadata
            .insert("message_id".to_string(), json!("legacy-system-3"));
        session.add(legacy.clone());
        store
            .append_message(&session, &legacy, "run_legacy", 3)
            .expect("append legacy system");

        let mut user = runtime_chat_message(Role::User, "Continue the migration".to_string());
        user.metadata.insert(
            "message_id".to_string(),
            json!(runtime_turn_message_id("run_legacy", "user", 4)),
        );
        session.add(user.clone());
        store
            .append_message(&session, &user, "run_legacy", 4)
            .expect("append user");
        store
            .save_state(&session, Some("run_legacy"))
            .expect("save legacy session");

        let mut restarted = FileSessionStore::new(root.join("sessions"))
            .load_session(session_id)
            .expect("restart session");
        let materialized =
            runtime_materialized_provider_context_for_agent(&store, &mut restarted, None);
        assert_eq!(materialized.source_message_count, 4);
        assert_eq!(materialized.messages, vec![user]);
        assert_eq!(materialized.system_sources.legacy_system_sources.len(), 1);
        assert_eq!(
            materialized
                .work_state
                .as_ref()
                .map(|work_state| work_state.summary.as_str()),
            Some("Resume the compacted implementation")
        );

        let build_options = ContextPackBuildOptions {
            trace_only: false,
            ..ContextPackBuildOptions::default()
        };
        let pack = runtime_context_pack_for_agent(
            &store,
            &mut restarted,
            &[],
            &BTreeMap::new(),
            None,
            None,
            build_options.clone(),
        );
        let dynamic_system = pack
            .messages
            .iter()
            .find(|message| {
                message
                    .metadata
                    .get("dynamic_system_prompt")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .expect("dynamic system");
        assert!(dynamic_system.content.contains("LEGACY_PROJECT_SYSTEM"));
        assert!(!dynamic_system.content.contains("STALE_PROFILE_SYSTEM"));
        assert_eq!(
            pack.system_diagnostics
                .as_ref()
                .map(|diagnostics| diagnostics.legacy_system_count),
            Some(1)
        );
        assert_eq!(pack.receipt.item_kind_counts.get("legacy_system"), Some(&1));
        pack.validate_provider_input().expect("provider input");

        let spec =
            runtime_context_replay_spec(&store, &mut restarted, &pack, None, None, build_options);
        runtime_persist_context_pack_receipt_with_replay(
            &store,
            &mut restarted,
            "run_legacy",
            1,
            &pack,
            None,
            Some(&spec),
        )
        .expect("persist replay receipt");
        let replay = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: format!("/api/sessions/{session_id}/context/replay"),
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            },
            &config,
        );
        assert_eq!(replay.status, 200);
        let replay = replay.body.expect("replay body");
        assert_eq!(replay["status"], "verified");
        assert_eq!(
            replay["rebuilt"]["receipt"]["pack_hash"],
            json!(pack.pack_hash)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn context_diagnostics_are_persistent_explainable_and_redacted() {
        let root = std::env::temp_dir().join(format!("openagent-http-context-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
        let session_id = created["session_id"].as_str().expect("session id");
        let store = FileSessionStore::new(session_root.clone());
        let mut session = store.load_session(session_id).expect("session");
        let mut required = ContextItem::new(
            "instruction:test",
            "instruction",
            "https://user:password@example.test/AGENTS.md?token=super-secret",
            "PRIVATE PROMPT BODY",
            100,
        );
        required.pinned = true;
        required.stable_prefix = true;
        required.token_estimate = 40;
        let mut dropped = ContextItem::new(
            "attachment:secret",
            "attachment_file",
            "session.messages[0].attachments[0]",
            "ATTACHMENT SECRET BODY",
            1,
        );
        dropped.token_estimate = 200;
        let pack = ContextPackBuilder::new(Some(ContextPackBuildOptions {
            token_budget: Some(120),
            trace_only: false,
            ..ContextPackBuildOptions::default()
        }))
        .build(ContextPackInput {
            extra_items: vec![required, dropped],
            ..ContextPackInput::default()
        });
        let mut performance = ContextPackPerformance::new();
        performance.materialize_us = 1_250;
        performance.build_us = 2_500;
        performance.persist_us = 3_750;
        performance.provider_payload_build_us = 400;
        performance.provider_payload_serialize_us = 500;
        performance.provider_payload_bytes = 8_192;
        performance.source_message_count = 2;
        performance.tool_count = 1;
        performance.item_count = 2;
        performance.refresh_warnings();
        runtime_persist_context_pack_receipt_with_diagnostics(
            &store,
            &mut session,
            "run_context",
            1,
            &pack,
            None,
            None,
            Some(&performance),
            None,
        )
        .expect("persist context receipt");

        let response = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: format!("/api/sessions/{session_id}/context?limit=4"),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(response.status, 200);
        let diagnostics = response.body.expect("context diagnostics body");
        assert_eq!(
            diagnostics["schema_version"],
            CONTEXT_DIAGNOSTICS_SCHEMA_VERSION
        );
        assert_eq!(diagnostics["status"], "ready");
        assert_eq!(diagnostics["latest"]["receipt"]["included_item_count"], 1);
        assert_eq!(diagnostics["latest"]["receipt"]["dropped_item_count"], 1);
        assert_eq!(
            diagnostics["latest"]["trace"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(diagnostics["latest"]["performance"]["status"], "ok");
        assert_eq!(
            diagnostics["latest"]["performance"]["provider_payload_bytes"],
            8_192
        );
        assert_eq!(
            diagnostics["latest"]["performance"]["source_message_count"],
            2
        );
        assert_eq!(
            diagnostics["latest"]["trace"][0]["source"],
            "https://example.test/AGENTS.md"
        );
        assert_eq!(diagnostics["redaction"]["content_included"], false);
        let serialized = stable_json_dumps(&diagnostics);
        for forbidden in [
            "PRIVATE PROMPT BODY",
            "ATTACHMENT SECRET BODY",
            "password",
            "super-secret",
        ] {
            assert!(!serialized.contains(forbidden), "leaked `{forbidden}`");
        }

        let restarted = session_context_diagnostics_payload(
            &config,
            session_id,
            &format!("/api/sessions/{session_id}/context?limit=1"),
        )
        .expect("diagnostics after restart");
        assert_eq!(restarted["latest"]["receipt"]["pack_hash"], pack.pack_hash);
        let event = context_updated_bridge_event(&session, "run_context", 1).expect("event");
        assert_eq!(event["method"], "context/updated");
        let event_text = stable_json_dumps(&event);
        assert!(!event_text.contains("PRIVATE PROMPT BODY"));
        assert!(!event_text.contains("ATTACHMENT SECRET BODY"));
        assert_eq!(
            event["params"]["diagnostics"]["performance"]["provider_payload_bytes"],
            8_192
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn context_diagnostics_expose_stable_failures_without_private_details() {
        let root =
            std::env::temp_dir().join(format!("openagent-http-context-failure-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
        let session_id = created["session_id"].as_str().expect("session id");
        let store = FileSessionStore::new(session_root);
        let mut session = store.load_session(session_id).expect("session");

        let unavailable = context_diagnostics_payload_for_session(&session, 4);
        assert_eq!(unavailable["status"], "unavailable");
        assert_eq!(
            unavailable["failure"]["code"],
            ContextFailureCode::Unavailable.as_str()
        );

        session
            .metadata
            .insert("context_pack".to_string(), json!({"corrupt": true}));
        let corrupt = context_diagnostics_payload_for_session(&session, 4);
        assert_eq!(corrupt["status"], "corrupt");
        assert_eq!(
            corrupt["failure"]["code"],
            ContextFailureCode::ReceiptCorrupt.as_str()
        );

        let private_failure = ContextFailure::new(
            ContextFailureCode::BudgetExceeded,
            "budget",
            "budget exceeded",
        )
        .with_details(BTreeMap::from([
            ("model".to_string(), json!("gpt-safe")),
            ("api_key".to_string(), json!("sk-never-public")),
            ("prompt".to_string(), json!("PRIVATE PROMPT")),
            ("estimated_input_tokens".to_string(), json!(120_000)),
        ]));
        let public = public_context_failure(&private_failure);
        assert_eq!(public["details"]["model"], "gpt-safe");
        assert_eq!(public["details"]["estimated_input_tokens"], 120_000);
        let serialized = stable_json_dumps(&public);
        assert!(!serialized.contains("sk-never-public"));
        assert!(!serialized.contains("PRIVATE PROMPT"));

        let drift = context_replay_failure("drifted", &["source_changed".to_string()])
            .expect("drift failure");
        assert_eq!(drift.code, ContextFailureCode::SourceDrift.as_str());
        let unsupported = context_replay_failure("unrecoverable", &["legacy".to_string()])
            .expect("unsupported failure");
        assert_eq!(
            unsupported.code,
            ContextFailureCode::ReplayUnsupported.as_str()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn context_replay_verifies_historical_boundary_and_recovers_corrupt_latest() {
        let root = std::env::temp_dir().join(format!("openagent-http-replay-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
        let session_id = created["session_id"].as_str().expect("session id");
        let store = FileSessionStore::new(session_root);
        let mut session = store.load_session(session_id).expect("session");
        let mut user =
            runtime_chat_message(Role::User, "private replay boundary message".to_string());
        user.metadata.insert(
            "message_id".to_string(),
            json!(runtime_turn_message_id("run_replay", "user", 0)),
        );
        session.add(user.clone());
        store
            .append_message(&session, &user, "run_replay", 0)
            .expect("append replay user");
        store
            .save_state(&session, Some("run_replay"))
            .expect("save replay user");

        let build_options = ContextPackBuildOptions {
            trace_only: false,
            ..ContextPackBuildOptions::default()
        };
        let pack = runtime_context_pack_for_agent(
            &store,
            &mut session,
            &[],
            &BTreeMap::new(),
            None,
            None,
            build_options.clone(),
        );
        let spec =
            runtime_context_replay_spec(&store, &mut session, &pack, None, None, build_options);
        runtime_persist_context_pack_receipt_with_replay(
            &store,
            &mut session,
            "run_replay",
            1,
            &pack,
            None,
            Some(&spec),
        )
        .expect("persist replayable receipt");

        let mut later = runtime_chat_message(
            Role::Assistant,
            "later message must not enter historical replay".to_string(),
        );
        later.metadata.insert(
            "message_id".to_string(),
            json!(runtime_turn_message_id("run_replay", "assistant", 1)),
        );
        let later_index = session.messages.len() as u64;
        session.add(later.clone());
        store
            .append_message(&session, &later, "run_replay", later_index)
            .expect("append later message");
        store
            .save_state(&session, Some("run_replay"))
            .expect("save later message");

        let verified_response = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: format!("/api/sessions/{session_id}/context/replay"),
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            },
            &config,
        );
        assert_eq!(verified_response.status, 200);
        let verified = verified_response.body.expect("verified replay body");
        assert_eq!(verified["status"], "verified");
        assert_eq!(
            verified["target"]["receipt"]["pack_hash"],
            json!(pack.pack_hash)
        );
        assert_eq!(
            verified["rebuilt"]["receipt"]["pack_hash"],
            verified["target"]["receipt"]["pack_hash"]
        );
        assert_eq!(verified["side_effects"]["provider_calls"], 0);
        assert_eq!(verified["side_effects"]["tool_calls"], 0);
        assert_eq!(verified["side_effects"]["checkpoint_restores"], 0);
        assert_eq!(verified["side_effects"]["mcp_lifecycle_changes"], 0);
        assert!(!stable_json_dumps(&verified).contains("private replay boundary message"));

        let mut corrupt = store.load_session(session_id).expect("reloaded session");
        corrupt
            .metadata
            .insert("context_pack".to_string(), json!({"corrupt": true}));
        store
            .save_state(&corrupt, Some("run_replay"))
            .expect("persist corrupt latest");
        let rebuilt_response = route_http_request(
            &HttpRequest {
                method: "POST".to_string(),
                path: format!("/api/sessions/{session_id}/context/replay"),
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            },
            &config,
        );
        assert_eq!(rebuilt_response.status, 200);
        let rebuilt = rebuilt_response.body.expect("rebuilt replay body");
        assert_eq!(rebuilt["status"], "rebuilt");
        assert_eq!(rebuilt["diagnostics"]["status"], "ready");
        assert_eq!(rebuilt["diagnostics"]["latest"]["mode"], "recovery");
        assert_eq!(
            rebuilt["diagnostics"]["latest"]["receipt"]["pack_hash"],
            json!(pack.pack_hash)
        );
        assert_eq!(
            rebuilt["diagnostics"]["last_replay"]["side_effects"]["provider_calls"],
            0
        );

        let restarted = FileSessionStore::new(root.join("sessions"))
            .load_session(session_id)
            .expect("recovery survives restart");
        let diagnostics = context_diagnostics_payload_for_session(&restarted, 4);
        assert_eq!(diagnostics["latest"]["mode"], "recovery");
        assert_eq!(diagnostics["last_replay"]["status"], "rebuilt");
        assert!(!stable_json_dumps(&diagnostics).contains("private replay boundary message"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn context_replay_spec_keeps_known_model_options_and_drops_unknown_values() {
        let options = BTreeMap::from([
            ("reasoning_effort".to_string(), json!("high")),
            ("service_tier".to_string(), json!("priority")),
            (
                "custom_auth_token".to_string(),
                json!("private-context-replay-secret"),
            ),
        ]);
        let (safe, unsafe_keys) = safe_context_replay_model_options(&options);
        assert_eq!(safe["reasoning_effort"], "high");
        assert_eq!(safe["service_tier"], "priority");
        assert_eq!(unsafe_keys, vec!["custom_auth_token"]);
        assert!(
            !serde_json::to_string(&safe)
                .expect("safe options")
                .contains("private-context-replay-secret")
        );
    }

    #[test]
    fn bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint() {
        let root = std::env::temp_dir().join(format!("openagent-http-trust-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_root.to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": workspace.to_string_lossy()})),
        )
        .expect("create session");
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

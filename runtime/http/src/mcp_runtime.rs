use super::*;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Debug)]
pub(super) struct McpConfigSource {
    pub(super) label: &'static str,
    pub(super) read_source: Option<String>,
    pub(super) writable_path: Option<PathBuf>,
    pub(super) readonly_reason: Option<&'static str>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum McpServersShape {
    Canonical,
    McpServers,
    McpNested,
    McpDirect,
    RootDirect,
}

pub(super) struct McpLifecycleEntry {
    pub(super) config_fingerprint: String,
    pub(super) session: StdioMcpSession,
    pub(super) descriptors: Vec<RemoteMcpToolDescriptor>,
    pub(super) started_at_ms: u64,
    pub(super) last_refreshed_at_ms: u64,
}

#[derive(Clone, Debug)]
pub(super) struct McpLifecycleSnapshot {
    pub(super) status: &'static str,
    pub(super) pid: Option<u32>,
    pub(super) started_at_ms: Option<u64>,
    pub(super) last_refreshed_at_ms: Option<u64>,
    pub(super) tool_count: usize,
}

#[derive(Debug)]
pub(super) struct McpConfigMutationError {
    pub(super) status: u16,
    pub(super) code: &'static str,
    pub(super) message: String,
}

pub(super) fn mcp_lifecycle_registry() -> &'static Mutex<BTreeMap<String, McpLifecycleEntry>> {
    static MCP_LIFECYCLE_REGISTRY: OnceLock<Mutex<BTreeMap<String, McpLifecycleEntry>>> =
        OnceLock::new();
    MCP_LIFECYCLE_REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn mcp_lifecycle_key(workspace: &Path, server_name: &str) -> String {
    format!("{}::{}", workspace.display(), server_name)
}

pub(super) fn mcp_server_fingerprint(server: &RemoteMcpServerConfig) -> String {
    let mut lifecycle_config = server.clone();
    lifecycle_config.enabled = true;
    serde_json::to_value(lifecycle_config)
        .map(|value| stable_json_dumps(&value))
        .unwrap_or_else(|_| server.name.clone())
}

pub(super) fn mcp_lifecycle_snapshot(
    server: &RemoteMcpServerConfig,
    workspace: &Path,
) -> McpLifecycleSnapshot {
    if server.server_type != McpServerType::Local {
        return McpLifecycleSnapshot {
            status: "not_applicable",
            pid: None,
            started_at_ms: None,
            last_refreshed_at_ms: None,
            tool_count: 0,
        };
    }
    let key = mcp_lifecycle_key(workspace, &server.name);
    let fingerprint = mcp_server_fingerprint(server);
    let Ok(mut registry) = mcp_lifecycle_registry().lock() else {
        return McpLifecycleSnapshot {
            status: "unavailable",
            pid: None,
            started_at_ms: None,
            last_refreshed_at_ms: None,
            tool_count: 0,
        };
    };
    let removal_snapshot = {
        let Some(entry) = registry.get_mut(&key) else {
            return McpLifecycleSnapshot {
                status: "stopped",
                pid: None,
                started_at_ms: None,
                last_refreshed_at_ms: None,
                tool_count: 0,
            };
        };
        if entry.config_fingerprint != fingerprint {
            Some(McpLifecycleSnapshot {
                status: "stale",
                pid: None,
                started_at_ms: None,
                last_refreshed_at_ms: None,
                tool_count: 0,
            })
        } else if entry.session.running() {
            return McpLifecycleSnapshot {
                status: "running",
                pid: Some(entry.session.pid()),
                started_at_ms: Some(entry.started_at_ms),
                last_refreshed_at_ms: Some(entry.last_refreshed_at_ms),
                tool_count: entry.descriptors.len(),
            };
        } else {
            Some(McpLifecycleSnapshot {
                status: "exited",
                pid: None,
                started_at_ms: Some(entry.started_at_ms),
                last_refreshed_at_ms: Some(entry.last_refreshed_at_ms),
                tool_count: entry.descriptors.len(),
            })
        }
    };
    if let Some(snapshot) = removal_snapshot {
        if let Some(entry) = registry.remove(&key) {
            drop(registry);
            entry.session.close();
        }
        return snapshot;
    }
    McpLifecycleSnapshot {
        status: "stopped",
        pid: None,
        started_at_ms: None,
        last_refreshed_at_ms: None,
        tool_count: 0,
    }
}

pub(super) fn refresh_mcp_lifecycle_server(
    server: &RemoteMcpServerConfig,
    workspace: &Path,
) -> Option<Result<Vec<RemoteMcpToolDescriptor>, String>> {
    if server.server_type != McpServerType::Local {
        return None;
    }
    let key = mcp_lifecycle_key(workspace, &server.name);
    let fingerprint = mcp_server_fingerprint(server);
    let Ok(mut registry) = mcp_lifecycle_registry().lock() else {
        return Some(Err("MCP lifecycle registry is unavailable".to_string()));
    };
    let remove_error = {
        let entry = registry.get_mut(&key)?;
        if entry.config_fingerprint != fingerprint {
            Some("MCP local lifecycle process was stopped because config changed".to_string())
        } else if !entry.session.running() {
            Some("MCP local lifecycle process exited".to_string())
        } else {
            match entry.session.tools_list() {
                Ok(tools) => {
                    let descriptors = build_tool_descriptors_from_values(server, &tools);
                    entry.last_refreshed_at_ms = now_ms();
                    entry.descriptors = descriptors.clone();
                    return Some(Ok(descriptors));
                }
                Err(error) => Some(error),
            }
        }
    };
    let entry = registry.remove(&key);
    drop(registry);
    if let Some(entry) = entry {
        entry.session.close();
    }
    remove_error.map(Err)
}

pub(super) fn stop_mcp_lifecycle_server(server: &RemoteMcpServerConfig, workspace: &Path) -> bool {
    let key = mcp_lifecycle_key(workspace, &server.name);
    let entry = mcp_lifecycle_registry()
        .lock()
        .ok()
        .and_then(|mut registry| registry.remove(&key));
    if let Some(entry) = entry {
        entry.session.close();
        true
    } else {
        false
    }
}

pub(super) fn apply_mcp_lifecycle_to_manager(manager: &mut RemoteMcpManager, workspace: &Path) {
    let updates = manager
        .config
        .servers
        .iter()
        .filter(|server| server.server_type == McpServerType::Local)
        .filter_map(|server| {
            let key = mcp_lifecycle_key(workspace, &server.name);
            let fingerprint = mcp_server_fingerprint(server);
            let update = {
                let Ok(mut registry) = mcp_lifecycle_registry().lock() else {
                    return None;
                };
                let entry = registry.get_mut(&key)?;
                if entry.config_fingerprint == fingerprint && entry.session.running() {
                    Some((
                        server.name.clone(),
                        entry.descriptors.clone(),
                        Some(entry.last_refreshed_at_ms as f64 / 1000.0),
                    ))
                } else {
                    None
                }
            };
            update
        })
        .collect::<Vec<_>>();
    for (server_name, descriptors, refreshed_at) in updates {
        let _ = manager.set_server_tools(
            &server_name,
            Some(McpTransport::Stdio),
            "connected",
            refreshed_at,
            descriptors,
        );
    }
}

pub(super) fn mcp_config_source(
    config: &HttpRuntimeConfig,
    env: &BTreeMap<String, String>,
) -> McpConfigSource {
    let default_workspace = workspace(config);
    mcp_config_source_for_workspace(config, env, &default_workspace)
}

pub(super) fn mcp_config_source_for_workspace(
    config: &HttpRuntimeConfig,
    env: &BTreeMap<String, String>,
    default_workspace: &Path,
) -> McpConfigSource {
    if let Some(raw) = config
        .mcp_config
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if looks_like_inline_json(raw) {
            return McpConfigSource {
                label: "config",
                read_source: Some(raw.to_string()),
                writable_path: None,
                readonly_reason: Some("inline_config_readonly"),
            };
        }
        let path = PathBuf::from(raw);
        return McpConfigSource {
            label: "config",
            read_source: path.exists().then(|| raw.to_string()),
            writable_path: Some(path),
            readonly_reason: None,
        };
    }
    if let Some(raw) = env
        .get("OPENAGENT_MCP_CONFIG")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if looks_like_inline_json(raw) {
            return McpConfigSource {
                label: "env",
                read_source: Some(raw.to_string()),
                writable_path: None,
                readonly_reason: Some("inline_env_readonly"),
            };
        }
        let path = PathBuf::from(raw);
        return McpConfigSource {
            label: "env",
            read_source: path.exists().then(|| raw.to_string()),
            writable_path: Some(path),
            readonly_reason: None,
        };
    }
    let default_path = default_workspace.join(".openagent").join("mcp.json");
    McpConfigSource {
        label: if default_path.exists() {
            "default"
        } else {
            "none"
        },
        read_source: default_path
            .exists()
            .then(|| default_path.to_string_lossy().to_string()),
        writable_path: Some(default_path),
        readonly_reason: None,
    }
}

pub(super) fn looks_like_inline_json(value: &str) -> bool {
    value.starts_with('{') || value.starts_with('[')
}

pub(super) fn mcp_payload(config: &HttpRuntimeConfig, request_path: &str) -> Value {
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    let refresh = query_flag(request_path, "refresh") || query_flag(request_path, "check");
    let source = mcp_config_source(config, &env);
    let loaded = match source
        .read_source
        .as_deref()
        .map(load_mcp_config)
        .transpose()
    {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "configured": false,
                "enabled": false,
                "server_count": 0,
                "tool_count": 0,
                "refresh_ttl_s": null,
                "source": source.label,
                "writable": source.writable_path.is_some(),
                "config_path": source.writable_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                "readonly_reason": source.readonly_reason,
                "status": "error",
                "error": error,
                "servers": [],
            });
        }
    };
    let Some(mcp_config) = loaded else {
        return json!({
            "configured": false,
            "enabled": false,
            "server_count": 0,
            "tool_count": 0,
            "refresh_ttl_s": null,
            "source": source.label,
            "writable": source.writable_path.is_some(),
            "config_path": source.writable_path.as_ref().map(|path| path.to_string_lossy().to_string()),
            "readonly_reason": source.readonly_reason,
            "status": "unconfigured",
            "error": null,
            "servers": [],
        });
    };
    let workspace_root = workspace(config);
    let mut manager = RemoteMcpManager::new(mcp_config);
    if refresh && manager.enabled() {
        refresh_mcp_manager_tools(&mut manager, &workspace_root);
    }
    apply_mcp_lifecycle_to_manager(&mut manager, &workspace_root);
    mcp_manager_payload(&source, &manager, &workspace_root)
}

pub(super) fn mcp_manager_payload(
    source: &McpConfigSource,
    manager: &RemoteMcpManager,
    workspace: &Path,
) -> Value {
    let mut tool_count = 0usize;
    let servers = manager
        .servers
        .values()
        .map(|state| {
            let lifecycle = mcp_lifecycle_snapshot(&state.config, workspace);
            let tools = state
                .tools_by_dynamic_name
                .values()
                .map(|descriptor| {
                    json!({
                        "name": descriptor.dynamic_name,
                        "original_name": descriptor.original_name,
                        "title": descriptor.title,
                        "description": descriptor.description,
                    })
                })
                .collect::<Vec<_>>();
            tool_count = tool_count.saturating_add(tools.len());
            json!({
                "name": &state.config.name,
                "type": state.config.server_type.as_str(),
                "enabled": state.config.enabled,
                "transport": state.config.transport.as_str(),
                "selected_transport": state.selected_transport.map(|transport| transport.as_str()),
                "status": &state.status,
                "tool_count": tools.len(),
                "tools": tools,
                "remote_url_configured": !state.config.url.is_empty(),
                "command": state.config.command.first().cloned().unwrap_or_default(),
                "args_count": state.config.command.len().saturating_sub(1),
                "cwd_configured": state.config.cwd.is_some(),
                "env_count": state.config.environment.len(),
                "header_count": state.config.headers.len(),
                "timeout_ms": state.config.timeout_ms,
                "lifecycle_status": lifecycle.status,
                "lifecycle_pid": lifecycle.pid,
                "lifecycle_started_at": lifecycle.started_at_ms.map(|value| value as f64 / 1000.0),
                "lifecycle_last_refreshed_at": lifecycle.last_refreshed_at_ms.map(|value| value as f64 / 1000.0),
                "lifecycle_tool_count": lifecycle.tool_count,
                "last_error": &state.last_error,
                "last_refreshed_at": state.last_refreshed_at,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "configured": !manager.servers.is_empty(),
        "enabled": manager.enabled(),
        "server_count": servers.len(),
        "tool_count": tool_count,
        "refresh_ttl_s": manager.config.refresh_ttl_s,
        "source": source.label,
        "writable": source.writable_path.is_some(),
        "config_path": source.writable_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "readonly_reason": source.readonly_reason,
        "status": mcp_manager_status(&manager),
        "error": null,
        "servers": servers,
    })
}

pub(super) fn mcp_add_server_payload(
    config: &HttpRuntimeConfig,
    body: &str,
) -> Result<Value, McpConfigMutationError> {
    mutate_mcp_config(config, |servers| {
        let payload = parse_mcp_server_request_body(body)?;
        let name = mcp_server_name_from_body(&payload)?;
        let server = build_mcp_server_config_value(&payload, None)?;
        servers.insert(name, server);
        Ok(())
    })
}

pub(super) fn mcp_update_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
    body: &str,
) -> Result<Value, McpConfigMutationError> {
    let name = mcp_server_name_from_path(encoded_name)?;
    mutate_mcp_config(config, |servers| {
        let existing = servers.get(&name).cloned().ok_or_else(|| {
            mcp_mutation_error(404, "mcp_server_not_found", "MCP server not found")
        })?;
        let payload = parse_mcp_server_request_body(body)?;
        let server = build_mcp_server_config_value(&payload, Some(&existing))?;
        servers.insert(name, server);
        Ok(())
    })
}

pub(super) fn mcp_delete_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let name = mcp_server_name_from_path(encoded_name)?;
    mutate_mcp_config(config, |servers| {
        if servers.remove(&name).is_none() {
            return Err(mcp_mutation_error(
                404,
                "mcp_server_not_found",
                "MCP server not found",
            ));
        }
        Ok(())
    })
}

pub(super) fn mcp_test_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let (source, mut manager, _server, workspace_root) =
        mcp_manager_for_server(config, encoded_name)?;
    let name = mcp_server_name_from_path(encoded_name)?;
    refresh_mcp_manager_server_tools(&mut manager, &name, &workspace_root);
    apply_mcp_lifecycle_to_manager(&mut manager, &workspace_root);
    Ok(mcp_manager_payload(&source, &manager, &workspace_root))
}

pub(super) fn mcp_lifecycle_start_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let (source, mut manager, server, workspace_root) =
        mcp_manager_for_server(config, encoded_name)?;
    ensure_local_mcp_server(&server)?;
    if let Some(result) = refresh_mcp_lifecycle_server(&server, &workspace_root) {
        match result {
            Ok(descriptors) => {
                let _ = manager.set_server_tools(
                    &server.name,
                    Some(McpTransport::Stdio),
                    "connected",
                    Some(now_ms() as f64 / 1000.0),
                    descriptors,
                );
                return Ok(mcp_manager_payload(&source, &manager, &workspace_root));
            }
            Err(error) => {
                let _ = manager.set_server_error(
                    &server.name,
                    "error",
                    sanitize_mcp_status_error(&error),
                    Some(now_ms() as f64 / 1000.0),
                );
                return Ok(mcp_manager_payload(&source, &manager, &workspace_root));
            }
        }
    }

    let started_at = now_ms();
    match StdioMcpSession::start(&server, &workspace_root) {
        Ok(mut session) => match session.tools_list() {
            Ok(tools) => {
                let descriptors = build_tool_descriptors_from_values(&server, &tools);
                stop_mcp_lifecycle_server(&server, &workspace_root);
                let key = mcp_lifecycle_key(&workspace_root, &server.name);
                if let Ok(mut registry) = mcp_lifecycle_registry().lock() {
                    registry.insert(
                        key,
                        McpLifecycleEntry {
                            config_fingerprint: mcp_server_fingerprint(&server),
                            session,
                            descriptors: descriptors.clone(),
                            started_at_ms: started_at,
                            last_refreshed_at_ms: started_at,
                        },
                    );
                    let _ = manager.set_server_tools(
                        &server.name,
                        Some(McpTransport::Stdio),
                        "connected",
                        Some(started_at as f64 / 1000.0),
                        descriptors,
                    );
                } else {
                    session.close();
                    let _ = manager.set_server_error(
                        &server.name,
                        "error",
                        "MCP lifecycle registry is unavailable",
                        Some(now_ms() as f64 / 1000.0),
                    );
                }
            }
            Err(error) => {
                session.close();
                let _ = manager.set_server_error(
                    &server.name,
                    "error",
                    sanitize_mcp_status_error(&error),
                    Some(now_ms() as f64 / 1000.0),
                );
            }
        },
        Err(error) => {
            let _ = manager.set_server_error(
                &server.name,
                "error",
                sanitize_mcp_status_error(&error),
                Some(now_ms() as f64 / 1000.0),
            );
        }
    }
    Ok(mcp_manager_payload(&source, &manager, &workspace_root))
}

pub(super) fn mcp_lifecycle_stop_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let (source, mut manager, server, workspace_root) =
        mcp_manager_for_server(config, encoded_name)?;
    ensure_local_mcp_server(&server)?;
    stop_mcp_lifecycle_server(&server, &workspace_root);
    let _ = manager.set_server_tools(
        &server.name,
        Some(McpTransport::Stdio),
        "stopped",
        Some(now_ms() as f64 / 1000.0),
        Vec::new(),
    );
    Ok(mcp_manager_payload(&source, &manager, &workspace_root))
}

pub(super) fn mcp_lifecycle_restart_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let (_source, _manager, server, workspace_root) = mcp_manager_for_server(config, encoded_name)?;
    ensure_local_mcp_server(&server)?;
    stop_mcp_lifecycle_server(&server, &workspace_root);
    mcp_lifecycle_start_server_payload(config, encoded_name)
}

pub(super) fn mcp_manager_for_server(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<
    (
        McpConfigSource,
        RemoteMcpManager,
        RemoteMcpServerConfig,
        PathBuf,
    ),
    McpConfigMutationError,
> {
    let name = mcp_server_name_from_path(encoded_name)?;
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    let source = mcp_config_source(config, &env);
    let loaded = source
        .read_source
        .as_deref()
        .map(load_mcp_config)
        .transpose()
        .map_err(|error| mcp_mutation_error(400, "mcp_config_invalid", error))?;
    let mcp_config = loaded.ok_or_else(|| {
        mcp_mutation_error(
            404,
            "mcp_config_unconfigured",
            "MCP config is not configured.",
        )
    })?;
    let manager = RemoteMcpManager::new(mcp_config);
    let server = manager
        .config
        .servers
        .iter()
        .find(|server| server.name == name)
        .cloned()
        .ok_or_else(|| mcp_mutation_error(404, "mcp_server_not_found", "MCP server not found"))?;
    Ok((source, manager, server, workspace(config)))
}

pub(super) fn ensure_local_mcp_server(
    server: &RemoteMcpServerConfig,
) -> Result<(), McpConfigMutationError> {
    if server.server_type == McpServerType::Local {
        Ok(())
    } else {
        Err(mcp_mutation_error(
            400,
            "mcp_lifecycle_remote_unsupported",
            "MCP lifecycle is only supported for local stdio servers.",
        ))
    }
}

pub(super) fn mcp_config_response(
    result: Result<Value, McpConfigMutationError>,
) -> HttpResponseSpec {
    match result {
        Ok(payload) => json_response(200, payload),
        Err(error) => json_response(
            error.status,
            json!({
                "error": error.message,
                "error_code": error.code,
            }),
        ),
    }
}

pub(super) fn mutate_mcp_config<F>(
    config: &HttpRuntimeConfig,
    mutation: F,
) -> Result<Value, McpConfigMutationError>
where
    F: FnOnce(&mut Map<String, Value>) -> Result<(), McpConfigMutationError>,
{
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    let source = mcp_config_source(config, &env);
    let path = source.writable_path.ok_or_else(|| {
        mcp_mutation_error(
            409,
            "mcp_config_readonly",
            source
                .readonly_reason
                .unwrap_or("MCP config source is read-only"),
        )
    })?;
    let mut raw = read_mutable_mcp_config(&path)?;
    let shape = detect_mcp_servers_shape(&raw)?;
    {
        let servers = mcp_servers_object_mut(&mut raw, shape)?;
        mutation(servers)?;
    }
    load_mcp_config_from_value(&raw)
        .map_err(|error| mcp_mutation_error(400, "mcp_config_invalid", error))?;
    write_mcp_config_file(&path, &raw)?;
    Ok(mcp_payload(config, "/api/mcp"))
}

pub(super) fn read_mutable_mcp_config(path: &Path) -> Result<Value, McpConfigMutationError> {
    if !path.exists() {
        return Ok(json!({"mcp": {"servers": {}}}));
    }
    let raw = fs::read_to_string(path).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_config_read_failed",
            format!("failed to read MCP config: {error}"),
        )
    })?;
    let value = serde_json::from_str::<Value>(&raw).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_config_invalid_json",
            format!("MCP config is not valid JSON: {error}"),
        )
    })?;
    if !value.is_object() {
        return Err(mcp_mutation_error(
            400,
            "mcp_config_invalid",
            "MCP config JSON must be an object.",
        ));
    }
    Ok(value)
}

pub(super) fn write_mcp_config_file(
    path: &Path,
    value: &Value,
) -> Result<(), McpConfigMutationError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            mcp_mutation_error(
                400,
                "mcp_config_write_failed",
                format!("failed to create MCP config directory: {error}"),
            )
        })?;
    }
    let rendered = serde_json::to_string_pretty(value).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_config_write_failed",
            format!("failed to serialize MCP config: {error}"),
        )
    })?;
    fs::write(path, format!("{rendered}\n")).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_config_write_failed",
            format!("failed to write MCP config: {error}"),
        )
    })
}

pub(super) fn detect_mcp_servers_shape(
    value: &Value,
) -> Result<McpServersShape, McpConfigMutationError> {
    let object = value.as_object().ok_or_else(|| {
        mcp_mutation_error(
            400,
            "mcp_config_invalid",
            "MCP config JSON must be an object.",
        )
    })?;
    if let Some(mcp_servers) = object.get("mcpServers") {
        if !mcp_servers.is_object() {
            return Err(mcp_mutation_error(
                400,
                "mcp_config_invalid",
                "mcpServers must be an object.",
            ));
        }
        return Ok(McpServersShape::McpServers);
    }
    if let Some(mcp) = object.get("mcp") {
        let Some(mcp_object) = mcp.as_object() else {
            return Err(mcp_mutation_error(
                400,
                "mcp_config_invalid",
                "mcp must be an object.",
            ));
        };
        if let Some(servers) = mcp_object.get("servers") {
            if !servers.is_object() {
                return Err(mcp_mutation_error(
                    400,
                    "mcp_config_invalid",
                    "mcp.servers must be an object.",
                ));
            }
            return Ok(McpServersShape::McpNested);
        }
        return Ok(McpServersShape::McpDirect);
    }
    if object.is_empty() {
        Ok(McpServersShape::Canonical)
    } else {
        Ok(McpServersShape::RootDirect)
    }
}

pub(super) fn mcp_servers_object_mut(
    value: &mut Value,
    shape: McpServersShape,
) -> Result<&mut Map<String, Value>, McpConfigMutationError> {
    match shape {
        McpServersShape::Canonical => {
            let object = value.as_object_mut().ok_or_else(|| {
                mcp_mutation_error(
                    400,
                    "mcp_config_invalid",
                    "MCP config JSON must be an object.",
                )
            })?;
            let mcp = object.entry("mcp").or_insert_with(|| json!({}));
            let mcp_object = mcp.as_object_mut().ok_or_else(|| {
                mcp_mutation_error(400, "mcp_config_invalid", "mcp must be an object.")
            })?;
            let servers = mcp_object.entry("servers").or_insert_with(|| json!({}));
            servers.as_object_mut().ok_or_else(|| {
                mcp_mutation_error(400, "mcp_config_invalid", "mcp.servers must be an object.")
            })
        }
        McpServersShape::McpServers => value
            .as_object_mut()
            .and_then(|object| object.get_mut("mcpServers"))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                mcp_mutation_error(400, "mcp_config_invalid", "mcpServers must be an object.")
            }),
        McpServersShape::McpNested => value
            .as_object_mut()
            .and_then(|object| object.get_mut("mcp"))
            .and_then(Value::as_object_mut)
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                mcp_mutation_error(400, "mcp_config_invalid", "mcp.servers must be an object.")
            }),
        McpServersShape::McpDirect => value
            .as_object_mut()
            .and_then(|object| object.get_mut("mcp"))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| mcp_mutation_error(400, "mcp_config_invalid", "mcp must be an object.")),
        McpServersShape::RootDirect => value.as_object_mut().ok_or_else(|| {
            mcp_mutation_error(
                400,
                "mcp_config_invalid",
                "MCP config JSON must be an object.",
            )
        }),
    }
}

pub(super) fn parse_mcp_server_request_body(
    body: &str,
) -> Result<Map<String, Value>, McpConfigMutationError> {
    let value = serde_json::from_str::<Value>(body).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_request_invalid_json",
            format!("request body is not valid JSON: {error}"),
        )
    })?;
    value.as_object().cloned().ok_or_else(|| {
        mcp_mutation_error(
            400,
            "mcp_request_invalid",
            "request body must be a JSON object.",
        )
    })
}

pub(super) fn mcp_server_name_from_body(
    payload: &Map<String, Value>,
) -> Result<String, McpConfigMutationError> {
    let raw = payload
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| mcp_mutation_error(400, "mcp_server_name_missing", "name is required"))?;
    validate_mcp_server_name(raw)
}

pub(super) fn mcp_server_name_from_path(
    encoded_name: &str,
) -> Result<String, McpConfigMutationError> {
    let decoded = percent_decode(encoded_name);
    validate_mcp_server_name(&decoded)
}

pub(super) fn validate_mcp_server_name(raw: &str) -> Result<String, McpConfigMutationError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(mcp_mutation_error(
            400,
            "mcp_server_name_invalid",
            "MCP server name must be non-empty.",
        ));
    }
    if name.len() > 128 || name.contains('/') || name.contains('\\') || name.contains('?') {
        return Err(mcp_mutation_error(
            400,
            "mcp_server_name_invalid",
            "MCP server name contains unsupported path characters.",
        ));
    }
    Ok(name.to_string())
}

pub(super) fn build_mcp_server_config_value(
    payload: &Map<String, Value>,
    existing: Option<&Value>,
) -> Result<Value, McpConfigMutationError> {
    let mut server = existing
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in [
        "type",
        "url",
        "transport",
        "enabled",
        "disabled",
        "command",
        "cwd",
        "env",
        "environment",
        "headers",
        "timeout_ms",
        "timeout",
        "tools",
    ] {
        if let Some(value) = payload.get(key) {
            server.insert(key.to_string(), value.clone());
        }
    }
    if let Some(command_text) = payload.get("command").and_then(Value::as_str) {
        let mut command = Vec::new();
        let command_text = command_text.trim();
        if !command_text.is_empty() {
            command.push(Value::String(command_text.to_string()));
        }
        if let Some(args) = payload.get("args").and_then(Value::as_array) {
            command.extend(
                args.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(|item| Value::String(item.to_string())),
            );
        }
        server.insert("command".to_string(), Value::Array(command));
    }
    if !server.contains_key("enabled") && !server.contains_key("disabled") {
        server.insert("enabled".to_string(), Value::Bool(true));
    }
    Ok(Value::Object(server))
}

pub(super) fn mcp_mutation_error(
    status: u16,
    code: &'static str,
    message: impl Into<String>,
) -> McpConfigMutationError {
    McpConfigMutationError {
        status,
        code,
        message: message.into(),
    }
}

pub(super) fn refresh_mcp_manager_tools(manager: &mut RemoteMcpManager, workspace: &Path) {
    let server_names = manager
        .config
        .servers
        .iter()
        .filter(|server| server.enabled)
        .map(|server| server.name.clone())
        .collect::<Vec<_>>();
    for server_name in server_names {
        refresh_mcp_manager_server_tools(manager, &server_name, workspace);
    }
}

pub(super) fn refresh_mcp_manager_server_tools(
    manager: &mut RemoteMcpManager,
    server_name: &str,
    workspace: &Path,
) {
    let Some(server) = manager
        .config
        .servers
        .iter()
        .find(|server| server.name == server_name)
        .cloned()
    else {
        return;
    };
    let refreshed_at = Some(now_ms() as f64 / 1000.0);
    if let Some(result) = refresh_mcp_lifecycle_server(&server, workspace) {
        match result {
            Ok(descriptors) => {
                let _ = manager.set_server_tools(
                    &server.name,
                    Some(McpTransport::Stdio),
                    "connected",
                    refreshed_at,
                    descriptors,
                );
            }
            Err(error) => {
                let _ = manager.set_server_error(
                    &server.name,
                    "error",
                    sanitize_mcp_status_error(&error),
                    refreshed_at,
                );
            }
        }
        return;
    }
    match discover_mcp_server_tools(&server, workspace) {
        Ok((transport, tools)) => {
            let descriptors = build_tool_descriptors_from_values(&server, &tools);
            let _ = manager.set_server_tools(
                &server.name,
                Some(transport),
                "connected",
                refreshed_at,
                descriptors,
            );
        }
        Err(error) => {
            let _ = manager.set_server_error(
                &server.name,
                "error",
                sanitize_mcp_status_error(&error),
                refreshed_at,
            );
        }
    }
}

pub(super) fn mcp_manager_status(manager: &RemoteMcpManager) -> &'static str {
    if !manager.enabled() {
        return "disabled";
    }
    if manager
        .servers
        .values()
        .filter(|state| state.config.enabled)
        .any(|state| state.status == "error")
    {
        return "error";
    }
    if manager
        .servers
        .values()
        .filter(|state| state.config.enabled)
        .any(|state| state.status == "connected")
    {
        return "connected";
    }
    "idle"
}

pub(super) fn sanitize_mcp_status_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if [
        "authorization",
        "bearer ",
        "api_key",
        "apikey",
        "password",
        "secret",
        "token=",
        "token:",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "MCP discovery failed with sensitive details redacted".to_string();
    }
    truncate_mcp_status_error(error, 500)
}

pub(super) fn truncate_mcp_status_error(error: &str, max_chars: usize) -> String {
    if error.chars().count() <= max_chars {
        return error.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", error.chars().take(keep).collect::<String>())
}

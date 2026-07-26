use super::*;

const CAPABILITY_STATE_SCHEMA: &str = "openagent.capabilities.v1";
const CAPABILITY_STATE_FILE: &str = ".openagent-runtime/capabilities.json";
const CAPABILITY_IDS: [&str; 3] = ["browser", "computer", "terminal"];

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManagedCapability {
    enabled: bool,
    policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_checked_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManagedCapabilityState {
    #[serde(default = "capability_state_schema")]
    schema_version: String,
    #[serde(default)]
    capabilities: BTreeMap<String, ManagedCapability>,
    updated_at_ms: u64,
}

fn capability_state_schema() -> String {
    CAPABILITY_STATE_SCHEMA.to_string()
}

fn default_capability(id: &str) -> ManagedCapability {
    ManagedCapability {
        enabled: id != "computer",
        policy: if id == "computer" { "deny" } else { "allow" }.to_string(),
        last_checked_at_ms: None,
        last_error: None,
    }
}

fn default_capability_state() -> ManagedCapabilityState {
    ManagedCapabilityState {
        schema_version: capability_state_schema(),
        capabilities: CAPABILITY_IDS
            .into_iter()
            .map(|id| (id.to_string(), default_capability(id)))
            .collect(),
        updated_at_ms: 0,
    }
}

fn capability_state_path_for_root(root: &Path) -> PathBuf {
    root.join(CAPABILITY_STATE_FILE)
}

fn capability_state_path(config: &HttpRuntimeConfig) -> PathBuf {
    capability_state_path_for_root(&session_root(config))
}

fn read_capability_state_from_root(root: &Path) -> ManagedCapabilityState {
    let mut state = fs::read_to_string(capability_state_path_for_root(root))
        .ok()
        .and_then(|raw| serde_json::from_str::<ManagedCapabilityState>(&raw).ok())
        .unwrap_or_else(default_capability_state);
    state.schema_version = capability_state_schema();
    for id in CAPABILITY_IDS {
        state
            .capabilities
            .entry(id.to_string())
            .or_insert_with(|| default_capability(id));
    }
    state
}

fn read_capability_state(config: &HttpRuntimeConfig) -> ManagedCapabilityState {
    read_capability_state_from_root(&session_root(config))
}

fn write_capability_state(
    config: &HttpRuntimeConfig,
    state: &ManagedCapabilityState,
) -> Result<(), String> {
    let path = capability_state_path(config);
    let parent = path
        .parent()
        .ok_or_else(|| "capability state path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temp = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_ms()));
    fs::write(
        &temp,
        serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    let backup = path.with_extension(format!("bak-{}-{}", std::process::id(), now_ms()));
    if path.exists() {
        fs::rename(&path, &backup).map_err(|error| error.to_string())?;
    }
    if let Err(error) = fs::rename(&temp, &path) {
        let _ = fs::rename(&backup, &path);
        return Err(error.to_string());
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

fn capability_id(value: &str) -> Result<&str, String> {
    let value = value.trim().to_ascii_lowercase();
    CAPABILITY_IDS
        .into_iter()
        .find(|id| *id == value)
        .ok_or_else(|| "capability must be browser, computer, or terminal".to_string())
}

fn capability_policy(value: &str) -> Result<&str, String> {
    let value = value.trim().to_ascii_lowercase();
    ["allow", "ask", "deny"]
        .into_iter()
        .find(|policy| *policy == value)
        .ok_or_else(|| "capability policy must be allow, ask, or deny".to_string())
}

fn terminal_probe() -> Result<String, String> {
    #[cfg(windows)]
    let result = Command::new("cmd").args(["/C", "exit", "0"]).status();
    #[cfg(not(windows))]
    let result = Command::new("sh").args(["-c", ":"]).status();
    match result {
        Ok(status) if status.success() => Ok("Workspace shell is available.".to_string()),
        Ok(status) => Err(format!("workspace shell exited with {status}")),
        Err(error) => Err(format!("workspace shell is unavailable: {error}")),
    }
}

fn capability_probe(id: &str) -> Result<String, String> {
    match id {
        "browser" => {
            let toolkit = Toolkit::with_builtins();
            if toolkit.registry.get("web_fetch").is_some() {
                Ok("Rust web_fetch is registered; HTTP(S) reachability is checked for each request."
                    .to_string())
            } else {
                Err("Rust web_fetch is not registered in this Runtime build.".to_string())
            }
        }
        "terminal" => terminal_probe(),
        "computer" => Err(
            "This Runtime build has no computer-control adapter. Browser and terminal remain available independently."
                .to_string(),
        ),
        _ => Err("unknown capability".to_string()),
    }
}

fn capability_metadata(id: &str) -> (&'static str, &'static str, &'static str, Vec<&'static str>) {
    match id {
        "browser" => (
            "Browser",
            "Read external HTTP(S) resources through the Rust web tool.",
            "rust-web-fetch",
            vec!["web_fetch"],
        ),
        "terminal" => (
            "Terminal",
            "Run workspace-scoped shell commands through the Rust tool runtime.",
            "workspace-shell",
            vec!["bash"],
        ),
        _ => (
            "Computer",
            "Control desktop applications through a future native adapter.",
            "not-installed",
            Vec::new(),
        ),
    }
}

fn capability_record(id: &str, capability: &ManagedCapability) -> Value {
    let (label, description, backend, tools) = capability_metadata(id);
    let probe = capability_probe(id);
    let available = probe.is_ok();
    let status = if !available {
        "unavailable"
    } else if !capability.enabled {
        "disabled"
    } else if capability.policy == "deny" {
        "blocked"
    } else {
        "ready"
    };
    json!({
        "id": id,
        "label": label,
        "description": description,
        "backend": backend,
        "tools": tools,
        "enabled": capability.enabled,
        "policy": capability.policy,
        "available": available,
        "status": status,
        "diagnostic": probe.as_ref().ok(),
        "availability_error": probe.as_ref().err(),
        "last_checked_at_ms": capability.last_checked_at_ms,
        "last_error": capability.last_error,
    })
}

pub(super) fn capabilities_payload(config: &HttpRuntimeConfig) -> Value {
    let state = read_capability_state(config);
    let capabilities = CAPABILITY_IDS
        .into_iter()
        .filter_map(|id| {
            state
                .capabilities
                .get(id)
                .map(|capability| capability_record(id, capability))
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": state.schema_version,
        "storage": "bridge_private_state",
        "updated_at_ms": state.updated_at_ms,
        "capabilities": capabilities,
    })
}

pub(super) fn mutate_capability_payload(
    config: &HttpRuntimeConfig,
    id: &str,
    body: &str,
) -> Result<Value, String> {
    let id = capability_id(id)?;
    let payload = serde_json::from_str::<Value>(body).map_err(|error| error.to_string())?;
    let mut state = read_capability_state(config);
    let capability = state
        .capabilities
        .entry(id.to_string())
        .or_insert_with(|| default_capability(id));
    if let Some(enabled) = payload.get("enabled").and_then(Value::as_bool) {
        capability.enabled = enabled;
    }
    if let Some(policy) = payload.get("policy").and_then(Value::as_str) {
        capability.policy = capability_policy(policy)?.to_string();
    }
    capability.last_error = None;
    state.updated_at_ms = now_ms();
    write_capability_state(config, &state)?;
    Ok(capabilities_payload(config))
}

pub(super) fn diagnose_capability_payload(
    config: &HttpRuntimeConfig,
    id: &str,
) -> Result<Value, String> {
    let id = capability_id(id)?;
    let probe = capability_probe(id);
    let mut state = read_capability_state(config);
    let capability = state
        .capabilities
        .entry(id.to_string())
        .or_insert_with(|| default_capability(id));
    capability.last_checked_at_ms = Some(now_ms());
    capability.last_error = probe.as_ref().err().cloned();
    state.updated_at_ms = now_ms();
    write_capability_state(config, &state)?;
    Ok(json!({
        "ok": probe.is_ok(),
        "capability_id": id,
        "message": probe.as_ref().ok(),
        "error": probe.as_ref().err(),
        "state": capabilities_payload(config),
    }))
}

fn capability_for_tool(tool_name: &str) -> Option<&'static str> {
    let normalized = tool_name.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "bash" | "shell" | "terminal")
        || normalized.starts_with("terminal_")
    {
        Some("terminal")
    } else if matches!(normalized.as_str(), "web_fetch" | "web_search" | "browser")
        || normalized.starts_with("browser_")
    {
        Some("browser")
    } else if matches!(normalized.as_str(), "computer" | "computer_use")
        || normalized.starts_with("computer_")
    {
        Some("computer")
    } else {
        None
    }
}

fn capability_allows_tool_from_root(root: &Path, tool_name: &str) -> bool {
    let Some(id) = capability_for_tool(tool_name) else {
        return true;
    };
    let state = read_capability_state_from_root(root);
    let capability = state
        .capabilities
        .get(id)
        .cloned()
        .unwrap_or_else(|| default_capability(id));
    capability.enabled && capability.policy != "deny" && capability_probe(id).is_ok()
}

pub(super) fn filter_runtime_tools_for_capabilities(
    root: &Path,
    tools: Vec<ToolSchema>,
) -> Vec<ToolSchema> {
    tools
        .into_iter()
        .filter(|tool| capability_allows_tool_from_root(root, &tool.name))
        .collect()
}

fn capability_tool_result(
    tool_call: &ToolCall,
    id: &str,
    message: String,
    error_kind: &str,
    requires_approval: bool,
) -> ToolResult {
    ToolResult {
        call_id: tool_call.call_id.clone(),
        output: String::new(),
        error: Some(message),
        metadata: BTreeMap::from([
            ("tool".to_string(), json!(tool_call.name)),
            ("input".to_string(), tool_call.input.clone()),
            ("capability_id".to_string(), json!(id)),
            (
                "permission_action".to_string(),
                json!(if requires_approval { "ask" } else { "deny" }),
            ),
            ("permission_pattern".to_string(), json!(tool_call.name)),
            ("permission_required".to_string(), json!(requires_approval)),
            ("requires_approval".to_string(), json!(requires_approval)),
            ("error_kind".to_string(), json!(error_kind)),
        ]),
    }
}

pub(super) fn capability_gate_for_tool(
    config: &HttpRuntimeConfig,
    tool_call: &ToolCall,
    ctx: &ToolContext,
) -> Option<ToolResult> {
    let id = capability_for_tool(&tool_call.name)?;
    let state = read_capability_state(config);
    let capability = state
        .capabilities
        .get(id)
        .cloned()
        .unwrap_or_else(|| default_capability(id));
    if let Err(error) = capability_probe(id) {
        return Some(capability_tool_result(
            tool_call,
            id,
            error,
            "capability_unavailable",
            false,
        ));
    }
    if !capability.enabled {
        return Some(capability_tool_result(
            tool_call,
            id,
            format!("{id} capability is disabled in Bridge Settings"),
            "capability_disabled",
            false,
        ));
    }
    if capability.policy == "deny" {
        return Some(capability_tool_result(
            tool_call,
            id,
            format!("{id} capability is denied by Bridge policy"),
            "capability_denied",
            false,
        ));
    }
    if capability.policy == "ask" && !ctx.dangerously_skip_permissions {
        return Some(capability_tool_result(
            tool_call,
            id,
            format!("{id} capability requires user confirmation"),
            "capability_permission_required",
            true,
        ));
    }
    None
}

pub(super) fn ensure_direct_capability_allowed(
    config: &HttpRuntimeConfig,
    id: &str,
) -> Result<(), String> {
    let id = capability_id(id)?;
    let state = read_capability_state(config);
    let capability = state
        .capabilities
        .get(id)
        .cloned()
        .unwrap_or_else(|| default_capability(id));
    capability_probe(id)?;
    if !capability.enabled {
        return Err(format!("{id} capability is disabled in Bridge Settings"));
    }
    match capability.policy.as_str() {
        "allow" => Ok(()),
        "ask" => Err(format!(
            "{id} capability is set to Ask; Agent turns can request approval, but direct Settings execution requires Allow"
        )),
        _ => Err(format!("{id} capability is denied by Bridge policy")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(name: &str) -> (PathBuf, HttpRuntimeConfig) {
        let root = std::env::temp_dir().join(format!("openagent-capability-{name}-{}", now_ms()));
        let workspace = root.join("workspace");
        let sessions = root.join("sessions");
        fs::create_dir_all(&workspace).expect("workspace");
        (
            root,
            HttpRuntimeConfig {
                workspace: Some(workspace.to_string_lossy().to_string()),
                session_store_root: Some(sessions.to_string_lossy().to_string()),
                ..HttpRuntimeConfig::default()
            },
        )
    }

    #[test]
    fn capability_state_persists_and_filters_runtime_tools() {
        let (root, config) = test_config("persist");
        let initial = capabilities_payload(&config);
        assert_eq!(initial["capabilities"][0]["id"], "browser");
        assert_eq!(initial["capabilities"][0]["available"], true);
        assert_eq!(initial["capabilities"][1]["id"], "computer");
        assert_eq!(initial["capabilities"][1]["available"], false);

        mutate_capability_payload(&config, "terminal", r#"{"enabled":true,"policy":"deny"}"#)
            .expect("deny terminal");
        let state_path = capability_state_path(&config);
        assert!(state_path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&state_path)
                    .expect("state metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let tools = Toolkit::with_builtins().get_all_tools("local");
        let filtered = filter_runtime_tools_for_capabilities(&session_root(&config), tools);
        assert!(!filtered.iter().any(|tool| tool.name == "bash"));
        assert!(filtered.iter().any(|tool| tool.name == "web_fetch"));

        let reloaded = capabilities_payload(&config);
        let terminal = reloaded["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .find(|item| item["id"] == "terminal")
            .expect("terminal");
        assert_eq!(terminal["policy"], "deny");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capability_ask_requires_approval_and_allowed_resume_skips_the_gate() {
        let (root, config) = test_config("approval");
        mutate_capability_payload(&config, "browser", r#"{"enabled":true,"policy":"ask"}"#)
            .expect("ask browser");
        let call = ToolCall {
            call_id: "call_browser".to_string(),
            name: "web_fetch".to_string(),
            input: json!({"url": "https://example.test"}),
        };
        let ctx = ToolContext::new(config.workspace.clone().expect("workspace"));
        let gated = capability_gate_for_tool(&config, &call, &ctx).expect("approval gate");
        assert_eq!(gated.metadata["requires_approval"], true);
        assert_eq!(gated.metadata["capability_id"], "browser");

        let approved = ctx.with_dangerously_skip_permissions(true);
        assert!(capability_gate_for_tool(&config, &call, &approved).is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn diagnose_reports_missing_computer_adapter_without_exposing_state_secrets() {
        let (root, config) = test_config("diagnose");
        let diagnosed = diagnose_capability_payload(&config, "computer").expect("diagnose");
        assert_eq!(diagnosed["ok"], false);
        assert!(
            diagnosed["error"]
                .as_str()
                .is_some_and(|error| error.contains("no computer-control adapter"))
        );
        let public = capabilities_payload(&config);
        assert_eq!(public["storage"], "bridge_private_state");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_policy_drives_approval_direct_execution_and_restart_recovery() {
        let (root, config) = test_config("terminal-flow");
        let created = create_session_payload(
            &config,
            &stable_json_dumps(&json!({"cwd": config.workspace.clone()})),
        )
        .expect("create session");
        let session_id = created["session_id"].as_str().expect("session id");
        mutate_capability_payload(&config, "terminal", r#"{"enabled":true,"policy":"ask"}"#)
            .expect("ask terminal");

        let started = start_turn_payload(
            &config,
            session_id,
            &stable_json_dumps(&json!({
                "input": "run capability-gated command",
                "permission": "FULL",
                "tool_call": {
                    "call_id": "call_capability_bash",
                    "name": "bash",
                    "input": {"command": "printf capability-approved"}
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
        assert_eq!(approval["metadata"]["capability_id"], "terminal");
        let turn_id = approval["turn_id"].as_str().expect("turn id");
        let request_id = approval["request_id"].as_str().expect("request id");
        let resolved = respond_approval_payload(
            &config,
            &format!("/api/turns/{turn_id}/approvals/{request_id}"),
            r#"{"action":"allow","scope":"once"}"#,
        )
        .expect("allow capability");
        assert!(resolved["events"].as_array().is_some_and(|events| {
            events.iter().any(|event| {
                event["method"] == "item/toolCall/completed"
                    && event["params"]["output"] == "capability-approved"
            })
        }));

        let direct_ask = terminal_run_payload(
            &config,
            &stable_json_dumps(&json!({"session_id": session_id, "command": "printf blocked"})),
        )
        .expect_err("ask blocks direct settings execution");
        assert!(direct_ask.contains("direct Settings execution requires Allow"));

        mutate_capability_payload(&config, "terminal", r#"{"policy":"allow"}"#)
            .expect("allow terminal");
        let direct = terminal_run_payload(
            &config,
            &stable_json_dumps(&json!({"session_id": session_id, "command": "printf direct-ok"})),
        )
        .expect("direct terminal");
        assert_eq!(direct["stdout"], "direct-ok");

        let recovered = read_capability_state(&config);
        assert_eq!(
            recovered.capabilities["terminal"].policy, "allow",
            "Bridge restart must reload the private capability state"
        );
        let _ = fs::remove_dir_all(root);
    }
}

use super::*;

const PROVIDER_STATE_SCHEMA: &str = "openagent.provider.v3";
const PROVIDER_STATE_FILE: &str = ".openagent-runtime/provider.json";
const PROVIDER_VALIDATE_TIMEOUT_SECS: u64 = 25;
const MAX_CONFIGURED_PROVIDER_MODELS: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManagedProviderState {
    #[serde(default = "provider_state_schema")]
    schema_version: String,
    #[serde(default)]
    active_config_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    configs: Vec<ManagedProviderConfig>,
    #[serde(default = "default_provider_id")]
    provider: String,
    #[serde(default = "default_provider_profile")]
    profile: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    models: Vec<String>,
    #[serde(default)]
    wire_api: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    updated_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManagedProviderConfig {
    id: String,
    label: String,
    #[serde(default = "default_provider_id")]
    provider: String,
    #[serde(default = "default_provider_profile")]
    profile: String,
    base_url: String,
    model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    models: Vec<String>,
    wire_api: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    updated_at_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ProviderMutationRequest {
    config_id: Option<String>,
    label: Option<String>,
    profile: Option<String>,
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    models: Option<Vec<String>>,
    wire_api: Option<String>,
    api_key: Option<String>,
    #[serde(default)]
    clear_api_key: bool,
}

fn provider_state_schema() -> String {
    PROVIDER_STATE_SCHEMA.to_string()
}

fn default_provider_id() -> String {
    "openai".to_string()
}

fn default_provider_profile() -> String {
    "gpt".to_string()
}

fn default_provider_config_id() -> String {
    "gpt".to_string()
}

pub(super) fn provider_state_path(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config).join(PROVIDER_STATE_FILE)
}

pub(super) fn provider_state_for_root(root: &Path) -> PathBuf {
    root.join(PROVIDER_STATE_FILE)
}

pub(super) fn managed_provider_record_for_model(
    path: Option<&Path>,
    requested_model: Option<&str>,
) -> Option<Value> {
    path.and_then(read_provider_state_path)
        .and_then(|state| provider_config_for_model(&state, requested_model))
        .and_then(|config| serde_json::to_value(config).ok())
}

fn read_provider_state(config: &HttpRuntimeConfig) -> Option<ManagedProviderState> {
    read_provider_state_path(&provider_state_path(config))
}

fn read_provider_state_path(path: &Path) -> Option<ManagedProviderState> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<ManagedProviderState>(&raw).ok())
        .map(normalize_provider_state)
}

fn normalize_config_id(value: Option<&str>) -> String {
    let normalized = value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || *character == '-' || *character == '_'
        })
        .take(64)
        .collect::<String>();
    if normalized.is_empty() {
        default_provider_config_id()
    } else {
        normalized
    }
}

fn default_config_label(id: &str) -> String {
    match id {
        "maas" => "MAAS / Kimi + GLM".to_string(),
        "gpt" => "GPT / Codex".to_string(),
        _ => "OpenAI compatible".to_string(),
    }
}

fn config_from_state(state: &ManagedProviderState, id: String) -> ManagedProviderConfig {
    ManagedProviderConfig {
        label: default_config_label(&id),
        id,
        provider: state.provider.clone(),
        profile: state.profile.clone(),
        base_url: state.base_url.clone(),
        model: state.model.clone(),
        models: state.models.clone(),
        wire_api: state.wire_api.clone(),
        api_key: state.api_key.clone(),
        updated_at_ms: state.updated_at_ms,
    }
}

fn active_provider_config(state: &ManagedProviderState) -> Option<ManagedProviderConfig> {
    let active_id = normalize_config_id(Some(&state.active_config_id));
    state
        .configs
        .iter()
        .find(|config| config.id == active_id)
        .cloned()
        .or_else(|| state.configs.first().cloned())
}

fn is_maas_model(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "kimi-k3" | "glm5.2" | "glm-5.2"
    )
}

fn config_matches_model(config: &ManagedProviderConfig, model: &str) -> bool {
    let model = model.trim();
    if model.is_empty() {
        return false;
    }
    if config.id == "gpt" && model.to_ascii_lowercase().starts_with("gpt-") {
        return true;
    }
    config.model.eq_ignore_ascii_case(model)
        || normalize_provider_models(&config.models, &config.model)
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(model))
}

fn matching_provider_config(
    state: &ManagedProviderState,
    requested_model: &str,
) -> Option<ManagedProviderConfig> {
    state
        .configs
        .iter()
        .find(|config| config_matches_model(config, requested_model))
        .cloned()
}

fn provider_config_for_model(
    state: &ManagedProviderState,
    requested_model: Option<&str>,
) -> Option<ManagedProviderConfig> {
    requested_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .and_then(|model| matching_provider_config(state, model))
        .or_else(|| active_provider_config(state))
}

pub(super) fn ensure_managed_provider_model_is_routed(
    path: Option<&Path>,
    requested_model: Option<&str>,
) -> Result<(), String> {
    let Some(model) = requested_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return Ok(());
    };
    let Some(state) = path.and_then(read_provider_state_path) else {
        return Ok(());
    };
    if state.configs.len() > 1 && matching_provider_config(&state, model).is_none() {
        let routes = state
            .configs
            .iter()
            .map(|config| {
                let models = normalize_provider_models(&config.models, &config.model);
                format!("{}: {}", config.label, models.join(", "))
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "model {model} is not assigned to a saved provider connection; available routes: {routes}"
        ));
    }
    Ok(())
}

fn normalize_provider_state(mut state: ManagedProviderState) -> ManagedProviderState {
    let fallback_id = if state.active_config_id.trim().is_empty() {
        default_provider_config_id()
    } else {
        normalize_config_id(Some(&state.active_config_id))
    };
    if state.configs.is_empty() && !state.base_url.trim().is_empty() {
        let mut migrated = config_from_state(&state, fallback_id.clone());
        if migrated.id == "gpt" {
            migrated.models.retain(|model| !is_maas_model(model));
        }
        state.configs.push(migrated);
    }
    let mut deduplicated = Vec::new();
    for mut config in state.configs {
        config.id = normalize_config_id(Some(&config.id));
        if config.label.trim().is_empty() {
            config.label = default_config_label(&config.id);
        }
        if config.provider.trim().is_empty() {
            config.provider = default_provider_id();
        }
        if config.profile.trim().is_empty() {
            config.profile = default_provider_profile();
        }
        if deduplicated
            .iter()
            .any(|existing: &ManagedProviderConfig| existing.id == config.id)
        {
            continue;
        }
        deduplicated.push(config);
    }
    state.configs = deduplicated;
    state.active_config_id = if state.configs.iter().any(|config| config.id == fallback_id) {
        fallback_id
    } else {
        state
            .configs
            .first()
            .map(|config| config.id.clone())
            .unwrap_or_else(default_provider_config_id)
    };
    if let Some(active) = active_provider_config(&state) {
        state.provider = active.provider;
        state.profile = active.profile;
        state.base_url = active.base_url;
        state.model = active.model;
        state.models = active.models;
        state.wire_api = active.wire_api;
        state.api_key = active.api_key;
        state.updated_at_ms = active.updated_at_ms;
    }
    state.schema_version = provider_state_schema();
    state
}
fn write_provider_state(
    config: &HttpRuntimeConfig,
    state: &ManagedProviderState,
) -> Result<(), String> {
    let path = provider_state_path(config);
    let parent = path
        .parent()
        .ok_or_else(|| "provider state path has no parent".to_string())?;
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

fn normalize_profile(value: Option<&str>) -> String {
    match value.unwrap_or("gpt").trim().to_ascii_lowercase().as_str() {
        "glm" | "zhipu" | "bigmodel" => "glm".to_string(),
        _ => "gpt".to_string(),
    }
}

fn profile_from_runtime(
    config: &RuntimeProviderConfig,
    state: Option<&ManagedProviderState>,
) -> String {
    state
        .map(|state| normalize_profile(Some(&state.profile)))
        .unwrap_or_else(|| {
            let haystack = format!(
                "{} {}",
                config.base_url.to_ascii_lowercase(),
                config.model.to_ascii_lowercase()
            );
            if haystack.contains("glm")
                || haystack.contains("bigmodel")
                || haystack.contains("zhipu")
            {
                "glm".to_string()
            } else {
                "gpt".to_string()
            }
        })
}

fn profile_label(profile: &str) -> &'static str {
    if profile == "glm" {
        "GLM / OpenAI compatible"
    } else {
        "GPT / OpenAI compatible"
    }
}

fn profile_default_base_url(profile: &str) -> &'static str {
    if profile == "glm" {
        "https://open.bigmodel.cn/api/paas/v4"
    } else {
        "https://api.openai.com/v1"
    }
}

fn profile_default_model(profile: &str) -> &'static str {
    if profile == "glm" {
        "glm-5.2"
    } else {
        "gpt-5.6-sol"
    }
}

fn profile_default_wire_api(profile: &str) -> &'static str {
    if profile == "glm" {
        "chat"
    } else {
        "responses"
    }
}

fn normalize_base_url(value: &str) -> Result<String, String> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        return Err("provider base_url is required".to_string());
    }
    let parsed =
        url::Url::parse(value).map_err(|error| format!("invalid provider base_url: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("provider base_url must be an absolute HTTP(S) URL".to_string());
    }
    Ok(value.to_string())
}

fn normalize_wire_api(value: Option<&str>, profile: &str) -> Result<String, String> {
    let value = value.unwrap_or_else(|| profile_default_wire_api(profile));
    match value.trim().to_ascii_lowercase().as_str() {
        "responses" | "response" => Ok("responses".to_string()),
        "chat" | "chat.completions" | "chat_completions" => Ok("chat".to_string()),
        _ => Err("wire_api must be responses or chat".to_string()),
    }
}

fn provider_builtin_models(profile: &str) -> Vec<&'static str> {
    if profile == "glm" {
        vec![
            "glm-5.2",
            "glm-4.5",
            "glm-4.5-air",
            "glm-4-plus",
            "glm-4-flash",
        ]
    } else {
        vec![
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-image-2",
            "gpt-image-1.5",
        ]
    }
}

fn model_profile(model: &str, fallback: &str) -> String {
    if model.to_ascii_lowercase().starts_with("glm") {
        "glm".to_string()
    } else if model.to_ascii_lowercase().starts_with("gpt-") {
        "gpt".to_string()
    } else {
        fallback.to_string()
    }
}

fn provider_model_capabilities(model: &str, wire_api: &str) -> Value {
    let normalized = model.to_ascii_lowercase();
    let image_output = normalized.starts_with("gpt-image");
    let responses = wire_api == "responses";
    let reasoning = normalized.starts_with("gpt-5")
        || normalized.starts_with("glm5")
        || normalized.starts_with("glm-5")
        || normalized.starts_with("glm-4.5");
    let context_window = if image_output {
        Value::Null
    } else if normalized.starts_with("gpt-5")
        || normalized.starts_with("glm5")
        || normalized.starts_with("glm-")
    {
        json!(128_000)
    } else {
        json!(32_768)
    };
    let dialect = if responses {
        ToolCallDialect::OpenAiResponses
    } else {
        ToolCallDialect::OpenAiChat
    };
    let provider = if responses {
        "openai"
    } else {
        "openai_compatible"
    };
    let tool_calling = provider_capabilities(provider, dialect);
    json!({
        "input_modalities": if image_output { json!(["text", "image"]) } else { json!(["text"]) },
        "output_modalities": if image_output { json!(["image"]) } else { json!(["text"]) },
        "responses": responses && !image_output,
        "chat_completions": !responses && !image_output,
        "streaming": !image_output,
        "reasoning": reasoning && !image_output,
        "context_window": context_window,
        "tools": !image_output,
        "tool_calling": tool_calling,
        "parallel_tool_calls": !image_output && tool_calling.supports(ProviderCapability::ParallelToolCalls),
        "strict_tool_schemas": !image_output && tool_calling.supports(ProviderCapability::StrictToolSchemas),
        "tool_choice": ["auto", "none", "required", "named"],
        "selectable": !image_output,
    })
}

fn model_record(
    model: &str,
    profile: &str,
    wire_api: &str,
    configured_model: &str,
    source: &str,
    config: Option<&ManagedProviderConfig>,
) -> Value {
    let model_profile = model_profile(model, profile);
    json!({
        "id": model,
        "name": model,
        "provider_id": "openai",
        "profile": model_profile,
        "wire_api": wire_api,
        "config_id": config.map(|config| config.id.as_str()),
        "config_label": config.map(|config| config.label.as_str()),
        "source": source,
        "default": model == configured_model,
        "capabilities": provider_model_capabilities(model, wire_api),
    })
}

fn normalize_provider_models<I>(models: I, configured_model: &str) -> Vec<String>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut normalized = Vec::new();
    for model in std::iter::once(configured_model.to_string())
        .chain(models.into_iter().map(|model| model.as_ref().to_string()))
    {
        let model = model.trim();
        if model.is_empty() || normalized.iter().any(|candidate| candidate == model) {
            continue;
        }
        normalized.push(model.to_string());
        if normalized.len() >= MAX_CONFIGURED_PROVIDER_MODELS {
            break;
        }
    }
    normalized
}

fn environment_provider_models(configured_model: &str) -> Vec<String> {
    let configured = std::env::var("OPENAGENT_PROVIDER_MODELS").unwrap_or_default();
    normalize_provider_models(configured.split(','), configured_model)
}

fn configured_provider_models(
    runtime: &RuntimeProviderConfig,
    state: Option<&ManagedProviderState>,
) -> Vec<String> {
    state
        .filter(|state| !state.models.is_empty())
        .map(|state| normalize_provider_models(&state.models, &runtime.model))
        .unwrap_or_else(|| environment_provider_models(&runtime.model))
}

fn provider_profiles_payload() -> Value {
    json!([
        {
            "id": "gpt",
            "provider_id": "openai",
            "label": profile_label("gpt"),
            "default_base_url": profile_default_base_url("gpt"),
            "default_model": profile_default_model("gpt"),
            "wire_apis": ["responses", "chat"],
        },
        {
            "id": "glm",
            "provider_id": "openai",
            "label": profile_label("glm"),
            "default_base_url": profile_default_base_url("glm"),
            "default_model": profile_default_model("glm"),
            "wire_apis": ["chat", "responses"],
        }
    ])
}

fn provider_public_config(
    runtime: &RuntimeProviderConfig,
    state: Option<&ManagedProviderState>,
) -> Value {
    let profile = profile_from_runtime(runtime, state);
    let models = configured_provider_models(runtime, state);
    let active_config = state.and_then(active_provider_config);
    json!({
        "schema_version": PROVIDER_STATE_SCHEMA,
        "config_id": active_config.as_ref().map(|config| config.id.as_str()).unwrap_or("gpt"),
        "label": active_config
            .as_ref()
            .map(|config| config.label.as_str())
            .unwrap_or("GPT / Codex"),
        "provider": runtime.provider,
        "profile": profile,
        "profile_label": profile_label(&profile),
        "base_url": runtime.base_url,
        "base_url_source": runtime.base_url_source,
        "model": runtime.model,
        "models": models,
        "model_source": runtime.model_source,
        "wire_api": runtime.wire_api,
        "wire_api_source": runtime.wire_api_source,
        "api_key_configured": runtime.api_key.is_some(),
        "api_key_source": runtime.api_key_source,
        "storage": if state.is_some() { "bridge_private_state" } else { "runtime_environment" },
        "updated_at_ms": state.map(|state| state.updated_at_ms),
    })
}

fn provider_configs_payload(
    state: Option<&ManagedProviderState>,
    runtime: &RuntimeProviderConfig,
) -> Value {
    let Some(state) = state else {
        return json!([{
            "config_id": "gpt",
            "label": "GPT / Codex",
            "provider": runtime.provider,
            "profile": profile_from_runtime(runtime, None),
            "base_url": runtime.base_url,
            "model": runtime.model,
            "models": environment_provider_models(&runtime.model),
            "wire_api": runtime.wire_api,
            "api_key_configured": runtime.api_key.is_some(),
            "active": true,
            "storage": "runtime_environment",
        }]);
    };
    Value::Array(
        state
            .configs
            .iter()
            .map(|config| {
                let uses_gpt_environment_key = config.id == "gpt"
                    && (runtime.api_key.is_some() && config.id == state.active_config_id
                        || gpt_environment_api_key_configured());
                json!({
                    "config_id": config.id,
                    "label": config.label,
                    "provider": config.provider,
                    "profile": normalize_profile(Some(&config.profile)),
                    "profile_label": profile_label(&normalize_profile(Some(&config.profile))),
                    "base_url": config.base_url,
                    "model": config.model,
                    "models": normalize_provider_models(&config.models, &config.model),
                    "wire_api": config.wire_api,
                    "api_key_configured": config.api_key.is_some() || uses_gpt_environment_key,
                    "active": config.id == state.active_config_id,
                    "storage": "bridge_private_state",
                    "updated_at_ms": config.updated_at_ms,
                })
            })
            .collect(),
    )
}

fn gpt_environment_api_key_configured() -> bool {
    ["OPENAI_API_KEY", "OPENAGENT_API_KEY"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
}

pub(super) fn providers_payload(config: &HttpRuntimeConfig, request_path: &str) -> Value {
    let state_path = provider_state_path(config);
    let provider =
        query_param(request_path, "provider").unwrap_or_else(|| active_provider_id(None));
    let runtime = runtime_provider_config(Some(&state_path), Some(&provider), None, None)
        .unwrap_or_else(|_| RuntimeProviderConfig::fallback(&provider));
    let state = read_provider_state(config);
    let profile = profile_from_runtime(&runtime, state.as_ref());
    let live_check = query_flag(request_path, "check") || query_flag(request_path, "refresh");
    let probe = if live_check {
        probe_runtime_models_endpoint(&runtime)
    } else {
        RuntimeModelProbe::not_checked(&runtime)
    };
    let configured_models = configured_provider_models(&runtime, state.as_ref());
    let routed_models = state
        .as_ref()
        .map(|state| {
            state
                .configs
                .iter()
                .flat_map(|config| normalize_provider_models(&config.models, &config.model))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut ids = provider_builtin_models(&profile)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    for model in &configured_models {
        if !ids.iter().any(|candidate| candidate == model) {
            ids.push(model.clone());
        }
    }
    for model in &routed_models {
        if !ids.iter().any(|candidate| candidate == model) {
            ids.push(model.clone());
        }
    }
    for model in &probe.model_ids {
        if state
            .as_ref()
            .is_some_and(|state| matching_provider_config(state, model).is_none())
        {
            continue;
        }
        if !ids.iter().any(|candidate| candidate == model) {
            ids.push(model.clone());
        }
    }
    if !ids.iter().any(|model| model == &runtime.model) {
        ids.insert(0, runtime.model.clone());
    }
    ids.sort_by(|left, right| right.cmp(left));
    ids.dedup();
    let models = ids
        .iter()
        .map(|model| {
            let routed_config = state
                .as_ref()
                .and_then(|state| matching_provider_config(state, model));
            let source = if probe.model_ids.iter().any(|candidate| candidate == model) {
                "remote"
            } else if routed_models.iter().any(|candidate| candidate == model)
                || configured_models.iter().any(|candidate| candidate == model)
            {
                "configured"
            } else {
                "builtin"
            };
            let route_profile = routed_config
                .as_ref()
                .map(|config| normalize_profile(Some(&config.profile)))
                .unwrap_or_else(|| profile.clone());
            let wire_api = routed_config
                .as_ref()
                .map(|config| config.wire_api.as_str())
                .unwrap_or(runtime.wire_api.as_str());
            model_record(
                model,
                &route_profile,
                wire_api,
                &runtime.model,
                source,
                routed_config.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": "openagent.provider-catalog.v1",
        "config": provider_public_config(&runtime, state.as_ref()),
        "active_config_id": state
            .as_ref()
            .map(|state| state.active_config_id.as_str())
            .unwrap_or("gpt"),
        "configs": provider_configs_payload(state.as_ref(), &runtime),
        "providers": provider_profiles_payload(),
        "provider": runtime.provider,
        "provider_label": runtime.provider_label,
        "base_url": runtime.base_url,
        "base_url_source": runtime.base_url_source,
        "model": runtime.model,
        "model_source": runtime.model_source,
        "wire_api": runtime.wire_api,
        "wire_api_source": runtime.wire_api_source,
        "api_key": if runtime.api_key.is_some() { "set" } else { "missing" },
        "api_key_env": runtime.api_key_env,
        "api_key_source": runtime.api_key_source,
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

fn provider_state_from_request(
    config: &HttpRuntimeConfig,
    request: ProviderMutationRequest,
) -> Result<ManagedProviderState, String> {
    let previous = read_provider_state(config);
    let config_id = normalize_config_id(request.config_id.as_deref().or_else(|| {
        previous
            .as_ref()
            .map(|state| state.active_config_id.as_str())
    }));
    let previous_config = previous.as_ref().and_then(|state| {
        state
            .configs
            .iter()
            .find(|candidate| candidate.id == config_id)
    });
    let profile = normalize_profile(
        request
            .profile
            .as_deref()
            .or_else(|| previous_config.map(|state| state.profile.as_str())),
    );
    let base_url = normalize_base_url(
        request
            .base_url
            .as_deref()
            .or_else(|| previous_config.map(|state| state.base_url.as_str()))
            .unwrap_or_else(|| profile_default_base_url(&profile)),
    )?;
    let model = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| previous_config.map(|state| state.model.clone()))
        .unwrap_or_else(|| {
            if config_id == "maas" {
                "kimi-k3".to_string()
            } else {
                profile_default_model(&profile).to_string()
            }
        });
    let requested_models = request
        .models
        .or_else(|| previous_config.map(|state| state.models.clone()))
        .unwrap_or_else(|| {
            if config_id == "maas" {
                vec!["kimi-k3".to_string(), "glm5.2".to_string()]
            } else {
                Vec::new()
            }
        });
    let models = normalize_provider_models(&requested_models, &model);
    if model.to_ascii_lowercase().starts_with("gpt-image") {
        return Err("image generation models cannot be selected for the Agent Runtime".to_string());
    }
    let wire_api = normalize_wire_api(
        request
            .wire_api
            .as_deref()
            .or_else(|| previous_config.map(|state| state.wire_api.as_str())),
        &profile,
    )?;
    let api_key = if request.clear_api_key {
        None
    } else {
        request
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .or_else(|| previous_config.and_then(|state| state.api_key.clone()))
    };
    let next = ManagedProviderConfig {
        id: config_id.clone(),
        label: request
            .label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_string)
            .or_else(|| previous_config.map(|state| state.label.clone()))
            .unwrap_or_else(|| default_config_label(&config_id)),
        provider: request
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
            .map(str::to_string)
            .or_else(|| previous_config.map(|state| state.provider.clone()))
            .unwrap_or_else(default_provider_id),
        profile,
        base_url,
        model,
        models,
        wire_api,
        api_key,
        updated_at_ms: now_ms(),
    };
    let mut configs = previous
        .as_ref()
        .map(|state| state.configs.clone())
        .unwrap_or_default();
    if let Some(index) = configs.iter().position(|config| config.id == config_id) {
        configs[index] = next.clone();
    } else {
        configs.push(next.clone());
    }
    Ok(normalize_provider_state(ManagedProviderState {
        schema_version: provider_state_schema(),
        active_config_id: config_id,
        configs,
        provider: next.provider.clone(),
        profile: next.profile.clone(),
        base_url: next.base_url.clone(),
        model: next.model.clone(),
        models: next.models.clone(),
        wire_api: next.wire_api.clone(),
        api_key: next.api_key.clone(),
        updated_at_ms: next.updated_at_ms,
    }))
}

pub(super) fn apply_provider_payload(
    config: &HttpRuntimeConfig,
    body: &str,
) -> Result<Value, String> {
    let request = serde_json::from_str::<ProviderMutationRequest>(body)
        .map_err(|error| format!("invalid provider config payload: {error}"))?;
    let state = provider_state_from_request(config, request)?;
    write_provider_state(config, &state)?;
    Ok(providers_payload(config, "/api/providers"))
}

fn validation_request_config(
    config: &HttpRuntimeConfig,
    request: ProviderMutationRequest,
) -> Result<RuntimeProviderConfig, String> {
    let state = provider_state_from_request(config, request)?;
    let current = runtime_provider_config(
        Some(&provider_state_path(config)),
        Some(&state.provider),
        None,
        None,
    )
    .unwrap_or_else(|_| RuntimeProviderConfig::fallback(&state.provider));
    Ok(RuntimeProviderConfig {
        config_id: state.active_config_id.clone(),
        config_label: active_provider_config(&state)
            .map(|config| config.label)
            .unwrap_or_else(|| default_config_label(&state.active_config_id)),
        provider: state.provider,
        provider_label: profile_label(&state.profile).to_string(),
        api_key_env: current.api_key_env,
        api_key: state.api_key.or_else(|| {
            if state.active_config_id == "gpt" {
                std::env::var("OPENAI_API_KEY")
                    .ok()
                    .filter(|key| !key.trim().is_empty())
                    .or_else(|| {
                        std::env::var("OPENAGENT_API_KEY")
                            .ok()
                            .filter(|key| !key.trim().is_empty())
                    })
            } else {
                None
            }
        }),
        api_key_source: Some("validation_request".to_string()),
        base_url: state.base_url,
        base_url_source: "validation_request".to_string(),
        model: state.model,
        model_source: "validation_request".to_string(),
        wire_api: state.wire_api,
        wire_api_source: "validation_request".to_string(),
        requires_api_key: true,
    })
}

fn provider_validation_body(config: &RuntimeProviderConfig) -> Value {
    if config.wire_api == "chat" {
        json!({
            "model": config.model,
            "messages": [{"role": "user", "content": "Reply with exactly: pong"}],
            "max_tokens": 16,
            "stream": false,
        })
    } else {
        json!({
            "model": config.model,
            "input": "Reply with exactly: pong",
            "max_output_tokens": 16,
            "stream": false,
        })
    }
}

fn validation_sample(value: &Value) -> Option<String> {
    value
        .get("output_text")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(160).collect())
}

pub(super) fn validate_provider_payload(
    config: &HttpRuntimeConfig,
    body: &str,
) -> Result<Value, String> {
    let request = serde_json::from_str::<ProviderMutationRequest>(body)
        .map_err(|error| format!("invalid provider validation payload: {error}"))?;
    let runtime = validation_request_config(config, request)?;
    let Some(api_key) = runtime.api_key.as_deref().filter(|key| !key.is_empty()) else {
        return Ok(json!({
            "ok": false,
            "profile": profile_from_runtime(&runtime, None),
            "model": runtime.model,
            "wire_api": runtime.wire_api,
            "models_ok": false,
            "response_ok": false,
            "message": "API Key 未配置，无法验证 Provider。",
        }));
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(PROVIDER_VALIDATE_TIMEOUT_SECS))
        .build()
        .map_err(|error| format!("failed to build provider validation client: {error}"))?;
    let models_endpoint = join_url(&runtime.base_url, "models");
    let models_response = client
        .get(&models_endpoint)
        .bearer_auth(api_key)
        .header("accept", "application/json")
        .send();
    let (models_status, models_ok, model_ids, models_message) = match models_response {
        Ok(response) => {
            let status = response.status().as_u16();
            let raw = response.text().unwrap_or_default();
            let ids = serde_json::from_str::<Value>(&raw)
                .ok()
                .map(|value| extract_openai_model_ids(&value))
                .unwrap_or_default();
            let ok = (200..300).contains(&status);
            let message = if ok {
                format!("/models HTTP {status}")
            } else {
                format!(
                    "/models HTTP {status}: {}",
                    summarize_http_error_body(&raw, "application/json")
                )
            };
            (Some(status), ok, ids, message)
        }
        Err(error) => (None, false, Vec::new(), format!("/models failed: {error}")),
    };
    let response_endpoint = join_url(
        &runtime.base_url,
        if runtime.wire_api == "chat" {
            "chat/completions"
        } else {
            "responses"
        },
    );
    let response = client
        .post(&response_endpoint)
        .bearer_auth(api_key)
        .header("accept", "application/json")
        .json(&provider_validation_body(&runtime))
        .send();
    let (response_status, response_ok, sample, response_message) = match response {
        Ok(response) => {
            let status = response.status().as_u16();
            let raw = response.text().unwrap_or_default();
            let value = serde_json::from_str::<Value>(&raw).unwrap_or_default();
            let ok = (200..300).contains(&status);
            let message = if ok {
                format!("minimal response HTTP {status}")
            } else {
                format!(
                    "minimal response HTTP {status}: {}",
                    summarize_http_error_body(&raw, "application/json")
                )
            };
            (Some(status), ok, validation_sample(&value), message)
        }
        Err(error) => (
            None,
            false,
            None,
            format!("minimal response failed: {error}"),
        ),
    };
    let model_available =
        (!model_ids.is_empty()).then(|| model_ids.iter().any(|model| model == &runtime.model));
    let message = format!("{models_message}。{response_message}。").replace(api_key, "[redacted]");
    Ok(json!({
        "ok": models_ok && response_ok,
        "profile": profile_from_runtime(&runtime, None),
        "model": runtime.model,
        "wire_api": runtime.wire_api,
        "models_ok": models_ok,
        "response_ok": response_ok,
        "model_available": model_available,
        "model_count": model_ids.len(),
        "models_status": models_status,
        "response_status": response_status,
        "message": message,
        "sample": sample,
        "models": model_ids.iter().map(|model| model_record(
            model,
            &model_profile(model, "gpt"),
            &runtime.wire_api,
            &runtime.model,
            "remote",
            None,
        )).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_config(root: &Path) -> HttpRuntimeConfig {
        HttpRuntimeConfig {
            workspace: Some(root.join("workspace").to_string_lossy().to_string()),
            session_store_root: Some(root.join("sessions").to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        }
    }

    #[test]
    fn provider_state_is_private_redacted_and_runtime_effective() {
        let root = std::env::temp_dir().join(format!("openagent-provider-state-{}", now_ms()));
        let config = fixture_config(&root);
        let payload = apply_provider_payload(
            &config,
            &json!({
                "profile": "glm",
                "base_url": "http://127.0.0.1:9800/v1",
                "api_key": "private-test-key",
                "model": "glm-5.2",
                "models": ["kimi-k3", "glm-5.2", "kimi-k3"],
                "wire_api": "chat"
            })
            .to_string(),
        )
        .expect("provider config");
        assert_eq!(payload["config"]["model"], "glm-5.2");
        assert_eq!(payload["config"]["models"], json!(["glm-5.2", "kimi-k3"]));
        assert_eq!(payload["config"]["api_key_configured"], true);
        assert!(!payload.to_string().contains("private-test-key"));
        let path = provider_state_path(&config);
        let runtime =
            runtime_provider_config(Some(&path), None, None, None).expect("runtime config");
        assert_eq!(runtime.model, "glm-5.2");
        assert_eq!(runtime.wire_api, "chat");
        assert_eq!(runtime.api_key.as_deref(), Some("private-test-key"));
        let persisted = read_provider_state(&config).expect("persisted provider state");
        assert_eq!(persisted.models, vec!["glm-5.2", "kimi-k3"]);
        let catalog = providers_payload(&config, "/api/providers");
        let kimi = catalog["models"]
            .as_array()
            .and_then(|models| models.iter().find(|model| model["id"] == "kimi-k3"))
            .expect("configured kimi model");
        assert_eq!(kimi["source"], "configured");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn provider_state_migrates_legacy_gpt_and_keeps_maas_as_a_separate_connection() {
        let root = std::env::temp_dir().join(format!("openagent-provider-registry-{}", now_ms()));
        let config = fixture_config(&root);
        let path = provider_state_path(&config);
        fs::create_dir_all(path.parent().expect("provider state parent"))
            .expect("provider state directory");
        fs::write(
            &path,
            json!({
                "schema_version": "openagent.provider.v2",
                "provider": "openai",
                "profile": "gpt",
                "base_url": "http://127.0.0.1:9900/v1",
                "model": "gpt-5.6-sol",
                "models": ["gpt-5.6-sol"],
                "wire_api": "responses",
                "api_key": "legacy-gpt-key",
                "updated_at_ms": 1,
            })
            .to_string(),
        )
        .expect("legacy provider state");

        let payload = apply_provider_payload(
            &config,
            &json!({
                "config_id": "maas",
                "base_url": "http://127.0.0.1:9900/v1",
                "api_key": "private-maas-key",
                "model": "kimi-k3",
                "models": ["kimi-k3", "glm5.2"],
                "wire_api": "responses",
            })
            .to_string(),
        )
        .expect("maas provider config");
        assert_eq!(payload["config"]["config_id"], "maas");
        assert_eq!(payload["configs"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            payload["configs"]
                .as_array()
                .and_then(|configs| configs.iter().find(|config| config["config_id"] == "gpt"))
                .and_then(|config| config.get("model")),
            Some(&json!("gpt-5.6-sol"))
        );
        assert!(!payload.to_string().contains("legacy-gpt-key"));
        assert!(!payload.to_string().contains("private-maas-key"));

        let runtime =
            runtime_provider_config(Some(&path), None, None, None).expect("maas runtime config");
        assert_eq!(runtime.model, "kimi-k3");
        assert_eq!(runtime.api_key.as_deref(), Some("private-maas-key"));
        assert_eq!(runtime.config_id, "maas");
        assert_eq!(runtime.config_label, "MAAS / Kimi + GLM");
        let state = read_provider_state(&config).expect("migrated provider state");
        assert_eq!(state.schema_version, PROVIDER_STATE_SCHEMA);
        assert_eq!(state.active_config_id, "maas");
        assert_eq!(state.configs.len(), 2);
        let gpt = state
            .configs
            .iter()
            .find(|provider| provider.id == "gpt")
            .expect("migrated gpt provider");
        assert_eq!(gpt.api_key.as_deref(), Some("legacy-gpt-key"));

        let gpt_payload =
            apply_provider_payload(&config, &json!({ "config_id": "gpt" }).to_string())
                .expect("switch back to gpt");
        assert_eq!(gpt_payload["config"]["config_id"], "gpt");
        let gpt_runtime =
            runtime_provider_config(Some(&path), None, None, None).expect("gpt runtime config");
        assert_eq!(gpt_runtime.model, "gpt-5.6-sol");
        assert_eq!(gpt_runtime.api_key.as_deref(), Some("legacy-gpt-key"));
        assert_eq!(gpt_runtime.config_id, "gpt");
        assert_eq!(gpt_runtime.config_label, "GPT / Codex");
        let maas_request = json!({ "model": "glm5.2" });
        let routed_maas = runtime_provider_config(Some(&path), None, Some(&maas_request), None)
            .expect("model-routed maas config");
        assert_eq!(routed_maas.model, "glm5.2");
        assert_eq!(routed_maas.api_key.as_deref(), Some("private-maas-key"));
        assert_eq!(routed_maas.config_id, "maas");
        assert_eq!(routed_maas.config_label, "MAAS / Kimi + GLM");
        let gpt_request = json!({ "model": "gpt-5.6-sol" });
        let routed_gpt = runtime_provider_config(Some(&path), None, Some(&gpt_request), None)
            .expect("model-routed gpt config");
        assert_eq!(routed_gpt.api_key.as_deref(), Some("legacy-gpt-key"));
        assert_eq!(routed_gpt.config_id, "gpt");
        let catalog = providers_payload(&config, "/api/providers");
        let glm = catalog["models"]
            .as_array()
            .and_then(|models| models.iter().find(|model| model["id"] == "glm5.2"))
            .expect("routed GLM model");
        assert_eq!(glm["config_id"], "maas");
        assert_eq!(glm["config_label"], "MAAS / Kimi + GLM");
        assert_eq!(glm["wire_api"], "responses");
        assert_eq!(glm["capabilities"]["responses"], true);
        assert_eq!(glm["capabilities"]["chat_completions"], false);
        let route_error = ensure_managed_provider_model_is_routed(Some(&path), Some("glm-5.2"))
            .expect_err("unassigned spelling must fail closed");
        assert!(route_error.contains("GPT / Codex: gpt-5.6-sol"));
        assert!(route_error.contains("MAAS / Kimi + GLM: kimi-k3, glm5.2"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn provider_catalog_describes_text_image_and_runtime_capabilities() {
        let root = std::env::temp_dir().join(format!("openagent-provider-catalog-{}", now_ms()));
        let config = fixture_config(&root);
        let payload = providers_payload(&config, "/api/providers");
        let models = payload["models"].as_array().expect("models");
        let text = models
            .iter()
            .find(|model| model["id"] == "gpt-5.5")
            .expect("text model");
        let image = models
            .iter()
            .find(|model| model["id"] == "gpt-image-2")
            .expect("image model");
        assert_eq!(text["capabilities"]["tools"], true);
        assert_eq!(text["capabilities"]["context_window"], 128_000);
        assert_eq!(image["capabilities"]["output_modalities"][0], "image");
        assert_eq!(image["capabilities"]["selectable"], false);
        let _ = fs::remove_dir_all(root);
    }
}

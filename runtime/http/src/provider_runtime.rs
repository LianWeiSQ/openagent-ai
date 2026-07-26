use super::*;

const PROVIDER_STATE_SCHEMA: &str = "openagent.provider.v1";
const PROVIDER_STATE_FILE: &str = ".openagent-runtime/provider.json";
const PROVIDER_VALIDATE_TIMEOUT_SECS: u64 = 25;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManagedProviderState {
    #[serde(default = "provider_state_schema")]
    schema_version: String,
    #[serde(default = "default_provider_id")]
    provider: String,
    #[serde(default = "default_provider_profile")]
    profile: String,
    base_url: String,
    model: String,
    wire_api: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    updated_at_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ProviderMutationRequest {
    profile: Option<String>,
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
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

pub(super) fn provider_state_path(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config).join(PROVIDER_STATE_FILE)
}

pub(super) fn provider_state_for_root(root: &Path) -> PathBuf {
    root.join(PROVIDER_STATE_FILE)
}

pub(super) fn managed_provider_record(path: Option<&Path>) -> Option<Value> {
    path.and_then(|path| fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<ManagedProviderState>(&raw).ok())
        .and_then(|state| serde_json::to_value(state).ok())
}

fn read_provider_state(config: &HttpRuntimeConfig) -> Option<ManagedProviderState> {
    managed_provider_record(Some(&provider_state_path(config)))
        .and_then(|value| serde_json::from_value(value).ok())
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
    if model.to_ascii_lowercase().starts_with("glm-") {
        "glm".to_string()
    } else if model.to_ascii_lowercase().starts_with("gpt-") {
        "gpt".to_string()
    } else {
        fallback.to_string()
    }
}

fn provider_model_capabilities(model: &str, profile: &str) -> Value {
    let normalized = model.to_ascii_lowercase();
    let image_output = normalized.starts_with("gpt-image");
    let reasoning = normalized.starts_with("gpt-5")
        || normalized.starts_with("glm-5")
        || normalized.starts_with("glm-4.5");
    let context_window = if image_output {
        Value::Null
    } else if normalized.starts_with("gpt-5") || normalized.starts_with("glm-") {
        json!(128_000)
    } else {
        json!(32_768)
    };
    let dialect = if profile == "gpt" {
        ToolCallDialect::OpenAiResponses
    } else {
        ToolCallDialect::OpenAiChat
    };
    let provider = if profile == "gpt" {
        "openai"
    } else {
        "openai_compatible"
    };
    let tool_calling = provider_capabilities(provider, dialect);
    json!({
        "input_modalities": if image_output { json!(["text", "image"]) } else { json!(["text"]) },
        "output_modalities": if image_output { json!(["image"]) } else { json!(["text"]) },
        "responses": profile == "gpt" && !image_output,
        "chat_completions": !image_output,
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

fn model_record(model: &str, profile: &str, configured_model: &str, source: &str) -> Value {
    let model_profile = model_profile(model, profile);
    json!({
        "id": model,
        "name": model,
        "provider_id": "openai",
        "profile": model_profile,
        "source": source,
        "default": model == configured_model,
        "capabilities": provider_model_capabilities(model, &model_profile),
    })
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
    json!({
        "schema_version": PROVIDER_STATE_SCHEMA,
        "provider": runtime.provider,
        "profile": profile,
        "profile_label": profile_label(&profile),
        "base_url": runtime.base_url,
        "base_url_source": runtime.base_url_source,
        "model": runtime.model,
        "model_source": runtime.model_source,
        "wire_api": runtime.wire_api,
        "wire_api_source": runtime.wire_api_source,
        "api_key_configured": runtime.api_key.is_some(),
        "api_key_source": runtime.api_key_source,
        "storage": if state.is_some() { "bridge_private_state" } else { "runtime_environment" },
        "updated_at_ms": state.map(|state| state.updated_at_ms),
    })
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
    let mut ids = provider_builtin_models(&profile)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    for model in &probe.model_ids {
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
            let source = if probe.model_ids.iter().any(|candidate| candidate == model) {
                "remote"
            } else if model == &runtime.model {
                "configured"
            } else {
                "builtin"
            };
            model_record(model, &profile, &runtime.model, source)
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": "openagent.provider-catalog.v1",
        "config": provider_public_config(&runtime, state.as_ref()),
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
    let profile = normalize_profile(
        request
            .profile
            .as_deref()
            .or_else(|| previous.as_ref().map(|state| state.profile.as_str())),
    );
    let base_url = normalize_base_url(
        request
            .base_url
            .as_deref()
            .or_else(|| previous.as_ref().map(|state| state.base_url.as_str()))
            .unwrap_or_else(|| profile_default_base_url(&profile)),
    )?;
    let model = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| previous.as_ref().map(|state| state.model.clone()))
        .unwrap_or_else(|| profile_default_model(&profile).to_string());
    if model.to_ascii_lowercase().starts_with("gpt-image") {
        return Err("image generation models cannot be selected for the Agent Runtime".to_string());
    }
    let wire_api = normalize_wire_api(
        request
            .wire_api
            .as_deref()
            .or_else(|| previous.as_ref().map(|state| state.wire_api.as_str())),
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
            .or_else(|| previous.as_ref().and_then(|state| state.api_key.clone()))
    };
    Ok(ManagedProviderState {
        schema_version: provider_state_schema(),
        provider: request
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
            .unwrap_or("openai")
            .to_string(),
        profile,
        base_url,
        model,
        wire_api,
        api_key,
        updated_at_ms: now_ms(),
    })
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
        provider: state.provider,
        provider_label: profile_label(&state.profile).to_string(),
        api_key_env: current.api_key_env,
        api_key: state.api_key.or(current.api_key),
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
        "models": model_ids.iter().map(|model| model_record(model, &model_profile(model, "gpt"), &runtime.model, "remote")).collect::<Vec<_>>(),
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
                "wire_api": "chat"
            })
            .to_string(),
        )
        .expect("provider config");
        assert_eq!(payload["config"]["model"], "glm-5.2");
        assert_eq!(payload["config"]["api_key_configured"], true);
        assert!(!payload.to_string().contains("private-test-key"));
        let path = provider_state_path(&config);
        let runtime =
            runtime_provider_config(Some(&path), None, None, None).expect("runtime config");
        assert_eq!(runtime.model, "glm-5.2");
        assert_eq!(runtime.wire_api, "chat");
        assert_eq!(runtime.api_key.as_deref(), Some("private-test-key"));
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

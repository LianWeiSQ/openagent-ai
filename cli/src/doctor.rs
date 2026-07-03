use super::*;

pub(super) fn doctor_payload_from_args(provider: &str, args: &[String]) -> Value {
    let config = match resolve_provider_config(provider, args) {
        Ok(config) => config,
        Err(error) => {
            return json!({
                "provider": provider,
                "provider_label": provider,
                "base_url": DEFAULT_BASE_URL,
                "model": DEFAULT_MODEL,
                "wire_api": DEFAULT_WIRE_API,
                "api_key_env": "OPENAI_API_KEY",
                "api_key_set": false,
                "native": false,
                "healthy": false,
                "dependency_checked": false,
                "dependency_ok": false,
                "dependency_message": error,
                "model_endpoint_checked": false,
                "model_endpoint_ok": false,
                "model_endpoint_message": "provider configuration could not be resolved",
            });
        }
    };
    if config.native {
        let healthy = !config.requires_api_key || config.api_key.is_some();
        return json!({
            "provider": config.provider,
            "provider_label": config.provider_label,
            "base_url": config.base_url,
            "base_url_source": config.base_url_source,
            "model": config.model,
            "model_source": config.model_source,
            "wire_api": config.wire_api,
            "wire_api_source": config.wire_api_source,
            "api_key_env": config.api_key_env,
            "api_key_set": config.api_key.is_some(),
            "api_key_source": config.api_key_source,
            "native": true,
            "healthy": healthy,
            "dependency_checked": true,
            "dependency_ok": true,
            "dependency_message": "native provider route is available in the Rust CLI",
            "model_endpoint_checked": false,
            "model_endpoint_ok": healthy,
            "model_endpoint_message": "skipped OpenAI-compatible /models probe for native provider",
        });
    }
    let probe = probe_openai_compatible_models(&config, args);
    let api_key_ok = !config.requires_api_key || config.api_key.is_some();
    json!({
        "provider": config.provider,
        "provider_label": config.provider_label,
        "base_url": config.base_url,
        "base_url_source": config.base_url_source,
        "model": config.model,
        "model_source": config.model_source,
        "wire_api": config.wire_api,
        "wire_api_source": config.wire_api_source,
        "api_key_env": config.api_key_env,
        "api_key_set": config.api_key.is_some(),
        "api_key_source": config.api_key_source,
        "native": false,
        "healthy": api_key_ok && probe.ok,
        "dependency_checked": false,
        "dependency_ok": true,
        "dependency_message": null,
        "model_endpoint_checked": probe.checked,
        "model_endpoint_ok": probe.ok,
        "model_endpoint_message": probe.message,
        "model_endpoint": probe.endpoint,
        "model_count": probe.model_count,
        "configured_model_available": probe.configured_model_available,
    })
}

#[derive(Clone, Debug)]
struct ModelEndpointProbe {
    checked: bool,
    ok: bool,
    message: String,
    endpoint: Option<String>,
    model_count: Option<usize>,
    configured_model_available: Option<bool>,
}

fn probe_openai_compatible_models(
    config: &ResolvedProviderConfig,
    args: &[String],
) -> ModelEndpointProbe {
    let endpoint = join_url(&config.base_url, "models");
    if config.requires_api_key && config.api_key.is_none() {
        return ModelEndpointProbe {
            checked: false,
            ok: false,
            message: format!(
                "missing API key in {}; /models was not checked",
                config.api_key_env
            ),
            endpoint: Some(endpoint),
            model_count: None,
            configured_model_available: None,
        };
    }
    let timeout = Duration::from_secs(
        value_for(args, &["--timeout-s"])
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(10),
    );
    let client = match reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return ModelEndpointProbe {
                checked: true,
                ok: false,
                message: format!("failed to build HTTP client: {error}"),
                endpoint: Some(endpoint),
                model_count: None,
                configured_model_available: None,
            };
        }
    };
    let mut request = client.get(&endpoint).header("accept", "application/json");
    if let Some(api_key) = config.api_key.as_deref()
        && !api_key.is_empty()
    {
        request = request.bearer_auth(api_key);
    }
    let response = match request.send() {
        Ok(response) => response,
        Err(error) => {
            return ModelEndpointProbe {
                checked: true,
                ok: false,
                message: format!("failed to GET {endpoint}: {error}"),
                endpoint: Some(endpoint),
                model_count: None,
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
            return ModelEndpointProbe {
                checked: true,
                ok: false,
                message: format!("failed to read {endpoint}: {error}"),
                endpoint: Some(endpoint),
                model_count: None,
                configured_model_available: None,
            };
        }
    };
    if !status.is_success() {
        return ModelEndpointProbe {
            checked: true,
            ok: false,
            message: format!(
                "HTTP {} from {endpoint}: {}",
                status.as_u16(),
                summarize_http_error_body(&raw, &content_type)
            ),
            endpoint: Some(endpoint),
            model_count: None,
            configured_model_available: None,
        };
    }
    let parsed = serde_json::from_str::<Value>(&raw).ok();
    let model_ids = parsed.as_ref().map(extract_model_ids).unwrap_or_default();
    let model_count = (!model_ids.is_empty()).then_some(model_ids.len());
    let configured_model_available =
        (!model_ids.is_empty()).then(|| model_ids.iter().any(|model| model == &config.model));
    let message = match (model_count, configured_model_available) {
        (Some(count), Some(true)) => {
            format!(
                "HTTP {} from {endpoint}; configured model is listed among {count} model(s)",
                status.as_u16()
            )
        }
        (Some(count), Some(false)) => {
            format!(
                "HTTP {} from {endpoint}; {count} model(s) listed, configured model '{}' was not listed",
                status.as_u16(),
                config.model
            )
        }
        _ => format!("HTTP {} from {endpoint}", status.as_u16()),
    };
    ModelEndpointProbe {
        checked: true,
        ok: true,
        message,
        endpoint: Some(endpoint),
        model_count,
        configured_model_available,
    }
}

fn extract_model_ids(value: &Value) -> Vec<String> {
    let Some(data) = value.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

pub(super) fn doctor_text_from_payload(payload: &Value) -> String {
    let object = payload.as_object().expect("doctor payload object");
    let healthy = bool_field(object, "healthy");
    let api_key = if bool_field(object, "api_key_set") {
        "set"
    } else {
        "missing"
    };
    if object
        .get("native")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let mut text = render_key_values(
            "OpenAgent Doctor",
            &[
                (
                    "Status",
                    if healthy { "ok" } else { "needs attention" }.to_string(),
                ),
                (
                    "Provider",
                    format!(
                        "{} ({})",
                        string_field(object, "provider_label"),
                        string_field(object, "provider")
                    ),
                ),
                ("Model", string_field(object, "model")),
                (
                    "API Key",
                    format!("{api_key} ({})", string_field(object, "api_key_env")),
                ),
                ("Base URL", string_field(object, "base_url")),
            ],
        );
        text.push_str("\n\n");
        text.push_str(&render_table(
            &["Check", "Status", "Detail"],
            &[
                vec![
                    "Dependency".to_string(),
                    if bool_field(object, "dependency_ok") {
                        "ok".to_string()
                    } else {
                        "missing".to_string()
                    },
                    string_field(object, "dependency_message"),
                ],
                vec![
                    "Model Endpoint".to_string(),
                    "skipped".to_string(),
                    string_field(object, "model_endpoint_message"),
                ],
            ],
        ));
        text.push('\n');
        return text;
    }
    let mut text = render_key_values(
        "OpenAgent Doctor",
        &[
            (
                "Status",
                if healthy { "ok" } else { "needs attention" }.to_string(),
            ),
            (
                "Provider",
                format!(
                    "{} ({})",
                    string_field(object, "provider_label"),
                    string_field(object, "provider")
                ),
            ),
            ("Model", string_field(object, "model")),
            ("Wire API", string_field(object, "wire_api")),
            (
                "API Key",
                format!("{api_key} ({})", string_field(object, "api_key_env")),
            ),
            ("Base URL", string_field(object, "base_url")),
        ],
    );
    text.push_str("\n\n");
    text.push_str(&render_table(
        &["Check", "Status", "Detail"],
        &[vec![
            "Model Endpoint".to_string(),
            if bool_field(object, "model_endpoint_ok") {
                "ok".to_string()
            } else {
                "failed".to_string()
            },
            string_field(object, "model_endpoint_message"),
        ]],
    ));
    text.push('\n');
    text
}

fn string_field(object: &Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn bool_field(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(false)
}

use super::*;

pub(super) fn models_command(args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(models_help());
    }
    let format = value_for(args, &["--format"]).unwrap_or_else(|| "table".to_string());
    let verbose = has_flag(args, &["--verbose"]);
    let catalog_requested = has_flag(args, &["--catalog", "--providers"]);
    let cache = ensure_models_cache(args);
    let provider = value_for(args, &["--provider"]).or_else(|| {
        positional_args(
            args,
            &[
                "--format",
                "--ttl-seconds",
                "--models-url",
                "--model",
                "-m",
                "--provider",
            ],
        )
        .first()
        .cloned()
    });
    if catalog_requested {
        let payload = models_catalog_payload(&cache);
        if format == "json" {
            return CliRunResult::ok_json(&payload);
        }
        return ok_text(models_catalog_text(&payload, verbose));
    }
    let provider = provider.unwrap_or_else(active_provider);
    let normalized = match normalize_provider(Some(&provider)) {
        Ok(provider) => provider,
        Err(error) => return err_text(2, error),
    };
    let cached = load_cached_provider_models_from_cache(&cache.value, &normalized);
    let models = if let Some(models) = cached.filter(|items| !items.is_empty()) {
        models
    } else {
        let model_id = value_for(args, &["--model", "-m"])
            .or_else(|| provider_env_value(&normalized, "model"))
            .unwrap_or_else(|| default_model_for_provider(&normalized));
        vec![if normalized == "anthropic" {
            serde_json::to_value(anthropic_model(&model_id, 200_000, 8192))
                .unwrap_or_else(|_| json!({}))
        } else {
            serde_json::to_value(openai_compatible_model(&normalized, &model_id))
                .unwrap_or_else(|_| json!({}))
        }]
    };
    let provider_info = provider_catalog_record(&cache.value, &normalized)
        .unwrap_or_else(|| fallback_provider_catalog_record(&normalized, models.len()));
    let payload = json!({
        "provider": normalized,
        "provider_label": provider_label(&provider).unwrap_or(provider),
        "provider_info": provider_info,
        "models": models,
        "cache": cache.to_value(),
        "cache_path": models_cache_path().to_string_lossy(),
        "refreshed": cache.refreshed,
        "stale": cache.stale,
        "fallback": cache.fallback,
    });
    if format == "json" {
        CliRunResult::ok_json(&payload)
    } else {
        let rows = payload["models"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|model| {
                let id = model.get("id").and_then(Value::as_str).unwrap_or("-");
                let context = model
                    .get("context_window")
                    .or_else(|| model.get("limit").and_then(|limit| limit.get("context")))
                    .and_then(Value::as_u64)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let output = model
                    .get("max_output")
                    .or_else(|| model.get("limit").and_then(|limit| limit.get("output")))
                    .and_then(Value::as_u64)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let capabilities = model
                    .get("capabilities")
                    .map(compact_capabilities)
                    .unwrap_or_else(|| "-".to_string());
                if verbose {
                    vec![
                        id.to_string(),
                        context,
                        output,
                        capabilities,
                        stable_json_dumps(model),
                    ]
                } else {
                    vec![id.to_string(), context, output, capabilities]
                }
            })
            .collect::<Vec<_>>();
        let provider_id = payload["provider"].as_str().unwrap_or("openai");
        let provider_label = payload["provider_label"].as_str().unwrap_or(provider_id);
        let mut text = render_key_values(
            "Models",
            &[
                ("Provider", format!("{provider_label} ({provider_id})")),
                ("Models", rows.len().to_string()),
                (
                    "Cache",
                    format!("{} ({})", cache.status, cache.path.to_string_lossy()),
                ),
            ],
        );
        if !rows.is_empty() {
            text.push_str("\n\n");
            text.push_str(&render_table(
                if verbose {
                    &["Model", "Context", "Output", "Capabilities", "Raw"]
                } else {
                    &["Model", "Context", "Output", "Capabilities"]
                },
                &rows,
            ));
        }
        ok_text(text)
    }
}

pub(super) fn models_cache_path() -> PathBuf {
    env::var("OPENAGENT_MODELS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".cache/openagent/models.json"))
}

#[derive(Clone, Debug)]
struct ModelsCacheLoad {
    value: Value,
    path: PathBuf,
    snapshot_path: PathBuf,
    status: String,
    refreshed: bool,
    stale: bool,
    fallback: bool,
    error: Option<String>,
}

impl ModelsCacheLoad {
    fn to_value(&self) -> Value {
        json!({
            "path": self.path.to_string_lossy(),
            "snapshot_path": self.snapshot_path.to_string_lossy(),
            "status": self.status,
            "refreshed": self.refreshed,
            "stale": self.stale,
            "fallback": self.fallback,
            "error": self.error,
            "source": self.value.get("source").cloned().unwrap_or(Value::Null),
        })
    }
}

struct ModelsCacheLock {
    path: PathBuf,
}

impl ModelsCacheLock {
    fn acquire(path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        if model_cache_lock_is_stale(&path) {
            let _ = fs::remove_file(&path);
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let _ = writeln!(
                    file,
                    "pid={} created_at_ms={}",
                    std::process::id(),
                    now_ms_cli()
                );
                Ok(Self { path })
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(format!(
                "models cache refresh locked: {}",
                path.to_string_lossy()
            )),
            Err(error) => Err(format!("failed to lock models cache: {error}")),
        }
    }
}

impl Drop for ModelsCacheLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn ensure_models_cache(args: &[String]) -> ModelsCacheLoad {
    let path = models_cache_path();
    let snapshot_path = models_cache_snapshot_path();
    let force_refresh = has_flag(args, &["--refresh"]);
    let offline = has_flag(args, &["--offline"]);
    let ttl_seconds = models_cache_ttl_seconds(args);
    let current = load_models_cache_file(&path, ttl_seconds);
    let current_stale = current
        .as_ref()
        .is_none_or(|value| models_cache_is_stale(value, ttl_seconds));
    let should_refresh = force_refresh || (current.is_some() && current_stale);
    if !offline && should_refresh {
        match refresh_models_cache(args) {
            Ok(value) => {
                let stale = models_cache_is_stale(&value, ttl_seconds);
                return ModelsCacheLoad {
                    value,
                    path,
                    snapshot_path,
                    status: "refreshed".to_string(),
                    refreshed: true,
                    stale,
                    fallback: false,
                    error: None,
                };
            }
            Err(error) => {
                if let Some(value) = current {
                    return ModelsCacheLoad {
                        value,
                        path,
                        snapshot_path,
                        status: "stale_refresh_failed".to_string(),
                        refreshed: false,
                        stale: true,
                        fallback: true,
                        error: Some(error),
                    };
                }
                if let Some(value) = load_models_cache_file(&snapshot_path, ttl_seconds) {
                    return ModelsCacheLoad {
                        value,
                        path,
                        snapshot_path,
                        status: "snapshot_fallback".to_string(),
                        refreshed: false,
                        stale: true,
                        fallback: true,
                        error: Some(error),
                    };
                }
                return ModelsCacheLoad {
                    value: empty_models_cache(ttl_seconds),
                    path,
                    snapshot_path,
                    status: "empty_refresh_failed".to_string(),
                    refreshed: false,
                    stale: true,
                    fallback: true,
                    error: Some(error),
                };
            }
        }
    }
    if let Some(value) = current {
        return ModelsCacheLoad {
            value,
            path,
            snapshot_path,
            status: if current_stale { "stale" } else { "hit" }.to_string(),
            refreshed: false,
            stale: current_stale,
            fallback: false,
            error: None,
        };
    }
    if let Some(value) = load_models_cache_file(&snapshot_path, ttl_seconds) {
        return ModelsCacheLoad {
            value,
            path,
            snapshot_path,
            status: "snapshot_fallback".to_string(),
            refreshed: false,
            stale: true,
            fallback: true,
            error: None,
        };
    }
    ModelsCacheLoad {
        value: empty_models_cache(ttl_seconds),
        path,
        snapshot_path,
        status: "empty".to_string(),
        refreshed: false,
        stale: true,
        fallback: true,
        error: None,
    }
}

fn refresh_models_cache(args: &[String]) -> Result<Value, String> {
    let path = models_cache_path();
    let _lock = ModelsCacheLock::acquire(models_cache_lock_path())?;
    let url = models_source_url(args);
    let endpoint = join_url(&url, "api.json");
    let raw = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(
            value_for(args, &["--timeout-s"])
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(20),
        ))
        .build()
        .map_err(|error| error.to_string())?
        .get(&endpoint)
        .send()
        .map_err(|error| format!("failed to fetch models cache: {error}"))?
        .text()
        .map_err(|error| format!("failed to read models cache: {error}"))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("models cache response was not JSON: {error}"))?;
    let normalized = normalize_models_catalog(&value, &endpoint, models_cache_ttl_seconds(args));
    write_json_file(&path, &normalized)?;
    write_json_file(&models_cache_snapshot_path(), &normalized)?;
    Ok(normalized)
}

fn load_models_cache_file(path: &Path, ttl_seconds: u64) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    if value
        .get("schema_version")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema == "openagent.models_cache.v1")
    {
        return Some(value);
    }
    Some(normalize_models_catalog(&value, "local-cache", ttl_seconds))
}

fn empty_models_cache(ttl_seconds: u64) -> Value {
    json!({
        "schema_version": "openagent.models_cache.v1",
        "source": {
            "url": null,
            "fetched_at_ms": 0,
            "ttl_seconds": ttl_seconds,
            "provider_count": 0,
            "model_count": 0,
            "raw_schema": "empty",
        },
        "providers": {},
        "catalog": [],
    })
}

fn normalize_models_catalog(value: &Value, source_url: &str, ttl_seconds: u64) -> Value {
    if value
        .get("schema_version")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema == "openagent.models_cache.v1")
    {
        return value.clone();
    }
    let mut providers = Map::new();
    let mut catalog = Vec::new();
    let mut model_count = 0_usize;
    let Some(source_providers) = value.as_object() else {
        return empty_models_cache(ttl_seconds);
    };
    for (raw_provider_id, provider_value) in source_providers {
        let Some(provider_object) = provider_value.as_object() else {
            continue;
        };
        let provider_id = normalize_models_provider_id(raw_provider_id);
        let upstream_id = provider_object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(raw_provider_id);
        let models = provider_object
            .get("models")
            .and_then(Value::as_object)
            .map(|items| {
                items
                    .iter()
                    .map(|(model_id, model)| {
                        normalize_models_dev_model(&provider_id, model_id, model)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let provider_model_count = models.len();
        model_count += provider_model_count;
        let label = provider_label(&provider_id).unwrap_or_else(|_| {
            provider_object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&provider_id)
                .to_string()
        });
        let record = json!({
            "id": provider_id,
            "source_id": upstream_id,
            "name": provider_object.get("name").and_then(Value::as_str).unwrap_or(label.as_str()),
            "label": label,
            "api": provider_object.get("api").cloned().unwrap_or(Value::Null),
            "doc": provider_object.get("doc").cloned().unwrap_or(Value::Null),
            "npm": provider_object.get("npm").cloned().unwrap_or(Value::Null),
            "env": provider_object.get("env").cloned().unwrap_or_else(|| json!([])),
            "default_model": provider_default_model(&provider_id).ok().flatten(),
            "default_base_url": provider_default_base_url(&provider_id).ok().flatten(),
            "requires_api_key": provider_requires_api_key(&provider_id).unwrap_or(true),
            "native_streaming": provider_native_streaming(&provider_id),
            "model_count": provider_model_count,
            "models": models,
        });
        catalog.push(provider_catalog_summary(&record));
        providers.insert(provider_id, record);
    }
    catalog.sort_by(|left, right| {
        left.get("id")
            .and_then(Value::as_str)
            .cmp(&right.get("id").and_then(Value::as_str))
    });
    json!({
        "schema_version": "openagent.models_cache.v1",
        "source": {
            "url": source_url,
            "fetched_at_ms": now_ms_cli(),
            "ttl_seconds": ttl_seconds,
            "provider_count": providers.len(),
            "model_count": model_count,
            "raw_schema": "models.dev/api.json",
        },
        "providers": providers,
        "catalog": catalog,
    })
}

fn normalize_models_dev_model(provider_id: &str, model_id: &str, model: &Value) -> Value {
    let id = model
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(model_id)
        .to_string();
    let provider_model_id = strip_provider_prefix(provider_id, &id);
    let context_window = model
        .get("limit")
        .and_then(|limit| limit.get("context").or_else(|| limit.get("input")))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| openai_compatible_model(provider_id, &provider_model_id).context_window);
    let max_output = model
        .get("limit")
        .and_then(|limit| limit.get("output"))
        .and_then(Value::as_u64)
        .unwrap_or(4096);
    let input_modalities = model
        .get("modalities")
        .and_then(|modalities| modalities.get("input"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let vision = input_modalities.iter().any(|item| {
        item.as_str()
            .is_some_and(|value| matches!(value, "image" | "video" | "pdf"))
    }) || model
        .get("attachment")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    json!({
        "id": id,
        "provider_model_id": provider_model_id,
        "provider_id": provider_id,
        "name": model.get("name").and_then(Value::as_str).unwrap_or(model_id),
        "family": model.get("family").cloned().unwrap_or(Value::Null),
        "context_window": context_window,
        "max_output": max_output,
        "capabilities": {
            "vision": vision,
            "tools": model.get("tool_call").and_then(Value::as_bool).unwrap_or(true),
            "streaming": true,
            "reasoning": model.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
            "structured_output": model.get("structured_output").and_then(Value::as_bool).unwrap_or(false),
            "temperature": model.get("temperature").and_then(Value::as_bool).unwrap_or(true),
        },
        "pricing": {
            "input_per_1m": model.get("cost").and_then(|cost| cost.get("input")).and_then(Value::as_f64).unwrap_or(0.0),
            "output_per_1m": model.get("cost").and_then(|cost| cost.get("output")).and_then(Value::as_f64).unwrap_or(0.0),
            "cache_read_per_1m": model.get("cost").and_then(|cost| cost.get("cache_read")).and_then(Value::as_f64).unwrap_or(0.0),
            "cache_write_per_1m": model.get("cost").and_then(|cost| cost.get("cache_write")).and_then(Value::as_f64).unwrap_or(0.0),
        },
        "modalities": model.get("modalities").cloned().unwrap_or(Value::Null),
        "knowledge": model.get("knowledge").cloned().unwrap_or(Value::Null),
        "release_date": model.get("release_date").cloned().unwrap_or(Value::Null),
        "last_updated": model.get("last_updated").cloned().unwrap_or(Value::Null),
        "open_weights": model.get("open_weights").cloned().unwrap_or(Value::Null),
        "raw": model,
    })
}

fn load_cached_provider_models_from_cache(cache: &Value, provider: &str) -> Option<Vec<Value>> {
    for key in provider_lookup_keys(provider) {
        if let Some(models) = cache
            .get("providers")
            .and_then(|providers| providers.get(&key))
            .and_then(|provider| provider.get("models"))
            .and_then(Value::as_array)
        {
            return Some(models.clone());
        }
        if let Some(models) = cache
            .get(&key)
            .and_then(|provider| provider.get("models"))
            .and_then(Value::as_object)
        {
            return Some(
                models
                    .iter()
                    .map(|(id, model)| {
                        let mut value = model.clone();
                        if let Some(object) = value.as_object_mut() {
                            object.entry("id".to_string()).or_insert_with(|| json!(id));
                        }
                        value
                    })
                    .collect(),
            );
        }
    }
    None
}

fn provider_catalog_record(cache: &Value, provider: &str) -> Option<Value> {
    provider_lookup_keys(provider).into_iter().find_map(|key| {
        cache
            .get("providers")
            .and_then(|providers| providers.get(&key))
            .cloned()
            .map(|record| {
                let mut summary = provider_catalog_summary(&record);
                if let Some(object) = summary.as_object_mut() {
                    object.insert("selected".to_string(), json!(key == provider));
                }
                summary
            })
    })
}

fn fallback_provider_catalog_record(provider: &str, model_count: usize) -> Value {
    json!({
        "id": provider,
        "label": provider_label(provider).unwrap_or_else(|_| provider.to_string()),
        "name": provider_label(provider).unwrap_or_else(|_| provider.to_string()),
        "default_model": provider_default_model(provider).ok().flatten(),
        "default_base_url": provider_default_base_url(provider).ok().flatten(),
        "requires_api_key": provider_requires_api_key(provider).unwrap_or(true),
        "native_streaming": provider_native_streaming(provider),
        "model_count": model_count,
        "source": "fallback",
    })
}

fn models_catalog_payload(cache: &ModelsCacheLoad) -> Value {
    json!({
        "schema_version": "openagent.models_catalog.v1",
        "cache": cache.to_value(),
        "providers": cache.value.get("catalog").cloned().unwrap_or_else(|| json!([])),
    })
}

fn models_catalog_text(payload: &Value, verbose: bool) -> String {
    let mut lines = vec![format!(
        "providers: {}",
        payload
            .get("providers")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    )];
    for provider in payload
        .get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = provider.get("id").and_then(Value::as_str).unwrap_or("-");
        let label = provider.get("label").and_then(Value::as_str).unwrap_or(id);
        let count = provider
            .get("model_count")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if verbose {
            lines.push(format!(
                "{id} ({label}) models={count} {}",
                stable_json_dumps(provider)
            ));
        } else {
            lines.push(format!("{id} ({label}) models={count}"));
        }
    }
    lines.join("\n")
}

fn provider_catalog_summary(provider: &Value) -> Value {
    json!({
        "id": provider.get("id").cloned().unwrap_or(Value::Null),
        "source_id": provider.get("source_id").cloned().unwrap_or(Value::Null),
        "name": provider.get("name").cloned().unwrap_or(Value::Null),
        "label": provider.get("label").cloned().unwrap_or(Value::Null),
        "api": provider.get("api").cloned().unwrap_or(Value::Null),
        "doc": provider.get("doc").cloned().unwrap_or(Value::Null),
        "npm": provider.get("npm").cloned().unwrap_or(Value::Null),
        "env": provider.get("env").cloned().unwrap_or_else(|| json!([])),
        "default_model": provider.get("default_model").cloned().unwrap_or(Value::Null),
        "default_base_url": provider.get("default_base_url").cloned().unwrap_or(Value::Null),
        "requires_api_key": provider.get("requires_api_key").cloned().unwrap_or(Value::Bool(true)),
        "native_streaming": provider.get("native_streaming").cloned().unwrap_or_else(|| json!({})),
        "model_count": provider.get("model_count").cloned().unwrap_or_else(|| json!(0)),
    })
}

fn provider_lookup_keys(provider: &str) -> Vec<String> {
    let normalized = normalize_models_provider_id(provider);
    let mut keys = vec![normalized.clone()];
    if normalized == "gemini" {
        keys.push("google".to_string());
    }
    keys.dedup();
    keys
}

fn normalize_models_provider_id(provider: &str) -> String {
    match provider {
        "google" => "gemini".to_string(),
        other => normalize_provider(Some(other)).unwrap_or_else(|_| other.to_string()),
    }
}

fn strip_provider_prefix(provider: &str, model_id: &str) -> String {
    for prefix in provider_lookup_keys(provider) {
        if let Some(value) = model_id.strip_prefix(&format!("{prefix}/")) {
            return value.to_string();
        }
    }
    if provider == "gemini"
        && let Some(value) = model_id.strip_prefix("google/")
    {
        return value.to_string();
    }
    model_id.to_string()
}

fn provider_native_streaming(provider: &str) -> Value {
    let dialect = match provider {
        "anthropic" => ToolCallDialect::Anthropic,
        "gemini" | "google" => ToolCallDialect::Gemini,
        _ => ToolCallDialect::OpenAiChat,
    };
    let tool_calling = provider_capabilities(provider, dialect);
    json!({
        "chat_completions_sse": !matches!(provider, "anthropic" | "gemini" | "google"),
        "responses_sse": !matches!(provider, "anthropic" | "gemini" | "google"),
        "anthropic_messages_sse": provider == "anthropic",
        "gemini_generate_content_sse": matches!(provider, "gemini" | "google"),
        "tool_calling": tool_calling,
        "parallel_tool_calls": tool_calling.supports(ProviderCapability::ParallelToolCalls),
        "strict_tool_schemas": tool_calling.supports(ProviderCapability::StrictToolSchemas),
        "implemented": matches!(provider, "anthropic" | "openai" | "openrouter" | "groq" | "mistral" | "deepseek" | "xai" | "ollama" | "gemini" | "azure-openai"),
    })
}

fn compact_capabilities(capabilities: &Value) -> String {
    let mut parts = Vec::new();
    for key in [
        "vision",
        "tools",
        "streaming",
        "reasoning",
        "structured_output",
    ] {
        if capabilities
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            parts.push(key);
        }
    }
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(",")
    }
}

fn models_cache_snapshot_path() -> PathBuf {
    models_cache_path().with_extension("snapshot.json")
}

fn models_cache_lock_path() -> PathBuf {
    models_cache_path().with_extension("lock")
}

fn models_source_url(args: &[String]) -> String {
    value_for(args, &["--models-url"])
        .or_else(|| env::var("OPENAGENT_MODELS_URL").ok())
        .unwrap_or_else(|| "https://models.dev".to_string())
}

fn models_cache_ttl_seconds(args: &[String]) -> u64 {
    value_for(args, &["--ttl-seconds"])
        .or_else(|| env::var("OPENAGENT_MODELS_TTL_SECONDS").ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(24 * 60 * 60)
}

fn models_cache_is_stale(cache: &Value, ttl_seconds: u64) -> bool {
    let fetched_at = cache
        .get("source")
        .and_then(|source| source.get("fetched_at_ms"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if fetched_at == 0 {
        return true;
    }
    now_ms_cli().saturating_sub(fetched_at) > ttl_seconds.saturating_mul(1000)
}

fn model_cache_lock_is_stale(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|duration| duration > Duration::from_secs(120))
        .unwrap_or(false)
}

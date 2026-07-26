//! Model provider adapters for the Rust rewrite.

use std::collections::{BTreeMap, BTreeSet};

use openagent_protocol::{
    ChatMessage, MaterializedPayload, Model, ModelCapabilities, ModelPricing, RUNTIME_OPTION_KEYS,
    Role, ToolCallPolicy, ToolChoice, ToolSchema, Usage, materialize_openai_compatible_payload,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

mod gemini;
mod tool_calling;

pub use gemini::{
    DEFAULT_GEMINI_BASE_URL, DEFAULT_GEMINI_MODEL, GeminiLanguageModelConfig, build_gemini_payload,
    normalize_gemini_events,
};
pub use tool_calling::{
    NegotiatedToolCallPolicy, ParsedTextToolCalls, ProviderCapability, ProviderCapabilitySet,
    TOOL_CALL_DIALECT_OPTION, ToolCallArgumentsFrame, ToolCallAssembler, ToolCallAssemblyError,
    ToolCallDialect, ToolCallFrame, apply_tool_call_dialect, negotiate_tool_call_policy,
    parse_text_tool_calls, provider_capabilities, tool_call_dialect_from_options,
    tool_call_policy_from_options,
};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
pub const DEFAULT_PROVIDER: &str = "openai";
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-5";
pub const DEFAULT_ANTHROPIC_CONTEXT_WINDOW: u64 = 200_000;
pub const DEFAULT_ANTHROPIC_MAX_OUTPUT: u64 = 8192;
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
pub const HTTP_ERROR_BODY_PREVIEW_CHARS: usize = 800;

#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[must_use]
pub fn protocol_crate_name() -> &'static str {
    openagent_protocol::crate_name()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ProviderStreamEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    Retry {
        attempt: u64,
        max_attempts: u64,
        delay_ms: u64,
        model: String,
        reason: String,
    },
    Reset {
        model: String,
        reason: String,
    },
    Fallback {
        from_model: String,
        to_model: String,
        reason: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        input: Value,
    },
    ToolCallError {
        error: ToolCallAssemblyError,
    },
    Finish {
        finish_reason: String,
        usage: Usage,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct ProviderManager {
    providers: BTreeMap<String, ProviderInfo>,
}

impl ProviderManager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_provider(&mut self, provider_id: impl Into<String>, name: impl Into<String>) {
        let id = provider_id.into();
        self.providers.insert(
            id.clone(),
            ProviderInfo {
                id,
                name: name.into(),
            },
        );
    }

    pub fn get_provider(&self, provider_id: &str) -> Result<&ProviderInfo, String> {
        self.providers
            .get(provider_id)
            .ok_or_else(|| format!("Unknown provider: {provider_id}"))
    }

    #[must_use]
    pub fn list_providers(&self) -> Vec<ProviderInfo> {
        self.providers.values().cloned().collect()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpenAiLanguageModelConfig {
    pub api_key: String,
    pub model_id: String,
    pub provider_id: String,
    pub base_url: String,
    pub timeout_s: f64,
    pub host_header: Option<String>,
    pub wire_api: String,
    pub reasoning_effort: Option<String>,
    pub disable_response_storage: bool,
}

impl OpenAiLanguageModelConfig {
    #[must_use]
    pub fn new(api_key: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model_id: model_id.into(),
            provider_id: DEFAULT_PROVIDER.to_string(),
            base_url: DEFAULT_OPENAI_BASE_URL.to_string(),
            timeout_s: 60.0,
            host_header: None,
            wire_api: "chat".to_string(),
            reasoning_effort: None,
            disable_response_storage: false,
        }
    }

    #[must_use]
    pub fn chat_headers(&self) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::from([
            ("accept".to_string(), "text/event-stream".to_string()),
            (
                "authorization".to_string(),
                format!("Bearer {}", self.api_key),
            ),
            ("content-type".to_string(), "application/json".to_string()),
        ]);
        if let Some(host_header) = self.host_header.as_ref().filter(|value| !value.is_empty()) {
            headers.insert("host".to_string(), host_header.clone());
        }
        headers
    }

    #[must_use]
    pub fn responses_headers(&self) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::from([
            ("accept".to_string(), "application/json".to_string()),
            (
                "authorization".to_string(),
                format!("Bearer {}", self.api_key),
            ),
            ("content-type".to_string(), "application/json".to_string()),
        ]);
        if let Some(host_header) = self.host_header.as_ref().filter(|value| !value.is_empty()) {
            headers.insert("host".to_string(), host_header.clone());
        }
        headers
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AnthropicLanguageModelConfig {
    pub api_key: String,
    pub model_id: String,
    pub base_url: Option<String>,
    pub timeout_s: f64,
    pub max_output: u64,
}

impl AnthropicLanguageModelConfig {
    #[must_use]
    pub fn new(api_key: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model_id: model_id.into(),
            base_url: None,
            timeout_s: 60.0,
            max_output: DEFAULT_ANTHROPIC_MAX_OUTPUT,
        }
    }
}

#[must_use]
pub fn openai_compatible_model(provider_id: &str, model_id: &str) -> Model {
    let label = provider_label(provider_id).unwrap_or_else(|_| provider_id.to_string());
    Model {
        id: model_id.to_string(),
        provider_id: provider_id.to_string(),
        name: format!("{label} Compatible/{model_id}"),
        context_window: 32_768,
        max_output: 4096,
        capabilities: ModelCapabilities {
            vision: false,
            tools: true,
            streaming: true,
            reasoning: false,
        },
        pricing: ModelPricing::default(),
    }
}

#[must_use]
pub fn openagent_text_model_supported(model_id: &str) -> bool {
    matches!(
        model_id,
        "gpt-5.4" | "gpt-5.5" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna"
    )
}

#[must_use]
pub fn openagent_context_model(
    provider_id: &str,
    model_id: &str,
    context_window: Option<u64>,
    max_output: Option<u64>,
) -> Model {
    let mut model = openai_compatible_model(provider_id, model_id);
    model.context_window = context_window.unwrap_or_else(|| {
        if openagent_text_model_supported(model_id) {
            128_000
        } else {
            model.context_window
        }
    });
    if let Some(max_output) = max_output {
        model.max_output = max_output;
    }
    model
}

#[must_use]
pub fn anthropic_model(model_id: &str, context_window: u64, max_output: u64) -> Model {
    Model {
        id: model_id.to_string(),
        provider_id: "anthropic".to_string(),
        name: format!("Anthropic/{model_id}"),
        context_window,
        max_output,
        capabilities: ModelCapabilities {
            vision: true,
            tools: true,
            streaming: true,
            reasoning: true,
        },
        pricing: ModelPricing::default(),
    }
}

pub fn normalize_provider(provider: Option<&str>) -> Result<String, String> {
    let raw = provider
        .unwrap_or(DEFAULT_PROVIDER)
        .trim()
        .to_ascii_lowercase();
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return Err(format!("Invalid provider id: {}", provider.unwrap_or("")));
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit())
        || !chars.all(|item| {
            item.is_ascii_lowercase()
                || item.is_ascii_digit()
                || item == '.'
                || item == '_'
                || item == '-'
        })
    {
        return Err(format!("Invalid provider id: {}", provider.unwrap_or("")));
    }
    Ok(raw)
}

pub fn default_env_mapping(provider: &str) -> Result<BTreeMap<String, String>, String> {
    let normalized = normalize_provider(Some(provider))?;
    if normalized == DEFAULT_PROVIDER {
        return Ok(BTreeMap::from([
            ("api_key".to_string(), "OPENAI_API_KEY".to_string()),
            ("base_url".to_string(), "OPENAI_BASE_URL".to_string()),
            ("model".to_string(), "OPENAI_MODEL".to_string()),
            ("wire_api".to_string(), "OPENAI_WIRE_API".to_string()),
        ]));
    }
    let base_provider = normalized
        .split(['.', '_', '-'])
        .next()
        .unwrap_or(normalized.as_str());
    let prefix = provider_env_prefix(&normalized)
        .or_else(|| provider_env_prefix(base_provider))
        .unwrap_or_else(|| {
            let replaced = normalized
                .chars()
                .map(|item| {
                    if item.is_ascii_alphanumeric() {
                        item.to_ascii_uppercase()
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            let trimmed = replaced.trim_matches('_').to_string();
            if trimmed.is_empty() {
                "OPENAGENT".to_string()
            } else {
                trimmed
            }
        });
    Ok(BTreeMap::from([
        ("api_key".to_string(), format!("{prefix}_API_KEY")),
        ("base_url".to_string(), format!("{prefix}_BASE_URL")),
        ("model".to_string(), format!("{prefix}_MODEL")),
        ("wire_api".to_string(), format!("{prefix}_WIRE_API")),
    ]))
}

#[must_use]
pub fn known_provider_ids() -> Vec<&'static str> {
    vec![
        "anthropic",
        "azure-openai",
        "cohere",
        "deepseek",
        "gemini",
        "groq",
        "mistral",
        "ollama",
        "openai",
        "openrouter",
        "xai",
    ]
}

pub fn provider_label(provider: &str) -> Result<String, String> {
    let normalized = normalize_provider(Some(provider))?;
    if let Some(metadata) = provider_metadata(&normalized)
        && let Some(label) = metadata.label
    {
        return Ok(label.to_string());
    }
    Ok(normalized.replace(['_', '.'], "-").to_title_case())
}

pub fn provider_default_base_url(provider: &str) -> Result<Option<String>, String> {
    let normalized = normalize_provider(Some(provider))?;
    Ok(provider_metadata(&normalized)
        .and_then(|metadata| metadata.default_base_url)
        .map(str::to_string))
}

pub fn provider_default_model(provider: &str) -> Result<Option<String>, String> {
    let normalized = normalize_provider(Some(provider))?;
    Ok(provider_metadata(&normalized)
        .and_then(|metadata| metadata.default_model)
        .map(str::to_string))
}

pub fn provider_requires_api_key(provider: &str) -> Result<bool, String> {
    let normalized = normalize_provider(Some(provider))?;
    Ok(provider_metadata(&normalized).is_none_or(|metadata| metadata.requires_api_key))
}

pub fn provider_auth_methods(
    provider: &str,
    present_env: &BTreeSet<String>,
) -> Result<Vec<Value>, String> {
    let normalized = normalize_provider(Some(provider))?;
    let env = default_env_mapping(&normalized)?;
    let requires_key = provider_requires_api_key(&normalized)?;
    let api_key_env = env.get("api_key").cloned().unwrap_or_default();
    let base_url_env = env.get("base_url").cloned().unwrap_or_default();
    let api_key_status = if present_env.contains(&api_key_env) {
        "set"
    } else if !requires_key {
        "not_required"
    } else {
        "missing"
    };
    let mut method = Map::from_iter([
        ("id".to_string(), json!("api_key")),
        ("type".to_string(), json!("api_key")),
        ("label".to_string(), json!("API key")),
        ("provider".to_string(), json!(normalized)),
        (
            "provider_label".to_string(),
            json!(provider_label(provider)?),
        ),
        ("env".to_string(), json!(env)),
        (
            "fields".to_string(),
            json!([
                {"name": "api_key", "env": api_key_env, "required": requires_key, "secret": true},
                {"name": "base_url", "env": base_url_env, "required": false, "secret": false},
                {"name": "model", "env": env.get("model").cloned().unwrap_or_default(), "required": false, "secret": false},
                {"name": "wire_api", "env": env.get("wire_api").cloned().unwrap_or_default(), "required": false, "secret": false},
            ]),
        ),
        ("status".to_string(), json!(api_key_status)),
        (
            "available".to_string(),
            json!(matches!(api_key_status, "set" | "not_required")),
        ),
        ("implemented".to_string(), json!(true)),
    ]);
    if let Some(default_base_url) = provider_default_base_url(provider)? {
        method.insert("default_base_url".to_string(), json!(default_base_url));
    }
    if let Some(default_model) = provider_default_model(provider)? {
        method.insert("default_model".to_string(), json!(default_model));
    }
    if let Some(notes) = provider_metadata(&normalize_provider(Some(provider))?)
        .and_then(|metadata| metadata.auth_notes)
    {
        method.insert("notes".to_string(), json!(notes));
    }
    Ok(vec![Value::Object(method)])
}

#[must_use]
pub fn build_openai_chat_payload(
    config: &OpenAiLanguageModelConfig,
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    temperature: Option<f64>,
    max_output_tokens: Option<u64>,
    options: Option<&BTreeMap<String, Value>>,
) -> Value {
    build_openai_chat_payload_with_policy(
        config,
        system,
        messages,
        tools,
        temperature,
        max_output_tokens,
        options,
        &ToolCallPolicy::default(),
    )
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_openai_chat_payload_with_policy(
    config: &OpenAiLanguageModelConfig,
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    temperature: Option<f64>,
    max_output_tokens: Option<u64>,
    options: Option<&BTreeMap<String, Value>>,
    tool_policy: &ToolCallPolicy,
) -> Value {
    let model = openai_compatible_model("openai", &config.model_id);
    let payload =
        materialize_openai_compatible_payload(system, messages, tools, Some(&model), options);
    let MaterializedPayload {
        messages,
        mut tools,
        model,
        provider_options,
    } = payload;
    let capabilities = provider_capabilities(&config.provider_id, ToolCallDialect::OpenAiChat);
    if !capabilities.supports(ProviderCapability::StrictToolSchemas) {
        for tool in &mut tools {
            tool.function.strict = false;
        }
    }
    let mut item = Map::from_iter([
        ("messages".to_string(), json!(messages)),
        ("tools".to_string(), json!(tools)),
    ]);
    if let Some(model) = model {
        item.insert("model".to_string(), json!(model));
    }
    item.insert("stream".to_string(), json!(true));
    if let Some(temperature) = temperature {
        item.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(max_output_tokens) = max_output_tokens {
        item.insert("max_tokens".to_string(), json!(max_output_tokens));
    }
    if !tools.is_empty() {
        item.insert(
            "tool_choice".to_string(),
            openai_chat_tool_choice(&tool_policy.choice),
        );
        if let Some(parallel) = tool_policy.parallel {
            item.insert("parallel_tool_calls".to_string(), json!(parallel));
        }
    }
    for (key, value) in provider_options {
        item.insert(key, value);
    }
    Value::Object(item)
}

#[must_use]
pub fn normalize_openai_chat_sse_chunks(chunks: &[Value]) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    let mut assembler = ToolCallAssembler::new(ToolCallDialect::OpenAiChat);
    let mut argument_snapshots = BTreeMap::<String, String>::new();
    let mut finish_reason_raw = Value::Null;
    let mut usage_raw = Value::Null;
    let mut emitted_text = String::new();

    for obj in chunks {
        let Some(choice0) = obj
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_object)
        else {
            continue;
        };
        let choice = Value::Object(choice0.clone());
        let text_snapshot = extract_choice_text(&choice);
        let (text_delta, next_emitted_text) = next_text_delta(&text_snapshot, &emitted_text);
        if !text_delta.is_empty() {
            events.push(ProviderStreamEvent::TextDelta { text: text_delta });
        }
        emitted_text = next_emitted_text;

        if let Some(tool_calls) = choice0
            .get("delta")
            .and_then(Value::as_object)
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for tool_call in tool_calls {
                let Some(tool_call_obj) = tool_call.as_object() else {
                    continue;
                };
                let idx = value_as_u64(tool_call_obj.get("index")).unwrap_or(0);
                let stream_id = idx.to_string();
                let function = tool_call_obj.get("function").and_then(Value::as_object);
                if let Err(error) = assembler.push(ToolCallFrame::Start {
                    stream_id: stream_id.clone(),
                    call_id: tool_call_obj
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    name: function
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }) {
                    events.push(ProviderStreamEvent::ToolCallError { error });
                    continue;
                }
                if let Some(arguments) = function
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                {
                    let emitted = argument_snapshots.entry(stream_id.clone()).or_default();
                    let (delta, next_emitted) = next_text_delta(arguments, emitted);
                    *emitted = next_emitted;
                    if !delta.is_empty()
                        && let Err(error) = assembler.push(ToolCallFrame::Arguments {
                            stream_id,
                            arguments: ToolCallArgumentsFrame::Delta { text: delta },
                        })
                    {
                        events.push(ProviderStreamEvent::ToolCallError { error });
                    }
                }
            }
        }

        if let Some(finish_reason) = choice0.get("finish_reason")
            && !finish_reason.is_null()
        {
            finish_reason_raw = finish_reason.clone();
        }
        if let Some(usage) = obj.get("usage").filter(|value| value.is_object()) {
            usage_raw = usage.clone();
        }
    }

    let completed = if finish_reason_raw.is_null() {
        assembler
            .abort_incomplete()
            .into_iter()
            .map(Err)
            .collect::<Vec<_>>()
    } else {
        assembler.finish_all()
    };
    let mut has_tool_calls = false;
    for result in completed {
        match result {
            Ok(call) => {
                has_tool_calls = true;
                events.push(ProviderStreamEvent::ToolCall {
                    call_id: call.call_id,
                    name: call.name,
                    input: call.input,
                });
            }
            Err(error) => events.push(ProviderStreamEvent::ToolCallError { error }),
        }
    }
    events.push(ProviderStreamEvent::Finish {
        finish_reason: map_openai_finish_reason(&finish_reason_raw, has_tool_calls).to_string(),
        usage: usage_from_openai(usage_raw.as_object()),
    });
    events
}

#[must_use]
pub fn normalize_openai_chat_response(value: &Value) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .unwrap_or(&Value::Null);
    let message = choice.get("message").unwrap_or(choice);
    let text = extract_text_content(message);
    if !text.is_empty() {
        events.push(ProviderStreamEvent::TextDelta { text });
    }
    let mut has_tool_calls = false;
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, tool_call) in tool_calls.iter().enumerate() {
            let function = tool_call.get("function").and_then(Value::as_object);
            let stream_id = index.to_string();
            let mut assembler = ToolCallAssembler::new(ToolCallDialect::OpenAiChat);
            let result = assembler
                .push(ToolCallFrame::Start {
                    stream_id: stream_id.clone(),
                    call_id: tool_call
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    name: function
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .and_then(|_| {
                    let arguments = function
                        .and_then(|function| function.get("arguments"))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    assembler.push(ToolCallFrame::Arguments {
                        stream_id: stream_id.clone(),
                        arguments: match arguments {
                            Value::String(text) => ToolCallArgumentsFrame::Snapshot { text },
                            value => ToolCallArgumentsFrame::Structured { value },
                        },
                    })
                })
                .and_then(|_| assembler.push(ToolCallFrame::End { stream_id }));
            match result {
                Ok(Some(call)) => {
                    has_tool_calls = true;
                    events.push(ProviderStreamEvent::ToolCall {
                        call_id: call.call_id,
                        name: call.name,
                        input: call.input,
                    });
                }
                Ok(None) => {}
                Err(error) => events.push(ProviderStreamEvent::ToolCallError { error }),
            }
        }
    }
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map_or_else(
            || {
                if has_tool_calls {
                    "tool_call".to_string()
                } else {
                    "stop".to_string()
                }
            },
            |reason| map_openai_finish_reason(&json!(reason), has_tool_calls).to_string(),
        );
    events.push(ProviderStreamEvent::Finish {
        finish_reason,
        usage: usage_from_openai(value.get("usage").and_then(Value::as_object)),
    });
    events
}

#[must_use]
pub fn parse_tool_arguments(arguments: &Value) -> Value {
    match arguments {
        Value::Object(_) => arguments.clone(),
        Value::Array(_) => json!({ "_value": arguments }),
        Value::String(raw) => {
            let raw_arguments = raw.trim();
            if raw_arguments.is_empty() {
                return json!({});
            }
            match serde_json::from_str::<Value>(raw_arguments) {
                Ok(Value::Object(item)) => Value::Object(item),
                Ok(value) => json!({ "_value": value }),
                Err(_) => match best_effort_load_json(raw_arguments) {
                    Some(Value::Object(item)) => Value::Object(item),
                    Some(value) => json!({ "_value": value }),
                    None => json!({ "_raw": raw }),
                },
            }
        }
        _ => json!({}),
    }
}

#[must_use]
pub fn summarize_http_error_body(raw: &str, content_type: &str) -> String {
    let text = raw;
    let lower_type = content_type.to_ascii_lowercase();
    let stripped = text.trim_start();
    let stripped_lower = stripped.to_ascii_lowercase();
    let looks_like_html = lower_type.contains("text/html")
        || stripped_lower.starts_with("<!doctype html")
        || stripped_lower.starts_with("<html");
    if looks_like_html {
        let title = extract_html_title(text);
        let suffix = if title.is_empty() {
            String::new()
        } else {
            format!(": {title}")
        };
        return format!("upstream returned HTML error page{suffix}");
    }
    let compact = compact_error_text(text);
    if compact.is_empty() {
        return "empty response body".to_string();
    }
    truncate_error_text(&compact)
}

#[must_use]
pub fn build_openai_responses_payload(
    config: &OpenAiLanguageModelConfig,
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    max_output_tokens: Option<u64>,
    options: Option<&BTreeMap<String, Value>>,
) -> Value {
    build_openai_responses_payload_with_policy(
        config,
        system,
        messages,
        tools,
        max_output_tokens,
        options,
        &ToolCallPolicy::default(),
    )
}

#[must_use]
pub fn build_openai_responses_payload_with_policy(
    config: &OpenAiLanguageModelConfig,
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    max_output_tokens: Option<u64>,
    options: Option<&BTreeMap<String, Value>>,
    tool_policy: &ToolCallPolicy,
) -> Value {
    let mut payload = Map::from_iter([
        ("model".to_string(), json!(config.model_id)),
        (
            "input".to_string(),
            materialize_responses_input(system, messages),
        ),
        ("stream".to_string(), json!(false)),
    ]);
    if let Some(system) = system.filter(|value| !value.is_empty()) {
        payload.insert("instructions".to_string(), json!(system));
    }
    if !tools.is_empty() {
        payload.insert(
            "tools".to_string(),
            materialize_responses_tools(
                tools,
                provider_capabilities(&config.provider_id, ToolCallDialect::OpenAiResponses)
                    .supports(ProviderCapability::StrictToolSchemas),
            ),
        );
        payload.insert(
            "tool_choice".to_string(),
            openai_responses_tool_choice(&tool_policy.choice),
        );
        if let Some(parallel) = tool_policy.parallel {
            payload.insert("parallel_tool_calls".to_string(), json!(parallel));
        }
    }
    if config.disable_response_storage {
        payload.insert("store".to_string(), json!(false));
    }
    if let Some(reasoning_effort) = config
        .reasoning_effort
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        payload.insert(
            "reasoning".to_string(),
            json!({ "effort": reasoning_effort }),
        );
    }
    if let Some(max_output_tokens) = max_output_tokens {
        payload.insert("max_output_tokens".to_string(), json!(max_output_tokens));
    }
    for (key, value) in provider_options(options) {
        if key != "stream" {
            payload.insert(key, value);
        }
    }
    Value::Object(payload)
}

#[must_use]
pub fn normalize_openai_responses_response(value: &Value) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    let content = extract_responses_text(value);
    if !content.is_empty() {
        events.push(ProviderStreamEvent::TextDelta { text: content });
    }
    let mut tool_call_count = 0;
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for (index, item) in output.iter().enumerate() {
            if !response_item_is_tool_call(item) {
                continue;
            }
            let stream_id = responses_stream_id(item, &json!({"output_index": index}));
            let mut assembler = ToolCallAssembler::new(ToolCallDialect::OpenAiResponses);
            let result = assembler
                .push(responses_start_frame(item, &json!({"output_index": index})))
                .and_then(|_| {
                    let arguments = item
                        .get("arguments")
                        .or_else(|| item.get("input"))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    assembler.push(ToolCallFrame::Arguments {
                        stream_id: stream_id.clone(),
                        arguments: match arguments {
                            Value::String(text) => ToolCallArgumentsFrame::Snapshot { text },
                            value => ToolCallArgumentsFrame::Structured { value },
                        },
                    })
                })
                .and_then(|_| assembler.push(ToolCallFrame::End { stream_id }));
            match result {
                Ok(Some(tool_call)) => {
                    tool_call_count += 1;
                    events.push(ProviderStreamEvent::ToolCall {
                        call_id: tool_call.call_id,
                        name: tool_call.name,
                        input: tool_call.input,
                    });
                }
                Ok(None) => {}
                Err(error) => events.push(ProviderStreamEvent::ToolCallError { error }),
            }
        }
    }
    events.push(ProviderStreamEvent::Finish {
        finish_reason: if tool_call_count == 0 {
            "stop".to_string()
        } else {
            "tool_call".to_string()
        },
        usage: usage_from_responses(value.get("usage").and_then(Value::as_object)),
    });
    events
}

#[must_use]
pub fn normalize_openai_responses_stream_events(chunks: &[Value]) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    let mut assembler = ToolCallAssembler::new(ToolCallDialect::OpenAiResponses);
    let mut finish_reason = "stop".to_string();
    let mut usage = Usage::default();
    let mut stream_completed = false;
    let mut has_tool_calls = false;
    for chunk in chunks {
        let event_type = chunk
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match event_type {
            "response.output_text.delta" | "response.refusal.delta" => {
                let text = chunk
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !text.is_empty() {
                    events.push(ProviderStreamEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }
            "response.output_item.added" => {
                let item = chunk.get("item").unwrap_or(&Value::Null);
                if response_item_is_tool_call(item)
                    && let Err(error) = assembler.push(responses_start_frame(item, chunk))
                {
                    events.push(ProviderStreamEvent::ToolCallError { error });
                }
            }
            "response.function_call_arguments.delta" => {
                let stream_id = responses_stream_id(chunk, chunk);
                let delta = chunk
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !delta.is_empty()
                    && let Err(error) = assembler.push(ToolCallFrame::Arguments {
                        stream_id,
                        arguments: ToolCallArgumentsFrame::Delta { text: delta },
                    })
                {
                    events.push(ProviderStreamEvent::ToolCallError { error });
                }
            }
            "response.function_call_arguments.done" => {
                let stream_id = responses_stream_id(chunk, chunk);
                let arguments = chunk
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Err(error) = assembler.push(ToolCallFrame::Arguments {
                    stream_id,
                    arguments: ToolCallArgumentsFrame::Snapshot { text: arguments },
                }) {
                    events.push(ProviderStreamEvent::ToolCallError { error });
                }
            }
            "response.output_item.done" => {
                let item = chunk.get("item").unwrap_or(&Value::Null);
                if response_item_is_tool_call(item) {
                    let stream_id = responses_stream_id(item, chunk);
                    if let Err(error) = assembler.push(responses_start_frame(item, chunk)) {
                        events.push(ProviderStreamEvent::ToolCallError { error });
                        continue;
                    }
                    if let Some(arguments) = item
                        .get("arguments")
                        .or_else(|| item.get("input"))
                        .and_then(Value::as_str)
                        && let Err(error) = assembler.push(ToolCallFrame::Arguments {
                            stream_id: stream_id.clone(),
                            arguments: ToolCallArgumentsFrame::Snapshot {
                                text: arguments.to_string(),
                            },
                        })
                    {
                        events.push(ProviderStreamEvent::ToolCallError { error });
                        continue;
                    }
                    match assembler.push(ToolCallFrame::End { stream_id }) {
                        Ok(Some(call)) => {
                            has_tool_calls = true;
                            finish_reason = "tool_call".to_string();
                            events.push(ProviderStreamEvent::ToolCall {
                                call_id: call.call_id,
                                name: call.name,
                                input: call.input,
                            });
                        }
                        Ok(None) => {}
                        Err(error) => {
                            events.push(ProviderStreamEvent::ToolCallError { error });
                        }
                    }
                }
            }
            "response.completed" => {
                stream_completed = true;
                if let Some(response) = chunk.get("response") {
                    let nested = normalize_openai_responses_response(response);
                    if !nested.is_empty() {
                        usage = nested
                            .iter()
                            .find_map(|event| match event {
                                ProviderStreamEvent::Finish { usage, .. } => Some(usage.clone()),
                                _ => None,
                            })
                            .unwrap_or(usage);
                    }
                }
            }
            "response.failed" | "response.incomplete" => {
                stream_completed = true;
                finish_reason = "error".to_string();
            }
            _ => {}
        }
    }
    let pending = if stream_completed {
        assembler.finish_all()
    } else {
        assembler.abort_incomplete().into_iter().map(Err).collect()
    };
    for result in pending {
        match result {
            Ok(call) => {
                has_tool_calls = true;
                events.push(ProviderStreamEvent::ToolCall {
                    call_id: call.call_id,
                    name: call.name,
                    input: call.input,
                });
            }
            Err(error) => events.push(ProviderStreamEvent::ToolCallError { error }),
        }
    }
    if has_tool_calls && finish_reason == "stop" {
        finish_reason = "tool_call".to_string();
    }
    events.push(ProviderStreamEvent::Finish {
        finish_reason,
        usage,
    });
    events
}

fn response_item_is_tool_call(item: &Value) -> bool {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    matches!(item_type, "function_call" | "custom_tool_call")
}

fn responses_stream_id(item: &Value, envelope: &Value) -> String {
    item.get("id")
        .or_else(|| item.get("item_id"))
        .or_else(|| envelope.get("item_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            item.get("output_index")
                .or_else(|| envelope.get("output_index"))
                .and_then(value_to_u64)
                .map(|index| format!("output_{index}"))
        })
        .unwrap_or_else(|| "responses_tool_call".to_string())
}

fn responses_start_frame(item: &Value, envelope: &Value) -> ToolCallFrame {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let name = item.get("name").and_then(Value::as_str).map(str::to_string);
    ToolCallFrame::Start {
        stream_id: responses_stream_id(item, envelope),
        call_id,
        name,
    }
}

#[must_use]
pub fn build_anthropic_payload(
    config: &AnthropicLanguageModelConfig,
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    temperature: Option<f64>,
    max_output_tokens: Option<u64>,
    options: Option<&BTreeMap<String, Value>>,
) -> Value {
    build_anthropic_payload_with_policy(
        config,
        system,
        messages,
        tools,
        temperature,
        max_output_tokens,
        options,
        &ToolCallPolicy::default(),
    )
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_anthropic_payload_with_policy(
    config: &AnthropicLanguageModelConfig,
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    temperature: Option<f64>,
    max_output_tokens: Option<u64>,
    options: Option<&BTreeMap<String, Value>>,
    tool_policy: &ToolCallPolicy,
) -> Value {
    let mut payload = Map::from_iter([
        ("model".to_string(), json!(config.model_id)),
        (
            "messages".to_string(),
            materialize_anthropic_messages(messages),
        ),
        (
            "max_tokens".to_string(),
            json!(max_output_tokens.unwrap_or(config.max_output)),
        ),
        ("stream".to_string(), json!(true)),
    ]);
    if let Some(system) = system.filter(|value| !value.is_empty()) {
        payload.insert("system".to_string(), json!(system));
    }
    if !tools.is_empty() && !matches!(tool_policy.choice, ToolChoice::None) {
        payload.insert("tools".to_string(), materialize_anthropic_tools(tools));
        payload.insert(
            "tool_choice".to_string(),
            anthropic_tool_choice(tool_policy),
        );
    }
    if let Some(temperature) = temperature {
        payload.insert("temperature".to_string(), json!(temperature));
    }
    for (key, value) in provider_options(options) {
        payload.insert(key, value);
    }
    Value::Object(payload)
}

#[must_use]
pub fn normalize_anthropic_events(source_events: &[Value]) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    let mut assembler = ToolCallAssembler::new(ToolCallDialect::Anthropic);
    let mut finish_reason_raw = Value::Null;
    let mut input_tokens = 0;
    let mut output_tokens = 0;
    let mut has_tool_calls = false;
    let mut message_stopped = false;

    for event in source_events {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match event_type {
            "message_start" => {
                if let Some(value) = event
                    .get("message")
                    .and_then(|message| message.get("usage"))
                    .and_then(|usage| usage.get("input_tokens"))
                    .and_then(value_to_u64)
                {
                    input_tokens = value;
                }
            }
            "content_block_start" => {
                let index = event.get("index").and_then(value_to_u64).unwrap_or(0);
                let block = event.get("content_block").unwrap_or(&Value::Null);
                match block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "text" => {
                        let text = block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if !text.is_empty() {
                            events.push(ProviderStreamEvent::TextDelta { text });
                        }
                    }
                    "tool_use" => {
                        let stream_id = index.to_string();
                        if let Err(error) = assembler.push(ToolCallFrame::Start {
                            stream_id: stream_id.clone(),
                            call_id: block.get("id").and_then(Value::as_str).map(str::to_string),
                            name: block
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        }) {
                            events.push(ProviderStreamEvent::ToolCallError { error });
                            continue;
                        }
                        if let Some(input) = block.get("input").filter(|input| {
                            input.as_object().is_some_and(|value| !value.is_empty())
                        }) && let Err(error) = assembler.push(ToolCallFrame::Arguments {
                            stream_id,
                            arguments: ToolCallArgumentsFrame::Structured {
                                value: input.clone(),
                            },
                        }) {
                            events.push(ProviderStreamEvent::ToolCallError { error });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let index = event.get("index").and_then(value_to_u64).unwrap_or(0);
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "text_delta" => {
                        let text = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if !text.is_empty() {
                            events.push(ProviderStreamEvent::TextDelta { text });
                        }
                    }
                    "input_json_delta" => {
                        let partial_json = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if !partial_json.is_empty()
                            && let Err(error) = assembler.push(ToolCallFrame::Arguments {
                                stream_id: index.to_string(),
                                arguments: ToolCallArgumentsFrame::Delta { text: partial_json },
                            })
                        {
                            events.push(ProviderStreamEvent::ToolCallError { error });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = event.get("index").and_then(value_to_u64).unwrap_or(0);
                match assembler.push(ToolCallFrame::End {
                    stream_id: index.to_string(),
                }) {
                    Ok(Some(call)) => {
                        has_tool_calls = true;
                        events.push(ProviderStreamEvent::ToolCall {
                            call_id: call.call_id,
                            name: call.name,
                            input: call.input,
                        });
                    }
                    Ok(None) => {}
                    Err(error) if error.code.as_ref() == "tool_call_missing_start" => {}
                    Err(error) => events.push(ProviderStreamEvent::ToolCallError { error }),
                }
            }
            "message_delta" => {
                if let Some(reason) = event
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                {
                    finish_reason_raw = reason.clone();
                }
                if let Some(value) = event
                    .get("usage")
                    .and_then(|usage| usage.get("output_tokens"))
                    .and_then(value_to_u64)
                {
                    output_tokens = value;
                }
            }
            "message_stop" => {
                message_stopped = true;
                break;
            }
            _ => {}
        }
    }
    let pending = if message_stopped {
        assembler.finish_all()
    } else {
        assembler.abort_incomplete().into_iter().map(Err).collect()
    };
    for result in pending {
        match result {
            Ok(call) => {
                has_tool_calls = true;
                events.push(ProviderStreamEvent::ToolCall {
                    call_id: call.call_id,
                    name: call.name,
                    input: call.input,
                });
            }
            Err(error) => events.push(ProviderStreamEvent::ToolCallError { error }),
        }
    }
    events.push(ProviderStreamEvent::Finish {
        finish_reason: map_anthropic_finish_reason(&finish_reason_raw, has_tool_calls).to_string(),
        usage: Usage {
            input_tokens,
            output_tokens,
            cost: 0.0,
        },
    });
    events
}

#[must_use]
pub fn normalize_anthropic_response(value: &Value) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    let mut has_tool_calls = false;
    if let Some(content) = value.get("content").and_then(Value::as_array) {
        for (index, block) in content.iter().enumerate() {
            match block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "text" => {
                    if let Some(text) = block.get("text").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        events.push(ProviderStreamEvent::TextDelta {
                            text: text.to_string(),
                        });
                    }
                }
                "thinking" => {
                    if let Some(text) = block
                        .get("thinking")
                        .or_else(|| block.get("text"))
                        .and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        events.push(ProviderStreamEvent::ReasoningDelta {
                            text: text.to_string(),
                        });
                    }
                }
                "tool_use" => {
                    let stream_id = index.to_string();
                    let mut assembler = ToolCallAssembler::new(ToolCallDialect::Anthropic);
                    let result = assembler
                        .push(ToolCallFrame::Start {
                            stream_id: stream_id.clone(),
                            call_id: block.get("id").and_then(Value::as_str).map(str::to_string),
                            name: block
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        })
                        .and_then(|_| {
                            assembler.push(ToolCallFrame::Arguments {
                                stream_id: stream_id.clone(),
                                arguments: ToolCallArgumentsFrame::Structured {
                                    value: block.get("input").cloned().unwrap_or_else(|| json!({})),
                                },
                            })
                        })
                        .and_then(|_| assembler.push(ToolCallFrame::End { stream_id }));
                    match result {
                        Ok(Some(call)) => {
                            has_tool_calls = true;
                            events.push(ProviderStreamEvent::ToolCall {
                                call_id: call.call_id,
                                name: call.name,
                                input: call.input,
                            });
                        }
                        Ok(None) => {}
                        Err(error) => {
                            events.push(ProviderStreamEvent::ToolCallError { error });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    events.push(ProviderStreamEvent::Finish {
        finish_reason: map_anthropic_finish_reason(
            value.get("stop_reason").unwrap_or(&Value::Null),
            has_tool_calls,
        )
        .to_string(),
        usage: Usage {
            input_tokens: value
                .get("usage")
                .and_then(|usage| usage.get("input_tokens"))
                .and_then(value_to_u64)
                .unwrap_or_default(),
            output_tokens: value
                .get("usage")
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(value_to_u64)
                .unwrap_or_default(),
            cost: 0.0,
        },
    });
    events
}

#[derive(Clone, Copy, Debug)]
struct ProviderMetadata {
    label: Option<&'static str>,
    default_base_url: Option<&'static str>,
    default_model: Option<&'static str>,
    requires_api_key: bool,
    auth_notes: Option<&'static str>,
}

trait TitleCase {
    fn to_title_case(&self) -> String;
}

impl TitleCase for str {
    fn to_title_case(&self) -> String {
        self.split('-')
            .map(|part| {
                let mut chars = part.chars();
                let Some(first) = chars.next() else {
                    return String::new();
                };
                format!(
                    "{}{}",
                    first.to_ascii_uppercase(),
                    chars.collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("-")
    }
}

fn provider_env_prefix(provider: &str) -> Option<String> {
    match provider {
        "anthropic" => Some("ANTHROPIC"),
        "azure" | "azure-openai" => Some("AZURE_OPENAI"),
        "cohere" => Some("COHERE"),
        "deepseek" => Some("DEEPSEEK"),
        "gemini" | "google" => Some("GOOGLE"),
        "groq" => Some("GROQ"),
        "mistral" => Some("MISTRAL"),
        "ollama" => Some("OLLAMA"),
        "openai" => Some("OPENAI"),
        "openrouter" => Some("OPENROUTER"),
        "xai" => Some("XAI"),
        _ => None,
    }
    .map(str::to_string)
}

fn provider_metadata(provider: &str) -> Option<ProviderMetadata> {
    let metadata = match provider {
        "anthropic" => ProviderMetadata {
            label: Some("Anthropic"),
            default_base_url: Some("https://api.anthropic.com/v1"),
            default_model: Some(DEFAULT_ANTHROPIC_MODEL),
            requires_api_key: true,
            auth_notes: Some(
                "Native Anthropic Messages routing is supported with ANTHROPIC_API_KEY; well-known provider URL login remains tracked separately.",
            ),
        },
        "azure-openai" => ProviderMetadata {
            label: Some("Azure OpenAI"),
            default_base_url: None,
            default_model: Some(DEFAULT_OPENAI_MODEL),
            requires_api_key: true,
            auth_notes: Some(
                "Set AZURE_OPENAI_BASE_URL to your deployment endpoint when using the OpenAI-compatible runtime.",
            ),
        },
        "cohere" => ProviderMetadata {
            label: Some("Cohere"),
            default_base_url: None,
            default_model: Some("command-a-03-2025"),
            requires_api_key: true,
            auth_notes: Some(
                "Native Cohere SDK routing is not implemented; use an OpenAI-compatible gateway/base URL for runtime calls.",
            ),
        },
        "deepseek" => ProviderMetadata {
            label: Some("DeepSeek"),
            default_base_url: Some("https://api.deepseek.com/v1"),
            default_model: Some("deepseek-chat"),
            requires_api_key: true,
            auth_notes: None,
        },
        "gemini" => ProviderMetadata {
            label: Some("Google Gemini"),
            default_base_url: Some(DEFAULT_GEMINI_BASE_URL),
            default_model: Some(DEFAULT_GEMINI_MODEL),
            requires_api_key: true,
            auth_notes: Some(
                "Native Gemini generateContent routing is supported with GOOGLE_API_KEY.",
            ),
        },
        "groq" => ProviderMetadata {
            label: Some("Groq"),
            default_base_url: Some("https://api.groq.com/openai/v1"),
            default_model: Some("llama-3.3-70b-versatile"),
            requires_api_key: true,
            auth_notes: None,
        },
        "mistral" => ProviderMetadata {
            label: Some("Mistral"),
            default_base_url: Some("https://api.mistral.ai/v1"),
            default_model: Some("mistral-large-latest"),
            requires_api_key: true,
            auth_notes: None,
        },
        "ollama" => ProviderMetadata {
            label: Some("Ollama"),
            default_base_url: Some("http://localhost:11434/v1"),
            default_model: Some("llama3.2"),
            requires_api_key: false,
            auth_notes: None,
        },
        "openai" => ProviderMetadata {
            label: Some("OpenAI"),
            default_base_url: Some(DEFAULT_OPENAI_BASE_URL),
            default_model: Some(DEFAULT_OPENAI_MODEL),
            requires_api_key: true,
            auth_notes: None,
        },
        "openrouter" => ProviderMetadata {
            label: Some("OpenRouter"),
            default_base_url: Some("https://openrouter.ai/api/v1"),
            default_model: Some("openai/gpt-4o-mini"),
            requires_api_key: true,
            auth_notes: None,
        },
        "xai" => ProviderMetadata {
            label: Some("xAI"),
            default_base_url: Some("https://api.x.ai/v1"),
            default_model: Some("grok-3-mini"),
            requires_api_key: true,
            auth_notes: None,
        },
        _ => return None,
    };
    Some(metadata)
}

fn provider_options(options: Option<&BTreeMap<String, Value>>) -> BTreeMap<String, Value> {
    let runtime_keys = RUNTIME_OPTION_KEYS.iter().copied().collect::<BTreeSet<_>>();
    options
        .into_iter()
        .flat_map(BTreeMap::iter)
        .filter(|(key, _value)| {
            !runtime_keys.contains(key.as_str())
                && !matches!(
                    key.as_str(),
                    "tool_choice" | "parallel_tool_calls" | TOOL_CALL_DIALECT_OPTION
                )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn openai_chat_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool { name } => json!({
            "type": "function",
            "function": {"name": name},
        }),
    }
}

fn openai_responses_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool { name } => json!({
            "type": "function",
            "name": name,
        }),
    }
}

fn anthropic_tool_choice(policy: &ToolCallPolicy) -> Value {
    let mut choice = match &policy.choice {
        ToolChoice::Auto => Map::from_iter([("type".to_string(), json!("auto"))]),
        ToolChoice::None => Map::from_iter([("type".to_string(), json!("none"))]),
        ToolChoice::Required => Map::from_iter([("type".to_string(), json!("any"))]),
        ToolChoice::Tool { name } => Map::from_iter([
            ("type".to_string(), json!("tool")),
            ("name".to_string(), json!(name)),
        ]),
    };
    if policy.parallel == Some(false) {
        choice.insert("disable_parallel_tool_use".to_string(), json!(true));
    }
    Value::Object(choice)
}

fn value_as_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(value_to_u64)
}

fn value_to_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|item| u64::try_from(item).ok()))
}

fn best_effort_load_json(raw_text: &str) -> Option<Value> {
    let mut best_candidate = None;
    let mut best_score: Option<(u8, usize, std::cmp::Reverse<usize>)> = None;
    for (index, char_value) in raw_text.char_indices() {
        if char_value != '{' && char_value != '[' {
            continue;
        }
        let slice = &raw_text[index..];
        let mut stream = serde_json::Deserializer::from_str(slice).into_iter::<Value>();
        let Some(Ok(candidate)) = stream.next() else {
            continue;
        };
        let end_index = index + stream.byte_offset();
        let trailing = raw_text[end_index..].trim();
        let score = (
            u8::from(trailing.is_empty()),
            end_index - index,
            std::cmp::Reverse(index),
        );
        if best_score.is_none_or(|current| score > current) {
            best_candidate = Some(candidate);
            best_score = Some(score);
            if score.0 == 1 {
                break;
            }
        }
    }
    best_candidate
}

fn extract_text_content(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(extract_text_content)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(""),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            if let Some(text_value) = map.get("text")
                && let Some(nested) = text_value.get("value").and_then(Value::as_str)
            {
                return nested.to_string();
            }
            if let Some(delta) = map.get("delta") {
                if let Some(delta) = delta.as_str() {
                    return delta.to_string();
                }
                if matches!(delta, Value::Object(_) | Value::Array(_)) {
                    let nested_delta = extract_text_content(delta);
                    if !nested_delta.is_empty() {
                        return nested_delta;
                    }
                }
            }
            for key in ["content", "value", "output_text"] {
                if let Some(nested) = map.get(key)
                    && matches!(
                        nested,
                        Value::String(_) | Value::Object(_) | Value::Array(_)
                    )
                {
                    let extracted = extract_text_content(nested);
                    if !extracted.is_empty() {
                        return extracted;
                    }
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

fn extract_choice_text(choice: &Value) -> String {
    if let Some(delta) = choice.get("delta").filter(|value| value.is_object()) {
        let extracted = extract_text_content(delta);
        if !extracted.is_empty() {
            return extracted;
        }
    }
    if let Some(message) = choice.get("message").filter(|value| value.is_object()) {
        let extracted = extract_text_content(message);
        if !extracted.is_empty() {
            return extracted;
        }
    }
    String::new()
}

fn next_text_delta(raw_text: &str, emitted_text: &str) -> (String, String) {
    if raw_text.is_empty() {
        return (String::new(), emitted_text.to_string());
    }
    if let Some(delta) = raw_text.strip_prefix(emitted_text) {
        return (delta.to_string(), raw_text.to_string());
    }
    if emitted_text.starts_with(raw_text) {
        return (String::new(), emitted_text.to_string());
    }
    (raw_text.to_string(), format!("{emitted_text}{raw_text}"))
}

fn usage_from_openai(usage: Option<&Map<String, Value>>) -> Usage {
    Usage {
        input_tokens: usage
            .and_then(|item| item.get("prompt_tokens"))
            .and_then(value_to_u64)
            .unwrap_or(0),
        output_tokens: usage
            .and_then(|item| item.get("completion_tokens"))
            .and_then(value_to_u64)
            .unwrap_or(0),
        cost: 0.0,
    }
}

fn usage_from_responses(usage: Option<&Map<String, Value>>) -> Usage {
    let input_tokens = usage
        .and_then(|item| {
            item.get("input_tokens")
                .or_else(|| item.get("prompt_tokens"))
                .and_then(value_to_u64)
        })
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|item| {
            item.get("output_tokens")
                .or_else(|| item.get("completion_tokens"))
                .and_then(value_to_u64)
        })
        .unwrap_or(0);
    Usage {
        input_tokens,
        output_tokens,
        cost: 0.0,
    }
}

fn map_openai_finish_reason(value: &Value, has_tool_calls: bool) -> &'static str {
    if let Some(reason) = value.as_str() {
        if reason == "stop" {
            return "stop";
        }
        if reason == "length" {
            return "length";
        }
        if reason == "tool_calls" || reason == "tool_call" {
            return "tool_call";
        }
    }
    if has_tool_calls {
        "tool_call"
    } else {
        "unknown"
    }
}

fn map_anthropic_finish_reason(value: &Value, has_tool_calls: bool) -> &'static str {
    let reason = value.as_str().unwrap_or_default().trim();
    match reason {
        "tool_use" => "tool_call",
        "end_turn" | "stop_sequence" | "stop" => "stop",
        "max_tokens" => "length",
        _ if has_tool_calls => "tool_call",
        _ => "unknown",
    }
}

fn compact_error_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_error_text(text: &str) -> String {
    if text.chars().count() <= HTTP_ERROR_BODY_PREVIEW_CHARS {
        return text.to_string();
    }
    let truncated = text
        .chars()
        .take(HTTP_ERROR_BODY_PREVIEW_CHARS)
        .collect::<String>();
    format!("{}...", truncated.trim_end())
}

fn extract_html_title(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    let Some(start_tag) = lower.find("<title") else {
        return String::new();
    };
    let Some(start_close) = lower[start_tag..].find('>') else {
        return String::new();
    };
    let content_start = start_tag + start_close + 1;
    let Some(end_tag) = lower[content_start..].find("</title>") else {
        return String::new();
    };
    let title = &raw[content_start..content_start + end_tag];
    truncate_error_text(&compact_error_text(&html_unescape_minimal(title)))
}

fn html_unescape_minimal(raw: &str) -> String {
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

fn materialize_responses_input(_system: Option<&str>, messages: &[ChatMessage]) -> Value {
    let mut normalized = Vec::new();
    for message in messages {
        let content = message.content.clone();
        match message.role {
            Role::Tool => {
                if let Some(call_id) = message
                    .tool_call_id
                    .as_ref()
                    .filter(|value| !value.is_empty())
                {
                    normalized.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": content,
                    }));
                }
            }
            Role::Assistant => {
                let tool_calls = message
                    .metadata
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .filter(|items| !items.is_empty());
                if let Some(tool_calls) = tool_calls {
                    for call in tool_calls {
                        if let Some(item) = responses_function_call_item(call) {
                            normalized.push(item);
                        }
                    }
                    if !content.is_empty() {
                        normalized.push(json!({"role": "assistant", "content": content}));
                    }
                    continue;
                }
                if !content.is_empty() {
                    normalized.push(json!({"role": "assistant", "content": content}));
                }
            }
            Role::User => {
                if !content.is_empty() {
                    normalized.push(json!({"role": "user", "content": content}));
                }
            }
            Role::System => {}
        }
    }
    Value::Array(normalized)
}

fn responses_function_call_item(call: &Value) -> Option<Value> {
    let call = call.as_object()?;
    let function = call.get("function").and_then(Value::as_object);
    let name = function
        .and_then(|item| item.get("name"))
        .and_then(Value::as_str)
        .or_else(|| call.get("name").and_then(Value::as_str))
        .unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let arguments = function
        .and_then(|item| item.get("arguments"))
        .cloned()
        .unwrap_or(Value::Null);
    let arguments = arguments.as_str().map_or_else(
        || {
            if arguments.is_null() {
                "{}".to_string()
            } else {
                serde_json::to_string(&arguments).unwrap_or_else(|_error| "{}".to_string())
            }
        },
        str::to_string,
    );
    let call_id = call
        .get("id")
        .or_else(|| call.get("call_id"))
        .and_then(Value::as_str)
        .or_else(|| call.get("tool_call_id").and_then(Value::as_str))
        .unwrap_or_default();
    if call_id.is_empty() {
        return None;
    }
    Some(json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    }))
}

fn materialize_responses_tools(tools: &[ToolSchema], supports_strict: bool) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                let mut definition = Map::from_iter([
                    ("type".to_string(), json!("function")),
                    ("name".to_string(), json!(tool.name)),
                    ("description".to_string(), json!(tool.description)),
                    (
                        "parameters".to_string(),
                        tool.schema
                            .clone()
                            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                    ),
                ]);
                if supports_strict && tool.strict {
                    definition.insert("strict".to_string(), json!(true));
                }
                Value::Object(definition)
            })
            .collect(),
    )
}

fn extract_responses_text(value: &Value) -> String {
    if !value.is_object() {
        return String::new();
    }
    if let Some(output_text) = value.get("output_text").and_then(Value::as_str)
        && !output_text.is_empty()
    {
        return output_text.to_string();
    }
    let mut texts = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            if !item.is_object() {
                continue;
            }
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    if !part.is_object() {
                        continue;
                    }
                    let extracted = extract_text_content(part);
                    if !extracted.is_empty() {
                        texts.push(extracted);
                    }
                }
            } else {
                let extracted = extract_text_content(item);
                if !extracted.is_empty() {
                    texts.push(extracted);
                }
            }
        }
    }
    if !texts.is_empty() {
        return texts.join("");
    }
    if let Some(first) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    {
        return extract_choice_text(first);
    }
    String::new()
}

fn parse_json_object(value: &Value) -> Value {
    match value {
        Value::Object(_) => value.clone(),
        Value::Array(_) => json!({ "_value": value }),
        Value::String(raw) => {
            let stripped = raw.trim();
            if stripped.is_empty() {
                return json!({});
            }
            match serde_json::from_str::<Value>(stripped) {
                Ok(Value::Object(item)) => Value::Object(item),
                Ok(value) => json!({ "_value": value }),
                Err(_) => json!({ "_raw": raw }),
            }
        }
        _ => json!({}),
    }
}

fn materialize_anthropic_messages(messages: &[ChatMessage]) -> Value {
    let mut normalized = Vec::new();
    for message in messages {
        let content = message.content.clone();
        match message.role {
            Role::System => {}
            Role::Tool => {
                let tool_call_id = message.tool_call_id.clone().unwrap_or_default();
                if tool_call_id.is_empty() {
                    continue;
                }
                normalized.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": content,
                    }],
                }));
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                if !content.is_empty() {
                    blocks.push(json!({"type": "text", "text": content}));
                }
                if let Some(tool_calls) =
                    message.metadata.get("tool_calls").and_then(Value::as_array)
                {
                    for call in tool_calls {
                        if let Some(block) = tool_call_content_block(call) {
                            blocks.push(block);
                        }
                    }
                }
                if !blocks.is_empty() {
                    normalized.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            Role::User => {
                if !content.is_empty() {
                    normalized.push(json!({"role": "user", "content": content}));
                }
            }
        }
    }
    Value::Array(normalized)
}

fn tool_call_content_block(call: &Value) -> Option<Value> {
    let call = call.as_object()?;
    let function = call.get("function").and_then(Value::as_object);
    let name = function
        .and_then(|item| item.get("name"))
        .and_then(Value::as_str)
        .or_else(|| call.get("name").and_then(Value::as_str))
        .unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let call_id = call
        .get("id")
        .or_else(|| call.get("call_id"))
        .or_else(|| call.get("tool_call_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if call_id.is_empty() {
        return None;
    }
    let arguments = function
        .and_then(|item| item.get("arguments"))
        .or_else(|| call.get("input"))
        .or_else(|| call.get("arguments"))
        .cloned()
        .unwrap_or(Value::Null);
    Some(json!({
        "type": "tool_use",
        "id": call_id,
        "name": name,
        "input": parse_json_object(&arguments),
    }))
}

fn materialize_anthropic_tools(tools: &[ToolSchema]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.schema.clone().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_to_protocol_crate() {
        assert_eq!(crate_name(), "openagent-provider");
        assert_eq!(protocol_crate_name(), "openagent-protocol");
    }

    #[test]
    fn provider_manager_reports_unknown_provider() {
        let mut manager = ProviderManager::new();
        manager.register_provider("fixture", "Fixture Provider");

        assert_eq!(
            manager
                .get_provider("fixture")
                .map(|item| item.name.as_str()),
            Ok("Fixture Provider")
        );
        assert_eq!(
            manager.get_provider("missing"),
            Err("Unknown provider: missing".to_string())
        );
    }

    #[test]
    fn provider_metadata_normalizes_like_legacy() {
        assert_eq!(
            default_env_mapping("custom.gateway").expect("env mapping"),
            BTreeMap::from([
                ("api_key".to_string(), "CUSTOM_GATEWAY_API_KEY".to_string()),
                (
                    "base_url".to_string(),
                    "CUSTOM_GATEWAY_BASE_URL".to_string()
                ),
                ("model".to_string(), "CUSTOM_GATEWAY_MODEL".to_string()),
                (
                    "wire_api".to_string(),
                    "CUSTOM_GATEWAY_WIRE_API".to_string()
                ),
            ])
        );
        assert!(normalize_provider(Some("bad provider")).is_err());
        assert_eq!(
            provider_label("custom.gateway").expect("label"),
            "Custom-Gateway"
        );
        assert!(!provider_requires_api_key("ollama").expect("requires key"));
    }

    #[test]
    fn openai_argument_parser_recovers_cumulative_snapshot() {
        let parsed = parse_tool_arguments(&Value::String(
            "{\"query\":\"climate tipping points\",\"num_results\":8,\"timeout\":60\
             {\"query\":\"climate tipping points\",\"num_results\":8,\"timeout\":60}"
                .to_string(),
        ));

        assert_eq!(
            parsed,
            json!({"query": "climate tipping points", "num_results": 8, "timeout": 60})
        );
    }
}

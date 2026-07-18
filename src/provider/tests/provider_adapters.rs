use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use openagent_protocol::{ChatMessage, ToolSchema};
use openagent_provider::{
    AnthropicLanguageModelConfig, OpenAiLanguageModelConfig, build_anthropic_payload,
    build_openai_chat_payload, build_openai_responses_payload, default_env_mapping,
    known_provider_ids, normalize_anthropic_events, normalize_openai_chat_sse_chunks,
    normalize_openai_responses_response, openagent_context_model, openagent_text_model_supported,
    parse_tool_arguments, provider_auth_methods, provider_default_base_url, provider_default_model,
    provider_label, provider_requires_api_key, summarize_http_error_body,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[test]
fn provider_adapters_fixture_matches_legacy_oracle() {
    let fixture = read_fixture();

    assert_eq!(
        json!({
            "known_provider_ids": known_provider_ids(),
            "openrouter_env": default_env_mapping("openrouter").expect("openrouter env"),
            "custom_env": default_env_mapping("custom.gateway").expect("custom env"),
            "anthropic_label": provider_label("anthropic").expect("anthropic label"),
            "unknown_label": provider_label("custom.gateway").expect("custom label"),
            "openrouter_default_base_url": provider_default_base_url("openrouter").expect("base url"),
            "anthropic_default_model": provider_default_model("anthropic").expect("model"),
            "ollama_requires_api_key": provider_requires_api_key("ollama").expect("requires key"),
            "openrouter_auth_methods": provider_auth_methods(
                "openrouter",
                &BTreeSet::from(["OPENROUTER_API_KEY".to_string()])
            ).expect("auth methods"),
        }),
        fixture["metadata"]
    );

    let openai = &fixture["openai"];
    assert_eq!(
        json!({
            "dict": parse_tool_arguments(&json!({"path": "."})),
            "list": parse_tool_arguments(&json!(["one", "two"])),
            "malformed": parse_tool_arguments(&json!(
                "{\"query\":\"climate tipping points\",\"num_results\":8,\"timeout\":60\
                 {\"query\":\"climate tipping points\",\"num_results\":8,\"timeout\":60}"
            )),
            "raw": parse_tool_arguments(&json!("{\"path\":")),
        }),
        openai["tool_arguments"]
    );
    assert_eq!(
        json!({
            "html": summarize_http_error_body("<html><title>Bad Gateway</title></html>", "text/html"),
            "empty": summarize_http_error_body("", "application/json"),
            "json": summarize_http_error_body("{\"error\": {\"message\": \"bad request\"}}", "application/json"),
        }),
        openai["http_errors"]
    );

    assert_openai_chat_stream(&openai["chat_stream"]);
    assert_openai_responses(&openai["responses"]);
    assert_anthropic(&fixture["anthropic"]);
}

#[test]
fn provider_factories_cover_models_and_missing_provider_errors() {
    let mut config = OpenAiLanguageModelConfig::new("test", "gpt-test");
    config.provider_id = "openrouter".to_string();
    config.base_url = "https://openrouter.ai/api/v1".to_string();
    config.host_header = Some("router.test".to_string());

    assert_eq!(config.chat_headers()["authorization"], "Bearer test");
    assert_eq!(config.chat_headers()["host"], "router.test");
    assert_eq!(
        provider_label("bad provider"),
        Err("Invalid provider id: bad provider".to_string())
    );
}

#[test]
fn openagent_context_models_share_runtime_limits() {
    assert!(openagent_text_model_supported("gpt-5.5"));
    assert!(!openagent_text_model_supported("custom-model"));
    assert_eq!(
        openagent_context_model("openai", "gpt-5.5", None, None).context_window,
        128_000
    );
    let overridden = openagent_context_model("openai", "gpt-5.5", Some(64_000), Some(8_192));
    assert_eq!(overridden.context_window, 64_000);
    assert_eq!(overridden.max_output, 8_192);
}

fn assert_openai_chat_stream(fixture: &Value) {
    let messages: Vec<ChatMessage> = from_fixture(&fixture["messages"]);
    let tools: Vec<ToolSchema> = from_fixture(&fixture["tools"]);
    let mut config = OpenAiLanguageModelConfig::new("test", "glm47");
    config.base_url = "https://gateway.example.test/v1".to_string();
    config.host_header = Some("model-gateway.example.test".to_string());

    let payload = build_openai_chat_payload(
        &config,
        Some("You are helpful."),
        &messages,
        &tools,
        None,
        None,
        None,
    );
    assert_eq!(payload, fixture["payload"]);
    assert_eq!(json!(config.chat_headers()), fixture["headers"]);

    let chunks: Vec<Value> = from_fixture(&fixture["chunks"]);
    let events = normalize_openai_chat_sse_chunks(&chunks);
    assert_eq!(json!(events), fixture["events"]);
}

fn assert_openai_responses(fixture: &Value) {
    let messages: Vec<ChatMessage> = from_fixture(&fixture["messages"]);
    let tools: Vec<ToolSchema> = from_fixture(&fixture["tools"]);
    let mut config = OpenAiLanguageModelConfig::new("test", "gpt-5.4");
    config.base_url = "https://example.invalid".to_string();
    config.wire_api = "responses".to_string();
    config.reasoning_effort = Some("xhigh".to_string());
    config.disable_response_storage = true;

    let payload =
        build_openai_responses_payload(&config, Some("Use tools."), &messages, &tools, None, None);
    assert_eq!(payload, fixture["payload"]);

    let events = normalize_openai_responses_response(&fixture["response"]);
    assert_eq!(json!(events), fixture["events"]);
}

fn assert_anthropic(fixture: &Value) {
    let messages: Vec<ChatMessage> = from_fixture(&fixture["messages"]);
    let tools: Vec<ToolSchema> = from_fixture(&fixture["tools"]);
    let config = AnthropicLanguageModelConfig::new("test", "claude-test");
    let options = BTreeMap::from([
        ("top_k".to_string(), json!(4)),
        ("trace".to_string(), json!({"enabled": true})),
    ]);
    let payload = build_anthropic_payload(
        &config,
        Some("Use tools."),
        &messages,
        &tools,
        Some(0.2),
        Some(123),
        Some(&options),
    );
    assert_eq!(payload, fixture["payload"]);

    let source_events: Vec<Value> = from_fixture(&fixture["source_events"]);
    let events = normalize_anthropic_events(&source_events);
    assert_eq!(json!(events), fixture["events"]);
}

fn read_fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/rust_rewrite/provider_adapters.json");
    let raw = fs::read_to_string(path).expect("read provider adapters fixture");
    serde_json::from_str(&raw).expect("parse provider adapters fixture")
}

fn from_fixture<T: DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).expect("fixture value deserializes")
}

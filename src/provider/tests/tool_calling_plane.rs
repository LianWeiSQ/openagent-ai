use std::collections::BTreeMap;

use openagent_protocol::{ChatMessage, Role, ToolCallPolicy, ToolChoice, ToolSchema};
use openagent_provider::{
    GeminiLanguageModelConfig, ProviderCapability, ProviderStreamEvent, ToolCallArgumentsFrame,
    ToolCallAssembler, ToolCallDialect, ToolCallFrame, apply_tool_call_dialect,
    build_gemini_payload, build_openai_chat_payload_with_policy,
    build_openai_responses_payload_with_policy, negotiate_tool_call_policy,
    normalize_gemini_events, normalize_openai_responses_stream_events, parse_text_tool_calls,
    provider_capabilities, tool_call_dialect_from_options, tool_call_policy_from_options,
};
use serde_json::json;

#[test]
fn assembler_keeps_interleaved_parallel_calls_isolated() {
    let mut assembler = ToolCallAssembler::new(ToolCallDialect::OpenAiChat);
    assembler
        .push(ToolCallFrame::Start {
            stream_id: "0".to_string(),
            call_id: Some("call_read".to_string()),
            name: Some("read".to_string()),
        })
        .expect("start read");
    assembler
        .push(ToolCallFrame::Start {
            stream_id: "1".to_string(),
            call_id: Some("call_search".to_string()),
            name: Some("search".to_string()),
        })
        .expect("start search");
    for (stream_id, text) in [
        ("0", "{\"path\":"),
        ("1", "{\"query\":"),
        ("0", "\"README.md\"}"),
        ("1", "\"tool frames\"}"),
    ] {
        assembler
            .push(ToolCallFrame::Arguments {
                stream_id: stream_id.to_string(),
                arguments: ToolCallArgumentsFrame::Delta {
                    text: text.to_string(),
                },
            })
            .expect("argument delta");
    }
    let read = assembler
        .push(ToolCallFrame::End {
            stream_id: "0".to_string(),
        })
        .expect("finish read")
        .expect("read call");
    let search = assembler
        .push(ToolCallFrame::End {
            stream_id: "1".to_string(),
        })
        .expect("finish search")
        .expect("search call");

    assert_eq!(read.input, json!({"path": "README.md"}));
    assert_eq!(search.input, json!({"query": "tool frames"}));
}

#[test]
fn assembler_fails_closed_for_malformed_and_truncated_arguments() {
    let mut malformed = ToolCallAssembler::new(ToolCallDialect::Anthropic);
    malformed
        .push(ToolCallFrame::Start {
            stream_id: "3".to_string(),
            call_id: Some("toolu_3".to_string()),
            name: Some("write".to_string()),
        })
        .expect("start");
    malformed
        .push(ToolCallFrame::Arguments {
            stream_id: "3".to_string(),
            arguments: ToolCallArgumentsFrame::Delta {
                text: "{\"path\":".to_string(),
            },
        })
        .expect("delta");
    let error = malformed
        .push(ToolCallFrame::End {
            stream_id: "3".to_string(),
        })
        .expect_err("malformed JSON must fail");
    assert_eq!(error.code.as_ref(), "tool_call_invalid_json");

    let mut truncated = ToolCallAssembler::new(ToolCallDialect::OpenAiResponses);
    truncated
        .push(ToolCallFrame::Start {
            stream_id: "item_1".to_string(),
            call_id: Some("call_1".to_string()),
            name: Some("read".to_string()),
        })
        .expect("start");
    let errors = truncated.abort_incomplete();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code.as_ref(), "tool_call_truncated");
}

#[test]
fn native_dialect_never_scans_ordinary_assistant_text_for_xml() {
    let text = "Explain this literal example: <tool_call>{\"name\":\"rm\"}</tool_call>";
    let events = apply_tool_call_dialect(
        vec![
            ProviderStreamEvent::TextDelta {
                text: text.to_string(),
            },
            finish("stop"),
        ],
        ToolCallDialect::OpenAiChat,
    );
    assert_eq!(
        events,
        vec![
            ProviderStreamEvent::TextDelta {
                text: text.to_string()
            },
            finish("stop")
        ]
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::ToolCall { .. }))
    );

    let text_dialect_events = apply_tool_call_dialect(
        vec![
            ProviderStreamEvent::TextDelta {
                text: "ordinary answer".to_string(),
            },
            finish("stop"),
        ],
        ToolCallDialect::Hermes,
    );
    assert!(text_dialect_events.iter().any(|event| matches!(
        event,
        ProviderStreamEvent::Finish { finish_reason, .. } if finish_reason == "stop"
    )));
}

#[test]
fn text_dialects_are_explicit_and_parse_their_native_shapes() {
    let hermes = parse_text_tool_calls(
        ToolCallDialect::Hermes,
        "preface\n<tool_call>{\"name\":\"read\",\"arguments\":{\"path\":\"README.md\"}}</tool_call>",
    )
    .expect("Hermes");
    assert_eq!(hermes.remaining_text, "preface");
    assert_eq!(hermes.calls[0].input, json!({"path": "README.md"}));

    let qwen = parse_text_tool_calls(
        ToolCallDialect::QwenXml,
        "<function=search><parameter=query>Rust agent</parameter><parameter=limit>3</parameter></function>",
    )
    .expect("Qwen");
    assert_eq!(qwen.calls[0].name, "search");
    assert_eq!(
        qwen.calls[0].input,
        json!({"query": "Rust agent", "limit": 3})
    );

    let deepseek = parse_text_tool_calls(
        ToolCallDialect::DeepSeek,
        "<｜tool▁call▁begin｜>function<｜tool▁sep｜>read\n```json\n{\"path\":\"Cargo.toml\"}\n```<｜tool▁call▁end｜>",
    )
    .expect("DeepSeek");
    assert_eq!(deepseek.calls[0].input, json!({"path": "Cargo.toml"}));

    let pythonic = parse_text_tool_calls(
        ToolCallDialect::Pythonic,
        "search(query='tool calling', limit=5, exact=true)",
    )
    .expect("Pythonic");
    assert_eq!(
        pythonic.calls[0].input,
        json!({"query": "tool calling", "limit": 5, "exact": true})
    );
}

#[test]
fn responses_stream_assembles_fragmented_arguments_and_rejects_eof() {
    let events = normalize_openai_responses_stream_events(&[
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"type": "function_call", "id": "item_1", "call_id": "call_1", "name": "read"}
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "item_1",
            "delta": "{\"path\":"
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "item_1",
            "delta": "\"README.md\"}"
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "item_1",
                "call_id": "call_1",
                "name": "read",
                "arguments": "{\"path\":\"README.md\"}"
            }
        }),
        json!({"type": "response.completed", "response": {"usage": {}}}),
    ]);
    assert!(events.iter().any(|event| matches!(
        event,
        ProviderStreamEvent::ToolCall { call_id, name, input }
            if call_id == "call_1" && name == "read" && input == &json!({"path": "README.md"})
    )));

    let truncated = normalize_openai_responses_stream_events(&[
        json!({
            "type": "response.output_item.added",
            "item": {"type": "function_call", "id": "item_2", "call_id": "call_2", "name": "read"}
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "item_2",
            "delta": "{\"path\":"
        }),
    ]);
    assert!(truncated.iter().any(|event| matches!(
        event,
        ProviderStreamEvent::ToolCallError { error } if error.code.as_ref() == "tool_call_truncated"
    )));
}

#[test]
fn policy_negotiation_maps_choice_strict_output_and_parallel_capability() {
    let safe = tool("read", true, true);
    let unsafe_tool = tool("write", false, false);
    let options = BTreeMap::from([
        (
            "tool_choice".to_string(),
            json!({"type": "tool", "name": "read"}),
        ),
        ("parallel_tool_calls".to_string(), json!(true)),
        ("tool_call_dialect".to_string(), json!("openai_responses")),
    ]);
    let dialect = tool_call_dialect_from_options("openai", "responses", &options).expect("dialect");
    let requested = tool_call_policy_from_options(&options);
    let mut capabilities = provider_capabilities("openai", dialect);
    capabilities
        .values
        .insert(ProviderCapability::ToolOutputSchemas);
    let negotiated =
        negotiate_tool_call_policy(requested, capabilities, &[safe.clone(), unsafe_tool])
            .expect("negotiate");
    assert_eq!(
        negotiated.effective.choice,
        ToolChoice::Tool {
            name: "read".to_string()
        }
    );
    assert_eq!(negotiated.effective.parallel, Some(false));
    assert_eq!(negotiated.strict_tools, vec!["read"]);
    assert_eq!(negotiated.output_schema_tools, vec!["read"]);
    assert!(
        negotiated
            .capabilities
            .supports(ProviderCapability::ParallelToolCalls)
    );
    assert!(
        !provider_capabilities("openrouter", ToolCallDialect::OpenAiChat)
            .supports(ProviderCapability::StrictToolSchemas)
    );
    assert!(
        !provider_capabilities("openai", ToolCallDialect::OpenAiChat)
            .supports(ProviderCapability::ToolOutputSchemas)
    );
    let no_parallel_control = negotiate_tool_call_policy(
        ToolCallPolicy {
            choice: ToolChoice::Auto,
            parallel: Some(true),
        },
        provider_capabilities("gemini", ToolCallDialect::Gemini),
        std::slice::from_ref(&safe),
    )
    .expect("Gemini negotiation");
    assert_eq!(no_parallel_control.effective.parallel, None);
    let native = BTreeMap::from([("tool_call_dialect".to_string(), json!("native"))]);
    assert_eq!(
        tool_call_dialect_from_options("gemini", "generate_content", &native),
        Ok(ToolCallDialect::Gemini)
    );
    assert_eq!(
        tool_call_dialect_from_options("anthropic", "messages", &native),
        Ok(ToolCallDialect::Anthropic)
    );
    assert_eq!(
        tool_call_dialect_from_options("openai", "responses", &native),
        Ok(ToolCallDialect::OpenAiResponses)
    );

    let error = negotiate_tool_call_policy(
        ToolCallPolicy {
            choice: ToolChoice::Tool {
                name: "missing".to_string(),
            },
            parallel: None,
        },
        provider_capabilities("openai", ToolCallDialect::OpenAiChat),
        &[safe],
    )
    .expect_err("unknown named tool");
    assert!(error.contains("unknown tool"));
}

#[test]
fn payloads_emit_negotiated_strict_choice_and_parallel_fields() {
    let tools = vec![tool("read", true, true)];
    let messages = vec![message(Role::User, "Read the file")];
    let policy = ToolCallPolicy {
        choice: ToolChoice::Required,
        parallel: Some(true),
    };
    let config = openagent_provider::OpenAiLanguageModelConfig::new("test", "gpt-test");
    let chat = build_openai_chat_payload_with_policy(
        &config,
        Some("Use tools"),
        &messages,
        &tools,
        None,
        None,
        None,
        &policy,
    );
    assert_eq!(chat["tool_choice"], json!("required"));
    assert_eq!(chat["parallel_tool_calls"], json!(true));
    assert_eq!(chat["tools"][0]["function"]["strict"], json!(true));

    let responses = build_openai_responses_payload_with_policy(
        &config,
        Some("Use tools"),
        &messages,
        &tools,
        None,
        None,
        &policy,
    );
    assert_eq!(responses["tool_choice"], json!("required"));
    assert_eq!(responses["parallel_tool_calls"], json!(true));
    assert_eq!(responses["tools"][0]["strict"], json!(true));
}

#[test]
fn native_gemini_payload_and_stream_preserve_structured_tool_calls() {
    let messages = vec![message(Role::User, "Read Cargo.toml")];
    let tools = vec![tool("read", true, true)];
    let payload = build_gemini_payload(
        Some("Use tools"),
        &messages,
        &tools,
        None,
        &ToolCallPolicy::default(),
    );
    assert_eq!(
        payload["tools"][0]["functionDeclarations"][0]["name"],
        json!("read")
    );
    assert_eq!(
        payload["toolConfig"]["functionCallingConfig"]["mode"],
        json!("AUTO")
    );

    let events = normalize_gemini_events(&[json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"text": "Checking. "},
                    {"functionCall": {"name": "read", "args": {"path": "Cargo.toml"}}}
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 3,
            "thoughtsTokenCount": 2
        }
    })]);
    assert!(events.iter().any(|event| matches!(
        event,
        ProviderStreamEvent::ToolCall { name, input, .. }
            if name == "read" && input == &json!({"path": "Cargo.toml"})
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ProviderStreamEvent::Finish { finish_reason, usage }
            if finish_reason == "tool_call" && usage.input_tokens == 10 && usage.output_tokens == 5
    )));

    let duplicate_calls = normalize_gemini_events(&[json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"functionCall": {"name": "read", "args": {"path": "same.txt"}}},
                    {"functionCall": {"name": "read", "args": {"path": "same.txt"}}}
                ]
            },
            "finishReason": "STOP"
        }]
    })]);
    assert_eq!(
        duplicate_calls
            .iter()
            .filter(|event| matches!(event, ProviderStreamEvent::ToolCall { .. }))
            .count(),
        2
    );

    let config = GeminiLanguageModelConfig::new("secret", "models/gemini-2.5-pro");
    assert_eq!(
        config.endpoint(true),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
    );
    assert!(!config.endpoint(true).contains("secret"));
}

fn tool(name: &str, strict: bool, parallel_safe: bool) -> ToolSchema {
    ToolSchema {
        name: name.to_string(),
        description: format!("{name} tool"),
        schema: Some(json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
        })),
        strict,
        output_schema: strict.then(|| json!({"type": "object"})),
        parallel_safe,
        group: "workspace".to_string(),
        dangerous: !parallel_safe,
    }
}

fn message(role: Role, content: &str) -> ChatMessage {
    ChatMessage {
        role,
        content: content.to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::new(),
    }
}

fn finish(reason: &str) -> ProviderStreamEvent {
    ProviderStreamEvent::Finish {
        finish_reason: reason.to_string(),
        usage: Default::default(),
    }
}

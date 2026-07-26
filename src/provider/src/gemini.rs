use std::collections::BTreeMap;

use openagent_protocol::{ChatMessage, Role, ToolCallPolicy, ToolChoice, ToolSchema, Usage};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{
    ProviderStreamEvent, ToolCallArgumentsFrame, ToolCallAssembler, ToolCallDialect, ToolCallFrame,
};

pub const DEFAULT_GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-2.5-pro";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GeminiLanguageModelConfig {
    pub api_key: String,
    pub model_id: String,
    pub base_url: String,
    pub timeout_s: f64,
}

impl GeminiLanguageModelConfig {
    #[must_use]
    pub fn new(api_key: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model_id: model_id.into(),
            base_url: DEFAULT_GEMINI_BASE_URL.to_string(),
            timeout_s: 60.0,
        }
    }

    #[must_use]
    pub fn endpoint(&self, stream: bool) -> String {
        let model = self
            .model_id
            .trim()
            .strip_prefix("models/")
            .unwrap_or(self.model_id.trim());
        let action = if stream {
            "streamGenerateContent?alt=sse"
        } else {
            "generateContent"
        };
        format!(
            "{}/models/{}:{action}",
            self.base_url.trim_end_matches('/'),
            encode_path_segment(model)
        )
    }
}

#[must_use]
pub fn build_gemini_payload(
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    options: Option<&BTreeMap<String, Value>>,
    tool_policy: &ToolCallPolicy,
) -> Value {
    let mut payload = Map::from_iter([(
        "contents".to_string(),
        materialize_gemini_messages(messages),
    )]);
    if let Some(system) = system.filter(|value| !value.trim().is_empty()) {
        payload.insert(
            "systemInstruction".to_string(),
            json!({"parts": [{"text": system}]}),
        );
    }
    if !tools.is_empty() && !matches!(tool_policy.choice, ToolChoice::None) {
        payload.insert(
            "tools".to_string(),
            json!([{
                "functionDeclarations": tools.iter().map(gemini_tool).collect::<Vec<_>>()
            }]),
        );
        payload.insert(
            "toolConfig".to_string(),
            gemini_tool_config(&tool_policy.choice),
        );
    }
    if let Some(generation) = gemini_generation_config(options) {
        payload.insert("generationConfig".to_string(), generation);
    }
    Value::Object(payload)
}

#[must_use]
pub fn normalize_gemini_events(chunks: &[Value]) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    let mut assembler = ToolCallAssembler::new(ToolCallDialect::Gemini);
    let mut next_call_index = 0_u64;
    let mut finish_reason = Value::Null;
    let mut usage = Usage::default();
    let mut has_tool_calls = false;

    for chunk in chunks {
        if let Some(metadata) = chunk.get("usageMetadata").and_then(Value::as_object) {
            usage = gemini_usage(metadata);
        }
        let Some(candidate) = chunk
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
        else {
            continue;
        };
        if let Some(reason) = candidate
            .get("finishReason")
            .filter(|value| !value.is_null())
        {
            finish_reason = reason.clone();
        }
        let Some(parts) = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str)
                && !text.is_empty()
            {
                if part.get("thought").and_then(Value::as_bool) == Some(true) {
                    events.push(ProviderStreamEvent::ReasoningDelta {
                        text: text.to_string(),
                    });
                } else {
                    events.push(ProviderStreamEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }
            let Some(function_call) = part.get("functionCall").and_then(Value::as_object) else {
                continue;
            };
            let name = function_call
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = function_call
                .get("args")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let index = next_call_index;
            next_call_index += 1;
            let stream_id = index.to_string();
            let call_id = function_call
                .get("id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("gemini_call_{index}"), str::to_string);
            let result = assembler
                .push(ToolCallFrame::Start {
                    stream_id: stream_id.clone(),
                    call_id: Some(call_id),
                    name: (!name.is_empty()).then(|| name.to_string()),
                })
                .and_then(|_| {
                    assembler.push(ToolCallFrame::Arguments {
                        stream_id: stream_id.clone(),
                        arguments: ToolCallArgumentsFrame::Structured { value: arguments },
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
    events.push(ProviderStreamEvent::Finish {
        finish_reason: gemini_finish_reason(&finish_reason, has_tool_calls).to_string(),
        usage,
    });
    events
}

fn materialize_gemini_messages(messages: &[ChatMessage]) -> Value {
    let tool_names = tool_names_by_call_id(messages);
    let mut contents = Vec::new();
    for message in messages {
        match message.role {
            Role::System => {}
            Role::User => {
                if !message.content.is_empty() {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{"text": message.content}],
                    }));
                }
            }
            Role::Assistant => {
                let mut parts = Vec::new();
                if !message.content.is_empty() {
                    parts.push(json!({"text": message.content}));
                }
                if let Some(tool_calls) =
                    message.metadata.get("tool_calls").and_then(Value::as_array)
                {
                    for call in tool_calls {
                        if let Some(function_call) = gemini_function_call(call) {
                            parts.push(function_call);
                        }
                    }
                }
                if !parts.is_empty() {
                    contents.push(json!({"role": "model", "parts": parts}));
                }
            }
            Role::Tool => {
                let call_id = message.tool_call_id.as_deref().unwrap_or_default();
                let name = message
                    .name
                    .as_deref()
                    .or_else(|| tool_names.get(call_id).map(String::as_str))
                    .unwrap_or("tool");
                contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": name,
                            "response": {
                                "name": name,
                                "content": message.content,
                            }
                        }
                    }],
                }));
            }
        }
    }
    Value::Array(contents)
}

fn tool_names_by_call_id(messages: &[ChatMessage]) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    for message in messages {
        let Some(tool_calls) = message.metadata.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in tool_calls {
            let Some(call_id) = call
                .get("id")
                .or_else(|| call.get("call_id"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let name = call
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| {
                    call.get("function")
                        .and_then(Value::as_object)
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                })
                .unwrap_or_default();
            if !name.is_empty() {
                names.insert(call_id.to_string(), name.to_string());
            }
        }
    }
    names
}

fn gemini_function_call(call: &Value) -> Option<Value> {
    let function = call.get("function").and_then(Value::as_object);
    let name = call.get("name").and_then(Value::as_str).or_else(|| {
        function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
    })?;
    let arguments = call
        .get("input")
        .or_else(|| call.get("arguments"))
        .or_else(|| function.and_then(|function| function.get("arguments")))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let arguments = if let Value::String(raw) = arguments {
        serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({}))
    } else {
        arguments
    };
    Some(json!({"functionCall": {"name": name, "args": arguments}}))
}

fn gemini_tool(tool: &ToolSchema) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": gemini_schema(
            &tool
                .schema
                .clone()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
        ),
    })
}

fn gemini_schema(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut output = Map::new();
            for key in [
                "type",
                "description",
                "format",
                "enum",
                "required",
                "minLength",
                "maxLength",
                "minimum",
                "maximum",
            ] {
                if let Some(value) = object.get(key) {
                    output.insert(key.to_string(), value.clone());
                }
            }
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                output.insert(
                    "properties".to_string(),
                    Value::Object(
                        properties
                            .iter()
                            .map(|(key, value)| (key.clone(), gemini_schema(value)))
                            .collect(),
                    ),
                );
            }
            if let Some(items) = object.get("items") {
                output.insert("items".to_string(), gemini_schema(items));
            }
            Value::Object(output)
        }
        _ => value.clone(),
    }
}

fn gemini_tool_config(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({"functionCallingConfig": {"mode": "AUTO"}}),
        ToolChoice::None => json!({"functionCallingConfig": {"mode": "NONE"}}),
        ToolChoice::Required => json!({"functionCallingConfig": {"mode": "ANY"}}),
        ToolChoice::Tool { name } => json!({
            "functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [name],
            }
        }),
    }
}

fn gemini_generation_config(options: Option<&BTreeMap<String, Value>>) -> Option<Value> {
    let options = options?;
    let mut generation = Map::new();
    for (source, target) in [
        ("max_output_tokens", "maxOutputTokens"),
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("top_k", "topK"),
        ("stop", "stopSequences"),
    ] {
        if let Some(value) = options.get(source) {
            generation.insert(target.to_string(), value.clone());
        }
    }
    if let Some(thinking) = options.get("thinking").filter(|value| value.is_object()) {
        generation.insert("thinkingConfig".to_string(), thinking.clone());
    }
    (!generation.is_empty()).then_some(Value::Object(generation))
}

fn gemini_usage(metadata: &Map<String, Value>) -> Usage {
    let input_tokens = value_u64(metadata.get("promptTokenCount"));
    let visible_output = value_u64(metadata.get("candidatesTokenCount"));
    let reasoning = value_u64(metadata.get("thoughtsTokenCount"));
    Usage {
        input_tokens,
        output_tokens: visible_output.saturating_add(reasoning),
        cost: 0.0,
    }
}

fn value_u64(value: Option<&Value>) -> u64 {
    value
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|value| value.try_into().ok()))
        })
        .unwrap_or_default()
}

fn gemini_finish_reason(value: &Value, has_tool_calls: bool) -> &'static str {
    match value.as_str().unwrap_or_default() {
        "STOP" if has_tool_calls => "tool_call",
        "STOP" | "" => "stop",
        "MAX_TOKENS" => "length",
        "MALFORMED_FUNCTION_CALL" => "error",
        "IMAGE_SAFETY" | "RECITATION" | "SAFETY" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
            "content_filter"
        }
        _ if has_tool_calls => "tool_call",
        _ => "unknown",
    }
}

fn encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            other => format!("%{other:02X}").chars().collect(),
        })
        .collect()
}

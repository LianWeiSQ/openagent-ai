use super::*;

#[derive(Clone, Debug)]
pub(super) struct ProviderRunResult {
    pub(super) answer: String,
    pub(super) tool_calls: Vec<ToolCall>,
    pub(super) usage: Usage,
    pub(super) source: String,
    pub(super) finish_reason: String,
}

pub(super) fn call_provider_for_run(
    args: &[String],
    provider: &str,
    model_id: &str,
    context_pack: &ContextPack,
    stream_sink: Option<&mut dyn FnMut(&ProviderStreamEvent)>,
) -> Result<ProviderRunResult, String> {
    context_pack.validate_provider_input()?;
    let messages = context_pack.messages.as_slice();
    let tools = context_pack.tools.as_slice();
    let model_options = &context_pack.model_options;
    if subagent_profile_id(messages).is_some()
        && let Ok(answer) = env::var("OPENAGENT_MOCK_SUBAGENT_ANSWER")
        && !answer.is_empty()
    {
        return Ok(ProviderRunResult {
            answer,
            tool_calls: Vec::new(),
            usage: Usage::default(),
            source: "mock_subagent".to_string(),
            finish_reason: "stop".to_string(),
        });
    }
    if !messages.iter().any(|message| message.role == Role::Tool)
        && let Some(tool_calls) = mock_tool_calls_from_env()?
    {
        return Ok(ProviderRunResult {
            answer: env::var("OPENAGENT_MOCK_TOOL_PREFACE").unwrap_or_default(),
            tool_calls,
            usage: Usage::default(),
            source: "mock".to_string(),
            finish_reason: "tool_call".to_string(),
        });
    }
    if let Ok(answer) = env::var("OPENAGENT_MOCK_ANSWER")
        && !answer.is_empty()
    {
        return Ok(ProviderRunResult {
            answer,
            tool_calls: Vec::new(),
            usage: Usage::default(),
            source: "mock".to_string(),
            finish_reason: "stop".to_string(),
        });
    }
    let api_key = provider_api_key(provider, args);
    if provider_requires_api_key(provider).unwrap_or(true) && api_key.is_none() {
        return Ok(ProviderRunResult {
            answer: "hello from openagent".to_string(),
            tool_calls: Vec::new(),
            usage: Usage::default(),
            source: "offline_fallback_missing_api_key".to_string(),
            finish_reason: "stop".to_string(),
        });
    }
    let api_key = api_key.unwrap_or_default();
    if provider == "anthropic" {
        call_anthropic_provider(
            args,
            &api_key,
            model_id,
            messages,
            tools,
            model_options,
            stream_sink,
        )
    } else {
        call_openai_compatible_provider(
            args,
            provider,
            &api_key,
            model_id,
            messages,
            tools,
            model_options,
            stream_sink,
        )
    }
}

fn apply_model_options_to_payload(
    payload: &mut Value,
    model_options: &BTreeMap<String, Value>,
    wire_api: &str,
) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    for (key, value) in model_options {
        if provider_payload_option_allowed(key) {
            object.insert(
                if wire_api == "chat" && key == "max_output_tokens" {
                    "max_tokens".to_string()
                } else {
                    key.clone()
                },
                value.clone(),
            );
        }
    }
}

fn provider_payload_option_allowed(key: &str) -> bool {
    !matches!(
        key,
        "model"
            | "messages"
            | "input"
            | "tools"
            | "tool_choice"
            | "stream"
            | "skill"
            | "skills"
            | "skill_roots"
            | "skill_permissions"
            | "skill_permission"
            | "context_budget"
            | "context_window"
    )
}

fn system_prompt_from_messages(messages: &[ChatMessage]) -> Option<String> {
    let prompt = messages
        .iter()
        .filter(|message| message.role == Role::System)
        .map(|message| message.content.trim())
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!prompt.is_empty()).then_some(prompt)
}

fn subagent_profile_id(messages: &[ChatMessage]) -> Option<&str> {
    messages.iter().find_map(|message| {
        (message.role == Role::System
            && message
                .metadata
                .get("agent_mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| matches!(mode, "subagent" | "all")))
        .then(|| {
            message
                .metadata
                .get("agent_profile")
                .and_then(Value::as_str)
        })
        .flatten()
    })
}

#[allow(clippy::too_many_arguments)]
fn call_openai_compatible_provider(
    args: &[String],
    provider: &str,
    api_key: &str,
    model_id: &str,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    model_options: &BTreeMap<String, Value>,
    mut stream_sink: Option<&mut dyn FnMut(&ProviderStreamEvent)>,
) -> Result<ProviderRunResult, String> {
    let base_url = provider_base_url(provider, args);
    if is_synthetic_endpoint(&base_url) {
        return Ok(ProviderRunResult {
            answer: "hello from openagent".to_string(),
            tool_calls: Vec::new(),
            usage: Usage::default(),
            source: "offline_fallback_synthetic_endpoint".to_string(),
            finish_reason: "stop".to_string(),
        });
    }
    let wire_api = provider_wire_api(provider, args);
    let system_prompt = system_prompt_from_messages(messages);
    let timeout = Duration::from_secs(
        value_for(args, &["--timeout-s"])
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(60),
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| error.to_string())?;
    let mut config = OpenAiLanguageModelConfig::new(api_key, model_id);
    config.provider_id = provider.to_string();
    config.base_url = base_url.clone();
    config.wire_api = wire_api.clone();
    config.reasoning_effort = model_options
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let stream = provider_streaming_enabled(args);
    let (endpoint, mut payload) = if wire_api == "chat" {
        let mut payload = build_openai_chat_payload(
            &config,
            system_prompt.as_deref(),
            messages,
            tools,
            None,
            None,
            None,
        );
        if let Some(object) = payload.as_object_mut() {
            object.insert("stream".to_string(), json!(stream));
        }
        (join_url(&base_url, "chat/completions"), payload)
    } else {
        let mut payload = build_openai_responses_payload(
            &config,
            system_prompt.as_deref(),
            messages,
            tools,
            None,
            None,
        );
        if stream && let Some(object) = payload.as_object_mut() {
            object.insert("stream".to_string(), json!(true));
        }
        (join_url(&base_url, "responses"), payload)
    };
    apply_model_options_to_payload(&mut payload, model_options, &wire_api);
    let response = send_provider_request_with_retries(
        &client,
        &endpoint,
        api_key,
        &payload,
        stream,
        provider_request_retries(args),
    )?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if stream {
        if !status.is_success() {
            let raw = response
                .text()
                .map_err(|error| format!("provider response read failed: {error}"))?;
            return Err(format!(
                "provider returned HTTP {}: {}",
                status.as_u16(),
                summarize_http_error_body(&raw, &content_type)
            ));
        }
        let mut chunks = Vec::new();
        read_sse_json_values_stream(response, |chunk| {
            if let Some(event) = openai_stream_text_delta(&wire_api, &chunk)
                && let Some(sink) = stream_sink.as_deref_mut()
            {
                sink(&event);
            }
            chunks.push(chunk);
            Ok(())
        })?;
        let events = if wire_api == "chat" {
            normalize_openai_chat_sse_chunks(&chunks)
        } else {
            normalize_openai_responses_stream_events(&chunks)
        };
        return Ok(provider_events_to_run_result(
            &events,
            format!("{provider}:{wire_api}:stream"),
            None,
        ));
    }
    let raw = response
        .text()
        .map_err(|error| format!("provider response read failed: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "provider returned HTTP {}: {}",
            status.as_u16(),
            summarize_http_error_body(&raw, &content_type)
        ));
    }
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("provider response was not JSON: {error}"))?;
    if wire_api == "chat" {
        let answer = extract_chat_answer(&value);
        let tool_calls = extract_chat_tool_calls(&value);
        let finish_reason = extract_chat_finish_reason(&value).unwrap_or_else(|| {
            if tool_calls.is_empty() {
                "stop"
            } else {
                "tool_call"
            }
            .to_string()
        });
        Ok(ProviderRunResult {
            answer: if answer.is_empty() && tool_calls.is_empty() {
                stable_json_dumps(&value)
            } else {
                answer
            },
            tool_calls,
            usage: usage_from_json(value.get("usage")),
            source: format!("{provider}:{wire_api}"),
            finish_reason,
        })
    } else {
        let events = normalize_openai_responses_response(&value);
        Ok(provider_events_to_run_result(
            &events,
            format!("{provider}:{wire_api}"),
            Some(&value),
        ))
    }
}

fn send_provider_request_with_retries(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    api_key: &str,
    payload: &Value,
    stream: bool,
    max_retries: u64,
) -> Result<reqwest::blocking::Response, String> {
    let mut attempt = 0_u64;
    loop {
        let mut request = client
            .post(endpoint)
            .bearer_auth(api_key)
            .header("content-type", "application/json");
        if stream {
            request = request.header("accept", "text/event-stream");
        }
        match request.json(payload).send() {
            Ok(response) => return Ok(response),
            Err(_) if attempt < max_retries => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(500 * attempt));
            }
            Err(error) => return Err(format!("provider request failed: {error}")),
        }
    }
}

fn provider_request_retries(args: &[String]) -> u64 {
    value_for(args, &["--provider-retries"])
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            env::var("OPENAGENT_PROVIDER_RETRIES")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or_else(|| {
            if env::var("OPENAGENT_BENCHMARK_MODE")
                .ok()
                .map(|value| {
                    matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "terminal-bench" | "terminal_bench" | "terminalbench"
                    )
                })
                .unwrap_or(false)
            {
                3
            } else {
                1
            }
        })
}

fn call_anthropic_provider(
    args: &[String],
    api_key: &str,
    model_id: &str,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    model_options: &BTreeMap<String, Value>,
    mut stream_sink: Option<&mut dyn FnMut(&ProviderStreamEvent)>,
) -> Result<ProviderRunResult, String> {
    let timeout = Duration::from_secs(60);
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| error.to_string())?;
    let mut config = AnthropicLanguageModelConfig::new(api_key, model_id);
    config.base_url = Some(provider_base_url("anthropic", args));
    let stream = provider_streaming_enabled(args);
    let system_prompt = system_prompt_from_messages(messages);
    let mut payload = build_anthropic_payload(
        &config,
        system_prompt.as_deref(),
        messages,
        tools,
        None,
        None,
        None,
    );
    if let Some(object) = payload.as_object_mut() {
        object.insert("stream".to_string(), json!(stream));
        for key in ["temperature", "top_p"] {
            if let Some(value) = model_options.get(key) {
                object.insert(key.to_string(), value.clone());
            }
        }
        if let Some(value) = model_options.get("max_output_tokens") {
            object.insert("max_tokens".to_string(), value.clone());
        }
    }
    let endpoint = join_url(
        config
            .base_url
            .as_deref()
            .unwrap_or("https://api.anthropic.com/v1"),
        "messages",
    );
    let mut request = client
        .post(endpoint)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");
    if stream {
        request = request.header("accept", "text/event-stream");
    }
    let response = request
        .json(&payload)
        .send()
        .map_err(|error| format!("anthropic request failed: {error}"))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if stream {
        if !status.is_success() {
            let raw = response
                .text()
                .map_err(|error| format!("anthropic response read failed: {error}"))?;
            return Err(format!(
                "anthropic returned HTTP {}: {}",
                status.as_u16(),
                summarize_http_error_body(&raw, &content_type)
            ));
        }
        let mut chunks = Vec::new();
        read_sse_json_values_stream(response, |chunk| {
            if let Some(event) = anthropic_stream_text_delta(&chunk)
                && let Some(sink) = stream_sink.as_deref_mut()
            {
                sink(&event);
            }
            chunks.push(chunk);
            Ok(())
        })?;
        let events = normalize_anthropic_events(&chunks);
        return Ok(provider_events_to_run_result(
            &events,
            "anthropic:messages:stream".to_string(),
            None,
        ));
    }
    let raw = response
        .text()
        .map_err(|error| format!("anthropic response read failed: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "anthropic returned HTTP {}: {}",
            status.as_u16(),
            summarize_http_error_body(&raw, &content_type)
        ));
    }
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("anthropic response was not JSON: {error}"))?;
    let answer = value
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    let tool_calls = extract_anthropic_tool_calls(&value);
    Ok(ProviderRunResult {
        answer,
        tool_calls,
        usage: usage_from_json(value.get("usage")),
        source: "anthropic:messages".to_string(),
        finish_reason: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("stop")
            .to_string(),
    })
}

fn provider_events_to_run_result(
    events: &[ProviderStreamEvent],
    source: String,
    fallback_json: Option<&Value>,
) -> ProviderRunResult {
    let mut answer = String::new();
    let mut usage = Usage::default();
    let mut tool_calls = Vec::new();
    let mut finish_reason = "stop".to_string();
    for event in events {
        match event {
            ProviderStreamEvent::TextDelta { text } => answer.push_str(text),
            ProviderStreamEvent::ReasoningDelta { .. } => {}
            ProviderStreamEvent::Finish {
                usage: item,
                finish_reason: reason,
            } => {
                usage = item.clone();
                finish_reason = reason.clone();
            }
            ProviderStreamEvent::ToolCall {
                call_id,
                name,
                input,
            } => {
                tool_calls.push(ToolCall {
                    name: name.clone(),
                    input: input.clone(),
                    call_id: call_id.clone(),
                });
            }
            ProviderStreamEvent::Retry { .. } | ProviderStreamEvent::Fallback { .. } => {}
        }
    }
    if answer.is_empty()
        && tool_calls.is_empty()
        && let Some(value) = fallback_json
    {
        answer = stable_json_dumps(value);
    }
    ProviderRunResult {
        answer,
        tool_calls,
        usage,
        source,
        finish_reason,
    }
}

fn provider_streaming_enabled(args: &[String]) -> bool {
    has_flag(args, &["--stream"])
        || env::var("OPENAGENT_STREAM").is_ok_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn read_sse_json_values_stream<R, F>(mut reader: R, mut on_value: F) -> Result<(), String>
where
    R: Read,
    F: FnMut(Value) -> Result<(), String>,
{
    let mut raw = String::new();
    let mut buffer = [0_u8; 4096];
    let mut saw_done = false;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(read) => read,
            Err(_error) if saw_done => break,
            Err(error) => return Err(format!("provider SSE read failed: {error}")),
        };
        if read == 0 {
            break;
        }
        raw.push_str(&String::from_utf8_lossy(&buffer[..read]));
        while let Some(index) = sse_frame_end(&raw) {
            let frame = raw[..index].to_string();
            let drain_to = if raw[index..].starts_with("\r\n\r\n") {
                index + 4
            } else {
                index + 2
            };
            raw.drain(..drain_to);
            if sse_frame_is_done(&frame) {
                saw_done = true;
            }
            if let Some(value) = parse_sse_frame_json(&frame)? {
                on_value(value)?;
            }
        }
    }
    if !raw.trim().is_empty()
        && let Some(value) = parse_sse_frame_json(&raw)?
    {
        on_value(value)?;
    }
    Ok(())
}

fn sse_frame_is_done(frame: &str) -> bool {
    frame.lines().any(|line| {
        let line = line.trim_end_matches('\r');
        line.strip_prefix("data:")
            .map(str::trim)
            .is_some_and(|data| data == "[DONE]")
    })
}

fn sse_frame_end(raw: &str) -> Option<usize> {
    match (raw.find("\r\n\r\n"), raw.find("\n\n")) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    }
}

fn parse_sse_frame_json(frame: &str) -> Result<Option<Value>, String> {
    let mut data_lines = Vec::new();
    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(|error| format!("provider SSE data was not JSON: {error}"))
}

fn openai_stream_text_delta(wire_api: &str, chunk: &Value) -> Option<ProviderStreamEvent> {
    if wire_api == "chat" {
        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|items| items.first());
        let delta = choice.and_then(|choice| choice.get("delta"));
        let text = delta
            .and_then(|delta| delta.get("content"))
            .or_else(|| choice.and_then(|choice| choice.get("text")))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !text.is_empty() {
            return Some(ProviderStreamEvent::TextDelta {
                text: text.to_string(),
            });
        }
        let reasoning = delta
            .and_then(|delta| {
                delta
                    .get("reasoning_content")
                    .or_else(|| delta.get("reasoning"))
                    .or_else(|| delta.get("thinking"))
            })
            .and_then(Value::as_str)
            .unwrap_or_default();
        return (!reasoning.is_empty()).then(|| ProviderStreamEvent::ReasoningDelta {
            text: reasoning.to_string(),
        });
    }
    let text = if matches!(
        chunk.get("type").and_then(Value::as_str),
        Some("response.output_text.delta" | "response.refusal.delta")
    ) {
        chunk
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default()
    } else {
        ""
    };
    (!text.is_empty()).then(|| ProviderStreamEvent::TextDelta {
        text: text.to_string(),
    })
}

fn anthropic_stream_text_delta(chunk: &Value) -> Option<ProviderStreamEvent> {
    let text = match chunk
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "content_block_start" => chunk
            .get("content_block")
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|block| block.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "content_block_delta" => chunk
            .get("delta")
            .filter(|delta| delta.get("type").and_then(Value::as_str) == Some("text_delta"))
            .and_then(|delta| delta.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        _ => "",
    };
    (!text.is_empty()).then(|| ProviderStreamEvent::TextDelta {
        text: text.to_string(),
    })
}

fn normalize_openai_responses_stream_events(chunks: &[Value]) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    let mut finish_reason = "stop".to_string();
    let mut usage = Usage::default();
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
            "response.output_item.done" => {
                if let Some(tool_call) =
                    response_stream_tool_call(chunk.get("item").unwrap_or(&Value::Null))
                {
                    finish_reason = "tool_call".to_string();
                    events.push(tool_call);
                }
            }
            "response.completed" => {
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
                finish_reason = "error".to_string();
            }
            _ => {}
        }
    }
    events.push(ProviderStreamEvent::Finish {
        finish_reason,
        usage,
    });
    events
}

fn response_stream_tool_call(item: &Value) -> Option<ProviderStreamEvent> {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    if !matches!(item_type, "function_call" | "custom_tool_call") {
        return None;
    }
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("responses_tool_call")
        .to_string();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input = item
        .get("arguments")
        .or_else(|| item.get("input"))
        .map(parse_tool_arguments)
        .unwrap_or_else(|| json!({}));
    Some(ProviderStreamEvent::ToolCall {
        call_id,
        name,
        input,
    })
}

fn extract_chat_answer(value: &Value) -> String {
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn extract_chat_tool_calls(value: &Value) -> Vec<ToolCall> {
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("message"))
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, call)| {
            let function = call.get("function");
            let name = function
                .and_then(|item| item.get("name"))
                .or_else(|| call.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            let call_id = call
                .get("id")
                .or_else(|| call.get("call_id"))
                .and_then(Value::as_str)
                .map_or_else(|| format!("chat_call_{index}"), str::to_string);
            let arguments = function
                .and_then(|item| item.get("arguments"))
                .or_else(|| call.get("arguments"))
                .or_else(|| call.get("input"))
                .unwrap_or(&Value::Null);
            Some(ToolCall {
                name: name.to_string(),
                input: parse_tool_arguments(arguments),
                call_id,
            })
        })
        .collect()
}

fn extract_chat_finish_reason(value: &Value) -> Option<String> {
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("finish_reason"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn extract_anthropic_tool_calls(value: &Value) -> Vec<ToolCall> {
    value
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, item)| {
            if item.get("type").and_then(Value::as_str) != Some("tool_use") {
                return None;
            }
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            Some(ToolCall {
                name: name.to_string(),
                input: item.get("input").cloned().unwrap_or_else(|| json!({})),
                call_id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("toolu_{index}"), str::to_string),
            })
        })
        .collect()
}

fn usage_from_json(value: Option<&Value>) -> Usage {
    let Some(value) = value else {
        return Usage::default();
    };
    Usage {
        input_tokens: value
            .get("input_tokens")
            .or_else(|| value.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        output_tokens: value
            .get("output_tokens")
            .or_else(|| value.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cost: 0.0,
    }
}

pub(super) fn add_usage(total: &mut Usage, item: &Usage) {
    total.input_tokens += item.input_tokens;
    total.output_tokens += item.output_tokens;
    total.cost += item.cost;
}

fn mock_tool_calls_from_env() -> Result<Option<Vec<ToolCall>>, String> {
    let Some(raw) = env::var("OPENAGENT_MOCK_TOOL_CALLS")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("OPENAGENT_MOCK_TOOL_CALLS is not JSON: {error}"))?;
    let items = if let Some(items) = value.as_array() {
        items.clone()
    } else {
        vec![value]
    };
    let mut calls = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "mock tool call requires name".to_string())?;
        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .map_or_else(|| format!("mock_call_{index}"), str::to_string);
        let input = item
            .get("input")
            .or_else(|| item.get("arguments"))
            .map(parse_tool_arguments)
            .unwrap_or_else(|| json!({}));
        calls.push(ToolCall {
            name: name.to_string(),
            input,
            call_id,
        });
    }
    Ok(Some(calls))
}

fn provider_api_key(provider: &str, args: &[String]) -> Option<String> {
    resolve_provider_config(provider, args)
        .ok()
        .and_then(|config| config.api_key)
}

fn provider_base_url(provider: &str, args: &[String]) -> String {
    resolve_provider_config(provider, args)
        .map(|config| config.base_url)
        .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn is_synthetic_endpoint(base_url: &str) -> bool {
    base_url.contains(".test")
        || base_url.contains("example.com")
        || base_url.contains("example/v1")
        || base_url.contains("localhost:0")
}

fn provider_wire_api(provider: &str, args: &[String]) -> String {
    resolve_provider_config(provider, args)
        .map(|config| config.wire_api)
        .unwrap_or_else(|_| DEFAULT_WIRE_API.to_string())
}

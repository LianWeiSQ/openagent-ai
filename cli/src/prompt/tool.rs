use super::*;
use openagent_tools::question_answers_from_json;

pub(super) fn execute_agent_tool(
    toolkit: &Toolkit,
    mcp_runtime: Option<&McpRuntime>,
    tool_call: &ToolCall,
    ctx: &mut ToolContext,
) -> ToolResult {
    if let Some(result) = execute_mcp_tool(mcp_runtime, tool_call) {
        return result;
    }
    toolkit.execute(
        &tool_call.name,
        tool_call.input.clone(),
        &tool_call.call_id,
        ctx,
    )
}

pub(super) fn approval_always_patterns(session: &Session) -> Vec<String> {
    session
        .metadata
        .get("approval_always_patterns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

pub(super) fn add_approval_always_pattern(session: &mut Session, pattern: String) {
    if pattern.is_empty() {
        return;
    }
    let mut patterns = approval_always_patterns(session);
    if !patterns.iter().any(|item| item == &pattern) {
        patterns.push(pattern);
    }
    session
        .metadata
        .insert("approval_always_patterns".to_string(), json!(patterns));
}

pub(super) fn assistant_message_for_provider_step(
    content: String,
    tool_calls: &[ToolCall],
) -> ChatMessage {
    let mut message = chat_message(Role::Assistant, content);
    if !tool_calls.is_empty() {
        message.metadata.insert(
            "tool_calls".to_string(),
            Value::Array(tool_calls.iter().map(openai_tool_call_value).collect()),
        );
    }
    message
}

fn openai_tool_call_value(call: &ToolCall) -> Value {
    json!({
        "id": call.call_id.clone(),
        "call_id": call.call_id.clone(),
        "type": "function",
        "function": {
            "name": call.name.clone(),
            "arguments": stable_json_dumps(&call.input),
        },
        "name": call.name.clone(),
        "input": call.input.clone(),
    })
}

pub(super) fn approval_payload_for_tool_call(
    session: &Session,
    run_id: &str,
    step: u64,
    call: &ToolCall,
    metadata: &BTreeMap<String, Value>,
) -> Value {
    json!({
        "request_id": format!("approval_{}", call.call_id),
        "session_id": session.id.clone(),
        "turn_id": run_id,
        "run_id": run_id,
        "step": step,
        "tool_name": call.name.clone(),
        "tool_input": call.input.clone(),
        "call_id": call.call_id.clone(),
        "created_at_ms": now_ms_cli(),
        "permission_action": metadata.get("permission_action").cloned().unwrap_or_else(|| json!("ask")),
        "permission_pattern": metadata.get("permission_pattern").cloned().unwrap_or_else(|| json!("")),
        "reason": metadata.get("error_kind").cloned().unwrap_or_else(|| json!("permission_required")),
        "metadata": metadata,
    })
}

pub(super) fn configured_question_answers(args: &[String]) -> Option<Vec<Vec<String>>> {
    let cli_answers = values_for(args, &["--answer"])
        .into_iter()
        .map(|answer| split_answer_items(&answer))
        .collect::<Vec<_>>();
    if !cli_answers.is_empty() {
        return Some(cli_answers);
    }
    let raw = env::var("OPENAGENT_QUESTION_ANSWERS")
        .ok()
        .filter(|value| !value.trim().is_empty())?;
    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
        if let Some(parsed) = question_answers_from_json(&value) {
            return Some(parsed);
        }
    }
    Some(
        raw.split(';')
            .filter(|item| !item.trim().is_empty())
            .map(split_answer_items)
            .collect(),
    )
}

pub(crate) fn split_answer_items(answer: &str) -> Vec<String> {
    answer
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

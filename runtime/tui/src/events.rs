use serde_json::{Value, json};

use crate::{
    InteractionFocus, TimelineLine, TuiState,
    interaction::{
        approval_matches_active, approval_request_summary, approval_response_summary,
        question_request_summary,
    },
    patch::patch_lines,
    util::{compact_json, object_value, string_field, usage_totals_value},
};

pub(crate) fn event_identity_key(event: &Value) -> String {
    if let Some(event_id) = event.get("event_id").and_then(Value::as_str)
        && !event_id.is_empty()
    {
        return format!("event_id:{event_id}");
    }
    let method = event
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("event");
    let params = event.get("params").cloned().unwrap_or(Value::Null);
    let turn_id = params
        .get("turn_id")
        .or_else(|| {
            params
                .get("approval")
                .and_then(|value| value.get("turn_id"))
        })
        .and_then(Value::as_str)
        .unwrap_or("-");
    let request_id = params
        .get("request_id")
        .or_else(|| {
            params
                .get("approval")
                .and_then(|value| value.get("request_id"))
        })
        .or_else(|| {
            params
                .get("question")
                .and_then(|value| value.get("request_id"))
        })
        .and_then(Value::as_str)
        .unwrap_or("-");
    let sequence = event
        .get("global_sequence")
        .or_else(|| event.get("sequence"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if sequence > 0 {
        format!("{method}:{turn_id}:{request_id}:{sequence}")
    } else {
        format!("{method}:{turn_id}:{request_id}:{}", compact_json(event))
    }
}

impl TuiState {
    fn merge_usage(&mut self, usage: &Value) {
        let input = self.usage_totals["input_tokens"]
            .as_u64()
            .unwrap_or_default()
            + usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_default();
        let output = self.usage_totals["output_tokens"]
            .as_u64()
            .unwrap_or_default()
            + usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_default();
        let total = self.usage_totals["total_tokens"]
            .as_u64()
            .unwrap_or_default()
            + usage
                .get("total_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_default();
        let cost = self.usage_totals["cost"].as_f64().unwrap_or_default()
            + usage
                .get("cost")
                .and_then(Value::as_f64)
                .unwrap_or_default();
        self.usage_totals = usage_totals_value(input, output, total, cost);
    }

    pub fn apply_bridge_event(&mut self, event: &Value) -> Value {
        let method = event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match method {
            "turn/started" => self.apply_turn_started(event),
            "turn/approval_requested" => self.apply_approval_requested(event),
            "turn/approval_resolved" => self.apply_approval_resolved(event),
            "item/toolCall/started" => self.apply_tool_started(event),
            "item/toolCall/delta" => self.apply_tool_delta(event),
            "item/toolCall/completed" => self.apply_tool_finished(event, false),
            "item/toolCall/failed" => self.apply_tool_finished(event, true),
            "item/agentMessage/started" => self.apply_agent_message_started(event),
            "item/agentMessage/delta" => self.apply_agent_message_delta(event),
            "item/agentMessage/completed" => self.apply_agent_message_completed(event),
            "item/question/requested" => self.apply_question_requested(event),
            "item/question/resolved" => self.apply_question_resolved(event),
            "item/reasoning/started" | "item/reasoning/delta" | "item/reasoning/completed" => {
                self.apply_reasoning_event(event)
            }
            "patch/detected" => self.apply_patch_event(event, "patch detected"),
            "patch/undone" => self.apply_patch_event(event, "patch undone"),
            "patch/redone" => self.apply_patch_event(event, "patch redone"),
            "turn/completed" => self.apply_turn_completed(event),
            "turn/failed" => self.apply_turn_failed(event, false),
            "turn/interrupted" => self.apply_turn_failed(event, true),
            "runtime/warning" | "warning" => self.apply_runtime_warning(event),
            _ => json!({"applied": false, "method": method, "unsupported": true}),
        }
    }

    fn apply_turn_started(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let turn_id = params
            .get("turn_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.status = "running".to_string();
        self.timeline.push(TimelineLine::new(
            "status",
            if turn_id.is_empty() {
                "turn started".to_string()
            } else {
                format!("turn started: {turn_id}")
            },
            true,
        ));
        json!({"applied": true, "method": "turn/started"})
    }

    fn apply_agent_message_started(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        self.streaming_assistant_line = None;
        self.status = "assistant streaming".to_string();
        json!({"applied": true, "method": "item/agentMessage/started"})
    }

    fn apply_agent_message_delta(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let text = params
            .get("delta")
            .and_then(Value::as_str)
            .or_else(|| {
                params
                    .get("event")
                    .and_then(|event| event.get("text"))
                    .and_then(Value::as_str)
            })
            .unwrap_or_default();
        if !text.is_empty() {
            if let Some(index) = self.streaming_assistant_line
                && let Some(line) = self.timeline.get_mut(index)
            {
                line.text.push_str(text);
            } else {
                self.timeline
                    .push(TimelineLine::new("assistant", text.to_string(), false));
                self.streaming_assistant_line = Some(self.timeline.len().saturating_sub(1));
            }
        }
        self.status = "assistant streaming".to_string();
        json!({"applied": true, "method": "item/agentMessage/delta"})
    }

    fn apply_agent_message_completed(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        self.streaming_assistant_line = None;
        self.status = "assistant completed".to_string();
        json!({"applied": true, "method": "item/agentMessage/completed"})
    }

    fn apply_turn_completed(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let had_streamed_answer = self.streaming_assistant_line.take().is_some();
        if let Some(answer) = params
            .get("final_answer")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            && !had_streamed_answer
        {
            self.timeline
                .push(TimelineLine::new("assistant", answer.to_string(), true));
        }
        if let Some(trace) = params.get("trace").filter(|value| value.is_object()) {
            self.timeline.push(TimelineLine::new(
                "status",
                format!("trace: {}", compact_json(trace)),
                false,
            ));
        }
        if let Some(usage) = params.get("usage").filter(|value| value.is_object()) {
            self.merge_usage(usage);
            self.timeline.push(TimelineLine::new(
                "status",
                format!(
                    "usage: {} totals={}",
                    compact_json(usage),
                    compact_json(&self.usage_totals)
                ),
                false,
            ));
        }
        self.status = params
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("completed")
            .to_string();
        json!({"applied": true, "method": "turn/completed"})
    }

    fn apply_turn_failed(&mut self, event: &Value, interrupted: bool) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let error = params
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or(if interrupted { "interrupted" } else { "failed" });
        self.status = if interrupted {
            "interrupted".to_string()
        } else {
            "failed".to_string()
        };
        self.timeline
            .push(TimelineLine::new("warning", error.to_string(), true));
        json!({"applied": true, "method": if interrupted { "turn/interrupted" } else { "turn/failed" }})
    }

    fn apply_tool_started(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let name = params
            .get("name")
            .or_else(|| params.get("tool_name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let input = params.get("input").cloned().unwrap_or_else(|| json!({}));
        self.status = format!("tool running: {name}");
        self.timeline.push(TimelineLine::new(
            "status",
            format!("tool started: {name} {}", compact_json(&input)),
            false,
        ));
        json!({"applied": true, "method": "item/toolCall/started"})
    }

    fn apply_tool_delta(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let call_id = params
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let name = params
            .get("name")
            .or_else(|| params.get("tool_name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let delta = params
            .get("delta")
            .or_else(|| params.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !delta.is_empty() {
            if let Some(index) = self.streaming_tool_lines.get(call_id).copied()
                && let Some(line) = self.timeline.get_mut(index)
            {
                line.text.push_str(delta);
            } else {
                let stream = params
                    .get("stream")
                    .and_then(Value::as_str)
                    .unwrap_or("stdout");
                let prefix = if stream == "stderr" { "stderr: " } else { "" };
                self.timeline.push(TimelineLine::new(
                    if stream == "stderr" {
                        "warning"
                    } else {
                        "tool"
                    },
                    format!("{prefix}{delta}"),
                    false,
                ));
                self.streaming_tool_lines
                    .insert(call_id.to_string(), self.timeline.len().saturating_sub(1));
            }
        }
        self.status = format!("tool streaming: {name}");
        json!({"applied": true, "method": "item/toolCall/delta"})
    }

    fn apply_tool_finished(&mut self, event: &Value, failed: bool) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let name = params
            .get("name")
            .or_else(|| params.get("tool_name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let output = params
            .get("error")
            .or_else(|| params.get("output"))
            .cloned()
            .unwrap_or(Value::Null);
        let call_id = params
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let had_streamed_output = self.streaming_tool_lines.remove(call_id).is_some();
        self.status = if failed {
            format!("tool failed: {name}")
        } else {
            format!("tool completed: {name}")
        };
        self.timeline.push(TimelineLine::new(
            if failed { "warning" } else { "status" },
            if had_streamed_output || output.is_null() || output.as_str() == Some("") {
                self.status.clone()
            } else {
                format!("{}: {}", self.status, compact_json(&output))
            },
            failed,
        ));
        if self.show_tool_details {
            let metadata = params.get("metadata").cloned().unwrap_or_else(|| json!({}));
            self.timeline.push(TimelineLine::new(
                "status",
                format!("tool details: {}", compact_json(&metadata)),
                false,
            ));
        }
        json!({"applied": true, "method": if failed { "item/toolCall/failed" } else { "item/toolCall/completed" }})
    }

    fn apply_question_resolved(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        let request_id = params
            .get("request_id")
            .or_else(|| {
                params
                    .get("question")
                    .and_then(|value| value.get("request_id"))
            })
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(active) = self.active_question.as_ref()
            && !request_id.is_empty()
            && string_field(active, "request_id") == request_id
        {
            self.active_question = None;
            self.clear_interaction(InteractionFocus::Question);
        }
        self.status = "question resolved".to_string();
        self.timeline
            .push(TimelineLine::new("status", "question resolved", true));
        json!({"applied": true, "method": "item/question/resolved"})
    }

    fn apply_reasoning_event(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let text = params
            .get("delta")
            .or_else(|| params.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !text.trim().is_empty() {
            self.timeline.push(TimelineLine::new(
                "status",
                format!("reasoning: {text}"),
                false,
            ));
        }
        self.status = "reasoning".to_string();
        json!({"applied": true, "method": event.get("method").and_then(Value::as_str).unwrap_or("item/reasoning")})
    }

    fn apply_runtime_warning(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        let message = params
            .get("message")
            .or_else(|| params.get("warning"))
            .and_then(Value::as_str)
            .unwrap_or("runtime warning");
        self.status = "runtime warning".to_string();
        self.runtime_warnings.push(message.to_string());
        if self.runtime_warnings.len() > 50 {
            self.runtime_warnings.remove(0);
        }
        self.timeline.push(TimelineLine::new(
            "warning",
            format!("warning: {message}"),
            true,
        ));
        json!({"applied": true, "method": event.get("method").and_then(Value::as_str).unwrap_or("warning")})
    }

    fn apply_patch_event(&mut self, event: &Value, label: &str) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let patch = params.get("patch").cloned().unwrap_or_else(|| json!({}));
        self.status = label.to_string();
        self.timeline.extend(patch_lines(label, &patch, true));
        json!({"applied": true, "method": event.get("method").and_then(Value::as_str).unwrap_or("patch")})
    }

    fn apply_approval_requested(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let approval = params
            .get("approval")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if string_field(&approval, "request_id").is_empty() {
            self.status = "approval invalid".to_string();
            return json!({"applied": false, "method": "turn/approval_requested", "error": "approval.request_id is required"});
        }
        self.active_approval = Some(approval.clone());
        self.focus_approval_interaction();
        self.status = "approval pending".to_string();
        self.timeline.push(TimelineLine::new(
            "warning",
            approval_request_summary(&approval),
            true,
        ));
        json!({"applied": true, "method": "turn/approval_requested"})
    }

    fn apply_approval_resolved(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let approval = params
            .get("approval")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if approval_matches_active(&self.active_approval, &approval) {
            self.active_approval = None;
            self.clear_interaction(InteractionFocus::Approval);
        }
        self.status = "approval resolved".to_string();
        self.timeline.push(TimelineLine::new(
            "status",
            approval_response_summary(&approval),
            true,
        ));
        json!({"applied": true, "method": "turn/approval_resolved"})
    }

    fn apply_question_requested(&mut self, event: &Value) -> Value {
        let params = object_value(event.get("params"));
        self.update_session_and_turn(&params);
        let question = params
            .get("event")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or(Value::Object(
                params.as_object().cloned().unwrap_or_default(),
            ));
        if string_field(&question, "request_id").is_empty() {
            self.status = "question invalid".to_string();
            return json!({"applied": false, "method": "item/question/requested", "error": "question.request_id is required"});
        }
        self.active_question = Some(question.clone());
        self.focus_question_interaction(&question);
        self.status = "question pending".to_string();
        self.timeline.push(TimelineLine::new(
            "warning",
            question_request_summary(&question),
            true,
        ));
        json!({"applied": true, "method": "item/question/requested"})
    }

    fn update_session_and_turn(&mut self, params: &Value) {
        if let Some(session_id) = params
            .get("session_id")
            .or_else(|| params.get("thread_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            self.session_id = Some(session_id.to_string());
        }
        if let Some(turn_id) = params
            .get("turn_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            self.current_turn_id = Some(turn_id.to_string());
        }
    }
}

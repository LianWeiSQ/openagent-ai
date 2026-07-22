use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::{ContextItem, estimate_context_message_tokens};

pub const CONTEXT_MICRO_COMPACTION_SCHEMA_VERSION: &str = "openagent.context_micro_compaction.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMicroCompactionStrategy {
    ToolOutputHeadTailV1,
}

impl ContextMicroCompactionStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolOutputHeadTailV1 => "tool_output_head_tail_v1",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRecoveryReferenceKind {
    SessionMessagePart,
    SessionMessage,
    ToolCall,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextRecoveryReference {
    pub kind: ContextRecoveryReferenceKind,
    pub reference: String,
    pub durable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextMicroCompaction {
    pub schema_version: String,
    pub reason: String,
    pub strategy: ContextMicroCompactionStrategy,
    pub original_content_hash: String,
    pub original_bytes: u64,
    pub original_lines: u64,
    pub preview_bytes: u64,
    pub preview_lines: u64,
    pub projected_bytes: u64,
    pub omitted_bytes: u64,
    pub omitted_lines: u64,
    pub original_token_estimate: u64,
    pub projected_token_estimate: u64,
    pub saved_token_estimate: u64,
    pub recovery: ContextRecoveryReference,
}

impl ContextMicroCompaction {
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.schema_version == CONTEXT_MICRO_COMPACTION_SCHEMA_VERSION
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.is_current() {
            return Err("unsupported micro-compaction schema");
        }
        if self.original_content_hash.len() != 45
            || !self.original_content_hash.starts_with("sha1:")
        {
            return Err("invalid original content hash");
        }
        if self.preview_bytes > self.projected_bytes
            || self.omitted_bytes > self.original_bytes
            || self.preview_lines > self.original_lines
            || self.omitted_lines > self.original_lines
            || self.preview_lines.saturating_add(self.omitted_lines) != self.original_lines
        {
            return Err("invalid micro-compaction size accounting");
        }
        if self.projected_token_estimate >= self.original_token_estimate
            || self.saved_token_estimate
                != self
                    .original_token_estimate
                    .saturating_sub(self.projected_token_estimate)
        {
            return Err("invalid micro-compaction token accounting");
        }
        let recovery_valid = match self.recovery.kind {
            ContextRecoveryReferenceKind::SessionMessagePart => {
                self.recovery.durable
                    && self.recovery.message_id.is_some()
                    && self.recovery.part_id.is_some()
            }
            ContextRecoveryReferenceKind::SessionMessage => {
                self.recovery.durable && self.recovery.message_id.is_some()
            }
            ContextRecoveryReferenceKind::ToolCall => {
                !self.recovery.durable && self.recovery.tool_call_id.is_some()
            }
            ContextRecoveryReferenceKind::Unavailable => {
                !self.recovery.durable && self.recovery.reference == "unavailable"
            }
        };
        if self.recovery.reference.is_empty() || !recovery_valid {
            return Err("invalid micro-compaction recovery reference");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextMicroCompactionOptions {
    pub enabled: bool,
    pub tool_output_max_bytes: u64,
    pub tool_output_max_lines: u64,
    pub tool_output_line_max_chars: u64,
}

impl Default for ContextMicroCompactionOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            tool_output_max_bytes: crate::DEFAULT_TOOL_CONTEXT_PREVIEW_BYTES,
            tool_output_max_lines: crate::DEFAULT_TOOL_CONTEXT_PREVIEW_LINES,
            tool_output_line_max_chars: crate::DEFAULT_TOOL_CONTEXT_LINE_MAX_CHARS,
        }
    }
}

pub(crate) fn compact_large_tool_result(
    item: &mut ContextItem,
    options: &ContextMicroCompactionOptions,
    bytes_per_token: u64,
) -> Option<ContextMicroCompaction> {
    if !options.enabled || item.kind != "tool_result" || tool_result_is_protected(item) {
        return None;
    }

    let original = item.content.clone();
    let original_bytes = original.len() as u64;
    let original_lines = line_count(&original);
    if original_bytes <= options.tool_output_max_bytes
        && original_lines <= options.tool_output_max_lines
    {
        return None;
    }
    let original_token_estimate = item.token_estimate;
    let preview = build_head_tail_preview(
        &original,
        options.tool_output_max_bytes as usize,
        options.tool_output_max_lines as usize,
        options.tool_output_line_max_chars as usize,
    );
    let recovery = recovery_reference(item);
    let tool_name = item
        .metadata
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = tool_result_status(item);
    let content_hash = format!("sha1:{}", sha1_hex(&original));
    let mut projected = item.clone();
    projected.content = [
        "[Tool output micro-compacted]".to_string(),
        format!("tool={}", compact_label(tool_name)),
        format!("status={status}"),
        format!("original_bytes={original_bytes}"),
        format!("original_lines={original_lines}"),
        format!("content_hash={content_hash}"),
        format!(
            "strategy={}",
            ContextMicroCompactionStrategy::ToolOutputHeadTailV1.as_str()
        ),
        "preview:".to_string(),
        preview.content.clone(),
        format!("full_output_ref={}", recovery.reference),
    ]
    .join("\n");
    sanitize_tool_result_metadata(&mut projected, &preview.content);
    projected.token_estimate =
        estimate_context_message_tokens(&super::item_to_message(&projected), bytes_per_token);
    if projected.token_estimate >= original_token_estimate {
        return None;
    }
    let compaction = ContextMicroCompaction {
        schema_version: CONTEXT_MICRO_COMPACTION_SCHEMA_VERSION.to_string(),
        reason: "large_tool_output".to_string(),
        strategy: ContextMicroCompactionStrategy::ToolOutputHeadTailV1,
        original_content_hash: content_hash,
        original_bytes,
        original_lines,
        preview_bytes: preview.content.len() as u64,
        preview_lines: preview.retained_lines,
        projected_bytes: projected.content.len() as u64,
        omitted_bytes: original_bytes.saturating_sub(preview.source_bytes),
        omitted_lines: original_lines.saturating_sub(preview.retained_lines),
        original_token_estimate,
        projected_token_estimate: projected.token_estimate,
        saved_token_estimate: original_token_estimate.saturating_sub(projected.token_estimate),
        recovery,
    };
    debug_assert!(compaction.validate().is_ok());
    projected
        .metadata
        .insert("context_truncated".to_string(), Value::Bool(true));
    projected.metadata.insert(
        "context_original_token_estimate".to_string(),
        json!(compaction.original_token_estimate),
    );
    projected.metadata.insert(
        "context_truncation_reason".to_string(),
        json!(compaction.reason),
    );
    projected.metadata.insert(
        "context_truncation_strategy".to_string(),
        json!(compaction.strategy.as_str()),
    );
    projected.metadata.insert(
        "context_micro_compaction".to_string(),
        serde_json::to_value(&compaction).expect("micro compaction serializes"),
    );
    *item = projected;
    Some(compaction)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ToolOutputPreview {
    content: String,
    retained_lines: u64,
    source_bytes: u64,
}

fn build_head_tail_preview(
    text: &str,
    max_bytes: usize,
    max_lines: usize,
    line_max_chars: usize,
) -> ToolOutputPreview {
    let lines = text.split('\n').collect::<Vec<_>>();
    let line_limit = max_lines.max(1);
    let head_count = line_limit.div_ceil(2).min(lines.len());
    let tail_count = line_limit
        .saturating_sub(head_count)
        .min(lines.len().saturating_sub(head_count));
    let marker_bytes = format!(
        "[... {} lines omitted by micro-compaction ...]\n",
        lines.len()
    )
    .len();
    let source_budget = max_bytes.saturating_sub(marker_bytes).max(1);
    let head_budget = if tail_count == 0 {
        source_budget
    } else {
        source_budget.div_ceil(2)
    };
    let tail_budget = source_budget.saturating_sub(head_budget);
    let mut source_bytes = 0u64;
    let head = collect_preview_lines(
        lines.iter().take(head_count).copied(),
        head_budget,
        line_max_chars,
        &mut source_bytes,
    );
    let mut tail = collect_preview_lines(
        lines.iter().rev().take(tail_count).copied(),
        tail_budget,
        line_max_chars,
        &mut source_bytes,
    );
    tail.reverse();
    let retained_lines = (head.len() + tail.len()) as u64;
    let omitted_line_count = lines.len().saturating_sub(retained_lines as usize);
    let mut parts = head;
    if omitted_line_count > 0 {
        parts.push(format!(
            "[... {omitted_line_count} lines omitted by micro-compaction ...]"
        ));
    }
    parts.extend(tail);
    let mut content = parts.join("\n");
    if content.len() > max_bytes {
        content = truncate_utf8(&content, max_bytes).to_string();
    }
    if content.is_empty() {
        content = "(empty preview)".to_string();
    }
    ToolOutputPreview {
        content,
        retained_lines,
        source_bytes,
    }
}

fn collect_preview_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
    max_bytes: usize,
    line_max_chars: usize,
    source_bytes: &mut u64,
) -> Vec<String> {
    let mut output = Vec::new();
    let mut used = 0usize;
    for line in lines {
        let (shortened, retained_source_bytes) = shorten_line(line, line_max_chars.max(1));
        let separator = usize::from(!output.is_empty());
        if used + separator >= max_bytes {
            break;
        }
        let remaining = max_bytes - used - separator;
        let retained = truncate_utf8(&shortened, remaining);
        if retained.is_empty() && !line.is_empty() {
            break;
        }
        *source_bytes =
            source_bytes.saturating_add(retained.len().min(retained_source_bytes) as u64);
        used += separator + retained.len();
        output.push(retained.to_string());
        if retained.len() < shortened.len() {
            break;
        }
    }
    output
}

fn shorten_line(value: &str, max_chars: usize) -> (String, usize) {
    if value.chars().count() <= max_chars {
        return (value.to_string(), value.len());
    }
    let head_chars = max_chars.div_ceil(2);
    let tail_chars = max_chars.saturating_sub(head_chars);
    let head = value.chars().take(head_chars).collect::<String>();
    let tail = value
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    let omitted_chars = value
        .chars()
        .count()
        .saturating_sub(head_chars + tail_chars);
    let retained_source_bytes = head.len() + tail.len();
    (
        format!("{head}[... {omitted_chars} chars omitted ...]{tail}"),
        retained_source_bytes,
    )
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn recovery_reference(item: &ContextItem) -> ContextRecoveryReference {
    let message_metadata = item
        .metadata
        .get("message_metadata")
        .and_then(Value::as_object);
    let message_id = message_metadata
        .and_then(|metadata| {
            metadata
                .get("session_message_id")
                .or_else(|| metadata.get("message_id"))
        })
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let part_id = message_metadata
        .and_then(|metadata| metadata.get("session_part_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let tool_call_id = item
        .metadata
        .get("tool_call_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let (kind, reference, durable) = match (&message_id, &part_id, &tool_call_id) {
        (Some(message_id), Some(part_id), _) => (
            ContextRecoveryReferenceKind::SessionMessagePart,
            format!("session_message:{message_id}#part:{part_id}"),
            true,
        ),
        (Some(message_id), _, _) => (
            ContextRecoveryReferenceKind::SessionMessage,
            format!("session_message:{message_id}"),
            true,
        ),
        (_, _, Some(call_id)) => (
            ContextRecoveryReferenceKind::ToolCall,
            format!("tool_call:{call_id}"),
            false,
        ),
        _ => (
            ContextRecoveryReferenceKind::Unavailable,
            "unavailable".to_string(),
            false,
        ),
    };
    ContextRecoveryReference {
        kind,
        reference,
        durable,
        message_id,
        part_id,
        tool_call_id,
    }
}

fn sanitize_tool_result_metadata(item: &mut ContextItem, preview: &str) {
    let Some(message_metadata) = item
        .metadata
        .get_mut("message_metadata")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let Some(tool_result) = message_metadata
        .get_mut("tool_result")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    tool_result.remove("output");
    if let Some(metadata) = tool_result
        .get_mut("metadata")
        .and_then(Value::as_object_mut)
    {
        for key in ["context_preview", "preview"] {
            if metadata.contains_key(key) {
                metadata.insert(key.to_string(), Value::String(preview.to_string()));
            }
        }
    }
}

fn tool_result_is_protected(item: &ContextItem) -> bool {
    if item.metadata.get("name").and_then(Value::as_str) == Some("skill") {
        return true;
    }
    let message_metadata = item
        .metadata
        .get("message_metadata")
        .and_then(Value::as_object);
    message_metadata
        .and_then(|metadata| metadata.get("context_micro_compaction_protected"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || message_metadata
            .and_then(|metadata| metadata.get("skill_name"))
            .and_then(Value::as_str)
            .is_some()
        || message_metadata
            .and_then(|metadata| metadata.get("tool_result"))
            .and_then(|result| result.get("metadata"))
            .and_then(|metadata| metadata.get("skill_name"))
            .and_then(Value::as_str)
            .is_some()
}

fn tool_result_status(item: &ContextItem) -> &'static str {
    let failed = item
        .metadata
        .get("message_metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("tool_result"))
        .and_then(|result| result.get("error"))
        .is_some_and(|error| !error.is_null());
    if failed { "error" } else { "ok" }
}

fn compact_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '\n' | '\r' | '\0'))
        .take(120)
        .collect()
}

fn line_count(value: &str) -> u64 {
    if value.is_empty() {
        0
    } else {
        value.split('\n').count() as u64
    }
}

fn sha1_hex(value: &str) -> String {
    format!("{:x}", Sha1::digest(value.as_bytes()))
}

pub(crate) fn micro_compaction_from_metadata(
    metadata: &BTreeMap<String, Value>,
) -> Option<ContextMicroCompaction> {
    metadata
        .get("context_micro_compaction")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .filter(|compaction: &ContextMicroCompaction| compaction.validate().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_long_unicode_line_preserves_both_ends_within_preview_budget() {
        let source = format!("UNICODE_HEAD_{}UNICODE_TAIL", "上下文".repeat(2_000));
        let preview = build_head_tail_preview(&source, 512, 6, 80);

        assert!(preview.content.contains("UNICODE_HEAD"));
        assert!(preview.content.contains("UNICODE_TAIL"));
        assert!(preview.content.contains("chars omitted"));
        assert!(preview.content.len() <= 512);
        assert_eq!(preview.retained_lines, 1);
        assert!(preview.source_bytes < source.len() as u64);
    }

    #[test]
    fn line_limit_can_trigger_profitable_micro_compaction_below_byte_limit() {
        let source = (0..100)
            .map(|index| format!("line-{index:03}-{}", "evidence".repeat(4)))
            .collect::<Vec<_>>()
            .join("\n");
        let mut item = ContextItem::new(
            "tool_result:call-lines",
            "tool_result",
            "session.messages[1]",
            source.clone(),
            crate::CONTEXT_PRIORITY_TOOL_RESULT,
        );
        item.token_estimate = 2_000;
        item.metadata = BTreeMap::from([
            ("role".to_string(), json!("tool")),
            ("name".to_string(), json!("grep")),
            ("tool_call_id".to_string(), json!("call-lines")),
            (
                "message_metadata".to_string(),
                json!({
                    "session_message_id": "msg-lines",
                    "session_part_id": "part-lines",
                    "tool_result": {"output": source, "error": null}
                }),
            ),
        ]);
        let options = ContextMicroCompactionOptions {
            enabled: true,
            tool_output_max_bytes: 10_000,
            tool_output_max_lines: 4,
            tool_output_line_max_chars: 80,
        };

        let compacted = compact_large_tool_result(&mut item, &options, 3)
            .expect("line threshold should compact a profitable result");
        assert_eq!(compacted.original_bytes, source.len() as u64);
        assert_eq!(compacted.original_lines, 100);
        assert_eq!(compacted.preview_lines, 4);
        assert_eq!(compacted.omitted_lines, 96);
        assert!(compacted.validate().is_ok());
        assert!(!item.content.contains("line-050"));
        assert_eq!(
            item.metadata
                .get("message_metadata")
                .and_then(|value| value.get("tool_result"))
                .and_then(|value| value.get("output")),
            None
        );
    }
}

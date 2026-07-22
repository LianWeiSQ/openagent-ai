//! Shared protocol contracts for OpenAgent.

mod context_epoch;
mod semantic_anchor;

pub use context_epoch::*;
pub use semantic_anchor::*;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

pub const RUNTIME_OPTION_KEYS: &[&str] = &[
    "compaction",
    "context_budget",
    "logging",
    "observability",
    "runtime_warnings",
    "session_store",
    "trace",
];

#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ModelPricing {
    pub input_per_1m: f64,
    pub output_per_1m: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelCapabilities {
    pub vision: bool,
    pub tools: bool,
    pub streaming: bool,
    pub reasoning: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            vision: false,
            tools: true,
            streaming: true,
            reasoning: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Model {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    pub context_window: u64,
    pub max_output: u64,
    pub capabilities: ModelCapabilities,
    pub pricing: ModelPricing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Pending,
    Running,
    #[default]
    Completed,
    Error,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePartKind {
    Text,
    Tool,
    Reasoning,
    File,
    Approval,
    Question,
    Usage,
    Patch,
    Compaction,
    Context,
    Subtask,
}

impl MessagePartKind {
    #[must_use]
    pub fn from_type(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "tool" | "tool_call" | "tool_result" => Self::Tool,
            "reasoning" => Self::Reasoning,
            "file" | "attachment" => Self::File,
            "approval" | "permission" => Self::Approval,
            "question" => Self::Question,
            "usage" => Self::Usage,
            "patch" | "diff" => Self::Patch,
            "compaction" | "compact" => Self::Compaction,
            "context" | "context_asset" => Self::Context,
            "subtask" => Self::Subtask,
            _ => Self::Text,
        }
    }

    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Tool => "tool",
            Self::Reasoning => "reasoning",
            Self::File => "file",
            Self::Approval => "approval",
            Self::Question => "question",
            Self::Usage => "usage",
            Self::Patch => "patch",
            Self::Compaction => "compaction",
            Self::Context => "context",
            Self::Subtask => "subtask",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MessageInfo {
    pub id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub role: Role,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    pub run_id: Option<String>,
    pub step_index: Option<u64>,
    pub status: MessageStatus,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MessagePart {
    pub id: String,
    pub message_id: String,
    pub session_id: String,
    pub seq: u64,
    pub kind: MessagePartKind,
    pub status: MessageStatus,
    pub content: Value,
    pub attributes: BTreeMap<String, Value>,
    pub timestamp_ms: u64,
    pub run_id: Option<String>,
    pub step_index: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MessageWithParts {
    pub info: MessageInfo,
    pub parts: Vec<MessagePart>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub schema: Option<Value>,
    pub group: String,
    pub dangerous: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub input: Value,
    pub call_id: String,
}

impl ToolCall {
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{}", self.name, stable_json_dumps(&self.input))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    pub error: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCall,
    Length,
    Error,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum StreamEvent {
    TextStart {
        id: String,
        metadata: Option<Value>,
    },
    TextDelta {
        id: String,
        text: String,
    },
    TextEnd {
        id: String,
    },
    ToolCall {
        name: String,
        input: Value,
        call_id: String,
    },
    ToolResult {
        call_id: String,
        output: String,
        error: Option<String>,
        metadata: Option<Value>,
    },
    StepStart {
        snapshot_id: String,
    },
    StepFinish {
        tokens: BTreeMap<String, u64>,
        cost: f64,
        finish_reason: FinishReason,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionAction {
    Allow,
    Deny,
    Ask,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PermissionRule {
    pub tool: String,
    pub action: PermissionAction,
    pub pattern: Option<String>,
    pub condition: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub enum PermissionRuleset {
    #[serde(rename = "FULL")]
    Full,
    #[serde(rename = "READONLY")]
    Readonly,
    #[serde(rename = "PLAN_ONLY")]
    PlanOnly,
    #[serde(rename = "NONE")]
    None,
}

impl PermissionRuleset {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "FULL",
            Self::Readonly => "READONLY",
            Self::PlanOnly => "PLAN_ONLY",
            Self::None => "NONE",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PermissionRulesetDef {
    pub name: PermissionRuleset,
    pub rules: Vec<PermissionRule>,
}

#[must_use]
pub fn ruleset(name: PermissionRuleset) -> PermissionRulesetDef {
    let rules = match name {
        PermissionRuleset::Full => vec![permission_rule("*", PermissionAction::Allow)],
        PermissionRuleset::Readonly => {
            let mut rules = vec![permission_rule("*", PermissionAction::Deny)];
            rules.extend(
                readonly_tools().map(|tool| permission_rule(tool, PermissionAction::Allow)),
            );
            rules
        }
        PermissionRuleset::PlanOnly => {
            let mut rules = vec![permission_rule("*", PermissionAction::Ask)];
            rules.extend(
                plan_only_tools().map(|tool| permission_rule(tool, PermissionAction::Allow)),
            );
            rules
        }
        PermissionRuleset::None => vec![permission_rule("*", PermissionAction::Deny)],
    };
    PermissionRulesetDef { name, rules }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpenAiFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpenAiTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAiFunction,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MaterializedPayload {
    pub messages: Vec<Value>,
    pub tools: Vec<OpenAiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_options: BTreeMap<String, Value>,
}

#[must_use]
pub fn materialize_openai_compatible_payload(
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    model: Option<&Model>,
    options: Option<&BTreeMap<String, Value>>,
) -> MaterializedPayload {
    MaterializedPayload {
        messages: materialize_openai_compatible_messages(system, messages),
        tools: materialize_openai_compatible_tools(tools),
        model: model.map(|item| item.id.clone()),
        provider_options: provider_options(options),
    }
}

#[must_use]
pub fn materialize_openai_compatible_payload_from_parts(
    system: Option<&str>,
    messages: &[MessageWithParts],
    tools: &[ToolSchema],
    model: Option<&Model>,
    options: Option<&BTreeMap<String, Value>>,
) -> MaterializedPayload {
    MaterializedPayload {
        messages: materialize_model_messages(system, messages),
        tools: materialize_openai_compatible_tools(tools),
        model: model.map(|item| item.id.clone()),
        provider_options: provider_options(options),
    }
}

#[must_use]
pub fn materialize_model_messages(
    system: Option<&str>,
    messages: &[MessageWithParts],
) -> Vec<Value> {
    let legacy_messages = message_parts_to_chat_messages(messages);
    materialize_openai_compatible_messages(system, &legacy_messages)
}

#[must_use]
pub fn message_parts_to_chat_messages(messages: &[MessageWithParts]) -> Vec<ChatMessage> {
    let mut normalized = Vec::new();
    for message in messages {
        match message.info.role {
            Role::Assistant => {
                let text = text_content_from_parts(&message.parts);
                let tool_states = tool_states_from_parts(&message.parts);
                let mut metadata = message.info.metadata.clone();
                metadata
                    .entry("message_id".to_string())
                    .or_insert_with(|| json_value_string(message.info.id.clone()));
                let tool_calls = tool_states
                    .iter()
                    .filter_map(ToolPartProjection::tool_call_value)
                    .collect::<Vec<_>>();
                if !tool_calls.is_empty() {
                    metadata.insert("tool_calls".to_string(), Value::Array(tool_calls));
                }
                if !text.is_empty() || !tool_states.is_empty() {
                    normalized.push(ChatMessage {
                        role: Role::Assistant,
                        content: text,
                        name: None,
                        tool_call_id: None,
                        metadata,
                    });
                }
                for tool_state in tool_states {
                    if let Some(tool_message) = tool_state.result_message() {
                        normalized.push(tool_message);
                    }
                }
            }
            Role::Tool => {
                if let Some(tool_message) = tool_message_from_parts(message) {
                    normalized.push(tool_message);
                }
            }
            Role::System | Role::User => {
                let content = text_content_from_parts(&message.parts);
                let attachments = attachment_content_from_parts(&message.parts);
                if content.is_empty() && attachments.is_empty() {
                    continue;
                }
                let mut metadata = message.info.metadata.clone();
                metadata
                    .entry("message_id".to_string())
                    .or_insert_with(|| json_value_string(message.info.id.clone()));
                if !attachments.is_empty() {
                    metadata.insert("context_attachments".to_string(), Value::Array(attachments));
                }
                normalized.push(ChatMessage {
                    role: message.info.role.clone(),
                    content,
                    name: None,
                    tool_call_id: None,
                    metadata,
                });
            }
        }
    }
    normalized
}

#[must_use]
pub fn materialize_openai_compatible_messages(
    system: Option<&str>,
    messages: &[ChatMessage],
) -> Vec<Value> {
    let mut normalized = Vec::new();
    if let Some(system) = system {
        normalized.push(json_object([
            ("role", Value::String("system".to_string())),
            ("content", Value::String(system.to_string())),
        ]));
    }

    for message in messages {
        let role = serde_json::to_value(&message.role).expect("role serializes");
        let mut item = Map::from_iter([
            ("role".to_string(), role),
            (
                "content".to_string(),
                Value::String(message.content.clone()),
            ),
        ]);
        if message.role != Role::Tool
            && let Some(name) = &message.name
        {
            item.insert("name".to_string(), Value::String(name.clone()));
        }
        if let Some(tool_call_id) = &message.tool_call_id {
            item.insert(
                "tool_call_id".to_string(),
                Value::String(tool_call_id.clone()),
            );
        }
        if message.role == Role::Assistant
            && let Some(tool_calls) = message.metadata.get("tool_calls")
            && matches!(tool_calls, Value::Array(items) if !items.is_empty())
        {
            item.insert("tool_calls".to_string(), tool_calls.clone());
            if message.content.is_empty() {
                item.insert("content".to_string(), Value::Null);
            }
        }
        normalized.push(Value::Object(item));
    }
    normalized
}

#[derive(Clone, Debug, Default)]
struct ToolPartProjection {
    call_id: String,
    name: String,
    message_id: String,
    part_id: String,
    input: Value,
    output: Option<String>,
    error: Option<String>,
    metadata: BTreeMap<String, Value>,
    status: MessageStatus,
}

impl ToolPartProjection {
    fn merge_part(&mut self, part: &MessagePart) {
        self.status = part.status.clone();
        self.message_id = part.message_id.clone();
        self.part_id = part.id.clone();
        if let Some(value) =
            string_from_part(part, "call_id").or_else(|| string_from_part(part, "id"))
        {
            self.call_id = value;
        }
        if let Some(value) = string_from_part(part, "name") {
            self.name = value;
        }
        if self.input.is_null()
            && let Some(value) = value_from_part(part, "input")
        {
            self.input = value;
        }
        if let Some(value) = string_from_part(part, "output") {
            self.output = Some(value);
        }
        if let Some(value) = string_from_part(part, "error") {
            self.error = Some(value);
        }
        if let Some(Value::Object(items)) = value_from_part(part, "metadata") {
            self.metadata.extend(items);
        }
        self.metadata.extend(part.attributes.clone());
    }

    fn tool_call_value(&self) -> Option<Value> {
        if self.call_id.is_empty() || self.name.is_empty() {
            return None;
        }
        Some(json_object([
            ("id", Value::String(self.call_id.clone())),
            ("call_id", Value::String(self.call_id.clone())),
            ("type", Value::String("function".to_string())),
            (
                "function",
                json_object([
                    ("name", Value::String(self.name.clone())),
                    ("arguments", Value::String(stable_json_dumps(&self.input))),
                ]),
            ),
            ("name", Value::String(self.name.clone())),
            ("input", self.input.clone()),
        ]))
    }

    fn result_message(&self) -> Option<ChatMessage> {
        if self.call_id.is_empty() {
            return None;
        }
        let pending_error = match self.status {
            MessageStatus::Pending | MessageStatus::Running | MessageStatus::Interrupted => {
                Some("Tool call interrupted before completion".to_string())
            }
            MessageStatus::Error | MessageStatus::Completed => None,
        };
        let error = self.error.clone().or(pending_error);
        let content = error.as_ref().map_or_else(
            || self.output.clone().unwrap_or_default(),
            |error| format!("Tool failed: {error}"),
        );
        let mut metadata = self.metadata.clone();
        if !self.message_id.is_empty() {
            metadata.insert(
                "session_message_id".to_string(),
                Value::String(self.message_id.clone()),
            );
        }
        if !self.part_id.is_empty() {
            metadata.insert(
                "session_part_id".to_string(),
                Value::String(self.part_id.clone()),
            );
        }
        metadata.insert(
            "tool_result".to_string(),
            json_object([
                ("call_id", Value::String(self.call_id.clone())),
                (
                    "output",
                    Value::String(self.output.clone().unwrap_or_default()),
                ),
                ("error", error.clone().map_or(Value::Null, Value::String)),
                (
                    "metadata",
                    Value::Object(metadata.clone().into_iter().collect()),
                ),
            ]),
        );
        Some(ChatMessage {
            role: Role::Tool,
            content,
            name: (!self.name.is_empty()).then(|| self.name.clone()),
            tool_call_id: Some(self.call_id.clone()),
            metadata,
        })
    }
}

fn tool_states_from_parts(parts: &[MessagePart]) -> Vec<ToolPartProjection> {
    let mut by_call_id: BTreeMap<String, ToolPartProjection> = BTreeMap::new();
    let mut anonymous = Vec::new();
    for part in parts
        .iter()
        .filter(|part| part.kind == MessagePartKind::Tool)
    {
        let call_id = string_from_part(part, "call_id")
            .or_else(|| string_from_part(part, "id"))
            .unwrap_or_default();
        if call_id.is_empty() {
            let mut state = ToolPartProjection {
                input: Value::Null,
                ..ToolPartProjection::default()
            };
            state.merge_part(part);
            anonymous.push(state);
            continue;
        }
        by_call_id
            .entry(call_id.clone())
            .or_insert_with(|| ToolPartProjection {
                call_id,
                input: Value::Null,
                ..ToolPartProjection::default()
            })
            .merge_part(part);
    }
    let mut states = by_call_id.into_values().collect::<Vec<_>>();
    states.extend(anonymous);
    states
}

fn tool_message_from_parts(message: &MessageWithParts) -> Option<ChatMessage> {
    let tool_part = message
        .parts
        .iter()
        .find(|part| part.kind == MessagePartKind::Tool)
        .or_else(|| message.parts.first())?;
    let mut state = ToolPartProjection {
        input: Value::Null,
        ..ToolPartProjection::default()
    };
    state.merge_part(tool_part);
    if state.call_id.is_empty() {
        state.call_id = message
            .info
            .metadata
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }
    if state.name.is_empty() {
        state.name = message
            .info
            .metadata
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }
    state.output = state
        .output
        .or_else(|| Some(text_content_from_parts(&message.parts)));
    state.result_message()
}

fn text_content_from_parts(parts: &[MessagePart]) -> String {
    parts
        .iter()
        .filter(|part| {
            matches!(
                part.kind,
                MessagePartKind::Text | MessagePartKind::Reasoning
            )
        })
        .filter_map(part_text)
        .collect::<Vec<_>>()
        .join("")
}

fn attachment_content_from_parts(parts: &[MessagePart]) -> Vec<Value> {
    parts
        .iter()
        .filter(|part| part.kind == MessagePartKind::File)
        .map(|part| match &part.content {
            Value::Object(_) => part.content.clone(),
            content => serde_json::json!({
                "id": part.id,
                "kind": "file",
                "content_type": "application/octet-stream",
                "size_bytes": 0,
                "content": content,
            }),
        })
        .collect()
}

fn part_text(part: &MessagePart) -> Option<String> {
    part.content
        .as_str()
        .map(ToString::to_string)
        .or_else(|| string_from_part(part, "text"))
        .or_else(|| string_from_part(part, "content"))
}

fn string_from_part(part: &MessagePart, key: &str) -> Option<String> {
    value_from_part(part, key).and_then(|value| match value {
        Value::String(value) => Some(value),
        Value::Null => None,
        other => Some(stable_json_dumps(&other)),
    })
}

fn value_from_part(part: &MessagePart, key: &str) -> Option<Value> {
    part.content
        .get(key)
        .cloned()
        .or_else(|| part.attributes.get(key).cloned())
}

fn json_value_string(value: String) -> Value {
    Value::String(value)
}

#[must_use]
pub fn materialize_openai_compatible_tools(tools: &[ToolSchema]) -> Vec<OpenAiTool> {
    tools
        .iter()
        .map(|tool| OpenAiTool {
            kind: "function".to_string(),
            function: OpenAiFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.schema.clone().unwrap_or_else(|| {
                    json_object([("type", Value::String("object".to_string()))])
                }),
            },
        })
        .collect()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Completed,
    Partial,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PermissionMode {
    #[serde(rename = "READONLY")]
    Readonly,
    #[serde(rename = "FULL")]
    Full,
    #[serde(rename = "PLAN_ONLY")]
    PlanOnly,
    #[serde(rename = "NONE")]
    None,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct RunLimits {
    pub max_steps: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub max_cost: Option<f64>,
    pub timeout_seconds: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FanoutBudget {
    pub max_concurrent: u64,
    pub max_total_workers: u64,
    pub max_total_tokens: Option<u64>,
    pub max_total_cost: Option<f64>,
}

impl Default for FanoutBudget {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            max_total_workers: 8,
            max_total_tokens: None,
            max_total_cost: None,
        }
    }
}

impl FanoutBudget {
    #[must_use]
    pub fn normalized(&self) -> Self {
        Self {
            max_concurrent: self.max_concurrent.max(1),
            max_total_workers: self.max_total_workers.max(1),
            max_total_tokens: self.max_total_tokens,
            max_total_cost: self.max_total_cost,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SwarmUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
    pub steps: u64,
    pub latency_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ArtifactRef {
    pub kind: String,
    pub uri: String,
    pub title: String,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AgentSpec {
    pub role: String,
    pub objective: String,
    pub context: String,
    pub boundaries: String,
    pub output_schema: Value,
    pub inputs: BTreeMap<String, Value>,
    pub limits: RunLimits,
    pub permissions: PermissionMode,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AgentResult {
    pub status: RunStatus,
    pub summary: String,
    pub evidence: Vec<String>,
    pub open_questions: Vec<String>,
    pub confidence: f64,
    pub artifacts: Vec<ArtifactRef>,
    pub usage: SwarmUsage,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AgentDescriptor {
    pub id: String,
    pub roles: Vec<String>,
    pub tool_groups: Vec<String>,
    pub model_tier: String,
    pub max_context: u64,
    pub supports_streaming: bool,
    pub kind: String,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunContext {
    pub run_id: String,
    pub parent_span_id: Option<String>,
    pub budget: FanoutBudget,
    pub cancellation: Option<Value>,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolConcurrency {
    Safe,
    Exclusive,
    Keyed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolExecutionSchema {
    pub read_only: bool,
    pub mutates_workspace: bool,
    pub mutates_session: bool,
    pub mutates_external: bool,
    pub external_io: bool,
    pub requires_user_interaction: bool,
    pub concurrency: ToolConcurrency,
    pub batch_group: String,
    pub conflict_key_template: Option<String>,
    pub max_parallelism: Option<u64>,
}

impl ToolExecutionSchema {
    #[must_use]
    pub fn readonly(batch_group: impl Into<String>, max_parallelism: Option<u64>) -> Self {
        Self {
            read_only: true,
            mutates_workspace: false,
            mutates_session: false,
            mutates_external: false,
            external_io: false,
            requires_user_interaction: false,
            concurrency: ToolConcurrency::Safe,
            batch_group: batch_group.into(),
            conflict_key_template: None,
            max_parallelism,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionScope {
    Workspace,
    Agnostic,
    HostOnly,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinitionSchemaFixture {
    pub schema_version: u64,
    pub tool_id: String,
    pub description: String,
    pub group: String,
    pub execution_scope: ToolExecutionScope,
    pub execution_schema: ToolExecutionSchema,
    pub parameters_schema: Value,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct WorkState {
    pub task: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    pub progress: Vec<String>,
    pub decisions: Vec<String>,
    pub files: Vec<WorkStateFile>,
    pub tool_findings: Vec<String>,
    pub todos: Vec<String>,
    pub open_questions: Vec<String>,
    pub blockers: Vec<String>,
    pub next_steps: Vec<String>,
    pub risks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critical_context: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct WorkStateFile {
    pub path: String,
    pub status: String,
    pub note: String,
}

#[must_use]
pub fn render_work_state(state: &WorkState) -> String {
    let mut sections = vec![
        "[Structured work state]".to_string(),
        "Task:".to_string(),
        if state.task.is_empty() {
            "(unspecified)".to_string()
        } else {
            state.task.clone()
        },
    ];

    append_text_section(&mut sections, "Constraints", &state.constraints);
    append_text_section(&mut sections, "Progress", &state.progress);
    append_text_section(&mut sections, "Decisions", &state.decisions);
    append_files_section(&mut sections, &state.files);
    append_text_section(&mut sections, "Tool findings", &state.tool_findings);
    append_text_section(&mut sections, "Todos", &state.todos);
    append_text_section(&mut sections, "Open questions", &state.open_questions);
    append_text_section(&mut sections, "Blockers", &state.blockers);
    append_text_section(&mut sections, "Next steps", &state.next_steps);
    append_text_section(&mut sections, "Risks", &state.risks);
    append_text_section(&mut sections, "Critical context", &state.critical_context);

    sections.join("\n").trim().to_string()
}

fn readonly_tools() -> impl Iterator<Item = &'static str> {
    [
        "read",
        "glob",
        "grep",
        "ls",
        "lsp",
        "skill",
        "web_fetch",
        "todoread",
        "question",
    ]
    .into_iter()
}

fn plan_only_tools() -> impl Iterator<Item = &'static str> {
    [
        "read",
        "glob",
        "grep",
        "ls",
        "lsp",
        "skill",
        "web_fetch",
        "todoread",
        "todowrite",
        "question",
    ]
    .into_iter()
}

fn permission_rule(tool: &str, action: PermissionAction) -> PermissionRule {
    PermissionRule {
        tool: tool.to_string(),
        action,
        pattern: Some("*".to_string()),
        condition: None,
    }
}

fn provider_options(options: Option<&BTreeMap<String, Value>>) -> BTreeMap<String, Value> {
    let runtime_keys = RUNTIME_OPTION_KEYS.iter().copied().collect::<BTreeSet<_>>();
    options
        .into_iter()
        .flat_map(BTreeMap::iter)
        .filter(|(key, _value)| !runtime_keys.contains(key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn append_text_section(lines: &mut Vec<String>, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    lines.push(String::new());
    lines.push(format!("{title}:"));
    lines.extend(items.iter().map(|item| format!("- {item}")));
}

fn append_files_section(lines: &mut Vec<String>, files: &[WorkStateFile]) {
    if files.is_empty() {
        return;
    }
    lines.push(String::new());
    lines.push("Files:".to_string());
    lines.extend(
        files
            .iter()
            .map(|file| format!("- {} ({}) - {}", file.path, file.status, file.note)),
    );
}

fn json_object(items: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(Map::from_iter(
        items
            .into_iter()
            .map(|(key, value)| (key.to_string(), value)),
    ))
}

fn stable_json_dumps(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => {
            if *value {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).expect("string serializes"),
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(stable_json_dumps)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        Value::Object(items) => {
            let inner = items
                .iter()
                .map(|(key, value)| {
                    let key = serde_json::to_string(key).expect("key serializes");
                    let value = stable_json_dumps(value);
                    format!("{key}: {value}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn exposes_crate_name() {
        assert_eq!(crate_name(), "openagent-protocol");
    }

    #[test]
    fn tool_call_key_matches_stable_json_format() {
        let call = ToolCall {
            name: "read".to_string(),
            input: json!({"path": "README.md"}),
            call_id: "call_fixture_read".to_string(),
        };
        assert_eq!(call.key(), "read:{\"path\": \"README.md\"}");
    }
}

//! Core permission, context, instruction, and skill behavior for the Rust rewrite.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use openagent_protocol::{
    ChatMessage, MaterializedPayload, Model, PermissionAction, PermissionRule, PermissionRuleset,
    Role, ToolSchema, Usage, WorkState, WorkStateFile, materialize_openai_compatible_payload,
    render_work_state, ruleset,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha1::{Digest, Sha1};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

pub const DEFAULT_BYTES_PER_TOKEN: u64 = 3;
pub const DEFAULT_GUARD_RATIO: f64 = 0.9;
pub const DEFAULT_INPUT_SAFETY_MARGIN_TOKENS: u64 = 1024;
pub const DEFAULT_TOOL_DISPLAY_MAX_BYTES: u64 = 50 * 1024;
pub const DEFAULT_TOOL_CONTEXT_PREVIEW_BYTES: u64 = 4096;
pub const DEFAULT_TOOL_CONTEXT_PREVIEW_LINES: u64 = 40;
pub const DEFAULT_TOOL_CONTEXT_LINE_MAX_CHARS: u64 = 240;
pub const DEFAULT_PRUNE_OLD_TOOL_OUTPUTS: bool = true;
pub const DEFAULT_PRUNE_KEEP_RECENT_USER_TURNS: u64 = 2;
pub const DEFAULT_PRUNE_PROTECT_INPUT_TOKENS: u64 = 12_000;
pub const DEFAULT_PRUNE_MIN_INPUT_TOKENS: u64 = 4_000;
pub const DEFAULT_COMPACT_SUMMARY_MAX_OUTPUT_TOKENS: u64 = 512;
pub const DEFAULT_COMPACT_REFRESH_MIN_NEW_MESSAGES: u64 = 6;
pub const DEFAULT_OVERFLOW_KEEP_RECENT_USER_TURNS: u64 = 2;
pub const DEFAULT_COMPACTION_MODE: &str = "structured_work_state";

pub const CONTEXT_PACK_SCHEMA_VERSION: &str = "openagent.context_pack.v1";
pub const CONTEXT_PACK_RECEIPT_SCHEMA_VERSION: &str = "openagent.context_pack_receipt.v1";
pub const CONTEXT_STABLE_PREFIX_SCHEMA_VERSION: &str = "openagent.context_stable_prefix.v1";
pub const CONTEXT_FAILURE_SCHEMA_VERSION: &str = "openagent.context_failure.v1";
pub const CONTEXT_PERFORMANCE_SCHEMA_VERSION: &str = "openagent.context_performance.v1";
pub const CONTEXT_SYSTEM_DIAGNOSTICS_SCHEMA_VERSION: &str =
    "openagent.context_system_diagnostics.v1";
pub const CONTEXT_BUILD_WARN_US: u64 = 250_000;
pub const CONTEXT_PROVIDER_PAYLOAD_SERIALIZE_WARN_US: u64 = 100_000;
pub const CONTEXT_PROVIDER_PAYLOAD_WARN_BYTES: u64 = 16 * 1024 * 1024;
pub const CONTEXT_PRIORITY_INSTRUCTION: i64 = 100;
pub const CONTEXT_PRIORITY_SKILL_PRELOADED: i64 = 98;
pub const CONTEXT_PRIORITY_GOAL: i64 = 97;
pub const CONTEXT_PRIORITY_PLAN: i64 = 96;
pub const CONTEXT_PRIORITY_WORK_STATE: i64 = 95;
pub const CONTEXT_PRIORITY_RUNTIME: i64 = 90;
pub const CONTEXT_PRIORITY_SANDBOX: i64 = 85;
pub const CONTEXT_PRIORITY_TODO: i64 = 80;
pub const CONTEXT_PRIORITY_ATTACHMENT: i64 = 75;
pub const CONTEXT_PRIORITY_SKILL_CATALOG: i64 = 70;
pub const CONTEXT_PRIORITY_CHECKPOINT: i64 = 65;
pub const CONTEXT_PRIORITY_TOOL_MANIFEST: i64 = 60;
pub const CONTEXT_PRIORITY_TOOL_RESULT: i64 = 50;
pub const CONTEXT_PRIORITY_MESSAGE: i64 = 40;

pub const DEFAULT_MAX_FILE_BYTES: usize = 16 * 1024;
pub const DEFAULT_MAX_TOTAL_BYTES: usize = 48 * 1024;
pub const DEFAULT_WORKSPACE_FILES: &[&str] = &["OPENAGENT.md", "AGENTS.md", "CLAUDE.md"];
pub const DEFAULT_USER_FILES: &[&str] = &["OPENAGENT.md", "instructions.md"];

const SUPPORTED_STRATEGIES: &[&str] = &["auto", "error", "compact"];
const SUPPORTED_COUNTING: &[&str] = &["auto", "tiktoken", "heuristic"];
const SUPPORTED_COMPACTION_MODES: &[&str] = &["structured_work_state"];
const REQUIRED_CONTEXT_HARD_MINIMUM_BYTES: usize = 96;
const REQUIRED_CONTEXT_TRUNCATION_REASON: &str = "required_context_budget";
const REQUIRED_CONTEXT_TRUNCATION_MARKER: &str =
    "\n[... context truncated to fit model budget ...]\n";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextFailureCode {
    Unavailable,
    ReceiptCorrupt,
    BudgetExceeded,
    SourceDrift,
    ReplayUnsupported,
}

impl ContextFailureCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "context_unavailable",
            Self::ReceiptCorrupt => "context_receipt_corrupt",
            Self::BudgetExceeded => "context_budget_exceeded",
            Self::SourceDrift => "context_source_drift",
            Self::ReplayUnsupported => "context_replay_unsupported",
        }
    }

    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::Unavailable | Self::ReceiptCorrupt)
    }

    #[must_use]
    pub const fn recoverable(self) -> bool {
        !matches!(self, Self::ReplayUnsupported)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextFailure {
    pub schema_version: String,
    pub code: String,
    pub stage: String,
    pub message: String,
    pub retryable: bool,
    pub recoverable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

impl ContextFailure {
    #[must_use]
    pub fn new(
        code: ContextFailureCode,
        stage: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: CONTEXT_FAILURE_SCHEMA_VERSION.to_string(),
            code: code.as_str().to_string(),
            stage: stage.into(),
            message: message.into(),
            retryable: code.retryable(),
            recoverable: code.recoverable(),
            details: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: BTreeMap<String, Value>) -> Self {
        self.details = details;
        self
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ContextPackPerformance {
    pub schema_version: String,
    pub materialize_us: u64,
    pub build_us: u64,
    pub persist_us: u64,
    pub provider_payload_build_us: u64,
    pub provider_payload_serialize_us: u64,
    pub provider_payload_bytes: u64,
    pub source_message_count: u64,
    pub tool_count: u64,
    pub item_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warning_codes: Vec<String>,
}

impl ContextPackPerformance {
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema_version: CONTEXT_PERFORMANCE_SCHEMA_VERSION.to_string(),
            ..Self::default()
        }
    }

    pub fn refresh_warnings(&mut self) {
        let mut warnings = Vec::new();
        if self.build_us > CONTEXT_BUILD_WARN_US {
            warnings.push("context_build_slow".to_string());
        }
        if self.provider_payload_serialize_us > CONTEXT_PROVIDER_PAYLOAD_SERIALIZE_WARN_US {
            warnings.push("provider_payload_serialize_slow".to_string());
        }
        if self.provider_payload_bytes > CONTEXT_PROVIDER_PAYLOAD_WARN_BYTES {
            warnings.push("provider_payload_large".to_string());
        }
        self.warning_codes = warnings;
    }

    #[must_use]
    pub fn status(&self) -> &'static str {
        if self.warning_codes.is_empty() {
            "ok"
        } else {
            "warning"
        }
    }
}
#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[must_use]
pub fn protocol_crate_name() -> &'static str {
    openagent_protocol::crate_name()
}

#[derive(Clone, Debug, Default)]
pub struct PermissionManager {
    rules: Vec<PermissionRule>,
}

impl PermissionManager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_ruleset(&mut self, name: PermissionRuleset) {
        self.rules = ruleset(name).rules;
    }

    pub fn add_rule(&mut self, rule: PermissionRule) {
        self.rules.push(rule);
    }

    #[must_use]
    pub fn evaluate(&self, tool: &str, pattern: &str) -> Option<&PermissionRule> {
        let mut matched = None;
        for rule in &self.rules {
            if glob_match(&rule.tool, tool)
                && rule
                    .pattern
                    .as_deref()
                    .is_none_or(|rule_pattern| glob_match(rule_pattern, pattern))
            {
                matched = Some(rule);
            }
        }
        matched
    }

    #[must_use]
    pub fn decide(&self, tool_call: &Value) -> PermissionAction {
        let tool = tool_call
            .get("name")
            .or_else(|| tool_call.get("tool"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let payload = tool_call.get("input").cloned().unwrap_or_else(|| json!({}));
        let pattern = pattern_for(&payload);
        self.evaluate(tool, &pattern)
            .map(|rule| rule.action.clone())
            .unwrap_or(PermissionAction::Ask)
    }

    pub fn check(&self, tool_call: &Value) -> Result<PermissionAction, String> {
        let tool = tool_call
            .get("name")
            .or_else(|| tool_call.get("tool"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        match self.decide(tool_call) {
            PermissionAction::Allow => Ok(PermissionAction::Allow),
            PermissionAction::Deny => Err(format!("Permission denied: {tool}")),
            PermissionAction::Ask => Err(format!("Permission requires user confirmation: {tool}")),
        }
    }
}

#[must_use]
pub fn permission_rule(
    tool: &str,
    action: PermissionAction,
    pattern: Option<&str>,
) -> PermissionRule {
    PermissionRule {
        tool: tool.to_string(),
        action,
        pattern: pattern.map(str::to_string),
        condition: None,
    }
}

#[must_use]
pub fn pattern_for(payload: &Value) -> String {
    if let Some(object) = payload.as_object() {
        for key in [
            "file_path",
            "filePath",
            "path",
            "pattern",
            "command",
            "subagent_type",
            "agent_type",
            "agent",
            "name",
        ] {
            if let Some(value) = object.get(key).and_then(Value::as_str)
                && !value.is_empty()
            {
                return value.to_string();
            }
        }
    }
    stable_json_dumps(payload)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextBudgetOptions {
    pub enabled: bool,
    pub strategy: String,
    pub counting: String,
    pub compaction_mode: String,
    pub reserve_output_tokens: u64,
    pub guard_ratio: f64,
    pub input_safety_margin_tokens: u64,
    pub use_safety_margin_tokens: bool,
    pub explicit_input_safety_margin_tokens: bool,
    pub bytes_per_token: u64,
    pub tool_display_max_bytes: u64,
    pub tool_context_preview_bytes: u64,
    pub tool_context_preview_lines: u64,
    pub tool_context_line_max_chars: u64,
    pub prune_old_tool_outputs: bool,
    pub prune_keep_recent_user_turns: u64,
    pub prune_protect_input_tokens: u64,
    pub prune_min_input_tokens: u64,
    pub compact_summary_max_output_tokens: u64,
    pub compact_refresh_min_new_messages: u64,
    pub overflow_keep_recent_user_turns: u64,
    pub overflow_disable_tools_on_final_attempt: bool,
    pub overflow_final_max_output_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextBudgetResult {
    pub estimated_input_tokens: u64,
    pub input_limit_tokens: u64,
    pub context_window: u64,
    pub reserved_output_tokens: u64,
    pub overflowed: bool,
    pub tool_message_count: u64,
    pub largest_tool_message_tokens: u64,
    pub largest_tool_message_name: String,
    pub counting_method: String,
    pub counting_exact: bool,
    pub fallback_stage: String,
    pub payload_kind: String,
}

#[must_use]
pub fn format_context_budget_error(result: &ContextBudgetResult) -> String {
    let mut message = format!(
        "Context budget exceeded before model call: estimated_input_tokens={}, input_limit_tokens={}, context_window={}, reserved_output_tokens={}, counting_method={}, counting_exact={}, payload_kind={}, fallback_stage={}",
        result.estimated_input_tokens,
        result.input_limit_tokens,
        result.context_window,
        result.reserved_output_tokens,
        result.counting_method,
        result.counting_exact,
        result.payload_kind,
        result.fallback_stage
    );
    if result.tool_message_count > 0 {
        message.push_str(&format!(
            ", tool_message_count={}, largest_tool_message_tokens={}, largest_tool_message_name={}",
            result.tool_message_count,
            result.largest_tool_message_tokens,
            if result.largest_tool_message_name.is_empty() {
                "unknown"
            } else {
                &result.largest_tool_message_name
            }
        ));
    }
    message
}

pub fn load_context_budget_options(
    options: Option<&Value>,
    model: Option<&Model>,
) -> Result<ContextBudgetOptions, String> {
    let merged = merge_compaction_facade_options(options)?;
    let enabled = expect_bool(
        merged.get("enabled").unwrap_or(&Value::Bool(true)),
        "enabled",
        "context_budget",
    )?;
    let strategy = expect_non_empty_string(
        merged
            .get("strategy")
            .unwrap_or(&Value::String("auto".to_string())),
        "context_budget.strategy",
    )?;
    let counting = expect_non_empty_string(
        merged
            .get("counting")
            .unwrap_or(&Value::String("auto".to_string())),
        "context_budget.counting",
    )?;
    let compaction_mode = expect_non_empty_string(
        merged
            .get("compaction_mode")
            .unwrap_or(&Value::String(DEFAULT_COMPACTION_MODE.to_string())),
        "context_budget.compaction_mode",
    )?;
    if !SUPPORTED_COMPACTION_MODES.contains(&compaction_mode.as_str()) {
        return Err(format!(
            "Unsupported context_budget.compaction_mode: {compaction_mode}. Supported modes: structured_work_state."
        ));
    }

    let model_max_output = model.map(|item| item.max_output).unwrap_or(0);
    let reserve_output_tokens = expect_int(
        merged
            .get("reserve_output_tokens")
            .unwrap_or(&json!(model_max_output)),
        "reserve_output_tokens",
        0,
        "context_budget",
    )?;
    let guard_ratio = expect_float(
        merged
            .get("guard_ratio")
            .unwrap_or(&json!(DEFAULT_GUARD_RATIO)),
        "guard_ratio",
        0.0,
        1.0,
        false,
    )?;
    let explicit_input_safety_margin_tokens = merged.contains_key("input_safety_margin_tokens");
    let use_safety_margin_tokens =
        explicit_input_safety_margin_tokens || !merged.contains_key("guard_ratio");
    let safety_margin_default = if use_safety_margin_tokens {
        DEFAULT_INPUT_SAFETY_MARGIN_TOKENS
    } else {
        0
    };
    let input_safety_margin_tokens = expect_int(
        merged
            .get("input_safety_margin_tokens")
            .unwrap_or(&json!(safety_margin_default)),
        "input_safety_margin_tokens",
        0,
        "context_budget",
    )?;
    let bytes_per_token = expect_int(
        merged
            .get("bytes_per_token")
            .unwrap_or(&json!(DEFAULT_BYTES_PER_TOKEN)),
        "bytes_per_token",
        1,
        "context_budget",
    )?;
    let tool_display_max_bytes = expect_int(
        merged
            .get("tool_display_max_bytes")
            .unwrap_or(&json!(DEFAULT_TOOL_DISPLAY_MAX_BYTES)),
        "tool_display_max_bytes",
        1,
        "context_budget",
    )?;
    let tool_context_preview_bytes = expect_int(
        merged
            .get("tool_context_preview_bytes")
            .unwrap_or(&json!(DEFAULT_TOOL_CONTEXT_PREVIEW_BYTES)),
        "tool_context_preview_bytes",
        1,
        "context_budget",
    )?;
    let tool_context_preview_lines = expect_int(
        merged
            .get("tool_context_preview_lines")
            .unwrap_or(&json!(DEFAULT_TOOL_CONTEXT_PREVIEW_LINES)),
        "tool_context_preview_lines",
        1,
        "context_budget",
    )?;
    let tool_context_line_max_chars = expect_int(
        merged
            .get("tool_context_line_max_chars")
            .unwrap_or(&json!(DEFAULT_TOOL_CONTEXT_LINE_MAX_CHARS)),
        "tool_context_line_max_chars",
        1,
        "context_budget",
    )?;
    let prune_old_tool_outputs = expect_bool(
        merged
            .get("prune_old_tool_outputs")
            .unwrap_or(&Value::Bool(DEFAULT_PRUNE_OLD_TOOL_OUTPUTS)),
        "prune_old_tool_outputs",
        "context_budget",
    )?;
    let prune_keep_recent_user_turns = expect_int(
        merged
            .get("prune_keep_recent_user_turns")
            .unwrap_or(&json!(DEFAULT_PRUNE_KEEP_RECENT_USER_TURNS)),
        "prune_keep_recent_user_turns",
        1,
        "context_budget",
    )?;
    let prune_protect_input_tokens = expect_int(
        merged
            .get("prune_protect_input_tokens")
            .unwrap_or(&json!(DEFAULT_PRUNE_PROTECT_INPUT_TOKENS)),
        "prune_protect_input_tokens",
        0,
        "context_budget",
    )?;
    let prune_min_input_tokens = expect_int(
        merged
            .get("prune_min_input_tokens")
            .unwrap_or(&json!(DEFAULT_PRUNE_MIN_INPUT_TOKENS)),
        "prune_min_input_tokens",
        0,
        "context_budget",
    )?;
    let compact_summary_max_output_tokens = expect_int(
        merged
            .get("compact_summary_max_output_tokens")
            .unwrap_or(&json!(DEFAULT_COMPACT_SUMMARY_MAX_OUTPUT_TOKENS)),
        "compact_summary_max_output_tokens",
        1,
        "context_budget",
    )?;
    let compact_refresh_min_new_messages = expect_int(
        merged
            .get("compact_refresh_min_new_messages")
            .unwrap_or(&json!(DEFAULT_COMPACT_REFRESH_MIN_NEW_MESSAGES)),
        "compact_refresh_min_new_messages",
        1,
        "context_budget",
    )?;
    let overflow_keep_recent_user_turns = expect_int(
        merged
            .get("overflow_keep_recent_user_turns")
            .unwrap_or(&json!(DEFAULT_OVERFLOW_KEEP_RECENT_USER_TURNS)),
        "overflow_keep_recent_user_turns",
        1,
        "context_budget",
    )?;
    let overflow_disable_tools_on_final_attempt = expect_bool(
        merged
            .get("overflow_disable_tools_on_final_attempt")
            .unwrap_or(&Value::Bool(true)),
        "overflow_disable_tools_on_final_attempt",
        "context_budget",
    )?;
    let overflow_final_max_output_default =
        model.map(|item| item.max_output.min(512)).unwrap_or(512);
    let overflow_final_max_output_tokens = expect_int(
        merged
            .get("overflow_final_max_output_tokens")
            .unwrap_or(&json!(overflow_final_max_output_default)),
        "overflow_final_max_output_tokens",
        1,
        "context_budget",
    )?;

    Ok(ContextBudgetOptions {
        enabled,
        strategy,
        counting,
        compaction_mode,
        reserve_output_tokens,
        guard_ratio,
        input_safety_margin_tokens,
        use_safety_margin_tokens,
        explicit_input_safety_margin_tokens,
        bytes_per_token,
        tool_display_max_bytes,
        tool_context_preview_bytes,
        tool_context_preview_lines,
        tool_context_line_max_chars,
        prune_old_tool_outputs,
        prune_keep_recent_user_turns,
        prune_protect_input_tokens,
        prune_min_input_tokens,
        compact_summary_max_output_tokens,
        compact_refresh_min_new_messages,
        overflow_keep_recent_user_turns,
        overflow_disable_tools_on_final_attempt,
        overflow_final_max_output_tokens,
    })
}

pub fn context_pack_build_options_for_model(
    options: Option<&Value>,
    model: &Model,
    trace_only: bool,
) -> Result<ContextPackBuildOptions, String> {
    let budget = load_context_budget_options(options, Some(model))?;
    Ok(ContextPackBuildOptions {
        token_budget: budget
            .enabled
            .then(|| compute_input_limit_tokens(model, &budget)),
        bytes_per_token: budget.bytes_per_token,
        tool_context_preview_bytes: budget.tool_context_preview_bytes,
        tool_context_preview_lines: budget.tool_context_preview_lines,
        tool_context_line_max_chars: budget.tool_context_line_max_chars,
        trace_only,
        model_id: Some(model.id.clone()),
        context_window: Some(model.context_window),
        reserved_output_tokens: budget.reserve_output_tokens,
        fit_required_context: budget.strategy != "error",
    })
}

pub fn check_context_budget(
    system: Option<&str>,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    model: Option<&Model>,
    options: Option<&Value>,
    fallback_stage: &str,
) -> Result<Option<ContextBudgetResult>, String> {
    let Some(model) = model else {
        return Ok(None);
    };
    if model.context_window == 0 {
        return Ok(None);
    }
    let config = load_context_budget_options(options, Some(model))?;
    if !config.enabled {
        return Ok(None);
    }
    if !SUPPORTED_STRATEGIES.contains(&config.strategy.as_str()) {
        return Err(format!(
            "Unsupported context budget strategy: {}. Supported strategies: auto, error, compact.",
            config.strategy
        ));
    }
    if !SUPPORTED_COUNTING.contains(&config.counting.as_str()) {
        return Err(format!(
            "Unsupported context budget counting mode: {}. Supported modes: auto, tiktoken, heuristic.",
            config.counting
        ));
    }

    let provider_options = options_to_btree(options);
    let payload = materialize_openai_compatible_payload(
        system,
        messages,
        tools,
        Some(model),
        Some(&provider_options),
    );
    let payload_kind = if is_openai_compatible_model(model) {
        "openai_compatible"
    } else {
        "generic"
    };
    let count = estimate_payload_tokens(&payload, config.bytes_per_token);
    let diagnostics = tool_message_diagnostics(messages, model, options, config.bytes_per_token);
    let input_limit_tokens = compute_input_limit_tokens(model, &config);
    Ok(Some(ContextBudgetResult {
        estimated_input_tokens: count,
        input_limit_tokens,
        context_window: model.context_window,
        reserved_output_tokens: config.reserve_output_tokens,
        overflowed: count > input_limit_tokens,
        tool_message_count: diagnostics.tool_message_count,
        largest_tool_message_tokens: diagnostics.largest_tool_message_tokens,
        largest_tool_message_name: diagnostics.largest_tool_message_name,
        counting_method: "heuristic".to_string(),
        counting_exact: false,
        fallback_stage: fallback_stage.to_string(),
        payload_kind: payload_kind.to_string(),
    }))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDelivery {
    #[default]
    Message,
    ToolManifest,
    TraceOnly,
}

fn context_delivery_is_message(delivery: &ContextDelivery) -> bool {
    *delivery == ContextDelivery::Message
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAttachmentKind {
    Text,
    #[default]
    File,
    Image,
    Pdf,
    Document,
    Folder,
}

impl ContextAttachmentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::File => "file",
            Self::Image => "image",
            Self::Pdf => "pdf",
            Self::Document => "document",
            Self::Folder => "folder",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "text" => Some(Self::Text),
            "file" => Some(Self::File),
            "image" => Some(Self::Image),
            "pdf" => Some(Self::Pdf),
            "document" | "doc" => Some(Self::Document),
            "folder" | "directory" => Some(Self::Folder),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextAttachment {
    pub id: String,
    pub kind: ContextAttachmentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub content_type: String,
    pub size_bytes: u64,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub media_metadata: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_content_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included_content_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_message_index: Option<usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

impl ContextAttachment {
    #[must_use]
    pub fn new(
        kind: ContextAttachmentKind,
        path: Option<String>,
        name: Option<String>,
        content_type: impl Into<String>,
        size_bytes: u64,
        content: impl Into<String>,
    ) -> Self {
        let content = content.into();
        let included_content_bytes = content.len() as u64;
        let mut attachment = Self {
            id: String::new(),
            kind,
            path,
            name,
            content_type: content_type.into(),
            size_bytes,
            content,
            source: None,
            page_count: None,
            media_metadata: BTreeMap::new(),
            truncated: false,
            truncation_reason: None,
            original_content_bytes: None,
            included_content_bytes: Some(included_content_bytes),
            source_message_index: None,
            metadata: BTreeMap::new(),
        };
        attachment.id = stable_context_attachment_id(&attachment);
        attachment
    }

    #[must_use]
    pub fn with_source_message_index(mut self, index: usize) -> Self {
        self.source_message_index = Some(index);
        self
    }

    #[must_use]
    pub fn stable_id(&self) -> String {
        stable_context_attachment_id(self)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextTodo {
    pub id: String,
    pub content: String,
    pub status: String,
    pub priority: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

impl ContextTodo {
    #[must_use]
    pub fn new(
        id: Option<String>,
        content: impl Into<String>,
        status: impl Into<String>,
        priority: impl Into<String>,
    ) -> Self {
        let content = content.into();
        let status = status.into();
        let priority = priority.into();
        let id = id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "todo_{}",
                    sha1_hex(&content).chars().take(16).collect::<String>()
                )
            });
        Self {
            id,
            content,
            status,
            priority,
            metadata: BTreeMap::new(),
        }
    }
}

pub const DURABLE_GOAL_SCHEMA_VERSION: &str = "openagent.durable_goal.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableGoalStatus {
    Active,
    Paused,
    Completed,
}

impl DurableGoalStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DurableGoal {
    pub schema_version: String,
    pub id: String,
    pub title: String,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    pub status: DurableGoalStatus,
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
}

impl DurableGoal {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        objective: impl Into<String>,
        acceptance_criteria: Vec<String>,
        now_ms: u64,
    ) -> Self {
        Self {
            schema_version: DURABLE_GOAL_SCHEMA_VERSION.to_string(),
            id: id.into(),
            title: title.into(),
            objective: objective.into(),
            acceptance_criteria,
            status: DurableGoalStatus::Active,
            revision: 1,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            completed_at_ms: None,
        }
    }
}

pub const DURABLE_PLAN_SCHEMA_VERSION: &str = "openagent.durable_plan.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DurablePlanStatus {
    Planning,
    Executing,
    Completed,
}

impl DurablePlanStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Executing => "executing",
            Self::Completed => "completed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DurablePlan {
    pub schema_version: String,
    pub id: String,
    pub title: String,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    pub status: DurablePlanStatus,
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
}

impl DurablePlan {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        objective: impl Into<String>,
        steps: Vec<String>,
        now_ms: u64,
    ) -> Self {
        Self {
            schema_version: DURABLE_PLAN_SCHEMA_VERSION.to_string(),
            id: id.into(),
            title: title.into(),
            objective: objective.into(),
            steps,
            status: DurablePlanStatus::Planning,
            revision: 1,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            execution_started_at_ms: None,
            completed_at_ms: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextCheckpoint {
    pub id: String,
    pub kind: String,
    pub run_id: String,
    pub timestamp_ms: u64,
    pub message_id: Option<String>,
    pub part_id: Option<String>,
    pub step_index: Option<u64>,
    pub file_count: u64,
    pub total_bytes: u64,
    pub restored: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextWorkState {
    pub id: String,
    pub summary: String,
    pub format: String,
    pub source: String,
    pub message_position: Option<usize>,
    pub compacted_until_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextItem {
    pub id: String,
    pub kind: String,
    pub source: String,
    pub content: String,
    pub priority: i64,
    pub token_estimate: u64,
    pub pinned: bool,
    pub stable_prefix: bool,
    pub ttl_turns: Option<u64>,
    pub metadata: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "context_delivery_is_message")]
    pub delivery: ContextDelivery,
}

impl ContextItem {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        source: impl Into<String>,
        content: impl Into<String>,
        priority: i64,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            source: source.into(),
            content: content.into(),
            priority,
            token_estimate: 0,
            pinned: false,
            stable_prefix: false,
            ttl_turns: None,
            metadata: BTreeMap::new(),
            delivery: ContextDelivery::Message,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextPackTraceEntry {
    pub item_id: String,
    pub kind: String,
    pub source: String,
    pub priority: i64,
    pub pinned: bool,
    pub stable_prefix: bool,
    pub token_estimate: u64,
    pub included: bool,
    pub drop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "context_delivery_is_message")]
    pub delivery: ContextDelivery,
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_token_estimate: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_duplicate_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<ContextAttachmentTrace>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextAttachmentTrace {
    pub id: String,
    pub kind: ContextAttachmentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub content_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub media_metadata: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub source_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_truncation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_content_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included_content_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ContextStablePrefix {
    pub schema_version: String,
    pub hash: String,
    pub item_count: u64,
    pub message_count: u64,
    pub tool_manifest_count: u64,
    pub token_estimate: u64,
    pub cache_eligible: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextPackReceipt {
    pub schema_version: String,
    pub pack_schema_version: String,
    pub pack_hash: String,
    pub provider_input_hash: String,
    pub message_count: u64,
    pub message_role_counts: BTreeMap<String, u64>,
    pub tool_manifest_count: u64,
    pub tool_names: Vec<String>,
    pub model_option_keys: Vec<String>,
    pub item_count: u64,
    pub included_item_count: u64,
    pub dropped_item_count: u64,
    pub item_kind_counts: BTreeMap<String, u64>,
    pub item_delivery_counts: BTreeMap<String, u64>,
    pub drop_reason_counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "u64_is_zero")]
    pub truncated_item_count: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub truncation_reason_counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub truncation_strategy_counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "u64_is_zero")]
    pub semantic_duplicate_count: u64,
    #[serde(default)]
    pub stable_prefix: ContextStablePrefix,
    pub estimated_input_tokens: u64,
    pub budget: ContextPackBudget,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ContextPackBudget {
    pub enabled: bool,
    pub model_id: Option<String>,
    pub context_window: Option<u64>,
    pub reserved_output_tokens: u64,
    pub input_limit_tokens: Option<u64>,
    pub fixed_overhead_tokens: u64,
    pub item_budget_tokens: Option<u64>,
    pub selected_item_tokens: u64,
    pub overflowed: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextPack {
    pub schema_version: String,
    pub pack_hash: String,
    pub provider_input_hash: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSchema>,
    pub model_options: BTreeMap<String, Value>,
    pub items: Vec<ContextItem>,
    pub trace: Vec<ContextPackTraceEntry>,
    #[serde(default)]
    pub stable_prefix: ContextStablePrefix,
    #[serde(default)]
    pub system_diagnostics: Option<ContextSystemDiagnostics>,
    pub estimated_input_tokens: u64,
    pub budget: ContextPackBudget,
    pub receipt: ContextPackReceipt,
}

impl ContextPack {
    pub fn validate_provider_input(&self) -> Result<(), String> {
        if self.schema_version != CONTEXT_PACK_SCHEMA_VERSION {
            return Err(format!(
                "unsupported context pack schema: {}",
                self.schema_version
            ));
        }
        if self.receipt.schema_version != CONTEXT_PACK_RECEIPT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported context receipt schema: {}",
                self.receipt.schema_version
            ));
        }
        let provider_input_hash =
            context_provider_input_hash(&self.messages, &self.tools, &self.model_options);
        if self.provider_input_hash != provider_input_hash
            || self.receipt.provider_input_hash != provider_input_hash
        {
            return Err("context pack provider input hash mismatch".to_string());
        }
        let pack_hash = context_pack_hash(
            &self.messages,
            &self.tools,
            &self.model_options,
            &self.items,
            &self.trace,
        );
        if self.pack_hash != pack_hash || self.receipt.pack_hash != pack_hash {
            return Err("context pack integrity hash mismatch".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextPackBuildOptions {
    pub token_budget: Option<u64>,
    pub bytes_per_token: u64,
    /// Maximum bytes from an individual tool result that are sent back to a
    /// provider. The complete result remains in the session transcript.
    pub tool_context_preview_bytes: u64,
    /// Maximum number of lines from an individual tool result included in the
    /// provider context.
    pub tool_context_preview_lines: u64,
    /// Maximum characters retained for each included tool-result line.
    pub tool_context_line_max_chars: u64,
    pub trace_only: bool,
    pub model_id: Option<String>,
    pub context_window: Option<u64>,
    pub reserved_output_tokens: u64,
    pub fit_required_context: bool,
}

impl Default for ContextPackBuildOptions {
    fn default() -> Self {
        Self {
            token_budget: None,
            bytes_per_token: DEFAULT_BYTES_PER_TOKEN,
            tool_context_preview_bytes: DEFAULT_TOOL_CONTEXT_PREVIEW_BYTES,
            tool_context_preview_lines: DEFAULT_TOOL_CONTEXT_PREVIEW_LINES,
            tool_context_line_max_chars: DEFAULT_TOOL_CONTEXT_LINE_MAX_CHARS,
            trace_only: true,
            model_id: None,
            context_window: None,
            reserved_output_tokens: 0,
            fit_required_context: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ContextPackBuilder {
    pub options: ContextPackBuildOptions,
}

impl ContextPackBuilder {
    #[must_use]
    pub fn new(options: Option<ContextPackBuildOptions>) -> Self {
        Self {
            options: options.unwrap_or_default(),
        }
    }

    #[must_use]
    pub fn build(&self, input: ContextPackInput) -> ContextPack {
        let input = normalize_context_pack_input(input);
        let system = input
            .system_sources
            .as_ref()
            .and_then(materialize_context_system_sources);
        let mut items = self.collect_items_with_system(&input, system.as_ref());
        items = self.semantic_dedupe_items(self.dedupe_items(items));
        items = self.with_estimates(items);
        // Tool output is persisted in full for inspection and replay, but it
        // must not be allowed to consume an entire model window on the next
        // agent step. This projection is intentionally applied only to the
        // provider-facing pack (not trace-only diagnostics).
        if !self.options.trace_only {
            items = self.limit_tool_result_context(items);
        }
        let fixed_overhead_tokens = estimate_context_pack_fixed_overhead(
            &input.tools,
            &input.model_options,
            self.options.bytes_per_token,
        );
        let item_budget_tokens = self
            .options
            .token_budget
            .map(|budget| budget.saturating_sub(fixed_overhead_tokens));
        if self.options.fit_required_context
            && let Some(item_budget_tokens) = item_budget_tokens
        {
            items = self.fit_required_items(items, item_budget_tokens);
        }
        let trace = self.project(&items, item_budget_tokens);
        let included_ids = trace
            .iter()
            .filter(|entry| entry.included)
            .map(|entry| entry.item_id.clone())
            .collect::<BTreeSet<_>>();
        let selected_item_tokens = items
            .iter()
            .filter(|item| {
                included_ids.contains(&item.id) && item.delivery == ContextDelivery::Message
            })
            .map(|item| item.token_estimate)
            .sum();
        let estimated_input_tokens = fixed_overhead_tokens.saturating_add(selected_item_tokens);
        let overflowed = trace.iter().any(|entry| {
            !entry.included && entry.drop_reason.as_deref() == Some("required_budget_exhausted")
        }) || self
            .options
            .token_budget
            .is_some_and(|budget| fixed_overhead_tokens > budget);
        let budget = ContextPackBudget {
            enabled: self.options.token_budget.is_some(),
            model_id: self.options.model_id.clone(),
            context_window: self.options.context_window,
            reserved_output_tokens: self.options.reserved_output_tokens,
            input_limit_tokens: self.options.token_budget,
            fixed_overhead_tokens,
            item_budget_tokens,
            selected_item_tokens,
            overflowed,
        };
        let messages = if self.options.trace_only {
            let mut messages = input.messages;
            if let Some(system) = system.as_ref() {
                messages.insert(0, item_to_message(&system.assembled));
            }
            messages
        } else {
            let selected = items.iter().filter(|item| {
                included_ids.contains(&item.id) && item.delivery == ContextDelivery::Message
            });
            selected
                .clone()
                .filter(|item| item.stable_prefix)
                .chain(selected.filter(|item| !item.stable_prefix))
                .map(item_to_message)
                .collect()
        };
        let tools = input.tools;
        let model_options = input.model_options;
        let stable_prefix = context_stable_prefix(
            &items,
            &trace,
            &tools,
            &model_options,
            self.options.model_id.as_deref(),
            fixed_overhead_tokens,
            overflowed,
        );
        let provider_input_hash = context_provider_input_hash(&messages, &tools, &model_options);
        let pack_hash = context_pack_hash(&messages, &tools, &model_options, &items, &trace);
        let receipt = context_pack_receipt(
            &pack_hash,
            &provider_input_hash,
            &messages,
            &tools,
            &model_options,
            &items,
            &trace,
            &stable_prefix,
            estimated_input_tokens,
            &budget,
        );
        ContextPack {
            schema_version: CONTEXT_PACK_SCHEMA_VERSION.to_string(),
            pack_hash,
            provider_input_hash,
            messages,
            tools,
            model_options,
            items,
            trace,
            stable_prefix,
            system_diagnostics: system.map(|system| system.diagnostics),
            estimated_input_tokens,
            budget,
            receipt,
        }
    }

    #[must_use]
    pub fn collect_items(&self, input: &ContextPackInput) -> Vec<ContextItem> {
        let input = normalize_context_pack_input(input.clone());
        let system = input
            .system_sources
            .as_ref()
            .and_then(materialize_context_system_sources);
        self.collect_items_with_system(&input, system.as_ref())
    }

    fn collect_items_with_system(
        &self,
        input: &ContextPackInput,
        system: Option<&ContextSystemMaterialization>,
    ) -> Vec<ContextItem> {
        let mut items = Vec::new();
        if let Some(system) = system {
            items.extend(system.sources.clone());
            items.push(system.assembled.clone());
        }
        if let Some(runtime_context) = input.runtime_context.as_ref().map(|item| item.trim())
            && !runtime_context.is_empty()
        {
            let mut item = ContextItem::new(
                "runtime:current",
                "runtime",
                "runtime",
                runtime_context,
                CONTEXT_PRIORITY_RUNTIME,
            );
            item.pinned = true;
            item.metadata
                .insert("synthetic".to_string(), Value::Bool(true));
            items.push(item);
        }
        let empty_execution = Value::Object(Map::new());
        let execution = input
            .sandbox_metadata
            .as_ref()
            .or_else(|| input.metadata.get("execution"))
            .unwrap_or(&empty_execution);
        if let Some(sandbox) = sandbox_item(execution) {
            items.push(sandbox);
        }
        let todos = todo_items(&input.todos);
        let goal = durable_goal_item(input.metadata.get("durable_goal"));
        let plan = durable_plan_item(input.metadata.get("durable_plan"));
        let checkpoints = checkpoint_items(&input.checkpoints);
        let work_state = work_state_item(
            input.work_state.as_ref(),
            &input.metadata,
            input.messages.len(),
        );
        items.extend(message_and_session_context_items(
            &input.messages,
            &input.attachments,
            work_state,
            goal,
            plan,
            todos,
            checkpoints,
        ));
        items.extend(input.skills.clone());
        items.extend(input.tool_manifests.clone());
        items.extend(input.extra_items.clone());
        items
    }

    fn with_estimates(&self, items: Vec<ContextItem>) -> Vec<ContextItem> {
        items
            .into_iter()
            .map(|mut item| {
                if item.token_estimate == 0 && item.delivery == ContextDelivery::Message {
                    item.token_estimate = estimate_context_message_tokens(
                        &item_to_message(&item),
                        self.options.bytes_per_token,
                    );
                }
                item
            })
            .collect()
    }

    fn limit_tool_result_context(&self, items: Vec<ContextItem>) -> Vec<ContextItem> {
        let max_bytes = self.options.tool_context_preview_bytes as usize;
        let max_lines = self.options.tool_context_preview_lines as usize;
        let max_line_chars = self.options.tool_context_line_max_chars as usize;
        items
            .into_iter()
            .map(|item| {
                if item.kind != "tool_result"
                    || item.content.len() <= max_bytes
                        && item.content.lines().count() <= max_lines
                        && item
                            .content
                            .lines()
                            .all(|line| line.chars().count() <= max_line_chars)
                {
                    return item;
                }
                truncate_tool_result_context_item(
                    &item,
                    max_bytes,
                    max_lines,
                    max_line_chars,
                    self.options.bytes_per_token,
                )
            })
            .collect()
    }

    fn project(
        &self,
        items: &[ContextItem],
        item_budget_tokens: Option<u64>,
    ) -> Vec<ContextPackTraceEntry> {
        let mut included = BTreeSet::new();
        let mut dropped = BTreeMap::new();
        let mut used = 0u64;
        let mut ranked = items.iter().enumerate().collect::<Vec<_>>();
        ranked.sort_by(|(left_index, left), (right_index, right)| {
            (
                !left.pinned,
                -left.priority,
                i64::try_from(*left_index).unwrap_or(i64::MAX),
            )
                .cmp(&(
                    !right.pinned,
                    -right.priority,
                    i64::try_from(*right_index).unwrap_or(i64::MAX),
                ))
        });
        for (_index, item) in ranked {
            if let Some(duplicate_of) = item
                .metadata
                .get("context_semantic_duplicate_of")
                .and_then(Value::as_str)
            {
                dropped.insert(item.id.clone(), "semantic_duplicate".to_string());
                debug_assert!(!duplicate_of.is_empty());
            } else if item.delivery != ContextDelivery::Message
                || item_budget_tokens.is_none()
                || used + item.token_estimate <= item_budget_tokens.unwrap_or(0)
            {
                included.insert(item.id.clone());
                if item.delivery == ContextDelivery::Message {
                    used += item.token_estimate;
                }
            } else {
                dropped.insert(
                    item.id.clone(),
                    if item.pinned {
                        "required_budget_exhausted".to_string()
                    } else {
                        "model_context_budget".to_string()
                    },
                );
            }
        }
        items
            .iter()
            .map(|item| {
                let is_included = included.contains(&item.id);
                let truncated = item
                    .metadata
                    .get("context_truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                ContextPackTraceEntry {
                    item_id: item.id.clone(),
                    kind: item.kind.clone(),
                    source: item.source.clone(),
                    priority: item.priority,
                    pinned: item.pinned,
                    stable_prefix: item.stable_prefix,
                    token_estimate: item.token_estimate,
                    included: is_included,
                    drop_reason: if is_included {
                        None
                    } else {
                        Some(
                            dropped
                                .get(&item.id)
                                .cloned()
                                .unwrap_or_else(|| "not_selected".to_string()),
                        )
                    },
                    delivery: item.delivery,
                    truncated,
                    original_token_estimate: truncated.then(|| {
                        item.metadata
                            .get("context_original_token_estimate")
                            .and_then(Value::as_u64)
                            .unwrap_or(item.token_estimate)
                    }),
                    truncation_reason: truncated.then(|| {
                        item.metadata
                            .get("context_truncation_reason")
                            .and_then(Value::as_str)
                            .unwrap_or("required_context_budget")
                            .to_string()
                    }),
                    truncation_strategy: truncated.then(|| {
                        item.metadata
                            .get("context_truncation_strategy")
                            .and_then(Value::as_str)
                            .unwrap_or("head_tail")
                            .to_string()
                    }),
                    semantic_duplicate_of: item
                        .metadata
                        .get("context_semantic_duplicate_of")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                    attachment: context_attachment_trace(item),
                }
            })
            .collect()
    }

    fn fit_required_items(
        &self,
        mut items: Vec<ContextItem>,
        item_budget_tokens: u64,
    ) -> Vec<ContextItem> {
        let mut required_indices = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.pinned && item.delivery == ContextDelivery::Message)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let required_tokens = required_indices
            .iter()
            .map(|index| items[*index].token_estimate)
            .sum::<u64>();
        if required_tokens <= item_budget_tokens {
            return items;
        }

        required_indices.sort_by_key(|index| {
            (
                required_context_truncation_order(&items[*index]),
                items[*index].priority,
                *index,
            )
        });
        let mut overflow = required_tokens.saturating_sub(item_budget_tokens);
        for index in &required_indices {
            if overflow == 0 {
                break;
            }
            let current = items[*index].clone();
            let minimum_bytes = required_context_minimum_bytes(&current);
            let minimum = truncate_required_context_item(
                &current,
                minimum_bytes,
                self.options.bytes_per_token,
            );
            let max_reduction = current
                .token_estimate
                .saturating_sub(minimum.token_estimate);
            if max_reduction == 0 {
                continue;
            }
            let target_tokens = current
                .token_estimate
                .saturating_sub(overflow.min(max_reduction));
            let fitted = fit_required_context_item_to_tokens(
                &current,
                target_tokens,
                minimum_bytes,
                self.options.bytes_per_token,
            );
            let actual_reduction = current.token_estimate.saturating_sub(fitted.token_estimate);
            overflow = overflow.saturating_sub(actual_reduction);
            items[*index] = fitted;
        }

        if overflow > 0 {
            for index in required_indices {
                if overflow == 0 {
                    break;
                }
                let current = items[index].clone();
                let minimum = truncate_required_context_item(
                    &current,
                    REQUIRED_CONTEXT_HARD_MINIMUM_BYTES,
                    self.options.bytes_per_token,
                );
                let reduction = current
                    .token_estimate
                    .saturating_sub(minimum.token_estimate);
                if reduction > 0 {
                    overflow = overflow.saturating_sub(reduction);
                    items[index] = minimum;
                }
            }
        }
        items
    }

    fn dedupe_items(&self, items: Vec<ContextItem>) -> Vec<ContextItem> {
        let mut by_id = BTreeMap::<String, ContextItem>::new();
        let mut order = Vec::new();
        for item in items {
            if let Some(existing) = by_id.get(&item.id) {
                if item_rank(&item) > item_rank(existing) {
                    by_id.insert(item.id.clone(), item);
                }
            } else {
                order.push(item.id.clone());
                by_id.insert(item.id.clone(), item);
            }
        }
        order
            .into_iter()
            .filter_map(|item_id| by_id.remove(&item_id))
            .collect()
    }

    fn semantic_dedupe_items(&self, mut items: Vec<ContextItem>) -> Vec<ContextItem> {
        let mut winners = BTreeMap::<String, String>::new();
        for item in &mut items {
            let Some(fingerprint) = semantic_context_fingerprint(item) else {
                continue;
            };
            if let Some(winner_id) = winners.get(&fingerprint) {
                item.metadata.insert(
                    "context_semantic_duplicate_of".to_string(),
                    Value::String(winner_id.clone()),
                );
                item.metadata.insert(
                    "context_semantic_fingerprint".to_string(),
                    Value::String(fingerprint),
                );
            } else {
                winners.insert(fingerprint, item.id.clone());
            }
        }
        items
    }
}

#[derive(Clone, Debug, Default)]
pub struct ContextPackInput {
    pub system_sources: Option<ContextSystemSources>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSchema>,
    pub model_options: BTreeMap<String, Value>,
    pub attachments: Vec<ContextAttachment>,
    pub work_state: Option<ContextWorkState>,
    pub todos: Vec<ContextTodo>,
    pub checkpoints: Vec<ContextCheckpoint>,
    pub skills: Vec<ContextItem>,
    pub tool_manifests: Vec<ContextItem>,
    pub metadata: BTreeMap<String, Value>,
    pub runtime_context: Option<String>,
    pub sandbox_metadata: Option<Value>,
    pub extra_items: Vec<ContextItem>,
}

fn normalize_context_pack_input(mut input: ContextPackInput) -> ContextPackInput {
    let history = materialize_context_history(input.messages);
    for attachment in &mut input.attachments {
        let Some(source_message_index) = attachment.source_message_index else {
            continue;
        };
        attachment.source_message_index = history
            .message_index_map
            .get(source_message_index)
            .copied()
            .flatten();
    }
    if let Some(work_state) = input.work_state.as_mut()
        && let Some(message_position) = work_state.message_position
    {
        work_state.message_position = history
            .message_position_map
            .get(message_position)
            .copied()
            .or_else(|| history.message_position_map.last().copied());
    }
    input.messages = history.messages;
    if input.work_state.is_none() {
        input.work_state = history.work_state;
    }
    if !history.legacy_system_sources.is_empty() {
        input
            .system_sources
            .get_or_insert_with(ContextSystemSources::default)
            .legacy_system_sources
            .extend(history.legacy_system_sources);
    }
    input
}

#[must_use]
pub fn context_provider_input_hash(
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    model_options: &BTreeMap<String, Value>,
) -> String {
    let messages = messages
        .iter()
        .map(|message| {
            json!({
                "role": role_str(&message.role),
                "content": message.content,
                "name": message.name,
                "tool_call_id": message.tool_call_id,
                "context_attachment": message.metadata.get("context_attachment"),
            })
        })
        .collect::<Vec<_>>();
    let payload = json!({
        "messages": messages,
        "tools": tools,
        "model_options": model_options,
    });
    format!("sha1:{}", sha1_hex(&stable_json_dumps(&payload)))
}

fn context_stable_prefix(
    items: &[ContextItem],
    trace: &[ContextPackTraceEntry],
    tools: &[ToolSchema],
    model_options: &BTreeMap<String, Value>,
    model_id: Option<&str>,
    fixed_overhead_tokens: u64,
    overflowed: bool,
) -> ContextStablePrefix {
    let included = trace
        .iter()
        .filter(|entry| entry.included)
        .map(|entry| entry.item_id.as_str())
        .collect::<BTreeSet<_>>();
    let stable_items = items
        .iter()
        .filter(|item| {
            item.stable_prefix
                && item.delivery != ContextDelivery::TraceOnly
                && included.contains(item.id.as_str())
        })
        .collect::<Vec<_>>();
    let messages = stable_items
        .iter()
        .filter(|item| item.delivery == ContextDelivery::Message)
        .map(|item| item_to_message(item))
        .collect::<Vec<_>>();
    let payload = json!({
        "schema_version": CONTEXT_STABLE_PREFIX_SCHEMA_VERSION,
        "model_id": model_id,
        "messages": messages.iter().map(|message| json!({
            "role": role_str(&message.role),
            "content": message.content,
            "name": message.name,
            "tool_call_id": message.tool_call_id,
        })).collect::<Vec<_>>(),
        "tools": tools,
        "model_options": model_options,
    });
    ContextStablePrefix {
        schema_version: CONTEXT_STABLE_PREFIX_SCHEMA_VERSION.to_string(),
        hash: format!("sha1:{}", sha1_hex(&stable_json_dumps(&payload))),
        item_count: stable_items.len() as u64,
        message_count: messages.len() as u64,
        tool_manifest_count: stable_items
            .iter()
            .filter(|item| item.delivery == ContextDelivery::ToolManifest)
            .count() as u64,
        token_estimate: fixed_overhead_tokens.saturating_add(
            stable_items
                .iter()
                .filter(|item| item.delivery == ContextDelivery::Message)
                .map(|item| item.token_estimate)
                .sum::<u64>(),
        ),
        cache_eligible: !overflowed && (!messages.is_empty() || !tools.is_empty()),
    }
}

fn semantic_context_fingerprint(item: &ContextItem) -> Option<String> {
    if !item.stable_prefix || item.content.trim().is_empty() {
        return None;
    }
    let kind = if item.kind == "instruction"
        || (item.kind == "message"
            && item.metadata.get("role").and_then(Value::as_str) == Some("system"))
    {
        "instruction"
    } else {
        item.kind.as_str()
    };
    let normalized = item
        .content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let payload = json!({
        "kind": kind,
        "delivery": context_delivery_name(item.delivery),
        "content": normalized.trim(),
    });
    Some(format!("sha1:{}", sha1_hex(&stable_json_dumps(&payload))))
}

fn estimate_context_pack_fixed_overhead(
    tools: &[ToolSchema],
    model_options: &BTreeMap<String, Value>,
    bytes_per_token: u64,
) -> u64 {
    estimate_serialized_tokens(
        &json!({
            "tools": tools,
            "model_options": model_options,
        }),
        bytes_per_token,
    )
}

fn estimate_context_message_tokens(message: &ChatMessage, bytes_per_token: u64) -> u64 {
    let materialized =
        materialize_openai_compatible_payload(None, std::slice::from_ref(message), &[], None, None);
    estimate_serialized_tokens(&Value::Array(materialized.messages), bytes_per_token)
}

fn required_context_truncation_order(item: &ContextItem) -> u8 {
    if item.kind == "checkpoint" {
        0
    } else if item.kind.starts_with("attachment_") {
        10
    } else if item.kind == "todo" {
        20
    } else if item.kind == "goal" {
        25
    } else if item.kind == "plan" {
        27
    } else if matches!(item.kind.as_str(), "runtime" | "sandbox") {
        30
    } else if item.kind == "work_state" {
        40
    } else if item.metadata.get("role").and_then(Value::as_str) == Some("system") {
        50
    } else if item.metadata.get("role").and_then(Value::as_str) == Some("user") {
        60
    } else {
        35
    }
}

fn required_context_minimum_bytes(item: &ContextItem) -> usize {
    if item.kind == "checkpoint" {
        128
    } else if item.kind.starts_with("attachment_") {
        320
    } else if matches!(
        item.kind.as_str(),
        "goal" | "plan" | "todo" | "runtime" | "sandbox"
    ) {
        256
    } else if item.kind == "work_state" {
        512
    } else if item.metadata.get("role").and_then(Value::as_str) == Some("system") {
        768
    } else if item.metadata.get("role").and_then(Value::as_str) == Some("user") {
        640
    } else {
        256
    }
}

fn fit_required_context_item_to_tokens(
    item: &ContextItem,
    target_tokens: u64,
    minimum_bytes: usize,
    bytes_per_token: u64,
) -> ContextItem {
    if item.token_estimate <= target_tokens || item.content.len() <= minimum_bytes {
        return item.clone();
    }
    let mut low = minimum_bytes.min(item.content.len());
    let mut high = item.content.len().saturating_sub(1);
    let mut best = truncate_required_context_item(item, low, bytes_per_token);
    if best.token_estimate > target_tokens {
        return best;
    }
    while low <= high {
        let middle = low + (high - low) / 2;
        let candidate = truncate_required_context_item(item, middle, bytes_per_token);
        if candidate.token_estimate <= target_tokens {
            best = candidate;
            low = middle.saturating_add(1);
        } else if middle == 0 {
            break;
        } else {
            high = middle - 1;
        }
    }
    best
}

fn truncate_required_context_item(
    item: &ContextItem,
    max_bytes: usize,
    bytes_per_token: u64,
) -> ContextItem {
    if item.content.len() <= max_bytes {
        return item.clone();
    }
    let (strategy, preserve_header) = required_context_truncation_strategy(item);
    let content = if strategy == "instruction_sections_head_tail" {
        truncate_instruction_sections_head_tail(&item.content, max_bytes)
    } else {
        truncate_context_head_tail(&item.content, max_bytes, preserve_header)
    };
    let mut fitted = item.clone();
    let original_token_estimate = fitted
        .metadata
        .get("context_original_token_estimate")
        .and_then(Value::as_u64)
        .unwrap_or(item.token_estimate);
    let original_bytes = fitted
        .metadata
        .get("context_original_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(item.content.len() as u64);
    fitted.content = content;
    fitted
        .metadata
        .insert("context_truncated".to_string(), Value::Bool(true));
    fitted.metadata.insert(
        "context_truncation_reason".to_string(),
        Value::String(REQUIRED_CONTEXT_TRUNCATION_REASON.to_string()),
    );
    fitted.metadata.insert(
        "context_truncation_strategy".to_string(),
        Value::String(strategy.to_string()),
    );
    fitted.metadata.insert(
        "context_original_token_estimate".to_string(),
        json!(original_token_estimate),
    );
    fitted
        .metadata
        .insert("context_original_bytes".to_string(), json!(original_bytes));
    fitted.metadata.insert(
        "context_retained_bytes".to_string(),
        json!(fitted.content.len()),
    );
    fitted.token_estimate =
        estimate_context_message_tokens(&item_to_message(&fitted), bytes_per_token);
    fitted
}

fn truncate_tool_result_context_item(
    item: &ContextItem,
    max_bytes: usize,
    max_lines: usize,
    max_line_chars: usize,
    bytes_per_token: u64,
) -> ContextItem {
    let original_bytes = item.content.len();
    let original_token_estimate = item.token_estimate;
    let mut lines = item
        .content
        .lines()
        .map(|line| truncate_tool_result_line(line, max_line_chars))
        .collect::<Vec<_>>();
    let original_line_count = lines.len();
    if lines.len() > max_lines {
        let head_count = max_lines.div_ceil(2);
        let tail_count = max_lines.saturating_sub(head_count);
        let omitted = lines.len().saturating_sub(head_count + tail_count);
        let mut selected = Vec::with_capacity(head_count + tail_count + 1);
        selected.extend(lines.drain(..head_count));
        selected.push(format!(
            "[... {omitted} tool-output lines omitted from model context ...]"
        ));
        if tail_count > 0 {
            selected.extend(
                lines
                    .into_iter()
                    .rev()
                    .take(tail_count)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
        }
        lines = selected;
    }
    let line_window = lines.join("\n");
    let content = truncate_context_head_tail(&line_window, max_bytes.max(1), false);
    let mut truncated = item.clone();
    truncated.content = content;
    truncated
        .metadata
        .insert("context_truncated".to_string(), Value::Bool(true));
    truncated.metadata.insert(
        "context_truncation_reason".to_string(),
        Value::String("tool_output_preview".to_string()),
    );
    truncated.metadata.insert(
        "context_truncation_strategy".to_string(),
        Value::String("line_window_head_tail".to_string()),
    );
    truncated.metadata.insert(
        "context_original_token_estimate".to_string(),
        json!(original_token_estimate),
    );
    truncated
        .metadata
        .insert("context_original_bytes".to_string(), json!(original_bytes));
    truncated.metadata.insert(
        "context_original_line_count".to_string(),
        json!(original_line_count),
    );
    truncated.metadata.insert(
        "context_retained_bytes".to_string(),
        json!(truncated.content.len()),
    );
    truncated.token_estimate =
        estimate_context_message_tokens(&item_to_message(&truncated), bytes_per_token);
    truncated
}

fn truncate_tool_result_line(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let marker = "… [line truncated]";
    let retained = max_chars.saturating_sub(marker.chars().count()).max(1);
    format!(
        "{}{}",
        line.chars().take(retained).collect::<String>(),
        marker
    )
}

fn required_context_truncation_strategy(item: &ContextItem) -> (&'static str, bool) {
    if item.kind.starts_with("attachment_") {
        ("attachment_header_head_tail", true)
    } else if matches!(item.kind.as_str(), "goal" | "plan" | "todo") {
        ("todo_header_head_tail", true)
    } else if item.kind == "work_state" {
        ("work_state_sections_head_tail", true)
    } else if item.metadata.get("role").and_then(Value::as_str) == Some("system") {
        ("instruction_sections_head_tail", false)
    } else if item.metadata.get("role").and_then(Value::as_str) == Some("user") {
        ("latest_user_head_tail", false)
    } else {
        ("head_tail", item.content.starts_with('['))
    }
}

fn truncate_instruction_sections_head_tail(text: &str, max_bytes: usize) -> String {
    let Some(instruction_start) = text.find("<instructions>") else {
        return truncate_context_head_tail(text, max_bytes, false);
    };
    let instruction_body_start = instruction_start + "<instructions>".len();
    let Some(relative_end) = text[instruction_body_start..].find("</instructions>") else {
        return truncate_context_head_tail(text, max_bytes, false);
    };
    let instruction_end = instruction_body_start + relative_end;
    let prefix = &text[..instruction_body_start];
    let instructions = &text[instruction_body_start..instruction_end];
    let suffix = &text[instruction_end..];
    let marker_count = 3usize;
    let marker_bytes = REQUIRED_CONTEXT_TRUNCATION_MARKER
        .len()
        .saturating_mul(marker_count);
    if max_bytes <= marker_bytes + 16 {
        return truncate_context_head_tail(text, max_bytes, false);
    }
    let retained_budget = max_bytes - marker_bytes;
    let prefix_budget = retained_budget / 5;
    let instruction_head_budget = retained_budget.saturating_mul(3) / 10;
    let instruction_tail_budget = retained_budget.saturating_mul(3) / 10;
    let suffix_budget = retained_budget
        .saturating_sub(prefix_budget)
        .saturating_sub(instruction_head_budget)
        .saturating_sub(instruction_tail_budget);
    format!(
        "{}{}{}{}{}{}{}",
        utf8_prefix(prefix, prefix_budget),
        REQUIRED_CONTEXT_TRUNCATION_MARKER,
        utf8_prefix(instructions, instruction_head_budget),
        REQUIRED_CONTEXT_TRUNCATION_MARKER,
        utf8_suffix(instructions, instruction_tail_budget),
        REQUIRED_CONTEXT_TRUNCATION_MARKER,
        utf8_suffix(suffix, suffix_budget),
    )
}

fn truncate_context_head_tail(text: &str, max_bytes: usize, preserve_header: bool) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes <= REQUIRED_CONTEXT_TRUNCATION_MARKER.len() + 8 {
        return utf8_prefix(text, max_bytes).to_string();
    }

    let (header, body) = if preserve_header {
        text.find('\n')
            .map(|newline| text.split_at(newline + 1))
            .filter(|(header, _)| {
                header.len() + REQUIRED_CONTEXT_TRUNCATION_MARKER.len() + 8 < max_bytes
            })
            .unwrap_or(("", text))
    } else {
        ("", text)
    };
    let retained_budget = max_bytes
        .saturating_sub(header.len())
        .saturating_sub(REQUIRED_CONTEXT_TRUNCATION_MARKER.len());
    let head_budget = retained_budget.saturating_mul(2) / 3;
    let tail_budget = retained_budget.saturating_sub(head_budget);
    format!(
        "{}{}{}{}",
        header,
        utf8_prefix(body, head_budget),
        REQUIRED_CONTEXT_TRUNCATION_MARKER,
        utf8_suffix(body, tail_budget),
    )
}

fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn utf8_suffix(text: &str, max_bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn estimate_serialized_tokens(value: &Value, bytes_per_token: u64) -> u64 {
    let bytes = stable_json_dumps(value).len() as u64;
    bytes
        .div_ceil(bytes_per_token.max(1))
        .max(u64::from(bytes > 0))
}

fn context_pack_hash(
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    model_options: &BTreeMap<String, Value>,
    items: &[ContextItem],
    trace: &[ContextPackTraceEntry],
) -> String {
    let payload = json!({
        "schema_version": CONTEXT_PACK_SCHEMA_VERSION,
        "provider_input_hash": context_provider_input_hash(messages, tools, model_options),
        "items": items,
        "trace": trace,
    });
    format!("sha1:{}", sha1_hex(&stable_json_dumps(&payload)))
}

#[allow(clippy::too_many_arguments)]
fn context_pack_receipt(
    pack_hash: &str,
    provider_input_hash: &str,
    messages: &[ChatMessage],
    tools: &[ToolSchema],
    model_options: &BTreeMap<String, Value>,
    items: &[ContextItem],
    trace: &[ContextPackTraceEntry],
    stable_prefix: &ContextStablePrefix,
    estimated_input_tokens: u64,
    budget: &ContextPackBudget,
) -> ContextPackReceipt {
    let mut message_role_counts = BTreeMap::new();
    for message in messages {
        increment_count(&mut message_role_counts, role_str(&message.role));
    }
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let model_option_keys = model_options.keys().cloned().collect::<Vec<_>>();
    let mut item_kind_counts = BTreeMap::new();
    let mut item_delivery_counts = BTreeMap::new();
    for item in items {
        increment_count(&mut item_kind_counts, &item.kind);
        increment_count(
            &mut item_delivery_counts,
            context_delivery_name(item.delivery),
        );
    }
    let mut drop_reason_counts = BTreeMap::new();
    for entry in trace.iter().filter(|entry| !entry.included) {
        increment_count(
            &mut drop_reason_counts,
            entry.drop_reason.as_deref().unwrap_or("not_selected"),
        );
    }
    let mut truncation_reason_counts = BTreeMap::new();
    let mut truncation_strategy_counts = BTreeMap::new();
    let truncated_item_count = trace.iter().filter(|entry| entry.truncated).count() as u64;
    let semantic_duplicate_count = trace
        .iter()
        .filter(|entry| entry.semantic_duplicate_of.is_some())
        .count() as u64;
    for entry in trace.iter().filter(|entry| entry.truncated) {
        increment_count(
            &mut truncation_reason_counts,
            entry
                .truncation_reason
                .as_deref()
                .unwrap_or("required_context_budget"),
        );
        increment_count(
            &mut truncation_strategy_counts,
            entry.truncation_strategy.as_deref().unwrap_or("head_tail"),
        );
    }
    let included_item_count = trace.iter().filter(|entry| entry.included).count() as u64;
    ContextPackReceipt {
        schema_version: CONTEXT_PACK_RECEIPT_SCHEMA_VERSION.to_string(),
        pack_schema_version: CONTEXT_PACK_SCHEMA_VERSION.to_string(),
        pack_hash: pack_hash.to_string(),
        provider_input_hash: provider_input_hash.to_string(),
        message_count: messages.len() as u64,
        message_role_counts,
        tool_manifest_count: tools.len() as u64,
        tool_names,
        model_option_keys,
        item_count: items.len() as u64,
        included_item_count,
        dropped_item_count: trace.len() as u64 - included_item_count,
        item_kind_counts,
        item_delivery_counts,
        drop_reason_counts,
        truncated_item_count,
        truncation_reason_counts,
        truncation_strategy_counts,
        semantic_duplicate_count,
        stable_prefix: stable_prefix.clone(),
        estimated_input_tokens,
        budget: budget.clone(),
    }
}

fn context_delivery_name(delivery: ContextDelivery) -> &'static str {
    match delivery {
        ContextDelivery::Message => "message",
        ContextDelivery::ToolManifest => "tool_manifest",
        ContextDelivery::TraceOnly => "trace_only",
    }
}

fn bool_is_false(value: &bool) -> bool {
    !*value
}

fn u64_is_zero(value: &u64) -> bool {
    *value == 0
}

fn increment_count(counts: &mut BTreeMap<String, u64>, key: &str) {
    let count = counts.entry(key.to_string()).or_default();
    *count = count.saturating_add(1);
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstructionLoadOptions {
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub include_user: bool,
    pub user_config_dir: Option<PathBuf>,
    pub workspace_files: Vec<String>,
    pub user_files: Vec<String>,
}

impl Default for InstructionLoadOptions {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            include_user: true,
            user_config_dir: None,
            workspace_files: DEFAULT_WORKSPACE_FILES
                .iter()
                .map(|item| (*item).to_string())
                .collect(),
            user_files: DEFAULT_USER_FILES
                .iter()
                .map(|item| (*item).to_string())
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstructionItem {
    pub path: String,
    pub display_path: String,
    pub source: String,
    pub scope: String,
    pub content: String,
    pub bytes_read: usize,
    pub truncated: bool,
}

impl InstructionItem {
    #[must_use]
    pub fn to_context_item(&self) -> ContextItem {
        let digest = sha1_hex_12(&self.path);
        let mut metadata = BTreeMap::new();
        metadata.insert("path".to_string(), json!(self.path));
        metadata.insert("display_path".to_string(), json!(self.display_path));
        metadata.insert("scope".to_string(), json!(self.scope));
        metadata.insert("bytes_read".to_string(), json!(self.bytes_read));
        metadata.insert("truncated".to_string(), json!(self.truncated));
        let mut item = ContextItem::new(
            format!("instruction:{}:{digest}", self.scope),
            "instruction",
            self.source.clone(),
            format!("[Instruction: {}]\n{}", self.display_path, self.content)
                .trim()
                .to_string(),
            CONTEXT_PRIORITY_INSTRUCTION,
        );
        item.pinned = true;
        item.stable_prefix = true;
        item.metadata = metadata;
        item
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstructionContext {
    pub items: Vec<InstructionItem>,
    pub total_bytes: usize,
    pub truncated: bool,
    pub issues: Vec<String>,
}

impl InstructionContext {
    #[must_use]
    pub fn to_context_items(&self) -> Vec<ContextItem> {
        self.items
            .iter()
            .map(InstructionItem::to_context_item)
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct AgentSystemPrompt {
    content: String,
    content_hash: String,
    preloaded_skill_names: Vec<String>,
    instruction_count: u64,
    instruction_total_bytes: usize,
    instructions_truncated: bool,
    instruction_issues: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ContextSystemSources {
    pub profile_id: Option<String>,
    pub profile_mode: Option<String>,
    pub profile_prompt: Option<String>,
    pub workspace_root: PathBuf,
    pub preloaded_skills: Vec<SkillDocument>,
    pub available_skills: Vec<SkillInfo>,
    #[serde(default)]
    pub legacy_system_sources: Vec<ContextLegacySystemSource>,
    pub include_instructions: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextLegacySystemSource {
    pub id: String,
    pub source: String,
    pub content: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextHistoryMaterialization {
    pub messages: Vec<ChatMessage>,
    pub legacy_system_sources: Vec<ContextLegacySystemSource>,
    pub work_state: Option<ContextWorkState>,
    pub discarded_profile_system_count: u64,
    pub message_index_map: Vec<Option<usize>>,
    pub message_position_map: Vec<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ContextSystemDiagnostics {
    pub schema_version: String,
    pub profile_id: Option<String>,
    pub profile_mode: Option<String>,
    pub content_hash: String,
    pub preloaded_skill_names: Vec<String>,
    pub instruction_count: u64,
    pub instruction_total_bytes: usize,
    pub instructions_truncated: bool,
    pub instruction_issues: Vec<String>,
    pub legacy_system_count: u64,
}

impl ContextSystemDiagnostics {
    #[must_use]
    pub fn session_metadata(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "profile_id": self.profile_id,
            "profile_mode": self.profile_mode,
            "hash": self.content_hash,
            "instruction_count": self.instruction_count,
            "instruction_total_bytes": self.instruction_total_bytes,
            "instructions_truncated": self.instructions_truncated,
            "instruction_issues": self.instruction_issues,
            "preloaded_skills": self.preloaded_skill_names,
            "legacy_system_count": self.legacy_system_count,
        })
    }
}

#[derive(Clone, Debug)]
struct ContextSystemMaterialization {
    assembled: ContextItem,
    sources: Vec<ContextItem>,
    diagnostics: ContextSystemDiagnostics,
}

#[must_use]
pub fn materialize_context_history(messages: Vec<ChatMessage>) -> ContextHistoryMaterialization {
    let mut materialized = ContextHistoryMaterialization::default();
    let compacted_until = latest_compaction_boundary_cutoff(&messages);
    for (index, message) in messages.into_iter().enumerate() {
        materialized
            .message_position_map
            .push(materialized.messages.len());
        materialized.message_index_map.push(None);
        // A compaction boundary is a durable replacement for the preceding
        // transcript. Keeping both the boundary's work state and the original
        // messages defeats compaction and eventually recreates the same
        // oversized provider request. The transcript remains on disk; only
        // the provider-facing projection omits the summarized prefix.
        if compacted_until.is_some_and(|cutoff| index <= cutoff) {
            continue;
        }
        if message.role != Role::System {
            materialized.message_index_map[index] = Some(materialized.messages.len());
            materialized.messages.push(message);
            continue;
        }
        if message.metadata.contains_key("agent_profile")
            || message
                .metadata
                .get("dynamic_system_prompt")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            materialized.discarded_profile_system_count = materialized
                .discarded_profile_system_count
                .saturating_add(1);
            continue;
        }
        if message.metadata.get("kind").and_then(Value::as_str) == Some("compaction_boundary") {
            materialized.work_state = Some(context_work_state_from_boundary(message));
            continue;
        }
        let content = message.content.trim().to_string();
        if content.is_empty() {
            continue;
        }
        let source = message
            .metadata
            .get("source")
            .and_then(Value::as_str)
            .filter(|source| !source.trim().is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("session.messages[{index}]"));
        let identity = message
            .metadata
            .get("message_id")
            .and_then(Value::as_str)
            .filter(|message_id| !message_id.trim().is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                format!(
                    "{index}:{}",
                    sha1_hex(&content).chars().take(16).collect::<String>()
                )
            });
        materialized
            .legacy_system_sources
            .push(ContextLegacySystemSource {
                id: format!("legacy_system:{identity}"),
                source,
                content,
            });
    }
    materialized
        .message_position_map
        .push(materialized.messages.len());
    materialized
}

fn latest_compaction_boundary_cutoff(messages: &[ChatMessage]) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(boundary_index, boundary)| {
            if boundary.metadata.get("kind").and_then(Value::as_str) != Some("compaction_boundary")
            {
                return None;
            }
            let compacted_until = boundary
                .metadata
                .get("compacted_until_message_id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())?;
            messages[..boundary_index].iter().rposition(|message| {
                message.metadata.get("message_id").and_then(Value::as_str) == Some(compacted_until)
            })
        })
}

fn context_work_state_from_boundary(message: ChatMessage) -> ContextWorkState {
    let mut metadata = message.metadata;
    let id = metadata
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or("compaction")
        .to_string();
    let format = metadata
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("session_compaction_boundary_v1")
        .to_string();
    let source = metadata
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("session.transcript.compaction")
        .to_string();
    let compacted_until_message_id = metadata
        .get("compacted_until_message_id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    for key in [
        "kind",
        "message_id",
        "compacted_until_message_id",
        "format",
        "source",
    ] {
        metadata.remove(key);
    }
    ContextWorkState {
        id,
        summary: message.content,
        format,
        source,
        message_position: Some(0),
        compacted_until_message_id,
        metadata,
    }
}

fn build_agent_system_prompt_from_context(
    profile_prompt: Option<&str>,
    instructions: Option<&InstructionContext>,
    preloaded_skills: &[SkillDocument],
    available_skills: &[SkillInfo],
    legacy_system_sources: &[ContextLegacySystemSource],
) -> Option<AgentSystemPrompt> {
    let mut prompt_parts = Vec::new();
    if let Some(prompt) = profile_prompt
        .map(|value| value.trim_start_matches('\u{feff}').trim())
        .filter(|value| !value.is_empty())
    {
        prompt_parts.push(prompt.to_string());
    }

    let mut instruction_count = 0;
    let mut instruction_total_bytes = 0;
    let mut instructions_truncated = false;
    let mut instruction_issues = Vec::new();
    if let Some(instructions) = instructions {
        instruction_count = instructions.items.len() as u64;
        instruction_total_bytes = instructions.total_bytes;
        instructions_truncated = instructions.truncated;
        instruction_issues = instructions.issues.clone();
        if let Some(rendered) = render_instruction_context_for_system_prompt(instructions) {
            prompt_parts.push(rendered);
        }
    }

    let preloaded_skill_names = preloaded_skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    if let Some(skills) = render_preloaded_skills(preloaded_skills) {
        prompt_parts.push(skills);
    }
    if let Some(skills) = render_available_skills(available_skills) {
        prompt_parts.push(skills);
    }
    for legacy in legacy_system_sources {
        prompt_parts.push(legacy.content.clone());
    }

    let content = prompt_parts
        .into_iter()
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if content.is_empty() {
        return None;
    }
    Some(AgentSystemPrompt {
        content_hash: sha1_hex_12(&content),
        content,
        preloaded_skill_names,
        instruction_count,
        instruction_total_bytes,
        instructions_truncated,
        instruction_issues,
    })
}

fn materialize_context_system_sources(
    sources: &ContextSystemSources,
) -> Option<ContextSystemMaterialization> {
    let instructions = sources
        .include_instructions
        .then(|| InstructionContextLoader::new(sources.workspace_root.clone(), None).load());
    let legacy_system_sources = dedupe_legacy_system_sources(&sources.legacy_system_sources);
    let system_prompt = build_agent_system_prompt_from_context(
        sources.profile_prompt.as_deref(),
        instructions.as_ref(),
        &sources.preloaded_skills,
        &sources.available_skills,
        &legacy_system_sources,
    )?;
    let diagnostics = ContextSystemDiagnostics {
        schema_version: CONTEXT_SYSTEM_DIAGNOSTICS_SCHEMA_VERSION.to_string(),
        profile_id: sources.profile_id.clone(),
        profile_mode: sources.profile_mode.clone(),
        content_hash: system_prompt.content_hash.clone(),
        preloaded_skill_names: system_prompt.preloaded_skill_names.clone(),
        instruction_count: system_prompt.instruction_count,
        instruction_total_bytes: system_prompt.instruction_total_bytes,
        instructions_truncated: system_prompt.instructions_truncated,
        instruction_issues: system_prompt.instruction_issues.clone(),
        legacy_system_count: legacy_system_sources.len() as u64,
    };
    let mut materialized_sources = Vec::new();
    if let Some(profile_prompt) = sources
        .profile_prompt
        .as_deref()
        .map(|value| value.trim_start_matches('\u{feff}').trim())
        .filter(|value| !value.is_empty())
    {
        let profile_id = sources.profile_id.as_deref().unwrap_or("default");
        let mut profile_item = ContextItem::new(
            format!("profile_prompt:{profile_id}"),
            "profile_prompt",
            format!("agent.profile:{profile_id}"),
            profile_prompt,
            CONTEXT_PRIORITY_INSTRUCTION,
        );
        profile_item.pinned = true;
        profile_item.stable_prefix = true;
        profile_item.delivery = ContextDelivery::TraceOnly;
        profile_item.metadata = BTreeMap::from([
            ("profile_id".to_string(), json!(sources.profile_id)),
            ("profile_mode".to_string(), json!(sources.profile_mode)),
            ("embedded_in".to_string(), json!("context.system_sources")),
        ]);
        materialized_sources.push(profile_item);
    }
    if let Some(instructions) = instructions.as_ref() {
        materialized_sources.extend(instructions.to_context_items().into_iter().map(|mut item| {
            item.delivery = ContextDelivery::TraceOnly;
            item.metadata
                .insert("embedded_in".to_string(), json!("context.system_sources"));
            item
        }));
    }
    materialized_sources.extend(skill_context_items(
        &sources.preloaded_skills,
        &sources.available_skills,
    ));
    materialized_sources.extend(legacy_system_sources.iter().map(|legacy| {
        let mut item = ContextItem::new(
            legacy.id.clone(),
            "legacy_system",
            legacy.source.clone(),
            legacy.content.clone(),
            CONTEXT_PRIORITY_INSTRUCTION,
        );
        item.pinned = true;
        item.stable_prefix = true;
        item.delivery = ContextDelivery::TraceOnly;
        item.metadata
            .insert("embedded_in".to_string(), json!("context.system_sources"));
        item
    }));

    let mut assembled = ContextItem::new(
        "system:context_sources",
        "message",
        "context.system_sources",
        system_prompt.content,
        CONTEXT_PRIORITY_INSTRUCTION,
    );
    assembled.pinned = true;
    assembled.stable_prefix = true;
    assembled
        .metadata
        .insert("role".to_string(), json!("system"));
    assembled
        .metadata
        .insert("dynamic_system_prompt".to_string(), json!(true));
    assembled.metadata.insert(
        "dynamic_system_prompt_info".to_string(),
        diagnostics.session_metadata(),
    );
    if let Some(profile_id) = sources.profile_id.as_ref() {
        assembled
            .metadata
            .insert("agent_profile".to_string(), json!(profile_id));
    }
    if let Some(profile_mode) = sources.profile_mode.as_ref() {
        assembled
            .metadata
            .insert("agent_mode".to_string(), json!(profile_mode));
    }
    let mut provider_metadata = assembled.metadata.clone();
    provider_metadata.remove("role");
    assembled.metadata.insert(
        "message_metadata".to_string(),
        serde_json::to_value(provider_metadata).unwrap_or_default(),
    );
    Some(ContextSystemMaterialization {
        assembled,
        sources: materialized_sources,
        diagnostics,
    })
}

fn dedupe_legacy_system_sources(
    sources: &[ContextLegacySystemSource],
) -> Vec<ContextLegacySystemSource> {
    let mut seen = BTreeSet::new();
    sources
        .iter()
        .filter(|source| {
            let normalized = source
                .content
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n");
            seen.insert(sha1_hex(normalized.trim()))
        })
        .cloned()
        .collect()
}

fn render_instruction_context_for_system_prompt(context: &InstructionContext) -> Option<String> {
    if context.items.is_empty() {
        return None;
    }
    let mut lines = vec![
        "Workspace and user instructions loaded for this turn.".to_string(),
        "<instructions>".to_string(),
    ];
    for item in &context.items {
        lines.push(format!(
            "  <instruction scope=\"{}\" path=\"{}\">",
            xml_escape(&item.scope),
            xml_escape(&item.display_path)
        ));
        lines.push(item.content.trim().to_string());
        lines.push("  </instruction>".to_string());
    }
    lines.push("</instructions>".to_string());
    Some(lines.join("\n"))
}

#[derive(Clone, Debug)]
pub struct InstructionContextLoader {
    workspace_root: PathBuf,
    options: InstructionLoadOptions,
}

impl InstructionContextLoader {
    #[must_use]
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        options: Option<InstructionLoadOptions>,
    ) -> Self {
        let root = canonicalize_existing(&workspace_root.into());
        Self {
            workspace_root: root,
            options: options.unwrap_or_default(),
        }
    }

    #[must_use]
    pub fn load(&self) -> InstructionContext {
        let mut issues = Vec::new();
        let mut items = Vec::new();
        let mut total_bytes = 0usize;
        let mut truncated = false;
        let mut seen = BTreeSet::new();
        for candidate in self.candidates() {
            let path = canonicalize_existing(&candidate.path);
            if seen.contains(&path) || !path.is_file() {
                continue;
            }
            seen.insert(path.clone());
            if !self.is_allowed_path(&path) {
                issues.push(format!("skipped_out_of_scope:{}", candidate.display_path));
                continue;
            }
            if total_bytes >= self.options.max_total_bytes {
                truncated = true;
                issues.push("total_limit_reached".to_string());
                break;
            }
            match self.load_candidate(&candidate, self.options.max_total_bytes - total_bytes) {
                Some((item, issue)) => {
                    if let Some(issue) = issue {
                        issues.push(issue);
                    }
                    total_bytes += item.bytes_read;
                    truncated |= item.truncated;
                    items.push(item);
                }
                None => issues.push(format!("skipped_unreadable:{}", candidate.display_path)),
            }
        }
        InstructionContext {
            items,
            total_bytes,
            truncated,
            issues,
        }
    }

    fn candidates(&self) -> Vec<InstructionCandidate> {
        let mut candidates = Vec::new();
        for base in self.workspace_ancestors() {
            for filename in &self.options.workspace_files {
                let path = base.join(filename);
                let display = self.display_workspace_path(&path);
                candidates.push(InstructionCandidate {
                    path,
                    display_path: display.clone(),
                    source: format!("instructions.workspace:{display}"),
                    scope: "workspace".to_string(),
                });
            }
            let path = base.join(".openagent").join("instructions.md");
            let display = self.display_workspace_path(&path);
            candidates.push(InstructionCandidate {
                path,
                display_path: display.clone(),
                source: format!("instructions.workspace:{display}"),
                scope: "workspace".to_string(),
            });
            let rules_dir = base.join(".openagent").join("rules");
            let mut rules = read_dir_paths(&rules_dir)
                .into_iter()
                .filter(|path| path.extension().and_then(OsStr::to_str) == Some("md"))
                .collect::<Vec<_>>();
            rules.sort();
            for rule in rules {
                let display = self.display_workspace_path(&rule);
                candidates.push(InstructionCandidate {
                    path: rule,
                    display_path: display.clone(),
                    source: format!("instructions.workspace:{display}"),
                    scope: "workspace".to_string(),
                });
            }
        }
        if self.options.include_user {
            let user_dir = self.user_config_dir();
            for filename in &self.options.user_files {
                candidates.push(InstructionCandidate {
                    path: user_dir.join(filename),
                    display_path: format!("~/.openagent/{filename}"),
                    source: format!("instructions.user:{filename}"),
                    scope: "user".to_string(),
                });
            }
            let mut rules = read_dir_paths(&user_dir.join("rules"))
                .into_iter()
                .filter(|path| path.extension().and_then(OsStr::to_str) == Some("md"))
                .collect::<Vec<_>>();
            rules.sort();
            for rule in rules {
                let name = rule
                    .file_name()
                    .and_then(OsStr::to_str)
                    .unwrap_or_default()
                    .to_string();
                candidates.push(InstructionCandidate {
                    path: rule,
                    display_path: format!("~/.openagent/rules/{name}"),
                    source: format!("instructions.user:rules/{name}"),
                    scope: "user".to_string(),
                });
            }
        }
        candidates
    }

    fn load_candidate(
        &self,
        candidate: &InstructionCandidate,
        remaining_bytes: usize,
    ) -> Option<(InstructionItem, Option<String>)> {
        let raw = fs::read(&candidate.path).ok()?;
        if raw.iter().take(1024).any(|byte| *byte == 0) {
            return None;
        }
        if std::str::from_utf8(&raw).is_err() {
            return None;
        }
        let mut allowed = raw
            .len()
            .min(self.options.max_file_bytes)
            .min(remaining_bytes);
        while allowed > 0 && std::str::from_utf8(&raw[..allowed]).is_err() {
            allowed -= 1;
        }
        if allowed == 0 {
            return None;
        }
        let truncated = allowed < raw.len();
        let content = std::str::from_utf8(&raw[..allowed])
            .ok()?
            .trim()
            .to_string();
        let path = canonicalize_existing(&candidate.path);
        let issue = truncated.then(|| format!("truncated:{}", candidate.display_path));
        Some((
            InstructionItem {
                path: path_to_string(&path),
                display_path: candidate.display_path.clone(),
                source: candidate.source.clone(),
                scope: candidate.scope.clone(),
                content,
                bytes_read: allowed,
                truncated,
            },
            issue,
        ))
    }

    fn workspace_ancestors(&self) -> Vec<PathBuf> {
        let mut result = vec![self.workspace_root.clone()];
        result.extend(
            self.workspace_root
                .ancestors()
                .skip(1)
                .map(Path::to_path_buf),
        );
        result
    }

    fn user_config_dir(&self) -> PathBuf {
        self.options
            .user_config_dir
            .as_ref()
            .map(|path| canonicalize_existing(path))
            .unwrap_or_else(|| default_home_dir().join(".openagent"))
    }

    fn is_allowed_path(&self, path: &Path) -> bool {
        if path.starts_with(&self.workspace_root) {
            return true;
        }
        for ancestor in self.workspace_root.ancestors().skip(1) {
            if path.parent() == Some(ancestor) || path.starts_with(ancestor.join(".openagent")) {
                return true;
            }
        }
        self.options.include_user && path.starts_with(self.user_config_dir())
    }

    fn display_workspace_path(&self, path: &Path) -> String {
        let resolved = canonicalize_existing(path);
        resolved
            .strip_prefix(&self.workspace_root)
            .map(path_to_string)
            .unwrap_or_else(|_| {
                resolved
                    .file_name()
                    .and_then(OsStr::to_str)
                    .unwrap_or_default()
                    .to_string()
            })
    }
}

#[derive(Clone, Debug)]
struct InstructionCandidate {
    path: PathBuf,
    display_path: String,
    source: String,
    scope: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub location: String,
    pub directory: String,
    pub metadata: BTreeMap<String, Value>,
    pub score: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SkillDocument {
    pub name: String,
    pub description: String,
    pub location: String,
    pub directory: String,
    pub metadata: BTreeMap<String, Value>,
    pub score: Option<i64>,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SkillIssue {
    pub kind: String,
    pub path: String,
    pub message: String,
    pub duplicate_of: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SkillDiscoveryReport {
    pub skills: Vec<SkillInfo>,
    pub scanned_files: u64,
    pub loaded_count: u64,
    pub invalid_count: u64,
    pub duplicate_count: u64,
    pub issues: Vec<SkillIssue>,
}

#[derive(Clone, Debug)]
pub struct SkillRegistry {
    session_root: PathBuf,
    roots: Vec<String>,
    extra_roots: Vec<String>,
    disabled_names: BTreeSet<String>,
    home_dir: PathBuf,
    include_builtin_skills: bool,
}

#[derive(Clone, Debug)]
pub struct SkillRegistryOptions {
    pub include_builtin_skills: bool,
}

impl Default for SkillRegistryOptions {
    fn default() -> Self {
        Self {
            include_builtin_skills: true,
        }
    }
}

impl SkillRegistry {
    #[must_use]
    pub fn new(
        session_root: Option<impl Into<PathBuf>>,
        roots: Option<Vec<String>>,
        home_dir: Option<impl Into<PathBuf>>,
    ) -> Self {
        Self::new_with_options(
            session_root,
            roots,
            home_dir,
            SkillRegistryOptions::default(),
        )
    }

    #[must_use]
    pub fn new_with_options(
        session_root: Option<impl Into<PathBuf>>,
        roots: Option<Vec<String>>,
        home_dir: Option<impl Into<PathBuf>>,
        options: SkillRegistryOptions,
    ) -> Self {
        Self {
            session_root: canonicalize_existing(
                &session_root.map(Into::into).unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                }),
            ),
            roots: roots.unwrap_or_default(),
            extra_roots: Vec::new(),
            disabled_names: BTreeSet::new(),
            home_dir: canonicalize_existing(
                &home_dir.map(Into::into).unwrap_or_else(default_home_dir),
            ),
            include_builtin_skills: options.include_builtin_skills,
        }
    }

    #[must_use]
    pub fn with_extra_roots(mut self, roots: impl IntoIterator<Item = String>) -> Self {
        self.extra_roots = roots
            .into_iter()
            .map(|root| root.trim().to_string())
            .filter(|root| !root.is_empty())
            .collect();
        self
    }

    #[must_use]
    pub fn with_disabled_names(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.disabled_names = names
            .into_iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
        self
    }

    #[must_use]
    pub fn all(&self) -> Vec<SkillInfo> {
        self.discover()
            .documents
            .values()
            .map(|document| to_skill_info(document, None))
            .collect()
    }

    #[must_use]
    pub fn search(&self, query: &str, limit: Option<usize>) -> Vec<SkillInfo> {
        let terms = query_terms(query);
        if terms.is_empty() {
            let all = self.all();
            return limit.map_or(all.clone(), |limit| all.into_iter().take(limit).collect());
        }
        let mut scored = self
            .discover()
            .documents
            .values()
            .filter_map(|document| {
                let score = score_document(document, &terms);
                (score > 0).then(|| to_skill_info(document, Some(score)))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .score
                .unwrap_or(0)
                .cmp(&left.score.unwrap_or(0))
                .then_with(|| left.name.cmp(&right.name))
        });
        limit.map_or(scored.clone(), |limit| {
            scored.into_iter().take(limit).collect()
        })
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<SkillDocument> {
        self.discover().documents.remove(name.trim())
    }

    #[must_use]
    pub fn report(&self, query: Option<&str>, limit: Option<usize>) -> SkillDiscoveryReport {
        let discovery = self.discover();
        let mut skills = if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
            let terms = query_terms(query);
            discovery
                .documents
                .values()
                .filter_map(|document| {
                    let score = score_document(document, &terms);
                    (score > 0).then(|| to_skill_info(document, Some(score)))
                })
                .collect::<Vec<_>>()
        } else {
            discovery
                .documents
                .values()
                .map(|document| to_skill_info(document, None))
                .collect::<Vec<_>>()
        };
        if query.is_some() {
            skills.sort_by(|left, right| {
                right
                    .score
                    .unwrap_or(0)
                    .cmp(&left.score.unwrap_or(0))
                    .then_with(|| left.name.cmp(&right.name))
            });
        }
        if let Some(limit) = limit {
            skills.truncate(limit);
        }
        let invalid_count = discovery
            .issues
            .iter()
            .filter(|issue| issue.kind == "invalid")
            .count() as u64;
        let duplicate_count = discovery
            .issues
            .iter()
            .filter(|issue| issue.kind == "duplicate")
            .count() as u64;
        SkillDiscoveryReport {
            skills,
            scanned_files: discovery.scanned_files,
            loaded_count: discovery.documents.len() as u64,
            invalid_count,
            duplicate_count,
            issues: discovery.issues,
        }
    }

    fn discover(&self) -> DiscoveryResult {
        let mut documents: BTreeMap<String, SkillDocument> = BTreeMap::new();
        let mut issues = Vec::new();
        let mut scanned_files = 0u64;
        for path in self.iter_skill_files() {
            scanned_files += 1;
            let document = match load_skill_document(&path) {
                Ok(document) => document,
                Err(error) => {
                    issues.push(SkillIssue {
                        kind: "invalid".to_string(),
                        path: path_to_string(&path),
                        message: error,
                        duplicate_of: None,
                    });
                    continue;
                }
            };
            if self.disabled_names.contains(&document.name) {
                continue;
            }
            if let Some(existing) = documents.get(&document.name) {
                issues.push(SkillIssue {
                    kind: "duplicate".to_string(),
                    path: document.location.clone(),
                    message: format!("Duplicate skill name: {}", document.name),
                    duplicate_of: Some(existing.location.clone()),
                });
                continue;
            }
            documents.insert(document.name.clone(), document);
        }
        DiscoveryResult {
            documents,
            issues,
            scanned_files,
        }
    }

    fn iter_skill_files(&self) -> Vec<PathBuf> {
        if !self.roots.is_empty() {
            let mut result = self.iter_explicit_skill_files();
            append_skill_roots(&self.session_root, &self.extra_roots, &mut result);
            return result;
        }
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        for base in self.workspace_ancestors() {
            result.extend(iter_pattern_matches(&base, &mut seen));
        }
        result.extend(iter_pattern_matches(&self.home_dir, &mut seen));
        if self.include_builtin_skills {
            for root in builtin_skill_roots() {
                if root.is_dir() {
                    for path in recursive_skill_files(&root) {
                        if seen.insert(path.clone()) {
                            result.push(path);
                        }
                    }
                }
            }
        }
        append_skill_roots(&self.session_root, &self.extra_roots, &mut result);
        result
    }

    fn iter_explicit_skill_files(&self) -> Vec<PathBuf> {
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        for raw_root in &self.roots {
            let raw = PathBuf::from(raw_root);
            let root = if raw.is_absolute() {
                canonicalize_existing(&raw)
            } else {
                canonicalize_existing(&self.session_root.join(raw))
            };
            if root.is_file() && root.file_name().and_then(OsStr::to_str) == Some("SKILL.md") {
                if seen.insert(root.clone()) {
                    result.push(root);
                }
                continue;
            }
            if root.is_dir() {
                for path in recursive_skill_files(&root) {
                    if seen.insert(path.clone()) {
                        result.push(path);
                    }
                }
            }
        }
        result
    }

    fn workspace_ancestors(&self) -> Vec<PathBuf> {
        let current = if self.session_root.is_dir() {
            self.session_root.clone()
        } else {
            self.session_root
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.session_root.clone())
        };
        let mut result = Vec::new();
        for ancestor in current.ancestors() {
            if ancestor != self.home_dir {
                result.push(ancestor.to_path_buf());
            }
        }
        result
    }
}

struct DiscoveryResult {
    documents: BTreeMap<String, SkillDocument>,
    issues: Vec<SkillIssue>,
    scanned_files: u64,
}

pub fn load_skill_document(path: impl AsRef<Path>) -> Result<SkillDocument, String> {
    let skill_path = canonicalize_existing(path.as_ref());
    if !skill_path.is_file() {
        return Err(format!("Skill file not found: {}", skill_path.display()));
    }
    let text = fs::read_to_string(&skill_path).map_err(io_error)?;
    let parsed = parse_frontmatter(&text, &skill_path)?;
    let name = parsed
        .data
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let description = parsed
        .data
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(format!(
            "Skill file missing required frontmatter field 'name': {}",
            skill_path.display()
        ));
    }
    if description.is_empty() {
        return Err(format!(
            "Skill file missing required frontmatter field 'description': {}",
            skill_path.display()
        ));
    }
    let metadata = parsed
        .data
        .into_iter()
        .filter(|(key, _value)| key != "name" && key != "description")
        .collect::<BTreeMap<_, _>>();
    Ok(SkillDocument {
        name,
        description,
        location: path_to_string(&skill_path),
        directory: path_to_string(skill_path.parent().unwrap_or_else(|| Path::new(""))),
        metadata,
        score: None,
        content: parsed.content,
    })
}

#[must_use]
pub fn render_skill_document(document: &SkillDocument, include_header: bool) -> String {
    let mut lines = Vec::new();
    if include_header {
        lines.extend([
            format!("## Skill: {}", document.name),
            String::new(),
            format!("**Base directory**: {}", document.directory),
            String::new(),
        ]);
    }
    lines.push(document.content.clone());
    lines.join("\n").trim().to_string()
}

#[must_use]
pub fn render_available_skills(skills: &[SkillInfo]) -> Option<String> {
    let mut described = skills
        .iter()
        .filter(|skill| skill_info_model_invocable(skill))
        .filter(|skill| !skill.description.trim().is_empty())
        .collect::<Vec<_>>();
    if described.is_empty() {
        return None;
    }
    described.sort_by(|left, right| left.name.cmp(&right.name));
    let mut lines = vec![
        "Skills provide specialized instructions and workflows for specific tasks.".to_string(),
        "Use the skill tool to load a skill when a task matches its description.".to_string(),
        "<available_skills>".to_string(),
    ];
    for skill in described {
        lines.extend([
            "  <skill>".to_string(),
            format!("    <name>{}</name>", xml_escape(&skill.name)),
            format!(
                "    <description>{}</description>",
                xml_escape(&skill_display_description(skill))
            ),
            format!("    <location>{}</location>", xml_escape(&skill.location)),
            "  </skill>".to_string(),
        ]);
    }
    lines.push("</available_skills>".to_string());
    Some(lines.join("\n"))
}

#[must_use]
pub fn skill_info_model_invocable(skill: &SkillInfo) -> bool {
    skill_metadata_model_invocable(&skill.metadata)
}

#[must_use]
pub fn skill_document_model_invocable(skill: &SkillDocument) -> bool {
    skill_metadata_model_invocable(&skill.metadata)
}

#[must_use]
pub fn skill_display_description(skill: &SkillInfo) -> String {
    let description = skill.description.trim();
    let when_to_use = skill
        .metadata
        .get("when_to_use")
        .or_else(|| skill.metadata.get("when-to-use"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match when_to_use {
        Some(when_to_use) => format!("{description} When to use: {when_to_use}"),
        None => description.to_string(),
    }
}

fn skill_metadata_model_invocable(metadata: &BTreeMap<String, Value>) -> bool {
    if metadata_bool(metadata, "disable-model-invocation").unwrap_or(false)
        || metadata_bool(metadata, "disable_model_invocation").unwrap_or(false)
    {
        return false;
    }
    !matches!(
        metadata_bool(metadata, "user-invocable")
            .or_else(|| metadata_bool(metadata, "user_invocable")),
        Some(false)
    )
}

fn metadata_bool(metadata: &BTreeMap<String, Value>, key: &str) -> Option<bool> {
    let value = metadata.get(key)?;
    if let Some(value) = value.as_bool() {
        return Some(value);
    }
    value
        .as_str()
        .map(str::trim)
        .map(|value| match value.to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" | "on" => Some(true),
            "false" | "no" | "0" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(None)
}

#[must_use]
pub fn render_preloaded_skills(skills: &[SkillDocument]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut loaded = skills.iter().collect::<Vec<_>>();
    loaded.sort_by(|left, right| left.name.cmp(&right.name));
    let mut lines = vec![
        "The following skills are already loaded in this agent context.".to_string(),
        "<preloaded_skills>".to_string(),
    ];
    for skill in loaded {
        lines.extend([
            format!("  <skill name=\"{}\">", xml_escape(&skill.name)),
            format!(
                "    <description>{}</description>",
                xml_escape(&skill.description)
            ),
            format!("    <location>{}</location>", xml_escape(&skill.location)),
            format!(
                "    <base_directory>{}</base_directory>",
                xml_escape(&skill.directory)
            ),
            "    <skill_content>".to_string(),
            skill.content.trim().to_string(),
            "    </skill_content>".to_string(),
            "  </skill>".to_string(),
        ]);
    }
    lines.push("</preloaded_skills>".to_string());
    Some(lines.join("\n"))
}

#[must_use]
pub fn skill_context_items(
    preloaded: &[SkillDocument],
    available: &[SkillInfo],
) -> Vec<ContextItem> {
    let mut items = Vec::new();
    for skill in preloaded
        .iter()
        .filter(|skill| skill_document_model_invocable(skill))
    {
        let Some(content) = render_preloaded_skills(std::slice::from_ref(skill)) else {
            continue;
        };
        let mut item = ContextItem::new(
            format!("skill:preloaded:{}", skill.name),
            "skill_preloaded",
            format!("skill.document:{}", skill.name),
            content,
            CONTEXT_PRIORITY_SKILL_PRELOADED,
        );
        item.pinned = true;
        item.stable_prefix = true;
        item.delivery = ContextDelivery::TraceOnly;
        item.metadata = BTreeMap::from([
            ("name".to_string(), json!(skill.name)),
            ("description".to_string(), json!(skill.description)),
            ("location".to_string(), json!(skill.location)),
            ("embedded_in".to_string(), json!("agent_system_prompt")),
        ]);
        items.push(item);
    }
    for skill in available
        .iter()
        .filter(|skill| skill_info_model_invocable(skill))
    {
        let Some(content) = render_available_skills(std::slice::from_ref(skill)) else {
            continue;
        };
        let mut item = ContextItem::new(
            format!("skill:available:{}", skill.name),
            "skill_available",
            format!("skill.catalog:{}", skill.name),
            content,
            CONTEXT_PRIORITY_SKILL_CATALOG,
        );
        item.stable_prefix = true;
        item.delivery = ContextDelivery::TraceOnly;
        item.metadata = BTreeMap::from([
            ("name".to_string(), json!(skill.name)),
            ("description".to_string(), json!(skill.description)),
            ("location".to_string(), json!(skill.location)),
            ("embedded_in".to_string(), json!("agent_system_prompt")),
        ]);
        items.push(item);
    }
    items.sort_by(|left, right| left.id.cmp(&right.id));
    items
}

#[must_use]
pub fn tool_manifest_context_item(
    id: impl Into<String>,
    source: impl Into<String>,
    tool: &ToolSchema,
    metadata: BTreeMap<String, Value>,
) -> ContextItem {
    let mut item = ContextItem::new(
        id,
        "mcp_tool_manifest",
        source,
        stable_json_dumps(&json!({
            "name": tool.name,
            "description": tool.description,
            "schema": tool.schema,
            "group": tool.group,
            "dangerous": tool.dangerous,
        })),
        CONTEXT_PRIORITY_TOOL_MANIFEST,
    );
    item.stable_prefix = true;
    item.delivery = ContextDelivery::ToolManifest;
    item.metadata = metadata;
    item.metadata
        .insert("tool_name".to_string(), json!(tool.name));
    item.metadata
        .insert("tool_group".to_string(), json!(tool.group));
    item
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ScriptedLoopInput {
    pub user_text: String,
    pub script: Vec<ScriptedLoopCall>,
    pub tools: Vec<String>,
    #[serde(default)]
    pub options: BTreeMap<String, Value>,
    pub max_steps: u64,
    pub doom_loop_threshold: u64,
    #[serde(default)]
    pub reply_questions: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ScriptedLoopCall {
    #[serde(default)]
    pub events: Vec<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ScriptedLoopOutput {
    pub events: Vec<Value>,
    pub event_types: Vec<String>,
    pub model_call_count: u64,
    pub seen_tools_by_call: Vec<Vec<String>>,
    pub seen_max_output_tokens_by_call: Vec<Option<u64>>,
    pub pause_statuses: Vec<String>,
    pub final_session_status: String,
}

#[must_use]
pub fn run_scripted_agent_loop(input: &ScriptedLoopInput) -> ScriptedLoopOutput {
    let mut runner = ScriptedAgentLoopRunner::new(input);
    runner.run();
    runner.finish()
}

struct ScriptedAgentLoopRunner<'a> {
    input: &'a ScriptedLoopInput,
    script_index: usize,
    events: Vec<Value>,
    seen_tools_by_call: Vec<Vec<String>>,
    seen_max_output_tokens_by_call: Vec<Option<u64>>,
    pause_statuses: Vec<String>,
    doom_history: Vec<String>,
    snapshot_count: u64,
    text_count: u64,
    final_session_status: String,
}

impl<'a> ScriptedAgentLoopRunner<'a> {
    fn new(input: &'a ScriptedLoopInput) -> Self {
        Self {
            input,
            script_index: 0,
            events: Vec::new(),
            seen_tools_by_call: Vec::new(),
            seen_max_output_tokens_by_call: Vec::new(),
            pause_statuses: Vec::new(),
            doom_history: Vec::new(),
            snapshot_count: 0,
            text_count: 0,
            final_session_status: "running".to_string(),
        }
    }

    fn run(&mut self) {
        let max_retry = 1_u64;
        for step_index in 1..=self.input.max_steps {
            self.snapshot_count += 1;
            self.events.push(json!({
                "type": "step-start",
                "snapshot_id": format!("snapshot_{}", self.snapshot_count),
            }));

            let mut attempt = 0_u64;
            let step = loop {
                attempt += 1;
                self.seen_tools_by_call.push(self.input.tools.clone());
                self.seen_max_output_tokens_by_call.push(Some(256));
                let Some(call) = self.next_script_call() else {
                    break ModelStep::default();
                };
                if let Some(error) = &call.error {
                    if attempt <= max_retry {
                        continue;
                    }
                    self.events.push(json!({"type": "error", "error": error}));
                    self.final_session_status = "stop".to_string();
                    return;
                }
                break self.process_model_events(&call.events);
            };

            for call in &step.tool_calls {
                if self.record_doom_loop(call) {
                    let name = call.get("name").and_then(Value::as_str).unwrap_or_default();
                    let input_value = call.get("input").cloned().unwrap_or_else(|| json!({}));
                    self.events.push(json!({
                        "type": "error",
                        "error": format!(
                            "Detected repeated tool-call loop (threshold={}): {} {}",
                            self.input.doom_loop_threshold,
                            name,
                            stable_json_dumps(&input_value)
                        ),
                    }));
                    self.final_session_status = "stop".to_string();
                    return;
                }
                let name = call.get("name").and_then(Value::as_str).unwrap_or_default();
                if name == "question" {
                    self.emit_question_result(call);
                } else {
                    self.emit_fixture_echo_result(call);
                }
            }

            for warning in
                step_usage_warnings_from_options(&self.input.options, &step.usage, step_index)
            {
                self.events.push(warning);
            }

            let finish_reason = if !step.tool_calls.is_empty() && step.finish_reason == "unknown" {
                "tool_call".to_string()
            } else {
                step.finish_reason.clone()
            };
            self.events.push(json!({
                "type": "step-finish",
                "tokens": {
                    "input": step.usage.input_tokens,
                    "output": step.usage.output_tokens,
                },
                "cost": step.usage.cost,
                "finish_reason": finish_reason,
            }));

            if !step.tool_calls.is_empty() {
                continue;
            }
            if finish_reason == "stop" || step_index >= self.input.max_steps {
                self.final_session_status = "stop".to_string();
                return;
            }
        }
        self.events
            .push(json!({"type": "error", "error": "max_steps exceeded"}));
        self.final_session_status = "stop".to_string();
    }

    fn finish(self) -> ScriptedLoopOutput {
        let event_types = self
            .events
            .iter()
            .filter_map(|event| event.get("type").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        ScriptedLoopOutput {
            events: self.events,
            event_types,
            model_call_count: self.seen_tools_by_call.len() as u64,
            seen_tools_by_call: self.seen_tools_by_call,
            seen_max_output_tokens_by_call: self.seen_max_output_tokens_by_call,
            pause_statuses: self.pause_statuses,
            final_session_status: self.final_session_status,
        }
    }

    fn next_script_call(&mut self) -> Option<ScriptedLoopCall> {
        let call = self.input.script.get(self.script_index).cloned();
        self.script_index += usize::from(call.is_some());
        call
    }

    fn process_model_events(&mut self, events: &[Value]) -> ModelStep {
        let mut step = ModelStep::default();
        let mut text_started = false;
        let mut text_id = String::new();
        for event in events {
            match event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "text-delta" => {
                    if !text_started {
                        text_started = true;
                        self.text_count += 1;
                        text_id = format!("text_{}", self.text_count);
                        self.events.push(json!({
                            "type": "text-start",
                            "id": text_id,
                            "metadata": Value::Null,
                        }));
                    }
                    self.events.push(json!({
                        "type": "text-delta",
                        "id": text_id,
                        "text": event.get("text").and_then(Value::as_str).unwrap_or_default(),
                    }));
                }
                "tool-call" => {
                    let call = json!({
                        "type": "tool-call",
                        "call_id": event.get("call_id").and_then(Value::as_str).unwrap_or_default(),
                        "name": event.get("name").and_then(Value::as_str).unwrap_or_default(),
                        "input": event.get("input").cloned().unwrap_or_else(|| json!({})),
                    });
                    self.events.push(call.clone());
                    step.tool_calls.push(call);
                }
                "finish" => {
                    step.finish_reason = event
                        .get("finish_reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    step.usage = usage_from_loop_event(event.get("usage"));
                }
                _ => {}
            }
        }
        if text_started {
            self.events.push(json!({"type": "text-end", "id": text_id}));
        }
        step
    }

    fn record_doom_loop(&mut self, call: &Value) -> bool {
        let name = call.get("name").and_then(Value::as_str).unwrap_or_default();
        let input_value = call.get("input").cloned().unwrap_or_else(|| json!({}));
        let key = format!("{name}:{}", stable_json_dumps(&input_value));
        self.doom_history.push(key);
        let threshold = self.input.doom_loop_threshold as usize;
        if self.doom_history.len() > threshold {
            self.doom_history.remove(0);
        }
        self.doom_history.len() == threshold
            && self
                .doom_history
                .first()
                .is_some_and(|first| self.doom_history.iter().all(|item| item == first))
    }

    fn emit_fixture_echo_result(&mut self, call: &Value) {
        let call_id = call
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let input = call.get("input").cloned().unwrap_or_else(|| json!({}));
        let value = input.get("value").and_then(Value::as_str).unwrap_or("ok");
        let output = format!("echo:{value}");
        let original_bytes = output.len() as u64;
        self.events.push(json!({
            "type": "tool-result",
            "call_id": call_id,
            "output": output,
            "error": Value::Null,
            "metadata": {
                "context_preview": output,
                "kind": "fixture_echo",
                "original_bytes": original_bytes,
                "original_lines": 1,
                "output_truncated": false,
                "title": "Echo",
                "tool": "fixture_echo",
                "truncated": false,
            },
        }));
    }

    fn emit_question_result(&mut self, call: &Value) {
        let call_id = call
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let questions = call
            .get("input")
            .and_then(|input| input.get("questions"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        self.pause_statuses.push("paused".to_string());
        self.events.push(json!({
            "type": "question-request",
            "request_id": "question_1",
            "session_id": "session_fixture",
            "tool_call_id": call_id,
            "questions": questions,
        }));
        if !self.input.reply_questions {
            self.events.push(json!({
                "type": "tool-result",
                "call_id": call_id,
                "output": "",
                "error": "The user dismissed this question",
                "metadata": {
                    "questions": questions,
                    "request_id": "question_1",
                    "count": questions.as_array().map_or(0, Vec::len),
                    "error_kind": "question_rejected",
                    "tool": "question",
                    "title": "Asked 1 question",
                    "truncated": false,
                    "output_truncated": false,
                    "original_lines": 0,
                    "original_bytes": 0,
                },
            }));
            return;
        }
        let question_text = questions
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("question"))
            .and_then(Value::as_str)
            .unwrap_or("Question");
        let output = format!(
            "User has answered your questions: \"{question_text}\"=\"Fast path\". You can now continue with the user's answers in mind."
        );
        let original_bytes = output.len() as u64;
        self.events.push(json!({
            "type": "tool-result",
            "call_id": call_id,
            "output": output,
            "error": Value::Null,
            "metadata": {
                "answers": [["Fast path"]],
                "context_preview": output,
                "count": questions.as_array().map_or(0, Vec::len),
                "original_bytes": original_bytes,
                "original_lines": 1,
                "output_truncated": false,
                "questions": questions,
                "request_id": "question_1",
                "title": "Asked 1 question",
                "tool": "question",
                "truncated": false,
            },
        }));
    }
}

#[derive(Clone, Debug)]
struct ModelStep {
    tool_calls: Vec<Value>,
    finish_reason: String,
    usage: Usage,
}

impl Default for ModelStep {
    fn default() -> Self {
        Self {
            tool_calls: Vec::new(),
            finish_reason: "unknown".to_string(),
            usage: Usage::default(),
        }
    }
}

fn usage_from_loop_event(value: Option<&Value>) -> Usage {
    let Some(value) = value else {
        return Usage::default();
    };
    Usage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cost: value.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
    }
}

fn step_usage_warnings_from_options(
    options: &BTreeMap<String, Value>,
    usage: &Usage,
    step_index: u64,
) -> Vec<Value> {
    let Some(raw) = options.get("runtime_warnings").and_then(Value::as_object) else {
        return Vec::new();
    };
    let threshold = raw
        .get("max_step_total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let enabled = raw
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(threshold > 0);
    if !enabled || threshold == 0 {
        return Vec::new();
    }
    let total_tokens = usage.input_tokens + usage.output_tokens;
    if total_tokens <= threshold {
        return Vec::new();
    }
    let message = format!("Step total tokens exceeded budget: {total_tokens} > {threshold}.");
    vec![json!({
        "type": "runtime-warning",
        "severity": "warning",
        "code": "step_total_tokens_exceeded",
        "message": message,
        "metrics": {
            "step_index": step_index,
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": total_tokens,
            "cost": usage.cost,
            "threshold": threshold,
        },
        "display": {
            "kind": "runtime_warning",
            "severity": "warning",
            "title": "Step token budget exceeded",
            "body": message,
            "metrics": {
                "step_index": step_index,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "total_tokens": total_tokens,
                "threshold": threshold,
            },
        },
    })]
}

#[must_use]
pub fn estimate_text_tokens(text: &str, bytes_per_token: u64) -> u64 {
    let bytes_per_token = if bytes_per_token == 0 {
        DEFAULT_BYTES_PER_TOKEN
    } else {
        bytes_per_token
    };
    let byte_count = text.len() as u64;
    byte_count.div_ceil(bytes_per_token).max(1)
}

fn merge_compaction_facade_options(
    options: Option<&Value>,
) -> Result<BTreeMap<String, Value>, String> {
    let raw_options = options.and_then(Value::as_object);
    let mut raw_context = match raw_options.and_then(|items| items.get("context_budget")) {
        Some(Value::Null) | None => BTreeMap::new(),
        Some(Value::Object(items)) => items
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        Some(_) => return Err("AgentConfig.options['context_budget'] must be a dict.".to_string()),
    };
    let Some(raw_compaction) = raw_options.and_then(|items| items.get("compaction")) else {
        return Ok(raw_context);
    };
    let Value::Object(compaction) = raw_compaction else {
        return Err("AgentConfig.options['compaction'] must be a dict.".to_string());
    };
    let mut merged = BTreeMap::new();
    if let Some(value) = compaction.get("auto") {
        let auto = expect_bool(value, "auto", "compaction")?;
        merged.insert(
            "strategy".to_string(),
            Value::String(if auto { "auto" } else { "error" }.to_string()),
        );
    }
    if let Some(value) = compaction.get("prune") {
        merged.insert(
            "prune_old_tool_outputs".to_string(),
            Value::Bool(expect_bool(value, "prune", "compaction")?),
        );
    }
    if let Some(value) = compaction.get("reserved") {
        merged.insert(
            "input_safety_margin_tokens".to_string(),
            json!(expect_int(value, "reserved", 0, "compaction")?),
        );
    }
    if let Some(value) = compaction.get("mode") {
        let mode = expect_non_empty_string(value, "compaction.mode")?;
        merged.insert("compaction_mode".to_string(), Value::String(mode));
    }
    merged.append(&mut raw_context);
    Ok(merged)
}

fn expect_non_empty_string(value: &Value, field_name: &str) -> Result<String, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("{field_name} must be a non-empty string."))?
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(format!("{field_name} must be a non-empty string."));
    }
    Ok(text)
}

fn expect_bool(value: &Value, field_name: &str, prefix: &str) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| format!("{prefix}.{field_name} must be a bool."))
}

fn expect_int(value: &Value, field_name: &str, minimum: u64, prefix: &str) -> Result<u64, String> {
    let Some(number) = value.as_u64() else {
        return Err(format!("{prefix}.{field_name} must be an int."));
    };
    if number < minimum {
        return Err(format!("{prefix}.{field_name} must be >= {minimum}."));
    }
    Ok(number)
}

fn expect_float(
    value: &Value,
    field_name: &str,
    minimum: f64,
    maximum: f64,
    include_minimum: bool,
) -> Result<f64, String> {
    let Some(number) = value.as_f64() else {
        return Err(format!("context_budget.{field_name} must be a number."));
    };
    if include_minimum {
        if number < minimum {
            return Err(format!("context_budget.{field_name} must be >= {minimum}."));
        }
    } else if number <= minimum {
        return Err(format!("context_budget.{field_name} must be > {minimum}."));
    }
    if number > maximum {
        return Err(format!("context_budget.{field_name} must be <= {maximum}."));
    }
    Ok(number)
}

fn compute_input_limit_tokens(model: &Model, config: &ContextBudgetOptions) -> u64 {
    if config.use_safety_margin_tokens {
        let limit = model
            .context_window
            .saturating_sub(config.reserve_output_tokens)
            .saturating_sub(config.input_safety_margin_tokens);
        if limit > 0 || config.explicit_input_safety_margin_tokens {
            return limit;
        }
    }
    ((model.context_window as f64 * config.guard_ratio) as u64)
        .saturating_sub(config.reserve_output_tokens)
}

fn options_to_btree(options: Option<&Value>) -> BTreeMap<String, Value> {
    options
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(Map::iter)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn is_openai_compatible_model(model: &Model) -> bool {
    matches!(
        model.provider_id.as_str(),
        "openai" | "azure_openai" | "openai_compatible"
    )
}

fn estimate_payload_tokens(payload: &MaterializedPayload, bytes_per_token: u64) -> u64 {
    let serialized = serde_json::to_string(payload).unwrap_or_default();
    let bytes_per_token = bytes_per_token.max(1);
    (serialized.len() as u64).div_ceil(bytes_per_token).max(1)
}

struct ToolDiagnostics {
    tool_message_count: u64,
    largest_tool_message_tokens: u64,
    largest_tool_message_name: String,
}

fn tool_message_diagnostics(
    messages: &[ChatMessage],
    model: &Model,
    options: Option<&Value>,
    bytes_per_token: u64,
) -> ToolDiagnostics {
    let mut result = ToolDiagnostics {
        tool_message_count: 0,
        largest_tool_message_tokens: 0,
        largest_tool_message_name: String::new(),
    };
    for message in messages {
        if message.role != Role::Tool {
            continue;
        }
        result.tool_message_count += 1;
        let payload = materialize_openai_compatible_payload(
            None,
            std::slice::from_ref(message),
            &[],
            Some(model),
            Some(&options_to_btree(options)),
        );
        let estimate = estimate_payload_tokens(&payload, bytes_per_token);
        if estimate > result.largest_tool_message_tokens {
            result.largest_tool_message_tokens = estimate;
            result.largest_tool_message_name = message.name.clone().unwrap_or_default();
        }
    }
    result
}

fn work_state_item(
    work_state: Option<&ContextWorkState>,
    metadata: &BTreeMap<String, Value>,
    message_count: usize,
) -> Option<ContextItem> {
    if let Some(work_state) = work_state.filter(|item| !item.summary.trim().is_empty()) {
        let mut item = ContextItem::new(
            format!("work_state:{}", work_state.id),
            "work_state",
            work_state.source.clone(),
            format!("[Work state]\n{}", work_state.summary.trim()),
            CONTEXT_PRIORITY_WORK_STATE,
        );
        item.pinned = true;
        item.metadata = BTreeMap::from([
            ("work_state_id".to_string(), json!(work_state.id)),
            ("format".to_string(), json!(work_state.format)),
            (
                "message_position".to_string(),
                json!(work_state.message_position),
            ),
            (
                "compacted_until_message_id".to_string(),
                json!(work_state.compacted_until_message_id),
            ),
        ]);
        item.metadata.extend(work_state.metadata.clone());
        return Some(item);
    }
    let compaction = get_context_compaction(metadata, message_count)?;
    let summary = compaction.get("summary")?.as_str()?.to_string();
    let mut item = ContextItem::new(
        "work_state:context_compaction",
        "work_state",
        "session.metadata.context_compaction",
        summary,
        CONTEXT_PRIORITY_WORK_STATE,
    );
    item.pinned = true;
    item.metadata.insert(
        "compacted_until".to_string(),
        compaction
            .get("compacted_until")
            .cloned()
            .unwrap_or(Value::Null),
    );
    item.metadata.insert(
        "format".to_string(),
        compaction.get("format").cloned().unwrap_or(Value::Null),
    );
    item.metadata.insert(
        "schema_version".to_string(),
        compaction
            .get("schema_version")
            .cloned()
            .unwrap_or(Value::Null),
    );
    item.metadata.insert(
        "source".to_string(),
        compaction.get("source").cloned().unwrap_or(Value::Null),
    );
    Some(item)
}

fn get_context_compaction(
    metadata: &BTreeMap<String, Value>,
    message_count: usize,
) -> Option<BTreeMap<String, Value>> {
    let raw = metadata.get("context_compaction")?.as_object()?;
    let compacted_until = raw.get("compacted_until")?.as_u64()?;
    if compacted_until == 0 || compacted_until as usize > message_count {
        return None;
    }
    let summary = render_compaction_summary(raw)?;
    if summary.trim().is_empty() {
        return None;
    }
    let mut result = BTreeMap::new();
    result.insert(
        "summary".to_string(),
        Value::String(summary.trim().to_string()),
    );
    result.insert("compacted_until".to_string(), json!(compacted_until));
    result.insert(
        "updated_at".to_string(),
        raw.get("updated_at")
            .and_then(Value::as_u64)
            .map_or_else(|| json!(0), |value| json!(value)),
    );
    for key in ["schema_version", "format", "state", "source", "parse_error"] {
        if let Some(value) = raw.get(key) {
            result.insert(key.to_string(), value.clone());
        }
    }
    Some(result)
}

fn render_compaction_summary(raw: &Map<String, Value>) -> Option<String> {
    if raw.get("format").and_then(Value::as_str) == Some("structured_work_state")
        && let Some(state) = raw.get("state").and_then(Value::as_object)
    {
        return Some(render_work_state(&work_state_from_map(state)));
    }
    if let Some(summary) = raw.get("summary").and_then(Value::as_str)
        && !summary.trim().is_empty()
    {
        return Some(summary.trim().to_string());
    }
    raw.get("state")
        .and_then(Value::as_object)
        .map(|state| render_work_state(&work_state_from_map(state)))
}

fn work_state_from_map(state: &Map<String, Value>) -> WorkState {
    WorkState {
        task: string_field(state, "task"),
        progress: string_vec_field(state, "progress"),
        decisions: string_vec_field(state, "decisions"),
        files: state
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .map(|item| WorkStateFile {
                path: string_field(item, "path"),
                status: string_field(item, "status"),
                note: string_field(item, "note"),
            })
            .collect(),
        tool_findings: string_vec_field(state, "tool_findings"),
        todos: string_vec_field(state, "todos"),
        open_questions: string_vec_field(state, "open_questions"),
        blockers: string_vec_field(state, "blockers"),
        next_steps: string_vec_field(state, "next_steps"),
        risks: string_vec_field(state, "risks"),
    }
}

fn sandbox_item(execution: &Value) -> Option<ContextItem> {
    let execution = execution.as_object()?;
    let mode = execution.get("mode").and_then(Value::as_str)?.trim();
    if mode.is_empty() || mode == "local" {
        return None;
    }
    let mut safe_payload = BTreeMap::new();
    for key in ["mode", "sandbox_id", "remote_workdir"] {
        if let Some(value) = execution.get(key)
            && !value.is_null()
        {
            safe_payload.insert(key.to_string(), value.clone());
        }
    }
    let mut item = ContextItem::new(
        "sandbox:execution",
        "sandbox",
        "session.metadata.execution",
        format!(
            "[Sandbox context]\n{}",
            stable_json_dumps(&json!(safe_payload))
        ),
        CONTEXT_PRIORITY_SANDBOX,
    );
    item.pinned = true;
    item.stable_prefix = true;
    item.metadata = safe_payload;
    Some(item)
}

fn durable_goal_item(value: Option<&Value>) -> Option<ContextItem> {
    let goal = serde_json::from_value::<DurableGoal>(value?.clone()).ok()?;
    if goal.schema_version != DURABLE_GOAL_SCHEMA_VERSION
        || goal.objective.trim().is_empty()
        || goal.status == DurableGoalStatus::Completed
    {
        return None;
    }
    let criteria = goal
        .acceptance_criteria
        .iter()
        .filter(|criterion| !criterion.trim().is_empty())
        .map(|criterion| format!("- {}", criterion.trim()))
        .collect::<Vec<_>>();
    let mut content = format!(
        "[Durable goal]\nTitle: {}\nStatus: {}\nObjective: {}",
        goal.title.trim(),
        goal.status.as_str(),
        goal.objective.trim(),
    );
    if !criteria.is_empty() {
        content.push_str("\nAcceptance criteria:\n");
        content.push_str(&criteria.join("\n"));
    }
    let mut item = ContextItem::new(
        format!("goal:{}", goal.id),
        "goal",
        "session.metadata.durable_goal",
        content,
        CONTEXT_PRIORITY_GOAL,
    );
    item.pinned = true;
    item.metadata = BTreeMap::from([
        ("goal_id".to_string(), json!(goal.id)),
        ("status".to_string(), json!(goal.status.as_str())),
        ("revision".to_string(), json!(goal.revision)),
        ("updated_at_ms".to_string(), json!(goal.updated_at_ms)),
        (
            "acceptance_criteria_count".to_string(),
            json!(goal.acceptance_criteria.len()),
        ),
    ]);
    Some(item)
}

fn durable_plan_item(value: Option<&Value>) -> Option<ContextItem> {
    let plan = serde_json::from_value::<DurablePlan>(value?.clone()).ok()?;
    if plan.schema_version != DURABLE_PLAN_SCHEMA_VERSION
        || plan.objective.trim().is_empty()
        || plan.status == DurablePlanStatus::Completed
    {
        return None;
    }
    let steps = plan
        .steps
        .iter()
        .filter(|step| !step.trim().is_empty())
        .enumerate()
        .map(|(index, step)| format!("{}. {}", index + 1, step.trim()))
        .collect::<Vec<_>>();
    let mut content = format!(
        "[Durable plan]\nTitle: {}\nMode: {}\nObjective: {}",
        plan.title.trim(),
        plan.status.as_str(),
        plan.objective.trim(),
    );
    if plan.status == DurablePlanStatus::Planning {
        content.push_str(
            "\nConstraint: inspect and plan only; do not modify files or execute mutating tools.",
        );
    } else {
        content
            .push_str("\nInstruction: execute this plan and keep progress aligned with its steps.");
    }
    if !steps.is_empty() {
        content.push_str("\nSteps:\n");
        content.push_str(&steps.join("\n"));
    }
    let mut item = ContextItem::new(
        format!("plan:{}", plan.id),
        "plan",
        "session.metadata.durable_plan",
        content,
        CONTEXT_PRIORITY_PLAN,
    );
    item.pinned = true;
    item.metadata = BTreeMap::from([
        ("plan_id".to_string(), json!(plan.id)),
        ("status".to_string(), json!(plan.status.as_str())),
        ("revision".to_string(), json!(plan.revision)),
        ("updated_at_ms".to_string(), json!(plan.updated_at_ms)),
        ("step_count".to_string(), json!(plan.steps.len())),
        (
            "runtime_read_only".to_string(),
            json!(plan.status == DurablePlanStatus::Planning),
        ),
    ]);
    Some(item)
}

fn todo_items(todos: &[ContextTodo]) -> Vec<ContextItem> {
    todos
        .iter()
        .filter(|todo| !todo.content.trim().is_empty())
        .map(|todo| {
            let status = todo.status.trim().to_ascii_lowercase();
            let active = !matches!(status.as_str(), "completed" | "cancelled" | "canceled");
            let mut item = ContextItem::new(
                format!("todo:{}", todo.id),
                "todo",
                format!("session.todos:{}", todo.id),
                format!(
                    "[Todo id={} status={} priority={}]\n{}",
                    todo.id,
                    if status.is_empty() {
                        "pending"
                    } else {
                        status.as_str()
                    },
                    if todo.priority.trim().is_empty() {
                        "medium"
                    } else {
                        todo.priority.trim()
                    },
                    todo.content.trim()
                ),
                if active {
                    CONTEXT_PRIORITY_TODO
                } else {
                    CONTEXT_PRIORITY_MESSAGE
                },
            );
            item.pinned = active;
            item.metadata = BTreeMap::from([
                ("todo_id".to_string(), json!(todo.id)),
                ("status".to_string(), json!(todo.status)),
                ("priority".to_string(), json!(todo.priority)),
                ("active".to_string(), json!(active)),
            ]);
            item.metadata.extend(todo.metadata.clone());
            item
        })
        .collect()
}

fn checkpoint_items(checkpoints: &[ContextCheckpoint]) -> Vec<ContextItem> {
    checkpoints
        .iter()
        .filter(|checkpoint| !checkpoint.id.trim().is_empty())
        .map(|checkpoint| {
            let mut item = ContextItem::new(
                format!("checkpoint:{}", checkpoint.id),
                "checkpoint",
                format!("session.checkpoints:{}", checkpoint.id),
                format!(
                    "[Checkpoint id={} kind={} step={} files={} bytes={} restored={}]",
                    checkpoint.id,
                    checkpoint.kind,
                    checkpoint
                        .step_index
                        .map_or_else(|| "unknown".to_string(), |step| step.to_string()),
                    checkpoint.file_count,
                    checkpoint.total_bytes,
                    checkpoint.restored,
                ),
                CONTEXT_PRIORITY_CHECKPOINT,
            );
            item.pinned = checkpoint.restored;
            item.metadata = BTreeMap::from([
                ("checkpoint_id".to_string(), json!(checkpoint.id)),
                ("kind".to_string(), json!(checkpoint.kind)),
                ("run_id".to_string(), json!(checkpoint.run_id)),
                ("timestamp_ms".to_string(), json!(checkpoint.timestamp_ms)),
                ("message_id".to_string(), json!(checkpoint.message_id)),
                ("part_id".to_string(), json!(checkpoint.part_id)),
                ("step_index".to_string(), json!(checkpoint.step_index)),
                ("file_count".to_string(), json!(checkpoint.file_count)),
                ("total_bytes".to_string(), json!(checkpoint.total_bytes)),
                ("restored".to_string(), json!(checkpoint.restored)),
            ]);
            item.metadata.extend(checkpoint.metadata.clone());
            item
        })
        .collect()
}

fn message_and_session_context_items(
    messages: &[ChatMessage],
    attachments: &[ContextAttachment],
    work_state: Option<ContextItem>,
    goal: Option<ContextItem>,
    plan: Option<ContextItem>,
    todos: Vec<ContextItem>,
    checkpoints: Vec<ContextItem>,
) -> Vec<ContextItem> {
    let mut items = Vec::new();
    let mut attached = BTreeSet::new();
    let work_state_position = work_state
        .as_ref()
        .and_then(|item| {
            item.metadata
                .get("message_position")
                .and_then(Value::as_u64)
        })
        .map(|position| (position as usize).min(messages.len()));
    let session_context_position = messages
        .iter()
        .rposition(|message| message.role == Role::User)
        .unwrap_or(messages.len());
    let mut work_state = work_state;
    let mut session_items = Some(
        goal.into_iter()
            .chain(plan)
            .chain(todos)
            .chain(checkpoints)
            .collect::<Vec<_>>(),
    );
    for position in 0..=messages.len() {
        if work_state_position == Some(position)
            && let Some(item) = work_state.take()
        {
            items.push(item);
        }
        if session_context_position == position
            && let Some(context) = session_items.take()
        {
            items.extend(context);
        }
        let Some(message) = messages.get(position) else {
            continue;
        };
        let index = position;
        items.push(message_context_item(
            index,
            message,
            index == session_context_position && message.role == Role::User,
        ));
        for (attachment_index, attachment) in attachments
            .iter()
            .enumerate()
            .filter(|(_, attachment)| attachment.source_message_index == Some(index))
        {
            items.push(attachment_context_item(attachment, index, attachment_index));
            attached.insert(attachment_index);
        }
    }
    if let Some(item) = work_state {
        items.push(item);
    }
    if let Some(context) = session_items {
        items.extend(context);
    }
    for (attachment_index, attachment) in attachments.iter().enumerate() {
        if attached.contains(&attachment_index) {
            continue;
        }
        let source_message_index = attachment
            .source_message_index
            .unwrap_or(messages.len().saturating_sub(1));
        items.push(attachment_context_item(
            attachment,
            source_message_index,
            attachment_index,
        ));
    }
    items
}

fn message_context_item(index: usize, message: &ChatMessage, latest_user: bool) -> ContextItem {
    let kind = if message.role == Role::Tool {
        "tool_result"
    } else {
        "message"
    };
    let identifier = message
        .tool_call_id
        .clone()
        .unwrap_or_else(|| format!("{}:{index}", role_str(&message.role)));
    let mut metadata = BTreeMap::new();
    metadata.insert("role".to_string(), json!(role_str(&message.role)));
    metadata.insert(
        "name".to_string(),
        message.name.clone().map_or(Value::Null, Value::String),
    );
    metadata.insert(
        "tool_call_id".to_string(),
        message
            .tool_call_id
            .clone()
            .map_or(Value::Null, Value::String),
    );
    if !message.metadata.is_empty() {
        metadata.insert(
            "message_metadata".to_string(),
            serde_json::to_value(&message.metadata).unwrap_or_default(),
        );
    }
    let mut item = ContextItem::new(
        format!("{kind}:{identifier}"),
        kind,
        format!("session.messages[{index}]"),
        message.content.clone(),
        if kind == "tool_result" {
            CONTEXT_PRIORITY_TOOL_RESULT
        } else {
            CONTEXT_PRIORITY_MESSAGE
        },
    );
    if message.role == Role::System {
        item.priority = CONTEXT_PRIORITY_INSTRUCTION;
        item.pinned = true;
        item.stable_prefix = true;
    } else if latest_user {
        item.priority = CONTEXT_PRIORITY_RUNTIME;
        item.pinned = true;
    }
    item.metadata = metadata;
    item
}

fn attachment_context_item(
    attachment: &ContextAttachment,
    source_message_index: usize,
    attachment_index: usize,
) -> ContextItem {
    let kind = format!("attachment_{}", attachment.kind.as_str());
    let mut item = ContextItem::new(
        format!(
            "attachment:{}:message:{source_message_index}",
            attachment.id
        ),
        kind,
        format!("session.messages[{source_message_index}].attachments[{attachment_index}]"),
        render_context_attachment(attachment),
        CONTEXT_PRIORITY_ATTACHMENT,
    );
    item.pinned = true;
    item.metadata = BTreeMap::from([
        ("role".to_string(), json!("user")),
        ("attachment_id".to_string(), json!(attachment.id)),
        ("attachment_kind".to_string(), json!(attachment.kind)),
        (
            "context_attachment".to_string(),
            serde_json::to_value(attachment).unwrap_or_default(),
        ),
        (
            "source_message_index".to_string(),
            json!(source_message_index),
        ),
    ]);
    if attachment.truncated {
        item.metadata
            .insert("context_truncated".to_string(), Value::Bool(true));
        item.metadata.insert(
            "context_truncation_reason".to_string(),
            json!(
                attachment
                    .truncation_reason
                    .as_deref()
                    .unwrap_or("attachment_source_truncated")
            ),
        );
        item.metadata.insert(
            "context_truncation_strategy".to_string(),
            json!(if attachment.content.is_empty() {
                "attachment_metadata_only"
            } else {
                "attachment_source_head_tail"
            }),
        );
    }
    item
}

fn context_attachment_trace(item: &ContextItem) -> Option<ContextAttachmentTrace> {
    let attachment = item
        .metadata
        .get("context_attachment")
        .cloned()
        .and_then(|value| serde_json::from_value::<ContextAttachment>(value).ok())?;
    Some(ContextAttachmentTrace {
        id: attachment.id,
        kind: attachment.kind,
        name: attachment.name,
        content_type: attachment.content_type,
        size_bytes: attachment.size_bytes,
        source: attachment.source,
        page_count: attachment.page_count,
        media_metadata: attachment.media_metadata,
        source_truncated: attachment.truncated,
        source_truncation_reason: attachment.truncation_reason,
        original_content_bytes: attachment.original_content_bytes,
        included_content_bytes: attachment.included_content_bytes,
    })
}

fn render_context_attachment(attachment: &ContextAttachment) -> String {
    let label = attachment
        .path
        .as_deref()
        .filter(|value| !value.is_empty())
        .or_else(|| attachment.name.as_deref().filter(|value| !value.is_empty()))
        .unwrap_or(attachment.id.as_str());
    let header = format!(
        "[Attachment id={} kind={} path={} content_type={} size_bytes={}]",
        attachment.id,
        attachment.kind.as_str(),
        label,
        attachment.content_type,
        attachment.size_bytes
    );
    if attachment.content.is_empty() {
        header
    } else {
        format!("{header}\n{}", attachment.content)
    }
}

fn item_to_message(item: &ContextItem) -> ChatMessage {
    let role = match item.metadata.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some("tool") => Role::Tool,
        _ => Role::System,
    };
    let mut metadata = if matches!(item.kind.as_str(), "message" | "tool_result") {
        item.metadata
            .get("message_metadata")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    } else {
        BTreeMap::from([
            ("synthetic_context_item".to_string(), json!(true)),
            ("context_item_id".to_string(), json!(item.id)),
            ("context_item_kind".to_string(), json!(item.kind)),
            ("context_item_source".to_string(), json!(item.source)),
        ])
    };
    if let Some(attachment) = item.metadata.get("context_attachment") {
        metadata.insert("context_attachment".to_string(), attachment.clone());
    }
    ChatMessage {
        role,
        content: item.content.clone(),
        name: item
            .metadata
            .get("name")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        tool_call_id: item
            .metadata
            .get("tool_call_id")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        metadata,
    }
}

fn item_rank(item: &ContextItem) -> (u8, i64, u64) {
    (u8::from(item.pinned), item.priority, item.token_estimate)
}

fn parse_frontmatter(text: &str, path: &Path) -> Result<ParsedFrontmatter, String> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some("---") {
        return Err(format!(
            "Skill file missing YAML frontmatter: {}",
            path.display()
        ));
    }
    let Some(closing_index) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (line.trim() == "---").then_some(index))
    else {
        return Err(format!(
            "Skill file has unterminated YAML frontmatter: {}",
            path.display()
        ));
    };
    let frontmatter_text = lines[1..closing_index].join("\n");
    let body = lines[closing_index + 1..].join("\n");
    let data = serde_yaml::from_str::<serde_yaml::Value>(&frontmatter_text).map_err(|error| {
        format!(
            "Failed to parse skill frontmatter: {}: {error}",
            path.display()
        )
    })?;
    let serde_yaml::Value::Mapping(mapping) = data else {
        return Err(format!(
            "Skill frontmatter must be a YAML object: {}",
            path.display()
        ));
    };
    let mut normalized = BTreeMap::new();
    for (key, value) in mapping {
        let key = match key {
            serde_yaml::Value::String(key) => key,
            other => serde_yaml::to_string(&other)
                .unwrap_or_default()
                .trim()
                .to_string(),
        };
        let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
        normalized.insert(key, value);
    }
    Ok(ParsedFrontmatter {
        data: normalized,
        content: body,
    })
}

struct ParsedFrontmatter {
    data: BTreeMap<String, Value>,
    content: String,
}

fn iter_pattern_matches(base_dir: &Path, seen: &mut BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for parts in [
        [".openagent", "skill"],
        [".openagent", "skills"],
        [".agents", "skill"],
        [".agents", "skills"],
        [".opencode", "skill"],
        [".opencode", "skills"],
        [".claude", "skills"],
    ] {
        let candidate = base_dir.join(parts[0]).join(parts[1]);
        if !candidate.is_dir() {
            continue;
        }
        for path in recursive_skill_files(&candidate) {
            if seen.insert(path.clone()) {
                result.push(path);
            }
        }
    }
    result
}

fn builtin_skill_roots() -> Vec<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![canonicalize_existing(
        &manifest_dir.join("../skill/openagent"),
    )]
}

fn append_skill_roots(session_root: &Path, roots: &[String], result: &mut Vec<PathBuf>) {
    let mut seen = result.iter().cloned().collect::<BTreeSet<_>>();
    for raw_root in roots {
        let raw = PathBuf::from(raw_root);
        let root = if raw.is_absolute() {
            canonicalize_existing(&raw)
        } else {
            canonicalize_existing(&session_root.join(raw))
        };
        for path in recursive_skill_files(&root) {
            if seen.insert(path.clone()) {
                result.push(path);
            }
        }
    }
}

fn recursive_skill_files(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut visited_directories = BTreeSet::new();
    let mut seen_files = BTreeSet::new();
    collect_skill_files(
        &canonicalize_existing(root),
        &mut visited_directories,
        &mut seen_files,
        &mut result,
    );
    result
}

fn collect_skill_files(
    root: &Path,
    visited_directories: &mut BTreeSet<PathBuf>,
    seen_files: &mut BTreeSet<PathBuf>,
    result: &mut Vec<PathBuf>,
) {
    let resolved = canonicalize_existing(root);
    if resolved.is_file() {
        if resolved.file_name().and_then(OsStr::to_str) == Some("SKILL.md")
            && seen_files.insert(resolved.clone())
        {
            result.push(resolved);
        }
        return;
    }
    if !resolved.is_dir() || !visited_directories.insert(resolved.clone()) {
        return;
    }
    let mut entries = read_dir_paths(&resolved);
    entries.sort();
    for entry in entries {
        collect_skill_files(&entry, visited_directories, seen_files, result);
    }
}

fn to_skill_info(document: &SkillDocument, score: Option<i64>) -> SkillInfo {
    SkillInfo {
        name: document.name.clone(),
        description: document.description.clone(),
        location: document.location.clone(),
        directory: document.directory.clone(),
        metadata: document.metadata.clone(),
        score,
    }
}

fn query_terms(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .replace(['_', '-'], " ")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn score_document(document: &SkillDocument, terms: &[String]) -> i64 {
    let name = document.name.to_lowercase();
    let description = document.description.to_lowercase();
    let content = document.content.to_lowercase();
    let metadata_text = document
        .metadata
        .iter()
        .map(|(key, value)| format!("{key} {value}"))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut score = 0;
    for term in terms {
        if name.contains(term) {
            score += 8;
        }
        if description.contains(term) {
            score += 5;
        }
        if metadata_text.contains(term) {
            score += 3;
        }
        if content.contains(term) {
            score += 1;
        }
    }
    score
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let regex = format!("^{}$", glob_to_regex(pattern));
    Regex::new(&regex)
        .map(|regex| regex.is_match(text))
        .unwrap_or(false)
}

fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::new();
    for ch in pattern.chars() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '[' | ']' | '{' | '}' | '\\' => {
                regex.push('\\');
                regex.push(ch);
            }
            other => regex.push(other),
        }
    }
    regex
}

fn stable_json_dumps(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_default(),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(stable_json_dumps)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(items) => {
            let mut keys = items.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| {
                        let value = items.get(key).unwrap_or(&Value::Null);
                        format!(
                            "{}: {}",
                            serde_json::to_string(key).unwrap_or_default(),
                            stable_json_dumps(value)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

fn string_field(state: &Map<String, Value>, key: &str) -> String {
    state
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn string_vec_field(state: &Map<String, Value>, key: &str) -> Vec<String> {
    state
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn role_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn read_dir_paths(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect()
}

fn canonicalize_existing(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn default_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn io_error(error: io::Error) -> String {
    error.to_string()
}

fn sha1_hex_12(value: &str) -> String {
    sha1_hex(value).chars().take(12).collect()
}

fn stable_context_attachment_id(attachment: &ContextAttachment) -> String {
    let content_hash = sha1_hex(&attachment.content);
    let identity = stable_json_dumps(&json!({
        "kind": attachment.kind,
        "path": attachment.path,
        "name": attachment.name,
        "content_type": attachment.content_type,
        "size_bytes": attachment.size_bytes,
        "content_hash": content_hash,
    }));
    format!(
        "att_{}",
        sha1_hex(&identity).chars().take(16).collect::<String>()
    )
}

fn sha1_hex(value: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    format!("{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_to_protocol_crate() {
        assert_eq!(crate_name(), "openagent-core");
        assert_eq!(protocol_crate_name(), "openagent-protocol");
    }
}

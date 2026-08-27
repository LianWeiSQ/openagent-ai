use std::{collections::BTreeMap, error::Error, fmt};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::TraceContext;

pub const TASK_CONTRACT_SCHEMA_VERSION: &str = "openharness.task.v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSurface {
    Bridge,
    Cli,
    Tui,
    Http,
    Swarm,
    Eval,
    Subagent,
}

impl RunSurface {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Bridge => "bridge",
            Self::Cli => "cli",
            Self::Tui => "tui",
            Self::Http => "http",
            Self::Swarm => "swarm",
            Self::Eval => "eval",
            Self::Subagent => "subagent",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Queued,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl ExecutionState {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Success,
    Degraded,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunOutcome {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeReason {
    None,
    ProviderFallback,
    ProviderExhausted,
    ContextTruncated,
    PartialToolFailure,
    ToolDenied,
    ToolTimeout,
    McpUnavailable,
    DeadlineExceeded,
    BudgetExhausted,
    LoopLimit,
    QueueTimeout,
    UserCancelled,
    ProcessInterrupted,
    InvalidResponse,
    InternalError,
    Other,
}

impl OutcomeReason {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ProviderFallback => "provider_fallback",
            Self::ProviderExhausted => "provider_exhausted",
            Self::ContextTruncated => "context_truncated",
            Self::PartialToolFailure => "partial_tool_failure",
            Self::ToolDenied => "tool_denied",
            Self::ToolTimeout => "tool_timeout",
            Self::McpUnavailable => "mcp_unavailable",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::BudgetExhausted => "budget_exhausted",
            Self::LoopLimit => "loop_limit",
            Self::QueueTimeout => "queue_timeout",
            Self::UserCancelled => "user_cancelled",
            Self::ProcessInterrupted => "process_interrupted",
            Self::InvalidResponse => "invalid_response",
            Self::InternalError => "internal_error",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct RuntimeBudgets {
    pub profile: Option<String>,
    pub source: Option<String>,
    pub deadline_at_ms: Option<u64>,
    pub max_elapsed_ms: Option<u64>,
    pub max_steps: Option<u64>,
    pub max_total_tokens: Option<u64>,
    pub max_cost_microunits: Option<u64>,
    pub max_tool_calls: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentIdentity {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelIdentity {
    pub provider: String,
    pub model: String,
    pub family: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VersionIdentity {
    pub harness_version: String,
    pub agent_version: String,
    pub prompt_version: String,
    pub skill_set_version: String,
    pub tool_set_version: String,
    pub config_fingerprint: String,
}

impl VersionIdentity {
    #[must_use]
    pub fn current_harness(
        agent_version: impl Into<String>,
        prompt_version: impl Into<String>,
        skill_set_version: impl Into<String>,
        tool_set_version: impl Into<String>,
        config: &Value,
    ) -> Self {
        Self {
            harness_version: env!("CARGO_PKG_VERSION").to_string(),
            agent_version: agent_version.into(),
            prompt_version: prompt_version.into(),
            skill_set_version: skill_set_version.into(),
            tool_set_version: tool_set_version.into(),
            config_fingerprint: canonical_json_fingerprint(config),
        }
    }

    #[must_use]
    pub fn trace_attributes(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "openharness.version".to_string(),
                self.harness_version.clone(),
            ),
            ("agent.version".to_string(), self.agent_version.clone()),
            ("prompt.version".to_string(), self.prompt_version.clone()),
            (
                "skill_set.version".to_string(),
                self.skill_set_version.clone(),
            ),
            (
                "tool_set.version".to_string(),
                self.tool_set_version.clone(),
            ),
            (
                "config.fingerprint".to_string(),
                self.config_fingerprint.clone(),
            ),
        ])
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskContractV1 {
    pub schema_version: String,
    pub task_id: String,
    pub session_id: String,
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub surface: RunSurface,
    pub agent: AgentIdentity,
    pub model: ModelIdentity,
    pub versions: VersionIdentity,
    pub budgets: RuntimeBudgets,
    pub trace: TraceContext,
    pub created_at_ms: u64,
}

impl TaskContractV1 {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        surface: RunSurface,
        agent: AgentIdentity,
        model: ModelIdentity,
        versions: VersionIdentity,
        budgets: RuntimeBudgets,
        trace: TraceContext,
        created_at_ms: u64,
    ) -> Self {
        Self {
            schema_version: TASK_CONTRACT_SCHEMA_VERSION.to_string(),
            task_id: task_id.into(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            parent_run_id: None,
            idempotency_key: None,
            surface,
            agent,
            model,
            versions,
            budgets,
            trace,
            created_at_ms,
        }
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != TASK_CONTRACT_SCHEMA_VERSION {
            return Err(ContractError::UnsupportedSchema(
                self.schema_version.clone(),
            ));
        }
        for (field, value) in [
            ("task_id", self.task_id.as_str()),
            ("session_id", self.session_id.as_str()),
            ("run_id", self.run_id.as_str()),
            ("agent.name", self.agent.name.as_str()),
            ("agent.version", self.agent.version.as_str()),
            ("model.provider", self.model.provider.as_str()),
            ("model.model", self.model.model.as_str()),
            ("model.family", self.model.family.as_str()),
            ("harness.version", self.versions.harness_version.as_str()),
            ("prompt.version", self.versions.prompt_version.as_str()),
            (
                "skill_set.version",
                self.versions.skill_set_version.as_str(),
            ),
            ("tool_set.version", self.versions.tool_set_version.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ContractError::MissingField(field));
            }
        }
        if self.versions.config_fingerprint.len() != 64
            || !self
                .versions
                .config_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ContractError::InvalidConfigFingerprint);
        }
        self.trace
            .validate()
            .map_err(|error| ContractError::InvalidTraceContext(error.to_string()))?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContractError {
    UnsupportedSchema(String),
    MissingField(&'static str),
    InvalidConfigFingerprint,
    InvalidTraceContext(String),
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema(value) => write!(formatter, "unsupported task schema: {value}"),
            Self::MissingField(field) => write!(formatter, "task contract field is empty: {field}"),
            Self::InvalidConfigFingerprint => {
                formatter.write_str("invalid configuration fingerprint")
            }
            Self::InvalidTraceContext(error) => write!(formatter, "invalid trace context: {error}"),
        }
    }
}

impl Error for ContractError {}

#[must_use]
pub fn canonical_json_fingerprint(value: &Value) -> String {
    let canonical = canonicalize(value);
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Object(items) => {
            let mut keys = items.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut result = Map::new();
            for key in keys {
                if let Some(value) = items.get(key) {
                    result.insert(key.clone(), canonicalize(value));
                }
            }
            Value::Object(result)
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_fingerprint_ignores_object_key_order() {
        assert_eq!(
            canonical_json_fingerprint(&json!({"b": 2, "a": {"d": 4, "c": 3}})),
            canonical_json_fingerprint(&json!({"a": {"c": 3, "d": 4}, "b": 2}))
        );
    }
}

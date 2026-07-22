use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{SemanticAnchorRegistry, WorkState};

pub const CONTEXT_EPOCH_SCHEMA_VERSION: &str = "openagent.context_epoch.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEpochTrigger {
    Manual,
    Automatic,
}

impl ContextEpochTrigger {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEpochFormat {
    SessionSummaryV1,
    StructuredWorkState,
}

impl ContextEpochFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionSummaryV1 => "session_summary_v1",
            Self::StructuredWorkState => "structured_work_state",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextEpoch {
    pub schema_version: String,
    pub epoch_id: String,
    pub session_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_epoch_id: Option<String>,
    pub trigger: ContextEpochTrigger,
    pub reason: String,
    pub format: ContextEpochFormat,
    pub source: String,
    pub created_at_ms: u64,
    pub compacted_message_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_until_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_pack_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_token_estimate: Option<u64>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<WorkState>,
    #[serde(default, skip_serializing_if = "SemanticAnchorRegistry::is_empty")]
    pub anchor_registry: SemanticAnchorRegistry,
}

impl ContextEpoch {
    #[must_use]
    pub fn manual(
        epoch_id: impl Into<String>,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        created_at_ms: u64,
        compacted_message_count: u64,
        compacted_until_message_id: Option<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: CONTEXT_EPOCH_SCHEMA_VERSION.to_string(),
            epoch_id: epoch_id.into(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            parent_epoch_id: None,
            trigger: ContextEpochTrigger::Manual,
            reason: "manual_request".to_string(),
            format: ContextEpochFormat::SessionSummaryV1,
            source: "manual_session_compaction_v1".to_string(),
            created_at_ms,
            compacted_message_count,
            compacted_until_message_id,
            boundary_message_id: None,
            before_pack_hash: None,
            step: None,
            summary_token_estimate: None,
            summary: summary.into(),
            state: None,
            anchor_registry: SemanticAnchorRegistry::default(),
        }
    }

    #[must_use]
    pub fn into_automatic(
        mut self,
        reason: impl Into<String>,
        before_pack_hash: impl Into<String>,
        step: u64,
        summary_token_estimate: u64,
        state: WorkState,
    ) -> Self {
        self.trigger = ContextEpochTrigger::Automatic;
        self.reason = reason.into();
        self.format = ContextEpochFormat::StructuredWorkState;
        self.source = "runtime_auto_compaction_v1".to_string();
        self.before_pack_hash = Some(before_pack_hash.into());
        self.step = Some(step);
        self.summary_token_estimate = Some(summary_token_estimate);
        self.state = Some(state);
        self
    }

    #[must_use]
    pub fn with_anchor_registry(mut self, anchor_registry: SemanticAnchorRegistry) -> Self {
        self.anchor_registry = anchor_registry;
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CONTEXT_EPOCH_SCHEMA_VERSION {
            return Err(format!(
                "unsupported context epoch schema: {}",
                self.schema_version
            ));
        }
        for (field, value) in [
            ("epoch_id", self.epoch_id.as_str()),
            ("session_id", self.session_id.as_str()),
            ("run_id", self.run_id.as_str()),
            ("reason", self.reason.as_str()),
            ("source", self.source.as_str()),
            ("summary", self.summary.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("context epoch {field} must not be empty"));
            }
        }
        if self.compacted_message_count == 0 && self.compacted_until_message_id.is_some() {
            return Err("empty context epoch must not reference a compacted message".to_string());
        }
        if self.compacted_message_count > 0
            && self
                .compacted_until_message_id
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err("non-empty context epoch requires compacted_until_message_id".to_string());
        }
        for (field, value) in [
            ("parent_epoch_id", self.parent_epoch_id.as_deref()),
            ("boundary_message_id", self.boundary_message_id.as_deref()),
            ("before_pack_hash", self.before_pack_hash.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(format!("context epoch {field} must not be empty"));
            }
        }
        if self.format == ContextEpochFormat::StructuredWorkState && self.state.is_none() {
            return Err("structured context epoch requires work state".to_string());
        }
        self.anchor_registry.validate()?;
        if self.trigger == ContextEpochTrigger::Automatic {
            if self.format != ContextEpochFormat::StructuredWorkState {
                return Err("automatic context epoch requires structured work state".to_string());
            }
            if self.before_pack_hash.is_none()
                || self.step.is_none()
                || self.summary_token_estimate.is_none()
            {
                return Err(
                    "automatic context epoch requires pack hash, step, and summary token estimate"
                        .to_string(),
                );
            }
        }
        Ok(())
    }

    pub fn validate_boundary(&self) -> Result<(), String> {
        self.validate()?;
        if self.compacted_message_count == 0 {
            return Err("context epoch boundary requires compacted messages".to_string());
        }
        if self.boundary_message_id.is_none() {
            return Err("context epoch boundary requires boundary_message_id".to_string());
        }
        Ok(())
    }

    #[must_use]
    pub fn is_current(&self) -> bool {
        self.schema_version == CONTEXT_EPOCH_SCHEMA_VERSION
    }

    #[must_use]
    pub fn diagnostics(&self) -> BTreeMap<String, Value> {
        let mut diagnostics = BTreeMap::from([
            ("schema_version".to_string(), json!(self.schema_version)),
            ("epoch_id".to_string(), json!(self.epoch_id)),
            ("session_id".to_string(), json!(self.session_id)),
            ("run_id".to_string(), json!(self.run_id)),
            ("trigger".to_string(), json!(self.trigger)),
            ("reason".to_string(), json!(self.reason)),
            ("format".to_string(), json!(self.format)),
            ("source".to_string(), json!(self.source)),
            ("created_at_ms".to_string(), json!(self.created_at_ms)),
            (
                "compacted_message_count".to_string(),
                json!(self.compacted_message_count),
            ),
            (
                "compacted_until_message_id".to_string(),
                json!(self.compacted_until_message_id),
            ),
            (
                "boundary_message_id".to_string(),
                json!(self.boundary_message_id),
            ),
            (
                "summary_chars".to_string(),
                json!(self.summary.chars().count() as u64),
            ),
        ]);
        for (key, value) in [
            ("parent_epoch_id", json!(self.parent_epoch_id)),
            ("before_pack_hash", json!(self.before_pack_hash)),
            ("step", json!(self.step)),
            ("summary_token_estimate", json!(self.summary_token_estimate)),
        ] {
            if !value.is_null() {
                diagnostics.insert(key.to_string(), value);
            }
        }
        let anchor_diagnostics = self.anchor_registry.diagnostics();
        diagnostics.insert(
            "semantic_anchor_registry_schema_version".to_string(),
            json!(anchor_diagnostics.schema_version),
        );
        diagnostics.insert(
            "semantic_anchor_registry_hash".to_string(),
            json!(anchor_diagnostics.registry_hash),
        );
        diagnostics.insert(
            "semantic_anchor_count".to_string(),
            json!(anchor_diagnostics.anchor_count),
        );
        diagnostics.insert(
            "semantic_anchor_kind_counts".to_string(),
            json!(anchor_diagnostics.kind_counts),
        );
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_epoch_validates_boundary_shape_and_redacts_diagnostics() {
        let epoch = ContextEpoch::manual(
            "epoch-1",
            "session-1",
            "run-1",
            10,
            2,
            Some("message-2".to_string()),
            "private summary body",
        );

        let mut epoch = epoch;
        epoch.boundary_message_id = Some("message-epoch-1".to_string());

        assert!(epoch.validate_boundary().is_ok());
        let diagnostics = epoch.diagnostics();
        assert_eq!(diagnostics["schema_version"], CONTEXT_EPOCH_SCHEMA_VERSION);
        assert_eq!(diagnostics["summary_chars"], 20);
        assert!(!diagnostics.contains_key("summary"));
        assert!(!diagnostics.contains_key("state"));
    }

    #[test]
    fn context_epoch_rejects_incomplete_or_unknown_contracts() {
        let mut epoch =
            ContextEpoch::manual("epoch-1", "session-1", "run-1", 10, 1, None, "summary");
        assert_eq!(
            epoch.validate().expect_err("missing boundary is invalid"),
            "non-empty context epoch requires compacted_until_message_id"
        );

        epoch.compacted_until_message_id = Some("message-1".to_string());
        epoch.schema_version = "openagent.context_epoch.v0".to_string();
        assert!(epoch.validate().is_err());
        assert!(!epoch.is_current());
    }
}

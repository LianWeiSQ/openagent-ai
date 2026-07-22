use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::{WorkState, WorkStateFile};

pub const SEMANTIC_ANCHOR_SCHEMA_VERSION: &str = "openagent.semantic_anchor.v1";
pub const SEMANTIC_ANCHOR_REGISTRY_SCHEMA_VERSION: &str = "openagent.semantic_anchor_registry.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticAnchorKind {
    Goal,
    Constraint,
    Decision,
    Progress,
    File,
    CriticalContext,
    Blocker,
    NextStep,
    RecoveryPoint,
}

impl SemanticAnchorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::Constraint => "constraint",
            Self::Decision => "decision",
            Self::Progress => "progress",
            Self::File => "file",
            Self::CriticalContext => "critical_context",
            Self::Blocker => "blocker",
            Self::NextStep => "next_step",
            Self::RecoveryPoint => "recovery_point",
        }
    }

    #[must_use]
    pub const fn default_priority(self) -> i64 {
        match self {
            Self::Goal | Self::Constraint => 94,
            Self::Decision | Self::Blocker => 92,
            Self::CriticalContext => 90,
            Self::File | Self::RecoveryPoint => 88,
            Self::NextStep => 86,
            Self::Progress => 82,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticAnchorScope {
    Session,
    Epoch,
}

impl SemanticAnchorScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Epoch => "epoch",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticAnchorAuthority {
    SessionMessage,
    StructuredWorkState,
    Todo,
    Checkpoint,
    ContextEpoch,
    Explicit,
}

impl SemanticAnchorAuthority {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionMessage => "session_message",
            Self::StructuredWorkState => "structured_work_state",
            Self::Todo => "todo",
            Self::Checkpoint => "checkpoint",
            Self::ContextEpoch => "context_epoch",
            Self::Explicit => "explicit",
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Self::SessionMessage => 10,
            Self::StructuredWorkState | Self::Todo | Self::Checkpoint => 20,
            Self::ContextEpoch => 30,
            Self::Explicit => 40,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticAnchor {
    pub schema_version: String,
    pub id: String,
    pub kind: SemanticAnchorKind,
    pub scope: SemanticAnchorScope,
    pub authority: SemanticAnchorAuthority,
    pub source: String,
    pub content: String,
    pub content_hash: String,
    pub priority: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
}

impl SemanticAnchor {
    #[must_use]
    pub fn new(
        kind: SemanticAnchorKind,
        key: impl AsRef<str>,
        content: impl Into<String>,
        authority: SemanticAnchorAuthority,
        scope: SemanticAnchorScope,
        source: impl Into<String>,
    ) -> Self {
        let content = normalize_content(&content.into());
        let identity = normalize_identity(key.as_ref());
        let suffix = if kind == SemanticAnchorKind::Goal && identity == "primary" {
            "primary".to_string()
        } else {
            sha1_hex(format!("{}:{identity}", kind.as_str()).as_bytes())
                .chars()
                .take(20)
                .collect()
        };
        Self {
            schema_version: SEMANTIC_ANCHOR_SCHEMA_VERSION.to_string(),
            id: format!("anchor:{}:{suffix}", kind.as_str()),
            kind,
            scope,
            authority,
            source: source.into().trim().to_string(),
            content_hash: content_hash(&content),
            content,
            priority: kind.default_priority(),
            references: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_priority(mut self, priority: i64) -> Self {
        self.priority = priority;
        self
    }

    #[must_use]
    pub fn with_references(mut self, references: Vec<String>) -> Self {
        self.references = references
            .into_iter()
            .map(|reference| reference.trim().to_string())
            .filter(|reference| !reference.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SEMANTIC_ANCHOR_SCHEMA_VERSION {
            return Err(format!(
                "unsupported semantic anchor schema: {}",
                self.schema_version
            ));
        }
        for (field, value) in [
            ("id", self.id.as_str()),
            ("source", self.source.as_str()),
            ("content", self.content.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("semantic anchor {field} must not be empty"));
            }
        }
        let prefix = format!("anchor:{}:", self.kind.as_str());
        if !self.id.starts_with(&prefix) {
            return Err("semantic anchor id does not match kind".to_string());
        }
        let suffix = self.id.strip_prefix(&prefix).unwrap_or_default();
        if (suffix == "primary" && self.kind != SemanticAnchorKind::Goal)
            || (suffix != "primary"
                && (suffix.len() != 20 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())))
        {
            return Err("semantic anchor id is not canonical".to_string());
        }
        if self.content_hash != content_hash(&self.content) {
            return Err("semantic anchor content hash mismatch".to_string());
        }
        if !is_sha1_hash(&self.content_hash) {
            return Err("semantic anchor content hash is not canonical sha1".to_string());
        }
        if self
            .references
            .iter()
            .any(|reference| reference.trim().is_empty())
        {
            return Err("semantic anchor reference must not be empty".to_string());
        }
        Ok(())
    }

    #[must_use]
    pub fn diagnostics(&self) -> SemanticAnchorDiagnostics {
        SemanticAnchorDiagnostics {
            schema_version: self.schema_version.clone(),
            id: self.id.clone(),
            kind: self.kind,
            scope: self.scope,
            authority: self.authority,
            source: self.source.clone(),
            content_hash: self.content_hash.clone(),
            priority: self.priority,
            reference_count: self.references.len() as u64,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticAnchorDiagnostics {
    pub schema_version: String,
    pub id: String,
    pub kind: SemanticAnchorKind,
    pub scope: SemanticAnchorScope,
    pub authority: SemanticAnchorAuthority,
    pub source: String,
    pub content_hash: String,
    pub priority: i64,
    pub reference_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticAnchorRegistry {
    pub schema_version: String,
    pub registry_hash: String,
    pub anchors: Vec<SemanticAnchor>,
    pub input_count: u64,
    pub duplicate_count: u64,
    pub superseded_count: u64,
    pub rejected_count: u64,
}

impl Default for SemanticAnchorRegistry {
    fn default() -> Self {
        Self::build(Vec::new())
    }
}

impl SemanticAnchorRegistry {
    #[must_use]
    pub fn build(candidates: Vec<SemanticAnchor>) -> Self {
        let input_count = candidates.len() as u64;
        let mut winners = BTreeMap::<String, SemanticAnchor>::new();
        let mut duplicate_count = 0u64;
        let mut superseded_count = 0u64;
        let mut rejected_count = 0u64;
        for anchor in candidates {
            if anchor.validate().is_err() {
                rejected_count = rejected_count.saturating_add(1);
                continue;
            }
            let Some(existing) = winners.get(&anchor.id) else {
                winners.insert(anchor.id.clone(), anchor);
                continue;
            };
            if existing.content_hash == anchor.content_hash {
                duplicate_count = duplicate_count.saturating_add(1);
                if anchor_wins(&anchor, existing) {
                    winners.insert(anchor.id.clone(), anchor);
                }
                continue;
            }
            superseded_count = superseded_count.saturating_add(1);
            if anchor_wins(&anchor, existing) {
                winners.insert(anchor.id.clone(), anchor);
            }
        }
        let mut anchors = winners.into_values().collect::<Vec<_>>();
        anchors.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.id.cmp(&right.id))
        });
        let registry_hash = registry_hash(&anchors);
        Self {
            schema_version: SEMANTIC_ANCHOR_REGISTRY_SCHEMA_VERSION.to_string(),
            registry_hash,
            anchors,
            input_count,
            duplicate_count,
            superseded_count,
            rejected_count,
        }
    }

    #[must_use]
    pub fn merged(&self, candidates: Vec<SemanticAnchor>) -> Self {
        let mut merged = self.anchors.clone();
        merged.extend(candidates);
        Self::build(merged)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    #[must_use]
    pub fn is_current(&self) -> bool {
        self.schema_version == SEMANTIC_ANCHOR_REGISTRY_SCHEMA_VERSION
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.is_current() {
            return Err(format!(
                "unsupported semantic anchor registry schema: {}",
                self.schema_version
            ));
        }
        if self.registry_hash != registry_hash(&self.anchors) {
            return Err("semantic anchor registry hash mismatch".to_string());
        }
        if !is_sha1_hash(&self.registry_hash) {
            return Err("semantic anchor registry hash is not canonical sha1".to_string());
        }
        let accounted = (self.anchors.len() as u64)
            .saturating_add(self.duplicate_count)
            .saturating_add(self.superseded_count)
            .saturating_add(self.rejected_count);
        if self.input_count != accounted {
            return Err("semantic anchor registry counters are inconsistent".to_string());
        }
        let mut ids = BTreeSet::new();
        for anchor in &self.anchors {
            anchor.validate()?;
            if !ids.insert(anchor.id.as_str()) {
                return Err(format!("duplicate semantic anchor id: {}", anchor.id));
            }
        }
        let mut sorted = self.anchors.clone();
        sorted.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.id.cmp(&right.id))
        });
        if sorted != self.anchors {
            return Err("semantic anchor registry order is not canonical".to_string());
        }
        Ok(())
    }

    #[must_use]
    pub fn diagnostics(&self) -> SemanticAnchorRegistryDiagnostics {
        let mut kind_counts = BTreeMap::new();
        let mut authority_counts = BTreeMap::new();
        for anchor in &self.anchors {
            increment(&mut kind_counts, anchor.kind.as_str());
            increment(&mut authority_counts, anchor.authority.as_str());
        }
        SemanticAnchorRegistryDiagnostics {
            schema_version: self.schema_version.clone(),
            registry_hash: self.registry_hash.clone(),
            anchor_count: self.anchors.len() as u64,
            kind_counts,
            authority_counts,
            input_count: self.input_count,
            duplicate_count: self.duplicate_count,
            superseded_count: self.superseded_count,
            rejected_count: self.rejected_count,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticAnchorRegistryDiagnostics {
    pub schema_version: String,
    pub registry_hash: String,
    pub anchor_count: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub kind_counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub authority_counts: BTreeMap<String, u64>,
    pub input_count: u64,
    pub duplicate_count: u64,
    pub superseded_count: u64,
    pub rejected_count: u64,
}

impl Default for SemanticAnchorRegistryDiagnostics {
    fn default() -> Self {
        SemanticAnchorRegistry::default().diagnostics()
    }
}

impl SemanticAnchorRegistryDiagnostics {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor_count == 0
    }
}

#[must_use]
pub fn semantic_anchors_from_work_state(state: &WorkState, source: &str) -> Vec<SemanticAnchor> {
    let mut anchors = Vec::new();
    if !state.task.trim().is_empty() {
        anchors.push(SemanticAnchor::new(
            SemanticAnchorKind::Goal,
            "primary",
            state.task.clone(),
            SemanticAnchorAuthority::StructuredWorkState,
            SemanticAnchorScope::Session,
            source,
        ));
    }
    extend_text_anchors(
        &mut anchors,
        SemanticAnchorKind::Constraint,
        &state.constraints,
        source,
    );
    extend_text_anchors(
        &mut anchors,
        SemanticAnchorKind::Decision,
        &state.decisions,
        source,
    );
    extend_text_anchors(
        &mut anchors,
        SemanticAnchorKind::Progress,
        &state.progress,
        source,
    );
    for file in &state.files {
        if let Some(anchor) = file_anchor(file, source) {
            anchors.push(anchor);
        }
    }
    for values in [&state.tool_findings, &state.open_questions, &state.risks] {
        extend_text_anchors(
            &mut anchors,
            SemanticAnchorKind::CriticalContext,
            values,
            source,
        );
    }
    extend_text_anchors(
        &mut anchors,
        SemanticAnchorKind::CriticalContext,
        &state.critical_context,
        source,
    );
    extend_text_anchors(
        &mut anchors,
        SemanticAnchorKind::Blocker,
        &state.blockers,
        source,
    );
    extend_text_anchors(
        &mut anchors,
        SemanticAnchorKind::NextStep,
        &state.todos,
        source,
    );
    extend_text_anchors(
        &mut anchors,
        SemanticAnchorKind::NextStep,
        &state.next_steps,
        source,
    );
    anchors
}

fn extend_text_anchors(
    anchors: &mut Vec<SemanticAnchor>,
    kind: SemanticAnchorKind,
    values: &[String],
    source: &str,
) {
    anchors.extend(values.iter().filter_map(|value| {
        let content = normalize_content(value);
        (!content.is_empty()).then(|| {
            let key = content.clone();
            SemanticAnchor::new(
                kind,
                key,
                content,
                SemanticAnchorAuthority::StructuredWorkState,
                SemanticAnchorScope::Session,
                source,
            )
        })
    }));
}

fn file_anchor(file: &WorkStateFile, source: &str) -> Option<SemanticAnchor> {
    let path = file.path.trim();
    if path.is_empty() {
        return None;
    }
    let status = file.status.trim();
    let note = file.note.trim();
    let mut content = path.to_string();
    if !status.is_empty() {
        content.push_str(&format!(" [{status}]"));
    }
    if !note.is_empty() {
        content.push_str(&format!(": {note}"));
    }
    Some(
        SemanticAnchor::new(
            SemanticAnchorKind::File,
            path,
            content,
            SemanticAnchorAuthority::StructuredWorkState,
            SemanticAnchorScope::Session,
            source,
        )
        .with_references(vec![path.to_string()]),
    )
}

fn anchor_wins(candidate: &SemanticAnchor, existing: &SemanticAnchor) -> bool {
    (candidate.authority.rank(), candidate.priority)
        >= (existing.authority.rank(), existing.priority)
}

fn registry_hash(anchors: &[SemanticAnchor]) -> String {
    let serialized = serde_json::to_string(anchors).unwrap_or_default();
    format!("sha1:{}", sha1_hex(serialized.as_bytes()))
}

fn content_hash(content: &str) -> String {
    format!("sha1:{}", sha1_hex(normalize_content(content).as_bytes()))
}

fn normalize_content(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn normalize_identity(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_sha1_hash(value: &str) -> bool {
    value.len() == 45
        && value
            .strip_prefix("sha1:")
            .is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn sha1_hex(value: &[u8]) -> String {
    format!("{:x}", Sha1::digest(value))
}

fn increment(counts: &mut BTreeMap<String, u64>, key: &str) {
    *counts.entry(key.to_string()).or_default() += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_deterministic_and_uses_authority_then_latest_tie() {
        let old = SemanticAnchor::new(
            SemanticAnchorKind::Goal,
            "primary",
            "Old goal",
            SemanticAnchorAuthority::StructuredWorkState,
            SemanticAnchorScope::Session,
            "epoch:old",
        );
        let new = SemanticAnchor::new(
            SemanticAnchorKind::Goal,
            "primary",
            "New goal",
            SemanticAnchorAuthority::StructuredWorkState,
            SemanticAnchorScope::Session,
            "epoch:new",
        );
        let explicit = SemanticAnchor::new(
            SemanticAnchorKind::Goal,
            "primary",
            "Explicit goal",
            SemanticAnchorAuthority::Explicit,
            SemanticAnchorScope::Session,
            "api",
        );
        let registry = SemanticAnchorRegistry::build(vec![old, new, explicit.clone()]);
        assert!(registry.validate().is_ok());
        assert_eq!(registry.anchors, vec![explicit]);
        assert_eq!(registry.superseded_count, 2);
    }

    #[test]
    fn anchor_identity_is_source_independent_and_preserves_case_sensitive_paths() {
        let first = SemanticAnchor::new(
            SemanticAnchorKind::Decision,
            "registry   contract",
            "Use typed anchors",
            SemanticAnchorAuthority::Explicit,
            SemanticAnchorScope::Session,
            "api:first",
        );
        let moved = SemanticAnchor::new(
            SemanticAnchorKind::Decision,
            "registry contract",
            "Use typed anchors",
            SemanticAnchorAuthority::Explicit,
            SemanticAnchorScope::Session,
            "api:second",
        );
        assert_eq!(first.id, moved.id);

        let upper = SemanticAnchor::new(
            SemanticAnchorKind::File,
            "src/Foo.rs",
            "src/Foo.rs",
            SemanticAnchorAuthority::StructuredWorkState,
            SemanticAnchorScope::Session,
            "work_state",
        );
        let lower = SemanticAnchor::new(
            SemanticAnchorKind::File,
            "src/foo.rs",
            "src/foo.rs",
            SemanticAnchorAuthority::StructuredWorkState,
            SemanticAnchorScope::Session,
            "work_state",
        );
        assert_ne!(upper.id, lower.id);
    }

    #[test]
    fn work_state_maps_to_all_compaction_anchor_families() {
        let state = WorkState {
            task: "Ship semantic anchors".to_string(),
            constraints: vec!["Do not leak private context".to_string()],
            decisions: vec!["Use an append-only epoch snapshot".to_string()],
            files: vec![WorkStateFile {
                path: "src/core.rs".to_string(),
                status: "modified".to_string(),
                note: "ContextPack integration".to_string(),
            }],
            critical_context: vec!["Replay must remain deterministic".to_string()],
            blockers: vec!["None".to_string()],
            next_steps: vec!["Run golden tests".to_string()],
            ..WorkState::default()
        };
        let registry = SemanticAnchorRegistry::build(semantic_anchors_from_work_state(
            &state,
            "context_epoch:epoch-1",
        ));
        let kinds = registry
            .anchors
            .iter()
            .map(|anchor| anchor.kind)
            .collect::<BTreeSet<_>>();
        assert!(kinds.contains(&SemanticAnchorKind::Goal));
        assert!(kinds.contains(&SemanticAnchorKind::Constraint));
        assert!(kinds.contains(&SemanticAnchorKind::Decision));
        assert!(kinds.contains(&SemanticAnchorKind::File));
        assert!(kinds.contains(&SemanticAnchorKind::CriticalContext));
        assert!(kinds.contains(&SemanticAnchorKind::Blocker));
        assert!(kinds.contains(&SemanticAnchorKind::NextStep));
    }
}
